// SPDX-License-Identifier: AGPL-3.0-or-later
//! The rip API client.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::StreamExt;
use reqwest::multipart::{Form, Part};
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use tokio_util::io::ReaderStream;
use url::Url;

use super::api::{
    ApiKey, CreatedItem, CreatedMedia, ErrorBody, ExistingMedia, ItemSummary, ItemType,
    LibraryInfo, WhoAmI,
};
use super::{LibraryError, REQUEST_TIMEOUT, Result, user_agent};
use crate::model::{CoverArt, NewItem, Sha256Digest, UploadFile};

/// Size of each chunk read from disk while uploading.
const UPLOAD_CHUNK: usize = 256 * 1024;
/// Longest error body we read for a message.
const ERROR_BODY_LIMIT: usize = 16 * 1024;
/// Longest message taken from a plain-text error body.
const ERROR_MESSAGE_LIMIT: usize = 300;

/// Upload progress for one file.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct UploadProgress {
    /// Bytes sent.
    pub sent: u64,
    /// File size.
    pub total: u64,
}

impl UploadProgress {
    /// Fraction done, 0.0–1.0 (1.0 for an empty file).
    pub fn fraction(self) -> f32 {
        if self.total == 0 {
            1.0
        } else {
            // Precision loss is fine for a progress bar.
            (self.sent as f64 / self.total as f64).clamp(0.0, 1.0) as f32
        }
    }
}

/// What [`LibraryClient::upload_files`] did.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct UploadSummary {
    /// Files sent, in order, with their new ids.
    pub uploaded: Vec<CreatedMedia>,
    /// Files skipped because the item already had them (same SHA-256).
    pub skipped: usize,
}

/// A client for one library. Cheap to clone.
#[derive(Clone, Debug)]
pub struct LibraryClient {
    http: reqwest::Client,
    base: Url,
    key: Option<ApiKey>,
}

impl LibraryClient {
    /// A client for `base` (the library's address); `key` is needed for
    /// everything except [`LibraryClient::info`] and the connect flow.
    ///
    /// `base` may include a path (a library behind a reverse proxy at
    /// `/library/`); a trailing slash is added, and any query or fragment
    /// dropped. Use [`super::http_client`] for `http`.
    pub fn new(http: reqwest::Client, base: Url, key: Option<ApiKey>) -> Self {
        LibraryClient {
            http,
            base: normalise_base(base),
            key,
        }
    }

    /// The same library with `key` (after connecting).
    pub fn with_key(&self, key: ApiKey) -> Self {
        LibraryClient {
            http: self.http.clone(),
            base: self.base.clone(),
            key: Some(key),
        }
    }

    /// Whether this client has a key.
    pub fn is_connected(&self) -> bool {
        self.key.is_some()
    }

    /// The library's base address (always ending in `/`).
    pub fn base(&self) -> &Url {
        &self.base
    }

    /// `path` (no leading slash) under the base address.
    pub(crate) fn endpoint(&self, path: &str) -> Result<Url> {
        Ok(self.base.join(path)?)
    }

    /// A request with our user agent and the JSON timeout.
    pub(crate) fn request(&self, method: reqwest::Method, url: Url) -> RequestBuilder {
        self.http
            .request(method, url)
            .header(reqwest::header::USER_AGENT, user_agent())
            .timeout(REQUEST_TIMEOUT)
    }

    fn authorised(&self, method: reqwest::Method, path: &str) -> Result<RequestBuilder> {
        let key = self.key.as_ref().ok_or(LibraryError::NotConnected)?;
        Ok(self
            .request(method, self.endpoint(path)?)
            .bearer_auth(key.expose()))
    }

    /// `GET /api/rip/info`, checking our version against `minRipperVersion`.
    ///
    /// Anything that is not a library (a 404, an HTML page, another
    /// server) gives [`LibraryError::NotALibrary`]; a library that needs a
    /// newer ripper gives [`LibraryError::TooOld`].
    pub async fn info(&self) -> Result<LibraryInfo> {
        let response = self
            .request(reqwest::Method::GET, self.endpoint("api/rip/info")?)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(match response.status() {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => {
                    LibraryError::NotALibrary(self.base.to_string())
                }
                _ => error_from(response).await,
            });
        }
        let bytes = response.bytes().await?;
        let info: LibraryInfo = serde_json::from_slice(&bytes)
            .map_err(|_| LibraryError::NotALibrary(self.base.to_string()))?;
        check_version(&info.min_ripper_version, crate::VERSION)?;
        Ok(info)
    }

    /// `GET /api/rip/whoami`.
    pub async fn whoami(&self) -> Result<WhoAmI> {
        let response = self
            .authorised(reqwest::Method::GET, "api/rip/whoami")?
            .send()
            .await?;
        read_json(check(response).await?).await
    }

    /// `GET /api/rip/types`.
    pub async fn types(&self) -> Result<Vec<ItemType>> {
        let response = self
            .authorised(reqwest::Method::GET, "api/rip/types")?
            .send()
            .await?;
        read_json(check(response).await?).await
    }

    /// `GET /api/rip/items?q=&type=`.
    pub async fn search_items(&self, query: &str, type_slug: &str) -> Result<Vec<ItemSummary>> {
        let response = self
            .authorised(reqwest::Method::GET, "api/rip/items")?
            .query(&[("q", query), ("type", type_slug)])
            .send()
            .await?;
        read_json(check(response).await?).await
    }

    /// `POST /api/rip/items`.
    pub async fn create_item(&self, item: &NewItem) -> Result<CreatedItem> {
        let response = self
            .authorised(reqwest::Method::POST, "api/rip/items")?
            .json(item)
            .send()
            .await?;
        read_json(check(response).await?).await
    }

    /// `POST /api/rip/items/{id}/photos` (front cover).
    pub async fn upload_photo(&self, item_id: i64, cover: &CoverArt) -> Result<()> {
        let part = Part::bytes(cover.bytes.to_vec())
            .file_name(format!("cover.{}", image_extension(&cover.mime)))
            .mime_str(&cover.mime)?;
        let response = self
            .authorised(
                reqwest::Method::POST,
                &format!("api/rip/items/{item_id}/photos"),
            )?
            .timeout(super::READ_TIMEOUT)
            .multipart(Form::new().part("file", part))
            .send()
            .await?;
        check(response).await?;
        Ok(())
    }

    /// `GET /api/rip/items/{id}/media`: everything already on an item.
    pub async fn media_on_item(&self, item_id: i64) -> Result<Vec<ExistingMedia>> {
        let response = self
            .authorised(
                reqwest::Method::GET,
                &format!("api/rip/items/{item_id}/media"),
            )?
            .send()
            .await?;
        read_json(check(response).await?).await
    }

    /// `GET /api/rip/items/{id}/media`: SHA-256s of the files already on
    /// an item, so an interrupted upload resumes by skipping them.
    pub async fn existing_media(&self, item_id: i64) -> Result<Vec<Sha256Digest>> {
        Ok(self
            .media_on_item(item_id)
            .await?
            .into_iter()
            .filter_map(|media| media.sha256)
            .collect())
    }

    /// `POST /api/rip/items/{id}/media`, streaming the file with progress.
    ///
    /// The file is hashed as it streams; if it no longer matches
    /// `file.sha256` the upload is abandoned with
    /// [`LibraryError::FileChanged`] (the server would refuse it anyway).
    /// `track` and `duration_ms` are left out when `None`.
    pub async fn upload_media(
        &self,
        item_id: i64,
        file: &UploadFile,
        progress: &(dyn Fn(UploadProgress) + Send + Sync),
    ) -> Result<CreatedMedia> {
        let key = self.key.as_ref().ok_or(LibraryError::NotConnected)?;
        let io_error = |source| LibraryError::Io {
            path: file.path.clone(),
            source,
        };
        let handle = tokio::fs::File::open(&file.path).await.map_err(io_error)?;
        let total = handle.metadata().await.map_err(io_error)?.len();

        let (sent_tx, mut sent_rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
        let changed = Arc::new(AtomicBool::new(false));
        let stream = hashing_stream(handle, file.sha256, sent_tx, Arc::clone(&changed));
        let part = Part::stream_with_length(reqwest::Body::wrap_stream(stream), total)
            .file_name(file_name(&file.path))
            .mime_str(media_mime(&file.path))?;

        let mut form = Form::new()
            .text("kind", file.kind.as_str())
            .text("title", file.title.clone())
            .text("disc", file.disc.to_string());
        if let Some(track) = file.track {
            form = form.text("track", track.get().to_string());
        }
        if let Some(duration) = file.duration_ms {
            form = form.text("duration_ms", duration.to_string());
        }
        form = form.text("sha256", file.sha256.to_hex()).part("file", part);

        // No overall timeout: big files on slow links take a while. The
        // client's read timeout still catches a stalled connection.
        let send = self
            .http
            .post(self.endpoint(&format!("api/rip/items/{item_id}/media"))?)
            .header(reqwest::header::USER_AGENT, user_agent())
            .bearer_auth(key.expose())
            .multipart(form)
            .send();
        progress(UploadProgress { sent: 0, total });
        tokio::pin!(send);
        let result = loop {
            tokio::select! {
                result = &mut send => break result,
                Some(sent) = sent_rx.recv() => progress(UploadProgress { sent, total }),
            }
        };
        while let Ok(sent) = sent_rx.try_recv() {
            progress(UploadProgress { sent, total });
        }
        if changed.load(Ordering::SeqCst) {
            return Err(LibraryError::FileChanged(file.path.clone()));
        }
        read_json(check(result?).await?).await
    }

    /// Uploads `files` in order to item `item_id`, skipping any whose
    /// SHA-256 is already on the item (so an interrupted upload resumes).
    /// `progress` gets every file's progress; a skipped file reports
    /// `sent == total` once.
    pub async fn upload_files(
        &self,
        item_id: i64,
        files: &[UploadFile],
        progress: &(dyn Fn(&UploadFile, UploadProgress) + Send + Sync),
    ) -> Result<UploadSummary> {
        let mut present: HashSet<Sha256Digest> =
            self.existing_media(item_id).await?.into_iter().collect();
        let mut summary = UploadSummary::default();
        for file in files {
            if present.contains(&file.sha256) {
                let total = tokio::fs::metadata(&file.path)
                    .await
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                progress(file, UploadProgress { sent: total, total });
                summary.skipped += 1;
                continue;
            }
            let created = self
                .upload_media(item_id, file, &|step| progress(file, step))
                .await?;
            present.insert(file.sha256);
            summary.uploaded.push(created);
        }
        Ok(summary)
    }

    /// `DELETE /api/rip/token`: revokes our own key.
    pub async fn revoke(&self) -> Result<()> {
        let response = self
            .authorised(reqwest::Method::DELETE, "api/rip/token")?
            .send()
            .await?;
        check(response).await?;
        Ok(())
    }
}

/// Adds a trailing slash and drops query and fragment, so `join` keeps
/// any path prefix.
pub(crate) fn normalise_base(mut base: Url) -> Url {
    base.set_query(None);
    base.set_fragment(None);
    if !base.path().ends_with('/') {
        let path = format!("{}/", base.path());
        base.set_path(&path);
    }
    base
}

/// Passes successful responses through; turns the rest into errors.
pub(crate) async fn check(response: Response) -> Result<Response> {
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(error_from(response).await)
    }
}

/// The error for an unsuccessful response, with the body's message.
pub(crate) async fn error_from(response: Response) -> LibraryError {
    let status = response.status();
    let message = error_message(response).await;
    match status {
        StatusCode::UNAUTHORIZED => LibraryError::Unauthorised,
        StatusCode::PAYLOAD_TOO_LARGE => LibraryError::TooLarge(message),
        StatusCode::UNPROCESSABLE_ENTITY => LibraryError::Rejected(message),
        other => LibraryError::Status {
            status: other.as_u16(),
            message,
        },
    }
}

/// `{"error": …, "fields": {…}}` → "error (field: message; …)", or the
/// start of a plain-text body, or the status's reason phrase.
pub(crate) async fn error_message(response: Response) -> String {
    let status = response.status();
    let fallback = || {
        status
            .canonical_reason()
            .unwrap_or("no explanation given")
            .to_owned()
    };
    let Ok(bytes) = response.bytes().await else {
        return fallback();
    };
    let bytes = bytes.get(..ERROR_BODY_LIMIT).unwrap_or(&bytes);
    if let Ok(body) = serde_json::from_slice::<ErrorBody>(bytes) {
        return match body.fields.filter(|fields| !fields.is_empty()) {
            Some(fields) => {
                let details: Vec<String> = fields
                    .iter()
                    .map(|(field, message)| format!("{field}: {message}"))
                    .collect();
                format!("{} ({})", body.error, details.join("; "))
            }
            None => body.error,
        };
    }
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    if text.is_empty() || text.starts_with('<') {
        fallback()
    } else {
        text.chars().take(ERROR_MESSAGE_LIMIT).collect()
    }
}

/// Reads a JSON body, reporting bad JSON as [`LibraryError::Unexpected`].
pub(crate) async fn read_json<T: DeserializeOwned>(response: Response) -> Result<T> {
    let bytes = response.bytes().await?;
    serde_json::from_slice(&bytes).map_err(|error| LibraryError::Unexpected(error.to_string()))
}

/// Fails when `current` is older than `required` (`major.minor.patch`,
/// pre-release suffixes ignored).
fn check_version(required: &str, current: &str) -> Result<()> {
    let parse = |text: &str| -> Option<(u64, u64, u64)> {
        let core = text.trim().split(['-', '+']).next()?;
        let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
        let major = parts.next()??;
        let minor = parts.next().unwrap_or(Some(0))?;
        let patch = parts.next().unwrap_or(Some(0))?;
        Some((major, minor, patch))
    };
    let not_a_version =
        |text: &str| LibraryError::Unexpected(format!("\"{text}\" is not a version number"));
    let wanted = parse(required).ok_or_else(|| not_a_version(required))?;
    let ours = parse(current).ok_or_else(|| not_a_version(current))?;
    if ours < wanted {
        Err(LibraryError::TooOld {
            required: required.to_owned(),
            current: current.to_owned(),
        })
    } else {
        Ok(())
    }
}

/// The chunks a file is read in.
type Chunk = <ReaderStream<tokio::fs::File> as futures_util::Stream>::Item;

/// Streams a file in chunks, sending the running byte count to `sent` and
/// hashing as it goes. At the end, if the digest is not `expected`, it
/// sets `changed` and fails the stream so the request is abandoned.
fn hashing_stream(
    file: tokio::fs::File,
    expected: Sha256Digest,
    sent: tokio::sync::mpsc::UnboundedSender<u64>,
    changed: Arc<AtomicBool>,
) -> impl futures_util::Stream<Item = Chunk> + Send + 'static {
    struct State {
        chunks: ReaderStream<tokio::fs::File>,
        hasher: Option<Sha256>,
        count: u64,
        sent: tokio::sync::mpsc::UnboundedSender<u64>,
        changed: Arc<AtomicBool>,
        expected: Sha256Digest,
    }
    let state = State {
        chunks: ReaderStream::with_capacity(file, UPLOAD_CHUNK),
        hasher: Some(Sha256::new()),
        count: 0,
        sent,
        changed,
        expected,
    };
    futures_util::stream::unfold(state, |mut state| async move {
        match state.chunks.next().await {
            Some(Ok(chunk)) => {
                if let Some(hasher) = state.hasher.as_mut() {
                    hasher.update(&chunk);
                }
                let length = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
                state.count = state.count.saturating_add(length);
                // The receiver is gone only if the request already ended.
                let _ = state.sent.send(state.count);
                Some((Ok(chunk), state))
            }
            Some(Err(error)) => {
                state.hasher = None;
                Some((Err(error), state))
            }
            None => {
                let hasher = state.hasher.take()?;
                let digest: [u8; 32] = hasher.finalize().into();
                if Sha256Digest(digest) == state.expected {
                    None
                } else {
                    state.changed.store(true, Ordering::SeqCst);
                    Some((
                        Err(std::io::Error::other("the file changed while uploading")),
                        state,
                    ))
                }
            }
        }
    })
}

/// The file's name for the multipart `filename`.
fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file")
        .to_owned()
}

/// The MIME type for an upload, from its extension.
fn media_mime(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("flac") => "audio/flac",
        Some("log" | "cue" | "txt") => "text/plain; charset=utf-8",
        Some("json") => "application/json",
        Some("pdf") => "application/pdf",
        Some("mkv") => "video/x-matroska",
        Some("webm") => "video/webm",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

/// A file extension for an image MIME type.
fn image_extension(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "jpg",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_keeps_its_path() {
        let base = normalise_base(Url::parse("https://example.org/library?x=1").unwrap());
        assert_eq!(base.as_str(), "https://example.org/library/");
        assert_eq!(
            base.join("api/rip/info").unwrap().as_str(),
            "https://example.org/library/api/rip/info"
        );
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(check_version("0.1.0", "0.1.0").is_ok());
        assert!(check_version("0.1", "0.10.0").is_ok());
        assert!(check_version("0.9.0", "0.10.0").is_ok());
        assert!(check_version("0.2.0-beta", "0.2.0").is_ok());
        assert!(matches!(
            check_version("1.0.0", "0.9.9"),
            Err(LibraryError::TooOld { .. })
        ));
        assert!(matches!(
            check_version("soon", "0.1.0"),
            Err(LibraryError::Unexpected(_))
        ));
    }

    #[test]
    fn mime_types_follow_extensions() {
        assert_eq!(media_mime(Path::new("a/01.FLAC")), "audio/flac");
        assert_eq!(media_mime(Path::new("rip.json")), "application/json");
        assert_eq!(
            media_mime(Path::new("rip.log")),
            "text/plain; charset=utf-8"
        );
        assert_eq!(media_mime(Path::new("x")), "application/octet-stream");
    }

    #[test]
    fn progress_fraction() {
        assert_eq!(UploadProgress { sent: 0, total: 0 }.fraction(), 1.0);
        assert_eq!(UploadProgress { sent: 5, total: 10 }.fraction(), 0.5);
    }
}
