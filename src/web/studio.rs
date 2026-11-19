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

#[derive(Serialize)]
struct Created {
    id: i64,
    title: String,
    kind: MediaKind,
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

/// Streams one multipart file field to `path`, refusing more than `max`
/// bytes. The partial file is removed on any failure.
async fn receive(field: &mut Field<'_>, path: &FsPath, max: u64) -> ApiResult<u64> {
    let result = async {
        let mut out = tokio::fs::File::create(path)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
        let mut size: u64 = 0;
        while let Some(chunk) = field.chunk().await.map_err(|e| {
            ApiError::BadRequest(format!("The upload was interrupted: {}", e.body_text()))
        })? {
            size = size.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
            if size > max {
                return Err(too_large(max));
            }
            out.write_all(&chunk)
                .await
                .map_err(|e| ApiError::Internal(e.into()))?;
        }
        out.flush()
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
        Ok(size)
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
    stored: String,
    original: String,
    kind: MediaKind,
    mime: &'static str,
    size: u64,
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
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::BadRequest(e.body_text()))?;
                meta.insert(name, text.chars().take(1000).collect());
                continue;
            };
            if name != "file" || received.is_some() {
                return Err(ApiError::BadRequest("Send one file per upload.".into()));
            }
            let wanted = meta.get("kind").and_then(|k| MediaKind::parse(k));
            let (kind, mime) =
                digital::classify(&file_name, wanted, field.content_type().unwrap_or(""))
                    .map_err(|m| invalid("file", m))?;
            let ext = digital::extension(&file_name).unwrap_or_default();
            let stored = digital::random_name(&ext);
            let partial = dir.join(format!("{stored}.part"));
            let size = receive(&mut field, &partial, max).await?;
            if size == 0 {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(invalid("file", "That file is empty."));
            }
            tokio::fs::rename(&partial, dir.join(&stored))
                .await
                .map_err(|e| ApiError::Internal(e.into()))?;
            let original = file_name
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&file_name)
                .to_owned();
            received = Some(Received {
                stored,
                original,
                kind,
                mime,
                size,
            });
        }
        Ok(())
    }
    .await;
    if let Err(e) = outcome {
        if let Some(r) = received {
            remove_files(dir, vec![r.stored]);
        }
        return Err(e);
    }
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
    };
    let title = new.title.clone();
    let kind = new.kind;
    match st.db.write(move |c| media_files::add(c, &new)).await {
        Ok(mid) => {
            tracing::info!(
                item = id,
                media = mid,
                bytes = r.size,
                "digital copy uploaded"
            );
            Ok((
                StatusCode::CREATED,
                Json(Created {
                    id: mid,
                    title,
                    kind,
                }),
            )
                .into_response())
        }
        Err(e) => {
            remove_files(dir, vec![r.stored]);
            Err(e.into())
        }
    }
}
