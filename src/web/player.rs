//! Serves the Player PWA, built by Vite into `player/dist`, at `/player/`.
//!
//! Unknown paths without a file extension get `index.html` (the Player does
//! its own routing). When the Player hasn't been built, a page says how.

use std::borrow::Cow;

use axum::body::Body;
use axum::extract::Path;
use axum::http::header::{self, HeaderValue};
use axum::http::{HeaderName, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "player/dist/"]
#[allow_missing = true]
struct PlayerAssets;

const NO_CACHE: &str = "no-cache";
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const SHORT: &str = "public, max-age=3600";

pub async fn redirect_to_root() -> Redirect {
    Redirect::permanent("/player/")
}

pub async fn root() -> Response {
    respond("", |p| PlayerAssets::get(p).map(|f| f.data))
}

pub async fn file(Path(path): Path<String>) -> Response {
    respond(&path, |p| PlayerAssets::get(p).map(|f| f.data))
}

/// Answers a request for `path` under `/player/` from `lookup`.
pub fn respond(path: &str, lookup: impl Fn(&str) -> Option<Cow<'static, [u8]>>) -> Response {
    let path = path.trim_start_matches('/');
    if path.split('/').any(|seg| seg == "..") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !path.is_empty()
        && !path.ends_with('/')
        && let Some(data) = lookup(path)
    {
        return asset(path, data);
    }
    let last = path.rsplit('/').next().unwrap_or("");
    if last.contains('.') {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }
    match lookup("index.html") {
        Some(data) => asset("index.html", data),
        None => not_built(),
    }
}

fn asset(path: &str, data: Cow<'static, [u8]>) -> Response {
    let mime = if path.ends_with(".webmanifest") {
        "application/manifest+json".to_owned()
    } else {
        mime_guess::from_path(path)
            .first_or_octet_stream()
            .essence_str()
            .to_owned()
    };
    let mime = if mime.starts_with("text/") || mime == "application/javascript" {
        format!("{mime}; charset=utf-8")
    } else {
        mime
    };
    let cache = if path.starts_with("assets/") {
        IMMUTABLE
    } else if matches!(
        path,
        "index.html" | "sw.js" | "manifest.webmanifest" | "registerSW.js"
    ) {
        NO_CACHE
    } else {
        SHORT
    };
    let mut res = (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, cache.to_owned()),
        ],
        Body::from(data.into_owned()),
    )
        .into_response();
    if path == "sw.js" {
        res.headers_mut().insert(
            HeaderName::from_static("service-worker-allowed"),
            HeaderValue::from_static("/player/"),
        );
    }
    res
}

fn not_built() -> Response {
    let page = maud::html! {
        (maud::DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "The Player hasn't been built" }
                link rel="stylesheet" href="/static/fonts/fonts.css";
                link rel="stylesheet" href={ "/static/css/app.css?v=" (super::asset_version()) };
            }
            body {
                main.wrap {
                    div.empty {
                        p { "The Player hasn't been built yet." }
                        p.small.muted {
                            "On the server, run "
                            code.mono { "npm --prefix player ci && npm --prefix player run build" }
                            " and then rebuild the library (" code.mono { "cargo build --release" } ")."
                        }
                        a.btn href="/" { "Back to the catalogue" }
                    }
                }
            }
        }
    };
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::CACHE_CONTROL, NO_CACHE)],
        Html(page.into_string()),
    )
        .into_response()
}
