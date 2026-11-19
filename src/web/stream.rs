//! Streaming digital copies: byte ranges (so players can seek), validators
//! (ETag, Last-Modified) and subtitles. Files are read from disk in chunks,
//! never loaded whole, and served exactly as uploaded.

use std::path::Path as FsPath;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::http::{HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use super::AppState;
use super::accounts::{ApiError, ApiResult, SignedIn};
use super::api::visible_media;
use crate::db::media_files;

const CACHE: &str = "private, max-age=0, must-revalidate";
const CHUNK: usize = 64 * 1024;

/// What a `Range` header asks for, given the file length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeRequest {
    /// No usable range: send the whole file.
    Full,
    /// Bytes `start..=end`.
    Partial { start: u64, end: u64 },
    /// A range that lies outside the file (416).
    Unsatisfiable,
}

/// Parses a single `bytes=` range (`a-b`, `a-` or `-n`). Several ranges, other
/// units and malformed headers are ignored (the whole file is sent), as the
/// HTTP spec allows.
pub fn parse_range(header: Option<&str>, len: u64) -> RangeRequest {
    let Some(value) = header else {
        return RangeRequest::Full;
    };
    let Some((unit, spec)) = value.trim().split_once('=') else {
        return RangeRequest::Full;
    };
    if !unit.trim().eq_ignore_ascii_case("bytes") || spec.contains(',') {
        return RangeRequest::Full;
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return RangeRequest::Full;
    };
    let (first, last) = (first.trim(), last.trim());
    let number = |s: &str| -> Option<u64> {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    if first.is_empty() {
        let Some(n) = number(last) else {
            return RangeRequest::Full;
        };
        if n == 0 || len == 0 {
            return RangeRequest::Unsatisfiable;
        }
        return RangeRequest::Partial {
            start: len.saturating_sub(n),
            end: len - 1,
        };
    }
    let Some(start) = number(first) else {
        return RangeRequest::Full;
    };
    let end = if last.is_empty() {
        None
    } else {
        match number(last) {
            Some(e) => Some(e),
            None => return RangeRequest::Full,
        }
    };
    if end.is_some_and(|e| e < start) {
        return RangeRequest::Full;
    }
    if start >= len {
        return RangeRequest::Unsatisfiable;
    }
    RangeRequest::Partial {
        start,
        end: end.map_or(len - 1, |e| e.min(len - 1)),
    }
}

/// Does an `If-None-Match` header match our ETag? (Weak comparison.)
fn none_match(header: &str, etag: &str) -> bool {
    let ours = etag.trim_start_matches("W/");
    header
        .split(',')
        .map(str::trim)
        .any(|t| t == "*" || t.trim_start_matches("W/") == ours)
}

/// `inline; filename="…"; filename*=UTF-8''…` for a stored file.
fn disposition(name: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::with_capacity(name.len() * 3);
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            encoded.push(char::from(b));
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    format!("inline; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

/// Makes a file response save to disk instead of opening in the browser.
pub fn make_attachment(res: &mut Response, name: &str) {
    if res.headers().contains_key(header::CONTENT_DISPOSITION) {
        let value = disposition(name).replacen("inline", "attachment", 1);
        res.headers_mut()
            .insert(header::CONTENT_DISPOSITION, header_value(&value));
    }
}

fn header_value(s: &str) -> HeaderValue {
    HeaderValue::from_str(s).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// Serves a file from disk honouring `Range`, `If-None-Match` and
/// `If-Range`. `name` is the file name offered to the browser.
pub async fn serve_file(path: &FsPath, mime: &str, name: &str, req: &HeaderMap) -> Response {
    let mut file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "media file missing");
            return ApiError::NotFound.into_response();
        }
    };
    let meta = match file.metadata().await {
        Ok(m) => m,
        Err(e) => return ApiError::Internal(e.into()).into_response(),
    };
    let len = meta.len();
    let modified = meta.modified().unwrap_or(UNIX_EPOCH);
    let mtime = modified
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let etag = format!("\"{len:x}-{mtime:x}\"");
    let last_modified = httpdate::fmt_http_date(modified.max(UNIX_EPOCH).min(SystemTime::now()));
    let get = |h: HeaderName| req.get(h).and_then(|v| v.to_str().ok());

    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(header::ETAG, header_value(&etag));
    headers.insert(header::LAST_MODIFIED, header_value(&last_modified));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(CACHE));

    if get(header::IF_NONE_MATCH).is_some_and(|h| none_match(h, &etag)) {
        return (StatusCode::NOT_MODIFIED, headers).into_response();
    }
    headers.insert(header::CONTENT_TYPE, header_value(mime));
    headers.insert(
        header::CONTENT_DISPOSITION,
        header_value(&disposition(name)),
    );

    // A range only applies if the copy the client has is still current.
    let range_applies = match get(header::IF_RANGE) {
        None => true,
        Some(v) => {
            let v = v.trim();
            v == etag || v == last_modified
        }
    };
    let range = if range_applies {
        parse_range(get(header::RANGE), len)
    } else {
        RangeRequest::Full
    };
    let (status, start, count) = match range {
        RangeRequest::Unsatisfiable => {
            headers.insert(
                header::CONTENT_RANGE,
                header_value(&format!("bytes */{len}")),
            );
            headers.remove(header::CONTENT_DISPOSITION);
            return (StatusCode::RANGE_NOT_SATISFIABLE, headers).into_response();
        }
        RangeRequest::Full => (StatusCode::OK, 0, len),
        RangeRequest::Partial { start, end } => {
            headers.insert(
                header::CONTENT_RANGE,
                header_value(&format!("bytes {start}-{end}/{len}")),
            );
            (StatusCode::PARTIAL_CONTENT, start, end - start + 1)
        }
    };
    if start > 0
        && let Err(e) = file.seek(std::io::SeekFrom::Start(start)).await
    {
        return ApiError::Internal(e.into()).into_response();
    }
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(count));
    let body = Body::from_stream(ReaderStream::with_capacity(file.take(count), CHUNK));
    (status, headers, body).into_response()
}

/// `GET /api/media/{id}/stream`
pub async fn stream(
    State(st): State<AppState>,
    who: SignedIn,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> ApiResult {
    let m = visible_media(&st, &who, id).await?;
    if !crate::media::is_safe_name(&m.file) {
        return Err(ApiError::NotFound);
    }
    let path = st.cfg.media_dir().join(&m.file);
    Ok(serve_file(&path, &m.mime, &m.original_name, &headers).await)
}

/// `GET /api/subtitles/{id}`
pub async fn subtitle(State(st): State<AppState>, who: SignedIn, Path(id): Path<i64>) -> ApiResult {
    let adult = who.adult();
    let (sub, restricted) = st
        .db
        .read(move |c| media_files::subtitle(c, id))
        .await?
        .ok_or(ApiError::NotFound)?;
    if restricted && !adult || !crate::media::is_safe_name(&sub.file) {
        return Err(ApiError::NotFound);
    }
    let text = tokio::fs::read(st.cfg.media_dir().join(&sub.file))
        .await
        .map_err(|_| ApiError::NotFound)?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/vtt; charset=utf-8"),
            (header::CACHE_CONTROL, "private, max-age=86400"),
        ],
        text,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        use RangeRequest::*;
        assert_eq!(parse_range(None, 100), Full);
        assert_eq!(
            parse_range(Some("bytes=0-9"), 100),
            Partial { start: 0, end: 9 }
        );
        assert_eq!(
            parse_range(Some("bytes=90-"), 100),
            Partial { start: 90, end: 99 }
        );
        assert_eq!(
            parse_range(Some("bytes=-10"), 100),
            Partial { start: 90, end: 99 }
        );
        assert_eq!(
            parse_range(Some("bytes=-500"), 100),
            Partial { start: 0, end: 99 }
        );
        assert_eq!(
            parse_range(Some("bytes=50-5000"), 100),
            Partial { start: 50, end: 99 }
        );
        assert_eq!(parse_range(Some("bytes=100-"), 100), Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=-0"), 100), Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=0-0"), 0), Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=9-1"), 100), Full);
        assert_eq!(parse_range(Some("bytes=0-1,5-6"), 100), Full);
        assert_eq!(parse_range(Some("items=0-1"), 100), Full);
        assert_eq!(parse_range(Some("bytes=x-1"), 100), Full);
        assert_eq!(parse_range(Some("bytes=+1-2"), 100), Full);
        assert_eq!(
            parse_range(Some("Bytes = 1-2"), 100),
            Partial { start: 1, end: 2 }
        );
    }

    #[test]
    fn validators_and_names() {
        assert!(none_match("\"a\"", "\"a\""));
        assert!(none_match("W/\"a\", \"b\"", "\"a\""));
        assert!(none_match("*", "\"a\""));
        assert!(!none_match("\"b\"", "\"a\""));
        assert_eq!(
            disposition("01 Café \"live\".mp3"),
            "inline; filename=\"01 Caf_ _live_.mp3\"; filename*=UTF-8''01%20Caf%C3%A9%20%22live%22.mp3"
        );
    }
}
