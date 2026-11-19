//! The upload studio (`/admin/items/{id}/media`): adding digital copies of an
//! item and tidying the ones already there. Uploads are streamed straight to
//! disk, one file per request, so films of many gigabytes never sit in
//! memory.

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};

use axum::Json;
use axum::Router;
use axum::extract::multipart::{Field, MultipartRejection};
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::{self, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use super::accounts::{ApiError, ApiResult};
use super::{Admin, AppError, AppState, Fields, Res, Viewer, html, see_other};
use crate::db::items::{self, Invalid};
use crate::db::media_files::{self, Direction, MediaKind, NewMediaFile};
use crate::digital;
use crate::views::admin as av;

/// Largest subtitle file accepted.
const MAX_SUBTITLE_BYTES: usize = 5 << 20;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/items/{id}/media", get(page))
        .route("/admin/items/{id}/media/list", get(list))
        .route("/admin/items/{id}/media/{mid}", post(update))
        .route("/admin/items/{id}/media/{mid}/move", post(shift))
        .route("/admin/items/{id}/media/{mid}/delete", post(delete))
        .route("/admin/items/{id}/media/{mid}/download", get(download))
        .route(
            "/admin/items/{id}/media/{mid}/subtitles",
            post(add_subtitle),
        )
        .route(
            "/admin/items/{id}/subtitles/{sid}/delete",
            post(delete_subtitle),
        )
}

/// The upload route, without the usual body limit (it enforces
/// `[media] max_upload_mb` itself while streaming).
pub fn upload_routes() -> Router<AppState> {
    Router::new().route(
        "/admin/items/{id}/media/upload",
        post(upload).layer(DefaultBodyLimit::disable()),
    )
}

#[derive(Deserialize, Default)]
pub struct PageParams {
    #[serde(default)]
    msg: String,
}

async fn studio_data(st: &AppState, id: i64) -> Res<av::StudioData> {
    st.db
        .read(move |c| {
            let Some(item) = items::get(c, id)? else {
                return Ok(None);
            };
            let files = media_files::for_item(c, id)?;
            let ids: Vec<i64> = files.iter().map(|m| m.id).collect();
            let subtitles = media_files::subtitles_for(c, &ids)?;
            Ok(Some(av::StudioData {
                item,
                files,
                subtitles,
            }))
        })
        .await?
        .ok_or(AppError::NotFound)
}

pub async fn page(
    State(st): State<AppState>,
    Admin(viewer): Admin,
    Path(id): Path<i64>,
    Query(p): Query<PageParams>,
) -> Res {
    let data = studio_data(&st, id).await?;
    let ctx = st.ctx(&viewer).await?;
    let flash = match p.msg.as_str() {
        "" => None,
        "saved" => Some("Saved."),
        "removed" => Some("Removed."),
        "subtitles" => Some("Subtitles added."),
        other => other.strip_prefix("err:"),
    };
    Ok(html(av::studio_page(
        &ctx,
        &data,
        flash,
        st.cfg.media.max_upload_bytes(),
        st.lookups.enabled(),
    )))
}

/// The list of files already uploaded (refreshed by the studio's script).
pub async fn list(State(st): State<AppState>, Admin(_): Admin, Path(id): Path<i64>) -> Res {
    let data = studio_data(&st, id).await?;
    Ok(html(av::media_list(&data)))
}

/// After a change: the refreshed list for htmx, or back to the page.
async fn after_change(st: &AppState, viewer: &Viewer, id: i64, msg: &str) -> Res {
    if viewer.htmx {
        let data = studio_data(st, id).await?;
        return Ok(html(av::media_list(&data)));
    }
    Ok(see_other(&format!(
        "/admin/items/{id}/media?msg={}",
        crate::views::urlencode(msg)
    )))
}

fn number(s: &str) -> Option<i64> {
    s.trim().parse().ok()
}

pub async fn update(
    State(st): State<AppState>,
    Admin(viewer): Admin,
    Path((id, mid)): Path<(i64, i64)>,
    form: Fields,
) -> Res {
    let (title, disc, track) = (
        form.get("title").to_owned(),
        number(form.get("disc")),
        number(form.get("track")),
    );
    let found = st
        .db
        .write(move |c| media_files::update(c, id, mid, &title, disc, track))
        .await?;
    if !found {
        return Err(AppError::NotFound);
    }
    after_change(&st, &viewer, id, "saved").await
}

pub async fn shift(
    State(st): State<AppState>,
    Admin(viewer): Admin,
    Path((id, mid)): Path<(i64, i64)>,
    form: Fields,
) -> Res {
    let dir = match form.get("dir") {
        "up" => Direction::Up,
        _ => Direction::Down,
    };
    st.db
        .write(move |c| media_files::shift(c, id, mid, dir))
        .await?;
    after_change(&st, &viewer, id, "saved").await
}

fn remove_files(dir: PathBuf, files: Vec<String>) {
    tokio::task::spawn_blocking(move || {
        for f in files.iter().filter(|f| crate::media::is_safe_name(f)) {
            if let Err(e) = std::fs::remove_file(dir.join(f))
                && e.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(file = %f, error = %e, "could not delete media file");
            }
        }
    });
}

pub async fn delete(
    State(st): State<AppState>,
    Admin(viewer): Admin,
    Path((id, mid)): Path<(i64, i64)>,
) -> Res {
    let files = st
        .db
        .write(move |c| media_files::remove(c, id, mid))
        .await?
        .ok_or(AppError::NotFound)?;
    remove_files(st.media_dir(), files);
    after_change(&st, &viewer, id, "removed").await
}

pub async fn delete_subtitle(
    State(st): State<AppState>,
    Admin(viewer): Admin,
    Path((id, sid)): Path<(i64, i64)>,
) -> Res {
    let file = st
        .db
        .write(move |c| media_files::remove_subtitle(c, id, sid))
        .await?
        .ok_or(AppError::NotFound)?;
    remove_files(st.media_dir(), vec![file]);
    after_change(&st, &viewer, id, "removed").await
}

fn wants_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/json"))
}

fn invalid(field: &str, msg: impl Into<String>) -> ApiError {
    let mut bad = Invalid::default();
    bad.add(field, msg);
    ApiError::Invalid(bad)
}

/// `GET /admin/items/{id}/media/{mid}/download`: any file of the item, as
/// a download (how the librarian gets archive files back).
pub async fn download(
    State(st): State<AppState>,
    Admin(_): Admin,
    Path((id, mid)): Path<(i64, i64)>,
    headers: HeaderMap,
) -> Res {
    let m = st
        .db
        .read(move |c| media_files::get(c, mid))
        .await?
        .filter(|m| m.item_id == id && crate::media::is_safe_name(&m.file))
        .ok_or(AppError::NotFound)?;
    let path = st.media_dir().join(&m.file);
    let mut res = super::stream::serve_file(&path, &m.mime, &m.original_name, &headers).await;
    super::stream::make_attachment(&mut res, &m.original_name);
    Ok(res)
}

/// Adds subtitles (.srt is converted to WebVTT) to a video of the item.
/// Answers JSON to the studio's script, or redirects a plain form.
pub async fn add_subtitle(
    State(st): State<AppState>,
    Admin(_): Admin,
    Path((id, mid)): Path<(i64, i64)>,
    headers: HeaderMap,
    form: Fields,
) -> Response {
    let json = wants_json(&headers);
    match save_subtitle(&st, id, mid, form).await {
        Ok(sid) if json => {
            (StatusCode::CREATED, Json(serde_json::json!({ "id": sid }))).into_response()
        }
        Ok(_) => see_other(&format!("/admin/items/{id}/media?msg=subtitles")),
        Err(e) if json => e.into_response(),
        Err(ApiError::NotFound) => AppError::NotFound.into_response(),
        Err(e) => {
            let msg = match e {
                ApiError::Invalid(bad) => bad.to_string(),
                ApiError::TooLarge(m) | ApiError::BadRequest(m) => m,
                other => return other.into_response(),
            };
            see_other(&format!(
                "/admin/items/{id}/media?msg={}",
                crate::views::urlencode(&format!("err:{msg}"))
            ))
        }
    }
}

async fn save_subtitle(st: &AppState, id: i64, mid: i64, form: Fields) -> ApiResult<i64> {
    let video = st
        .db
        .read(move |c| media_files::get(c, mid))
        .await?
        .filter(|m| m.item_id == id)
        .ok_or(ApiError::NotFound)?;
    if video.kind != MediaKind::Video {
        return Err(invalid("file", "Subtitles go with films and videos."));
    }
    let Some(upload) = form.files.iter().find(|f| f.name == "file") else {
        return Err(invalid("file", "Choose a subtitle file (.srt or .vtt)."));
    };
    if upload.bytes.len() > MAX_SUBTITLE_BYTES {
        return Err(ApiError::TooLarge(
            "That subtitle file is too big for a subtitle file.".into(),
        ));
    }
    let original = upload.file_name.clone();
    // Without a usable name, try both formats.
    let vtt = match digital::extension(&original).as_deref() {
        Some("srt" | "vtt") => digital::to_webvtt(&original, &upload.bytes),
        _ => digital::to_webvtt("x.vtt", &upload.bytes)
            .or_else(|_| digital::to_webvtt("x.srt", &upload.bytes)),
    }
    .map_err(|m| invalid("file", m))?;
    let language = match form.get("language").trim() {
        "" => digital::guess_language(&original).unwrap_or_else(|| "und".into()),
        l => digital::clean_language(l),
    };
    let label = match crate::util::clean(form.get("label"), 60) {
        l if l.is_empty() => digital::language_label(&language),
        l => l,
    };
    let stored = digital::random_name("vtt");
    let dir = st.media_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    tokio::fs::write(dir.join(&stored), vtt.as_bytes())
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    let file = stored.clone();
    match st
        .db
        .write(move |c| media_files::add_subtitle(c, mid, &language, &label, &file))
        .await
    {
        Ok(sid) => Ok(sid),
        Err(e) => {
            remove_files(dir, vec![stored]);
            Err(e.into())
        }
    }
}

/// Largest text field (title, kind, …) accepted alongside an upload.
const MAX_TEXT_FIELD_BYTES: usize = 4 << 10;
/// Most text fields accepted alongside an upload.
const MAX_TEXT_FIELDS: usize = 32;
/// Characters of a text field kept.
const TEXT_FIELD_CHARS: usize = 1000;

/// Reads one non-file multipart field, at most [`MAX_TEXT_FIELD_BYTES`]
/// (chunk by chunk, so a huge field is refused without being held in
/// memory), keeping its first [`TEXT_FIELD_CHARS`] characters.
async fn read_text_field(field: &mut Field<'_>) -> ApiResult<String> {
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|e| ApiError::BadRequest(e.body_text()))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_TEXT_FIELD_BYTES {
            return Err(ApiError::TooLarge(format!(
                "The {} field is too long (at most {} KB).",
                field.name().unwrap_or("form"),
                MAX_TEXT_FIELD_BYTES >> 10
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| ApiError::BadRequest("A form field isn't valid UTF-8 text.".into()))?;
    Ok(text.chars().take(TEXT_FIELD_CHARS).collect())
}

/// Deletes an upload's file unless told it is safe: while it is streamed
/// (`….part`), and after it is renamed until its database row exists. A
/// request dropped half way (the client went away, the server is shutting
/// down) so leaves nothing behind.
struct PendingFile {
    path: Option<PathBuf>,
}

impl PendingFile {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    /// The file is now at `path` (after a rename); delete that instead.
    fn moved_to(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    /// The file is recorded: keep it.
    fn keep(mut self) {
        self.path = None;
    }
}

impl Drop for PendingFile {
    fn drop(&mut self) {
        if let Some(path) = self.path.take()
            && let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(file = %path.display(), error = %e, "could not delete an unfinished upload");
        }
    }
}

/// Deletes uploads left half-written (`*.part`) in the media folder by a
/// server that stopped mid-upload. Run at start-up, before any upload can
/// be in progress. Returns how many were removed.
pub fn sweep_partial_uploads(media_dir: &FsPath) -> std::io::Result<usize> {
    let entries = match std::fs::read_dir(media_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let is_partial = name.to_str().is_some_and(|n| n.ends_with(".part"));
        if is_partial && entry.file_type()?.is_file() {
            std::fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Streams one multipart file field to `path`, refusing more than `max`
/// bytes, and works out its SHA-256 on the way. Returns the size and the
/// digest (lower-case hex). The partial file is removed on any failure.
async fn receive(field: &mut Field<'_>, path: &FsPath, max: u64) -> ApiResult<(u64, String)> {
    let result = async {
        let mut out = tokio::fs::File::create(path)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
        let mut hasher = Sha256::new();
        let mut size: u64 = 0;
        while let Some(chunk) = field.chunk().await.map_err(|e| {
            ApiError::BadRequest(format!("The upload was interrupted: {}", e.body_text()))
        })? {
            size = size.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
            if size > max {
                return Err(too_large(max));
            }
            hasher.update(&chunk);
            out.write_all(&chunk)
                .await
                .map_err(|e| ApiError::Internal(e.into()))?;
        }
        out.flush()
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
        Ok((size, super::rip::hex(&hasher.finalize())))
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(path).await;
    }
    result
}

fn too_large(max: u64) -> ApiError {
    ApiError::TooLarge(format!(
        "That file is bigger than the {} MB limit (set by [media] max_upload_mb).",
        max / (1024 * 1024)
    ))
}

/// A stored upload waiting for its database row.
struct Received {
    /// Deletes the stored file unless the row is written.
    file: PendingFile,
    stored: String,
    original: String,
    kind: MediaKind,
    mime: &'static str,
    size: u64,
    sha256: String,
}

/// A media file just added.
#[derive(Serialize)]
pub struct Created {
    pub id: i64,
    pub title: String,
    pub kind: MediaKind,
}

/// The `sha256` field, if sent: 64 hex digits (any case).
fn expected_sha256(meta: &HashMap<String, String>) -> ApiResult<Option<String>> {
    match meta.get("sha256").map(|h| h.trim().to_ascii_lowercase()) {
        None => Ok(None),
        Some(h) if h.is_empty() => Ok(None),
        Some(h) if media_files::is_sha256_hex(&h) => Ok(Some(h)),
        Some(_) => Err(invalid(
            "sha256",
            "That isn't a SHA-256 checksum (64 hexadecimal digits).",
        )),
    }
}

/// `POST /admin/items/{id}/media/upload`: one file plus its details
/// (`kind`, `title`, `disc`, `track`, `duration_ms`, sent before the file).
pub async fn upload(
    State(st): State<AppState>,
    Admin(_): Admin,
    Path(id): Path<i64>,
    headers: HeaderMap,
    multipart: Result<Multipart, MultipartRejection>,
) -> ApiResult {
    let created = receive_media(&st, id, &headers, multipart, false).await?;
    Ok((StatusCode::CREATED, Json(created)).into_response())
}

/// Receives one media file for item `id` (shared by the studio and the rip
/// API): text fields `kind`, `title`, `disc`, `track`, `duration_ms` and
/// `sha256` come before the `file`, which is streamed to disk while its
/// SHA-256 is worked out. A `sha256` that doesn't match refuses the file;
/// with `require_sha256` (Library Rip) one must be sent.
pub(super) async fn receive_media(
    st: &AppState,
    id: i64,
    headers: &HeaderMap,
    multipart: Result<Multipart, MultipartRejection>,
    require_sha256: bool,
) -> ApiResult<Created> {
    let max = st.cfg.media.max_upload_bytes();
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    // Allow a little for the form's own framing around the file.
    if declared.is_some_and(|n| n > max.saturating_add(64 * 1024)) {
        return Err(too_large(max));
    }
    let exists = st
        .db
        .read(move |c| Ok(items::get(c, id)?.is_some()))
        .await?;
    if !exists {
        return Err(ApiError::NotFound);
    }
    let mut mp = multipart.map_err(|e| ApiError::BadRequest(e.body_text()))?;
    let dir = st.media_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    let mut meta: HashMap<String, String> = HashMap::new();
    let mut received: Option<Received> = None;
    let outcome: ApiResult<()> = async {
        while let Some(mut field) = mp
            .next_field()
            .await
            .map_err(|e| ApiError::BadRequest(e.body_text()))?
        {
            let name = field.name().unwrap_or("").to_owned();
            let Some(file_name) = field.file_name().map(str::to_owned) else {
                if meta.len() >= MAX_TEXT_FIELDS && !meta.contains_key(&name) {
                    return Err(ApiError::BadRequest(
                        "Too many form fields came with the file.".into(),
                    ));
                }
                let text = read_text_field(&mut field).await?;
                meta.insert(name, text);
                continue;
            };
            if name != "file" || received.is_some() {
                return Err(ApiError::BadRequest("Send one file per upload.".into()));
            }
            let expected = expected_sha256(&meta)?;
            if require_sha256 && expected.is_none() {
                return Err(invalid(
                    "sha256",
                    "Send the file's SHA-256 (the sha256 field) before the file.",
                ));
            }
            let wanted = match meta.get("kind").map(|k| k.trim()) {
                None | Some("") => None,
                Some(k) => Some(MediaKind::parse(k).ok_or_else(|| {
                    invalid(
                        "kind",
                        "The kind should be audio, video, document or archive.",
                    )
                })?),
            };
            let (kind, mime) =
                digital::classify(&file_name, wanted, field.content_type().unwrap_or(""))
                    .map_err(|m| invalid("file", m))?;
            let ext = digital::extension(&file_name).unwrap_or_default();
            let stored = digital::random_name(&ext);
            let partial = dir.join(format!("{stored}.part"));
            // Deletes the file on every way out but success, including the
            // request being dropped mid-write.
            let mut file = PendingFile::new(partial.clone());
            let (size, sha256) = receive(&mut field, &partial, max).await?;
            if size == 0 {
                return Err(invalid("file", "That file is empty."));
            }
            if expected.as_deref().is_some_and(|e| e != sha256) {
                return Err(invalid(
                    "sha256",
                    "The file arrived damaged: its SHA-256 doesn't match the one sent. Send it again.",
                ));
            }
            let final_path = dir.join(&stored);
            tokio::fs::rename(&partial, &final_path)
                .await
                .map_err(|e| ApiError::Internal(e.into()))?;
            file.moved_to(final_path);
            let original = file_name
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&file_name)
                .to_owned();
            received = Some(Received {
                file,
                stored,
                original,
                kind,
                mime,
                size,
                sha256,
            });
        }
        Ok(())
    }
    .await;
    // On failure, dropping `received` deletes its file.
    outcome?;
    let Some(r) = received else {
        return Err(invalid("file", "No file arrived. Choose a file to upload."));
    };
    let get = |k: &str| meta.get(k).map(String::as_str).unwrap_or("");
    let new = NewMediaFile {
        item_id: id,
        kind: r.kind,
        title: media_files::clean_title(get("title"), digital::stem(&r.original)),
        disc: number(get("disc")).unwrap_or(1),
        track: number(get("track")),
        file: r.stored.clone(),
        original_name: r.original.clone(),
        mime: r.mime.to_owned(),
        size_bytes: i64::try_from(r.size).unwrap_or(i64::MAX),
        duration_ms: get("duration_ms")
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|d| d.is_finite() && *d >= 0.0 && *d < 1e12)
            .map(|d| d.round() as i64),
        sha256: Some(r.sha256),
    };
    let title = new.title.clone();
    let kind = new.kind;
    let pending = r.file;
    match st.db.write(move |c| media_files::add(c, &new)).await {
        Ok(mid) => {
            pending.keep();
            tracing::info!(
                item = id,
                media = mid,
                bytes = r.size,
                "digital copy uploaded"
            );
            Ok(Created {
                id: mid,
                title,
                kind,
            })
        }
        // Dropping `pending` deletes the file.
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_files_are_deleted_unless_kept() {
        let dir = tempfile::tempdir().unwrap();
        let partial = dir.path().join("a.mp3.part");
        let stored = dir.path().join("a.mp3");

        // Dropped while streaming.
        std::fs::write(&partial, b"x").unwrap();
        drop(PendingFile::new(partial.clone()));
        assert!(!partial.exists());

        // Renamed, then dropped before the database row was written.
        std::fs::write(&partial, b"x").unwrap();
        let mut file = PendingFile::new(partial.clone());
        std::fs::rename(&partial, &stored).unwrap();
        file.moved_to(stored.clone());
        drop(file);
        assert!(!stored.exists());

        // Recorded: kept.
        std::fs::write(&stored, b"x").unwrap();
        PendingFile::new(stored.clone()).keep();
        assert!(stored.exists());

        // Already gone: nothing to do.
        drop(PendingFile::new(dir.path().join("missing.part")));
    }

    #[test]
    fn start_up_sweeps_only_partial_uploads() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("media");
        assert_eq!(sweep_partial_uploads(&media).unwrap(), 0, "no folder yet");
        std::fs::create_dir_all(media.join("keep.part")).unwrap();
        for f in ["a.flac.part", "b.mp4.part", "c.flac", "d.vtt", "part"] {
            std::fs::write(media.join(f), b"x").unwrap();
        }
        assert_eq!(sweep_partial_uploads(&media).unwrap(), 2);
        let mut left: Vec<String> = std::fs::read_dir(&media)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["c.flac", "d.vtt", "keep.part", "part"]);
    }
}
