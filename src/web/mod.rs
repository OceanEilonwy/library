//! HTTP: routing, shared state, extractors, middleware and static files.

pub mod admin;
pub mod auth;
pub mod public;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{
    DefaultBodyLimit, FromRequest, FromRequestParts, Multipart, Path, Request, State,
};
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::http::{Method, StatusCode, request::Parts};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use maud::Markup;
use rust_embed::RustEmbed;
use sha2::{Digest, Sha256};
use tower_http::compression::CompressionLayer;

use crate::config::Config;
use crate::db::{Db, settings, types};
use crate::embed::Semantic;
use crate::lookup::{CandidateCache, Lookups};
use crate::views::{self, Art, Ctx};
use auth::Auth;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: Db,
    pub semantic: Semantic,
    pub lookups: Lookups,
    pub candidates: Arc<CandidateCache>,
    pub auth: Arc<Auth>,
    pub art: Arc<ArtCache>,
}

impl AppState {
    /// Builds the page context for a request.
    pub async fn ctx(&self, viewer: &Viewer) -> Result<Ctx, AppError> {
        let default_title = self.cfg.default_title.clone();
        let (branding, types) = self
            .db
            .read(move |c| Ok((settings::branding(c, &default_title)?, types::list(c)?)))
            .await?;
        Ok(Ctx {
            admin: viewer.admin,
            admin_configured: self.auth.configured(),
            branding,
            types,
            asset_version: asset_version(),
            art: self.art.get(),
            here: viewer.here.clone(),
        })
    }

    pub fn photos_dir(&self) -> PathBuf {
        self.cfg.photos_dir()
    }
}

pub fn router(state: AppState) -> Router {
    let admin = admin::routes().layer(DefaultBodyLimit::max(100 << 20));
    Router::new()
        .route("/", get(public::landing))
        .route("/search", get(public::search))
        .route("/advanced", get(public::advanced))
        .route("/advanced/preview", get(public::advanced_preview))
        .route("/items/{id}", get(public::item))
        .route("/items/{id}/row", get(public::item_row))
        .route(
            "/items/{id}/loan",
            get(public::loan_form).post(public::lend),
        )
        .route("/items/{id}/return", post(public::give_back))
        .route("/unlock", get(public::unlock_page).post(public::unlock))
        .route("/photos/{file}", get(photo))
        .route("/art/{file}", get(art_file))
        .route("/static/{*path}", get(static_file))
        .route("/healthz", get(|| async { "ok" }))
        .merge(admin)
        .layer(DefaultBodyLimit::max(64 << 10))
        .layer(middleware::from_fn(same_origin))
        .layer(middleware::from_fn(security_headers))
        .layer(CompressionLayer::new())
        .with_state(state)
}

// ---------- errors ----------

#[derive(Debug)]
pub enum AppError {
    NotFound,
    BadRequest(String),
    Forbidden(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::Internal(e)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            AppError::NotFound => (
                StatusCode::NOT_FOUND,
                "That page isn't on the shelves.".to_owned(),
            ),
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            AppError::Forbidden(m) => (StatusCode::FORBIDDEN, m),
            AppError::Internal(e) => {
                tracing::error!(error = ?e, "request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Something went wrong. Please try again.".to_owned(),
                )
            }
        };
        let body = maud::html! {
            (maud::DOCTYPE)
            html lang="en" {
                head {
                    meta charset="utf-8";
                    meta name="viewport" content="width=device-width, initial-scale=1";
                    title { (status.as_u16()) }
                    link rel="stylesheet" href="/static/fonts/fonts.css";
                    link rel="stylesheet" href={ "/static/css/app.css?v=" (asset_version()) };
                }
                body { main.wrap { div.empty { p { (msg) } a.btn href="/" { "Back to the catalogue" } } } }
            }
        };
        (status, Html(body.into_string())).into_response()
    }
}

pub type Res<T = Response> = Result<T, AppError>;

pub fn html(m: Markup) -> Response {
    Html(m.into_string()).into_response()
}

pub fn html_status(status: StatusCode, m: Markup) -> Response {
    (status, Html(m.into_string())).into_response()
}

pub fn see_other(to: &str) -> Response {
    Redirect::to(to).into_response()
}

// ---------- who is asking ----------

/// The visitor: whether they have unlocked editing, and where they are.
pub struct Viewer {
    pub admin: bool,
    pub here: String,
    pub htmx: bool,
    pub hx_target: String,
}

impl FromRequestParts<AppState> for Viewer {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let header = |n: &str| {
            parts
                .headers
                .get(n)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned()
        };
        let htmx = header("hx-request") == "true";
        let here = if htmx {
            // For fragments, "here" is the page the browser is on.
            header("hx-current-url")
                .split_once("://")
                .and_then(|(_, rest)| rest.find('/').map(|i| rest[i..].to_owned()))
                .unwrap_or_else(|| "/".to_owned())
        } else {
            parts
                .uri
                .path_and_query()
                .map(|p| p.as_str().to_owned())
                .unwrap_or_else(|| "/".into())
        };
        Ok(Viewer {
            admin: state.auth.is_admin(&parts.headers),
            here,
            htmx,
            hx_target: header("hx-target"),
        })
    }
}

/// Proof that the request comes from an unlocked session.
pub struct Admin(pub Viewer);

impl FromRequestParts<AppState> for Admin {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let viewer = Viewer::from_request_parts(parts, state)
            .await
            .map_err(IntoResponse::into_response)?;
        if viewer.admin {
            return Ok(Admin(viewer));
        }
        let next = views::urlencode(&viewer.here);
        if parts.method == Method::GET && !viewer.htmx {
            Err(see_other(&format!("/unlock?next={next}")))
        } else if viewer.htmx {
            let mut r = StatusCode::FORBIDDEN.into_response();
            r.headers_mut().insert(
                HeaderName::from_static("hx-redirect"),
                HeaderValue::from_str(&format!("/unlock?next={next}"))
                    .unwrap_or(HeaderValue::from_static("/unlock")),
            );
            Err(r)
        } else {
            Err(
                AppError::Forbidden("Unlock editing with the admin password first.".into())
                    .into_response(),
            )
        }
    }
}

// ---------- form bodies (urlencoded or multipart) ----------

pub struct Upload {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// A submitted form, from either encoding. Keeps every value in order.
#[derive(Default)]
pub struct Fields {
    pairs: Vec<(String, String)>,
    pub files: Vec<Upload>,
}

impl Fields {
    pub fn get(&self, key: &str) -> &str {
        self.pairs
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    }
    pub fn has(&self, key: &str) -> bool {
        self.pairs.iter().any(|(k, v)| k == key && !v.is_empty())
    }
    pub fn pairs(&self) -> &[(String, String)] {
        &self.pairs
    }
}

impl<S: Send + Sync> FromRequest<S> for Fields {
    type Rejection = AppError;
    async fn from_request(req: Request, state: &S) -> Result<Self, AppError> {
        let ct = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        if ct.starts_with("multipart/form-data") {
            let mut mp = Multipart::from_request(req, state)
                .await
                .map_err(|e| AppError::BadRequest(e.body_text()))?;
            let mut out = Fields::default();
            while let Some(field) = mp
                .next_field()
                .await
                .map_err(|e| AppError::BadRequest(e.body_text()))?
            {
                let name = field.name().unwrap_or("").to_owned();
                if field.file_name().is_some() {
                    let bytes = field
                        .bytes()
                        .await
                        .map_err(|e| AppError::BadRequest(e.body_text()))?;
                    if !bytes.is_empty() {
                        out.files.push(Upload {
                            name,
                            bytes: bytes.to_vec(),
                        });
                    }
                } else {
                    let text = field
                        .text()
                        .await
                        .map_err(|e| AppError::BadRequest(e.body_text()))?;
                    out.pairs.push((name, text));
                }
            }
            Ok(out)
        } else {
            let Form(pairs) = Form::<Vec<(String, String)>>::from_request(req, state)
                .await
                .map_err(|e| AppError::BadRequest(e.body_text()))?;
            Ok(Fields {
                pairs,
                files: Vec::new(),
            })
        }
    }
}

// ---------- middleware ----------

const CSP: &str = "default-src 'self'; img-src 'self' data: blob:; style-src 'self'; script-src 'self'; \
    font-src 'self'; connect-src 'self'; media-src 'self' blob:; object-src 'none'; base-uri 'self'; \
    form-action 'self'; frame-ancestors 'none'";

async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(self), microphone=(), geolocation=()"),
    );
    if !h.contains_key(header::CACHE_CONTROL) {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }
    res
}

/// Rejects state-changing requests from other sites (CSRF). Browsers send
/// Origin on POST; Sec-Fetch-Site covers the rest.
async fn same_origin(req: Request, next: Next) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }
    if !is_same_origin(req.headers()) {
        return (StatusCode::FORBIDDEN, "Cross-site request refused.").into_response();
    }
    next.run(req).await
}

pub fn is_same_origin(h: &HeaderMap) -> bool {
    let get = |n: HeaderName| h.get(n).and_then(|v| v.to_str().ok()).map(str::to_owned);
    if let Some(site) = h.get("sec-fetch-site").and_then(|v| v.to_str().ok())
        && site == "cross-site"
    {
        return false;
    }
    match get(header::ORIGIN) {
        None => true,
        Some(origin) if origin == "null" => false,
        Some(origin) => {
            let origin_host = origin.split_once("://").map(|(_, h)| h).unwrap_or(&origin);
            let host = get(HeaderName::from_static("x-forwarded-host"))
                .or_else(|| get(header::HOST))
                .unwrap_or_default();
            origin_host.eq_ignore_ascii_case(host.split(',').next().unwrap_or("").trim())
        }
    }
}

/// Whether the browser reached us over HTTPS (directly or via the proxy).
pub fn is_https(h: &HeaderMap) -> bool {
    h.get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|p| p.eq_ignore_ascii_case("https"))
        || h.get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|o| o.starts_with("https://"))
}

// ---------- static files ----------

#[derive(RustEmbed)]
#[folder = "static/"]
#[exclude = "art/*"]
struct Assets;

/// A short hash of the CSS/JS so their URLs change when they do.
pub fn asset_version() -> &'static str {
    static V: OnceLock<String> = OnceLock::new();
    V.get_or_init(|| {
        let mut h = Sha256::new();
        for f in ["css/app.css", "js/app.js", "js/htmx.min.js"] {
            if let Some(a) = Assets::get(f) {
                h.update(&a.data);
            }
        }
        h.finalize()
            .iter()
            .take(5)
            .map(|b| format!("{b:02x}"))
            .collect()
    })
}

async fn static_file(Path(path): Path<String>) -> Response {
    let Some(asset) = Assets::get(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    let immutable = path.starts_with("fonts/") || path.ends_with(".css") || path.ends_with(".js");
    let cache = if immutable {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=3600"
    };
    (
        [
            (header::CONTENT_TYPE, mime.as_ref().to_owned()),
            (header::CACHE_CONTROL, cache.to_owned()),
        ],
        Body::from(asset.data.into_owned()),
    )
        .into_response()
}

async fn photo(State(st): State<AppState>, Path(file): Path<String>) -> Response {
    serve_file(
        st.photos_dir().join(&file),
        &file,
        "public, max-age=31536000, immutable",
    )
    .await
}

async fn art_file(State(st): State<AppState>, Path(file): Path<String>) -> Response {
    serve_file(st.cfg.art_dir().join(&file), &file, "public, max-age=3600").await
}

async fn serve_file(path: PathBuf, name: &str, cache: &'static str) -> Response {
    if !crate::media::is_safe_name(name) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(name).first_or_octet_stream();
            (
                [
                    (header::CONTENT_TYPE, mime.as_ref().to_owned()),
                    (header::CACHE_CONTROL, cache.to_owned()),
                ],
                bytes,
            )
                .into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Which hand-drawn art files exist, rechecked at most every 30 seconds.
pub struct ArtCache {
    dir: PathBuf,
    cached: Mutex<Option<(Instant, Art)>>,
}

impl ArtCache {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            cached: Mutex::new(None),
        }
    }

    pub fn get(&self) -> Art {
        let mut c = self.cached.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((t, art)) = c.as_ref()
            && t.elapsed() < Duration::from_secs(30)
        {
            return art.clone();
        }
        let find = |stem: &str| {
            ["svg", "png", "webp", "jpg"].iter().find_map(|ext| {
                let name = format!("{stem}.{ext}");
                self.dir
                    .join(&name)
                    .is_file()
                    .then(|| format!("/art/{name}"))
            })
        };
        let art = Art {
            hero: find("hero"),
            margin_left: find("margin-left"),
            margin_right: find("margin-right"),
            empty: find("empty"),
        };
        *c = Some((Instant::now(), art.clone()));
        art
    }
}

/// Graceful shutdown on Ctrl-C or SIGTERM.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    tracing::info!("shutting down");
}
