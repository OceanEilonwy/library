// SPDX-License-Identifier: AGPL-3.0-or-later
//! The rip API (`/api/rip`, docs/rip.md): how Library Rip, the desktop disc
//! ripper, connects to the library and hands it what it ripped.
//!
//! Connecting gives the ripper an API key, either through the browser
//! (`/connect`, an OAuth-style loopback redirect with PKCE) or with a short
//! code typed on `/connect/device`. One-time codes live in memory only:
//! they last minutes, and a restart simply asks the librarian to connect
//! again. Keys are stored hashed (`db::api_keys`) and are sent as
//! `Authorization: Bearer lrk_…`; the [`RipKey`] extractor checks them. A
//! key opens these endpoints only, never the Player API or the desk.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use axum::Json;
use axum::Router;
use axum::extract::multipart::MultipartRejection;
use axum::extract::{DefaultBodyLimit, FromRequestParts, Multipart, Path, Query, State};
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::http::{StatusCode, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::{Rng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::accounts::{ApiError, ApiJson, ApiResult};
use super::auth::Throttle;
use super::{AppState, ClientIp, Fields, studio};
use crate::db::api_keys::{self, ApiKey};
use crate::db::items::{self, Invalid, ItemInput};
use crate::db::media_files::{self, MediaKind};
use crate::db::{now, photos, settings, types};

/// The oldest Library Rip this server works with.
pub const MIN_RIPPER_VERSION: &str = "0.1.0";
/// The `client` a connect link must name.
pub const CLIENT: &str = "library-rip";
/// `last_used_at` is rewritten at most this often per key.
const TOUCH_EVERY_SECS: i64 = 60;
/// Largest cover picture accepted.
const MAX_PHOTO_BYTES: usize = 25 << 20;
/// Most device codes waiting at once.
const MAX_WAITING_DEVICES: usize = 50;
/// Expired device codes are remembered this long, so polling says "expired"
/// rather than "unknown".
const EXPIRED_GRACE: Duration = Duration::from_secs(15 * 60);

// ---------- one-time codes ----------

/// How long codes last and how often the ripper may poll.
#[derive(Debug, Clone, Copy)]
pub struct Timings {
    /// Life of a code from the browser handshake.
    pub code_lifetime: Duration,
    /// Life of a device code.
    pub device_lifetime: Duration,
    /// Seconds the ripper waits between polls with a device code.
    pub poll_interval: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            code_lifetime: Duration::from_secs(5 * 60),
            device_lifetime: Duration::from_secs(10 * 60),
            poll_interval: Duration::from_secs(5),
        }
    }
}

/// A code from `/connect`, bound to the PKCE challenge and redirect URI.
#[derive(Debug, Clone)]
struct AuthCode {
    name: String,
    challenge: String,
    redirect_uri: String,
    expires: Instant,
}

/// Where a device code stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceDecision {
    Waiting,
    Approved,
    Refused,
}

#[derive(Debug, Clone)]
struct DeviceGrant {
    name: String,
    user_code: String,
    decision: DeviceDecision,
    expires: Instant,
    last_poll: Option<Instant>,
}

/// The answer to polling with a device code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    /// The librarian hasn't answered yet.
    Pending,
    /// Asked again sooner than the interval.
    SlowDown,
    /// The librarian said no.
    Refused,
    /// The code ran out.
    Expired,
    /// The librarian said yes: make a key with this name.
    Approved(String),
    /// Never issued (or long forgotten).
    Unknown,
}

/// A waiting device code, as the librarian sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitingDevice {
    pub name: String,
    pub user_code: String,
}

/// Rate limits on the token and device endpoints, counted per client
/// address ([`ClientIp`]) so one client can't lock everyone else out.
pub struct RipThrottles {
    /// Codes that didn't work (wrong, reused, expired, never issued), per
    /// address. Only failures count and only failures are refused: a good
    /// code, or an approved device code, always works.
    pub token_failures: Throttle,
    /// New device codes, per address.
    pub devices: Throttle,
}

impl Default for RipThrottles {
    fn default() -> Self {
        Self {
            token_failures: Throttle::new(30, Duration::from_secs(10 * 60)),
            devices: Throttle::new(20, Duration::from_secs(10 * 60)),
        }
    }
}

/// In-memory state of the connect handshakes, plus a cache of the SHA-256
/// of each ripper download.
#[derive(Default)]
pub struct RipState {
    timings: Timings,
    codes: Mutex<HashMap<String, AuthCode>>,
    devices: Mutex<HashMap<String, DeviceGrant>>,
    pub throttles: RipThrottles,
    /// Download path → (size, modified, SHA-256 hex).
    hashes: Mutex<HashMap<PathBuf, (u64, SystemTime, String)>>,
}

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// Letters that can't be mistaken for digits or each other.
const USER_CODE_LETTERS: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";

/// A fresh user code like `WXYZ-1234`.
fn new_user_code() -> String {
    let mut rng = rand::thread_rng();
    let letters: String = (0..4)
        .map(|_| {
            let i = rng.gen_range(0..USER_CODE_LETTERS.len());
            USER_CODE_LETTERS.get(i).map_or('X', |b| char::from(*b))
        })
        .collect();
    format!("{letters}-{:04}", rng.gen_range(0..10_000u32))
}

/// Reads a user code as typed: any case, spaces or a missing dash.
pub fn normalise_user_code(typed: &str) -> Option<String> {
    let chars: Vec<char> = typed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let (letters, digits) = chars.split_at_checked(4)?;
    let ok = digits.len() == 4
        && letters.iter().all(char::is_ascii_alphabetic)
        && digits.iter().all(char::is_ascii_digit);
    ok.then(|| {
        format!(
            "{}-{}",
            letters.iter().collect::<String>(),
            digits.iter().collect::<String>()
        )
    })
}

impl RipState {
    pub fn timings(&self) -> Timings {
        self.timings
    }

    /// Issues a single-use code for the browser handshake.
    pub fn issue_code(&self, name: &str, challenge: &str, redirect_uri: &str) -> String {
        let code = random_token(32);
        let mut codes = self.codes.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        codes.retain(|_, c| c.expires > now);
        codes.insert(
            code.clone(),
            AuthCode {
                name: name.to_owned(),
                challenge: challenge.to_owned(),
                redirect_uri: redirect_uri.to_owned(),
                expires: now + self.timings.code_lifetime,
            },
        );
        code
    }

    /// Uses up a code: whatever happens next, it never works again.
    fn take_code(&self, code: &str) -> Option<AuthCode> {
        self.codes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(code)
            .filter(|c| c.expires > Instant::now())
    }

    /// Starts a device-code handshake; `None` when too many are waiting.
    pub fn start_device(&self, name: &str) -> Option<(String, String)> {
        let mut devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        devices.retain(|_, d| d.expires + EXPIRED_GRACE > now);
        let waiting = devices.values().filter(|d| d.expires > now).count();
        if waiting >= MAX_WAITING_DEVICES {
            return None;
        }
        // User codes must be unique among the live ones.
        let user_code = loop {
            let candidate = new_user_code();
            if !devices.values().any(|d| d.user_code == candidate) {
                break candidate;
            }
        };
        let device_code = random_token(32);
        devices.insert(
            device_code.clone(),
            DeviceGrant {
                name: name.to_owned(),
                user_code: user_code.clone(),
                decision: DeviceDecision::Waiting,
                expires: now + self.timings.device_lifetime,
                last_poll: None,
            },
        );
        Some((device_code, user_code))
    }

    /// The device waiting for an answer under this user code.
    pub fn waiting_device(&self, user_code: &str) -> Option<WaitingDevice> {
        let user_code = normalise_user_code(user_code)?;
        let devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        devices
            .values()
            .find(|d| {
                d.user_code == user_code && d.expires > now && d.decision == DeviceDecision::Waiting
            })
            .map(|d| WaitingDevice {
                name: d.name.clone(),
                user_code: d.user_code.clone(),
            })
    }

    /// Records the librarian's answer; `None` if no device waits under the code.
    pub fn decide_device(&self, user_code: &str, approve: bool) -> Option<WaitingDevice> {
        let user_code = normalise_user_code(user_code)?;
        let mut devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let grant = devices.values_mut().find(|d| {
            d.user_code == user_code && d.expires > now && d.decision == DeviceDecision::Waiting
        })?;
        grant.decision = if approve {
            DeviceDecision::Approved
        } else {
            DeviceDecision::Refused
        };
        Some(WaitingDevice {
            name: grant.name.clone(),
            user_code: grant.user_code.clone(),
        })
    }

    /// The ripper asking whether its device code was approved.
    pub fn poll_device(&self, device_code: &str) -> Poll {
        let mut devices = self.devices.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let Some(grant) = devices.get_mut(device_code) else {
            return Poll::Unknown;
        };
        if grant.expires <= now {
            return Poll::Expired;
        }
        match grant.decision {
            DeviceDecision::Approved => {
                let name = grant.name.clone();
                devices.remove(device_code);
                Poll::Approved(name)
            }
            DeviceDecision::Refused => {
                devices.remove(device_code);
                Poll::Refused
            }
            DeviceDecision::Waiting => {
                // A second of leeway for timers that fire a little early.
                let interval = self
                    .timings
                    .poll_interval
                    .saturating_sub(Duration::from_secs(1));
                let too_soon = grant
                    .last_poll
                    .is_some_and(|t| now.duration_since(t) < interval);
                grant.last_poll = Some(now);
                if too_soon {
                    Poll::SlowDown
                } else {
                    Poll::Pending
                }
            }
        }
    }

    /// Makes every code and device code run out now (tests).
    #[cfg(test)]
    pub fn expire_all(&self) {
        let past = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        for c in self
            .codes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values_mut()
        {
            c.expires = past;
        }
        for d in self
            .devices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values_mut()
        {
            d.expires = past;
        }
    }

    /// The SHA-256 of a download, worked out once per size and date.
    pub async fn download_sha256(&self, path: PathBuf, size: u64, modified: SystemTime) -> String {
        if let Some((s, m, h)) = self
            .hashes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&path)
            && *s == size
            && *m == modified
        {
            return h.clone();
        }
        let p = path.clone();
        let hashed = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
            let mut file = std::fs::File::open(&p)?;
            let mut hasher = Sha256::new();
            std::io::copy(&mut file, &mut hasher)?;
            Ok(hex(&hasher.finalize()))
        })
        .await;
        match hashed {
            Ok(Ok(h)) => {
                self.hashes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(path, (size, modified, h.clone()));
                h
            }
            Ok(Err(e)) => {
                tracing::warn!(path = %path.display(), error = %e, "could not hash a ripper download");
                String::new()
            }
            Err(e) => {
                tracing::warn!(error = %e, "hashing task failed");
                String::new()
            }
        }
    }
}

/// Lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------- checking the handshake ----------

/// Accepts only `http://127.0.0.1:<port>/…` and `http://localhost:<port>/…`:
/// the ripper's own listener on this computer. Anything else could send
/// the code to someone else.
pub fn loopback_redirect(uri: &str) -> bool {
    let Some(rest) = uri
        .strip_prefix("http://127.0.0.1:")
        .or_else(|| uri.strip_prefix("http://localhost:"))
    else {
        return false;
    };
    let port_len = rest.bytes().take_while(u8::is_ascii_digit).count();
    let (port, path) = rest.split_at(port_len);
    let port_ok = port.len() <= 5 && port.parse::<u16>().is_ok_and(|p| p > 0);
    port_ok
        && path.starts_with('/')
        && uri.len() <= 300
        && !uri
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '#' | '\\' | '@'))
}

/// A PKCE S256 challenge is 43 base64url characters.
pub fn valid_challenge(challenge: &str) -> bool {
    challenge.len() == 43
        && challenge
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// RFC 7636: 43–128 characters of `[A-Za-z0-9-._~]`.
pub fn valid_verifier(verifier: &str) -> bool {
    (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// Does the verifier hash to the challenge? (Compared in constant time.)
pub fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    let computed = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    valid_verifier(verifier) && bool::from(computed.as_bytes().ct_eq(challenge.as_bytes()))
}

/// `redirect_uri` with `?code=…&state=…` (or `error=…`) added.
pub fn redirect_with(redirect_uri: &str, pairs: &[(&str, &str)]) -> String {
    let mut out = redirect_uri.to_owned();
    let mut sep = if out.contains('?') { '&' } else { '?' };
    for (k, v) in pairs {
        out.push(sep);
        out.push_str(k);
        out.push('=');
        out.push_str(&query_escape(v));
        sep = '&';
    }
    out
}

/// Percent-encodes a query value (spaces as %20, unlike forms).
fn query_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The library's own address as the request reached it.
pub fn base_url(headers: &HeaderMap) -> String {
    let get = |n: &str| headers.get(n).and_then(|v| v.to_str().ok());
    let host = get("x-forwarded-host")
        .or_else(|| get("host"))
        .and_then(|h| h.split(',').next())
        .map(str::trim)
        .filter(|h| {
            !h.is_empty()
                && h.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
        })
        .unwrap_or("localhost");
    let scheme = if super::is_https(headers) {
        "https"
    } else {
        "http"
    };
    format!("{scheme}://{host}")
}

// ---------- the key extractor ----------

/// A request carrying a valid, unrevoked API key.
pub struct RipKey {
    pub key: ApiKey,
}

/// The secret from `Authorization: Bearer …`.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.trim().split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim())
        .filter(|t| !t.is_empty())
}

impl FromRequestParts<AppState> for RipKey {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let secret = bearer(&parts.headers).ok_or(ApiError::BadKey)?.to_owned();
        let key = state
            .db
            .read(move |c| api_keys::for_secret(c, &secret))
            .await?
            .ok_or(ApiError::BadKey)?;
        let stale = key
            .last_used_at
            .is_none_or(|t| now() - t >= TOUCH_EVERY_SECS);
        if stale {
            let id = key.id;
            if let Err(e) = state.db.write(move |c| api_keys::touch(c, id)).await {
                tracing::warn!(error = %e, "could not record api key use");
            }
        }
        Ok(RipKey { key })
    }
}

// ---------- JSON shapes ----------

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InfoJson {
    pub name: String,
    pub version: String,
    pub min_ripper_version: String,
    pub source_url: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LibraryJson {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TokenJson {
    pub api_key: String,
    pub key_id: i64,
    pub key_name: String,
    pub library: LibraryJson,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WhoAmIJson {
    pub key_id: i64,
    pub key_name: String,
    pub library: LibraryJson,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceJson {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u64,
    pub expires_in: u64,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TypeJson {
    pub id: i64,
    pub name: String,
    pub slug: String,
    pub lookup: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ItemJson {
    pub id: i64,
    pub title: String,
    pub creator: String,
    pub year: Option<i64>,
    pub identifier: String,
    pub file_count: i64,
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreatedJson {
    pub id: i64,
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MediaJson {
    pub id: i64,
    pub kind: MediaKind,
    pub title: String,
    pub disc: i64,
    pub track: Option<i64>,
    pub sha256: Option<String>,
}

async fn library(st: &AppState, headers: &HeaderMap) -> ApiResult<LibraryJson> {
    let default_title = st.cfg.default_title.clone();
    let name = st
        .db
        .read(move |c| Ok(settings::branding(c, &default_title)?.title))
        .await?;
    Ok(LibraryJson {
        name,
        url: base_url(headers),
    })
}

// ---------- token errors ----------

/// Failures of the token and device endpoints: an OAuth-style code in
/// `error` (what the ripper acts on) and plain English in `message`.
#[derive(Debug)]
pub enum TokenError {
    /// 400: the request itself is wrong.
    BadRequest(String),
    /// 400: the code is wrong, used, expired or doesn't match.
    InvalidGrant,
    /// 428: the librarian hasn't answered yet.
    Pending,
    /// 429: polled too soon.
    SlowDown,
    /// 403: the librarian said no.
    Denied,
    /// 410: the device code ran out.
    Expired,
    /// 429: too many failures or new device codes.
    TooMany,
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for TokenError {
    fn from(e: anyhow::Error) -> Self {
        TokenError::Internal(e)
    }
}

impl From<ApiError> for TokenError {
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::BadRequest(m) => TokenError::BadRequest(m),
            ApiError::Internal(e) => TokenError::Internal(e),
            other => TokenError::Internal(anyhow::anyhow!("{other:?}")),
        }
    }
}

#[derive(Serialize)]
struct TokenErrorBody {
    error: &'static str,
    message: String,
}

impl IntoResponse for TokenError {
    fn into_response(self) -> Response {
        let (status, error, message) = match self {
            TokenError::BadRequest(m) => (StatusCode::BAD_REQUEST, "invalid_request", m),
            TokenError::InvalidGrant => (
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "That code didn't work. It may have been used already or run out. Start connecting again."
                    .to_owned(),
            ),
            TokenError::Pending => (
                StatusCode::PRECONDITION_REQUIRED,
                "authorization_pending",
                "Waiting for the librarian to say yes.".to_owned(),
            ),
            TokenError::SlowDown => (
                StatusCode::TOO_MANY_REQUESTS,
                "slow_down",
                "Asking too often. Wait a few seconds between tries.".to_owned(),
            ),
            TokenError::Denied => (
                StatusCode::FORBIDDEN,
                "access_denied",
                "The librarian said no, so this computer wasn't connected.".to_owned(),
            ),
            TokenError::Expired => (
                StatusCode::GONE,
                "expired_token",
                "The code ran out before the librarian said yes. Start connecting again."
                    .to_owned(),
            ),
            TokenError::TooMany => (
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_requests",
                "Too many tries just now. Wait ten minutes and try again.".to_owned(),
            ),
            TokenError::Internal(e) => {
                tracing::error!(error = ?e, "rip token request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "server_error",
                    "Something went wrong. Please try again.".to_owned(),
                )
            }
        };
        (status, Json(TokenErrorBody { error, message })).into_response()
    }
}

// ---------- routes ----------

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/rip/info", get(info))
        .route("/api/rip/whoami", get(whoami))
        .route("/api/rip/types", get(item_types))
        .route("/api/rip/items", get(search_items).post(create_item))
        .route(
            "/api/rip/items/{id}/photos",
            post(upload_photo).layer(DefaultBodyLimit::max(MAX_PHOTO_BYTES + (64 << 10))),
        )
        .route(
            "/api/rip/items/{id}/media",
            // The upload streams to disk and checks the size itself.
            get(list_media)
                .post(upload_media)
                .layer(DefaultBodyLimit::disable()),
        )
        .route("/api/rip/token", post(token).delete(revoke_own))
        .route("/api/rip/device", post(device))
}

// ---------- handlers ----------

/// `GET /api/rip/info` (no key needed).
pub async fn info(State(st): State<AppState>) -> ApiResult<Json<InfoJson>> {
    let default_title = st.cfg.default_title.clone();
    let name = st
        .db
        .read(move |c| Ok(settings::branding(c, &default_title)?.title))
        .await?;
    Ok(Json(InfoJson {
        name,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        min_ripper_version: MIN_RIPPER_VERSION.to_owned(),
        source_url: st.cfg.source_url.clone(),
    }))
}

#[derive(Deserialize)]
#[serde(tag = "grantType", rename_all = "snake_case")]
pub enum TokenBody {
    #[serde(rename_all = "camelCase")]
    AuthorizationCode {
        #[serde(default)]
        code: String,
        #[serde(default)]
        code_verifier: String,
        #[serde(default)]
        redirect_uri: String,
    },
    #[serde(rename_all = "camelCase")]
    DeviceCode {
        #[serde(default)]
        device_code: String,
    },
}

/// `POST /api/rip/token`: swaps a code for an API key.
pub async fn token(
    State(st): State<AppState>,
    client: ClientIp,
    headers: HeaderMap,
    body: Result<ApiJson<TokenBody>, ApiError>,
) -> Result<Response, TokenError> {
    let ApiJson(body) = body?;
    let rip = &st.rip;
    let client = client.key();
    // Checked only when a code didn't work, so a client with too many bad
    // codes is slowed down without ever turning away a good one (codes are
    // far too long to guess anyway).
    let fail = || {
        let failures = &rip.throttles.token_failures;
        if failures.blocked(&client) {
            TokenError::TooMany
        } else {
            failures.record(&client);
            TokenError::InvalidGrant
        }
    };
    let name = match body {
        TokenBody::AuthorizationCode {
            code,
            code_verifier,
            redirect_uri,
        } => {
            let grant = rip.take_code(&code).ok_or_else(fail)?;
            let redirect_ok =
                bool::from(grant.redirect_uri.as_bytes().ct_eq(redirect_uri.as_bytes()));
            if !redirect_ok || !pkce_matches(&code_verifier, &grant.challenge) {
                return Err(fail());
            }
            grant.name
        }
        TokenBody::DeviceCode { device_code } => match rip.poll_device(&device_code) {
            Poll::Approved(name) => name,
            Poll::Pending => return Err(TokenError::Pending),
            Poll::SlowDown => return Err(TokenError::SlowDown),
            Poll::Refused => return Err(TokenError::Denied),
            Poll::Expired => return Err(TokenError::Expired),
            Poll::Unknown => return Err(fail()),
        },
    };
    let (key, secret) = st.db.write(move |c| api_keys::create(c, &name)).await?;
    tracing::info!(key = key.id, name = %key.name, "library rip connected");
    let library = library(&st, &headers).await?;
    let mut res = Json(TokenJson {
        api_key: secret,
        key_id: key.id,
        key_name: key.name,
        library,
    })
    .into_response();
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

#[derive(Deserialize)]
pub struct DeviceBody {
    #[serde(default)]
    name: String,
}

/// `POST /api/rip/device`: starts the code-typing handshake.
pub async fn device(
    State(st): State<AppState>,
    client: ClientIp,
    headers: HeaderMap,
    body: Result<ApiJson<DeviceBody>, ApiError>,
) -> Result<Json<DeviceJson>, TokenError> {
    let ApiJson(body) = body?;
    let rip = &st.rip;
    let client = client.key();
    if rip.throttles.devices.blocked(&client) {
        return Err(TokenError::TooMany);
    }
    rip.throttles.devices.record(&client);
    let name = api_keys::clean_name(&body.name);
    let (device_code, user_code) = rip.start_device(&name).ok_or(TokenError::TooMany)?;
    let timings = rip.timings();
    Ok(Json(DeviceJson {
        device_code,
        user_code,
        verification_uri: format!("{}/connect/device", base_url(&headers)),
        interval: timings.poll_interval.as_secs(),
        expires_in: timings.device_lifetime.as_secs(),
    }))
}

/// `DELETE /api/rip/token`: the ripper disconnecting itself.
pub async fn revoke_own(State(st): State<AppState>, who: RipKey) -> ApiResult {
    let id = who.key.id;
    st.db.write(move |c| api_keys::revoke(c, id)).await?;
    tracing::info!(key = id, name = %who.key.name, "library rip disconnected itself");
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `GET /api/rip/whoami`
pub async fn whoami(
    State(st): State<AppState>,
    who: RipKey,
    headers: HeaderMap,
) -> ApiResult<Json<WhoAmIJson>> {
    Ok(Json(WhoAmIJson {
        key_id: who.key.id,
        key_name: who.key.name,
        library: library(&st, &headers).await?,
    }))
}

/// `GET /api/rip/types`
pub async fn item_types(
    State(st): State<AppState>,
    _who: RipKey,
) -> ApiResult<Json<Vec<TypeJson>>> {
    let list = st.db.read(types::list).await?;
    Ok(Json(
        list.into_iter()
            .map(|t| TypeJson {
                id: t.id,
                name: t.name,
                slug: t.slug,
                lookup: t.lookup.as_str().to_owned(),
            })
            .collect(),
    ))
}

#[derive(Deserialize, Default)]
pub struct SearchParams {
    #[serde(default)]
    q: String,
    #[serde(default, rename = "type")]
    type_slug: String,
}

/// `GET /api/rip/items?q=&type=`: catalogue entries to attach files to.
pub async fn search_items(
    State(st): State<AppState>,
    _who: RipKey,
    headers: HeaderMap,
    Query(p): Query<SearchParams>,
) -> ApiResult<Json<Vec<ItemJson>>> {
    let q: String = p.q.chars().take(200).collect();
    let type_slug = p.type_slug;
    let found = st
        .db
        .read(move |c| items::find_for_attach(c, &q, &type_slug, 25))
        .await?;
    let base = base_url(&headers);
    Ok(Json(
        found
            .into_iter()
            .map(|i| ItemJson {
                url: format!("{base}/items/{}", i.id),
                id: i.id,
                title: i.title,
                creator: i.creator,
                year: i.year,
                identifier: i.identifier,
                file_count: i.file_count,
            })
            .collect(),
    ))
}

/// A number sent as a number or as text.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum NumberOrText {
    Number(i64),
    Text(String),
}

impl NumberOrText {
    fn text(self) -> String {
        match self {
            NumberOrText::Number(n) => n.to_string(),
            NumberOrText::Text(t) => t,
        }
    }
}

/// Tags as a list or as one comma-separated string.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum Tags {
    List(Vec<String>),
    Text(String),
}

impl Tags {
    fn text(self) -> String {
        match self {
            Tags::List(list) => list
                .iter()
                .map(|t| t.trim())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(", "),
            Tags::Text(t) => t,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewItemBody {
    #[serde(default)]
    type_slug: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    creator: Option<String>,
    #[serde(default)]
    year: Option<NumberOrText>,
    #[serde(default)]
    identifier: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    tags: Option<Tags>,
    #[serde(default)]
    musicbrainz_release_id: Option<String>,
}

/// Is this a MusicBrainz id (a UUID)?
pub fn is_mbid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// `POST /api/rip/items`: a new catalogue entry for a ripped disc.
pub async fn create_item(
    State(st): State<AppState>,
    _who: RipKey,
    headers: HeaderMap,
    ApiJson(body): ApiJson<NewItemBody>,
) -> ApiResult<Response> {
    let slug = match body.type_slug.trim() {
        "" => "music".to_owned(),
        s => s.to_ascii_lowercase(),
    };
    let mbid = body
        .musicbrainz_release_id
        .map(|m| m.trim().to_ascii_lowercase())
        .filter(|m| !m.is_empty());
    if mbid.as_deref().is_some_and(|m| !is_mbid(m)) {
        let mut bad = Invalid::default();
        bad.add(
            "musicbrainzReleaseId",
            "That isn't a MusicBrainz release id.",
        );
        return Err(ApiError::Invalid(bad));
    }
    // No column holds a release id (see docs/player.md), so it goes in the
    // librarian's notes as a link.
    let notes = mbid
        .map(|m| format!("MusicBrainz release: https://musicbrainz.org/release/{m}"))
        .unwrap_or_default();
    let input_slug = slug.clone();
    let mut input = ItemInput {
        type_id: 0,
        title: body.title,
        creator: body.creator.unwrap_or_default(),
        year: body.year.map(NumberOrText::text).unwrap_or_default(),
        identifier: body.identifier.unwrap_or_default(),
        description: body.description.unwrap_or_default(),
        tags: body.tags.map(Tags::text).unwrap_or_default(),
        notes,
        ..Default::default()
    };
    let id = st
        .db
        .write(move |c| {
            let Some(t) = types::by_slug(c, &input_slug)? else {
                let mut bad = Invalid::default();
                bad.add(
                    "typeSlug",
                    format!("This library has no type called “{input_slug}”."),
                );
                return Err(bad.into());
            };
            input.type_id = t.id;
            items::create(c, &input)
        })
        .await?;
    st.semantic.poke();
    tracing::info!(item = id, "catalogue entry made by library rip");
    Ok((
        StatusCode::CREATED,
        Json(CreatedJson {
            id,
            url: format!("{}/items/{id}", base_url(&headers)),
        }),
    )
        .into_response())
}

async fn item_exists(st: &AppState, id: i64) -> ApiResult<()> {
    let exists = st
        .db
        .read(move |c| Ok(items::get(c, id)?.is_some()))
        .await?;
    exists.then_some(()).ok_or(ApiError::NotFound)
}

/// `POST /api/rip/items/{id}/photos`: the front cover (multipart `file`).
pub async fn upload_photo(
    State(st): State<AppState>,
    _who: RipKey,
    Path(id): Path<i64>,
    form: Result<Fields, super::AppError>,
) -> ApiResult<Response> {
    item_exists(&st, id).await?;
    let form = form.map_err(|e| match e {
        super::AppError::BadRequest(m) => ApiError::BadRequest(m),
        _ => ApiError::BadRequest("The picture didn't arrive. Send it again.".into()),
    })?;
    let Some(upload) = form.files.into_iter().find(|f| f.name == "file") else {
        let mut bad = Invalid::default();
        bad.add("file", "Send the cover picture as the “file” field.");
        return Err(ApiError::Invalid(bad));
    };
    let dir = st.photos_dir();
    let bytes = upload.bytes;
    let saved = tokio::task::spawn_blocking(move || crate::media::save_photo(&dir, &bytes))
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .map_err(|_| {
            let mut bad = Invalid::default();
            bad.add(
                "file",
                "That picture couldn't be read. Send a JPEG, PNG or WebP.",
            );
            ApiError::Invalid(bad)
        })?;
    let pid = st
        .db
        .write(move |c| photos::add(c, id, &saved.file, saved.width, saved.height, "Cover"))
        .await?;
    Ok((StatusCode::CREATED, Json(serde_json::json!({ "id": pid }))).into_response())
}

/// `GET /api/rip/items/{id}/media`: the files already on an item, so an
/// interrupted upload can skip what arrived.
pub async fn list_media(
    State(st): State<AppState>,
    _who: RipKey,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<MediaJson>>> {
    item_exists(&st, id).await?;
    let files = st.db.read(move |c| media_files::for_item(c, id)).await?;
    Ok(Json(
        files
            .into_iter()
            .map(|m| MediaJson {
                id: m.id,
                kind: m.kind,
                title: m.title,
                disc: m.disc,
                track: m.track,
                sha256: m.sha256,
            })
            .collect(),
    ))
}

/// `POST /api/rip/items/{id}/media`: one file, with the same rules as the
/// upload studio, plus a `sha256` the server checks while it writes.
pub async fn upload_media(
    State(st): State<AppState>,
    _who: RipKey,
    Path(id): Path<i64>,
    headers: HeaderMap,
    multipart: Result<Multipart, MultipartRejection>,
) -> ApiResult<Response> {
    let created = studio::receive_media(&st, id, &headers, multipart, true).await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": created.id })),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_redirects_only() {
        assert!(loopback_redirect("http://127.0.0.1:53682/callback"));
        assert!(loopback_redirect("http://localhost:8000/"));
        assert!(loopback_redirect("http://127.0.0.1:1/cb?x=1"));
        for bad in [
            "https://127.0.0.1:5000/callback",
            "http://127.0.0.1/callback",
            "http://127.0.0.1:/callback",
            "http://127.0.0.1:0/callback",
            "http://127.0.0.1:70000/callback",
            "http://127.0.0.1:5000",
            "http://127.0.0.1:5000@evil.com/",
            "http://127.0.0.1.evil.com:5000/",
            "http://localhost.evil.com:80/",
            "http://evil.com:5000/callback",
            "http://127.0.0.1:5000/cb#frag",
            "http://127.0.0.1:5000/a b",
            "http://127.0.0.1:5000\\@evil.com/",
            "",
        ] {
            assert!(!loopback_redirect(bad), "{bad}");
        }
    }

    #[test]
    fn pkce_rfc7636_example() {
        // RFC 7636 appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert!(valid_challenge(challenge));
        assert!(pkce_matches(verifier, challenge));
        assert!(!pkce_matches(&verifier.replace('d', "e"), challenge));
        assert!(!pkce_matches("short", challenge));
        assert!(!valid_challenge("tooshort"));
    }

    #[test]
    fn user_codes() {
        for _ in 0..50 {
            let code = new_user_code();
            assert_eq!(normalise_user_code(&code).as_deref(), Some(code.as_str()));
            assert_eq!(code.len(), 9);
        }
        assert_eq!(
            normalise_user_code(" wxyz 1234 ").as_deref(),
            Some("WXYZ-1234")
        );
        assert_eq!(
            normalise_user_code("WXYZ1234").as_deref(),
            Some("WXYZ-1234")
        );
        assert_eq!(normalise_user_code("WXY-1234"), None);
        assert_eq!(normalise_user_code("1234-WXYZ"), None);
        assert_eq!(normalise_user_code(""), None);
    }

    #[test]
    fn codes_are_single_use_and_expire() {
        let rip = RipState::default();
        let code = rip.issue_code("Desk", "c", "http://127.0.0.1:1/cb");
        assert!(rip.take_code(&code).is_some());
        assert!(rip.take_code(&code).is_none(), "single use");
        let code = rip.issue_code("Desk", "c", "http://127.0.0.1:1/cb");
        rip.expire_all();
        assert!(rip.take_code(&code).is_none(), "expired");
    }

    #[test]
    fn device_polling() {
        let rip = RipState::default();
        let (device, user) = rip.start_device("Desk").unwrap();
        assert_eq!(rip.poll_device(&device), Poll::Pending);
        assert_eq!(rip.poll_device(&device), Poll::SlowDown, "too soon");
        assert_eq!(rip.poll_device("nope"), Poll::Unknown);
        assert_eq!(
            rip.waiting_device(&user.to_lowercase()).map(|d| d.name),
            Some("Desk".into())
        );
        assert!(rip.decide_device(&user, true).is_some());
        assert!(rip.decide_device(&user, true).is_none(), "answered once");
        assert_eq!(rip.poll_device(&device), Poll::Approved("Desk".into()));
        assert_eq!(rip.poll_device(&device), Poll::Unknown, "used up");

        let (device, user) = rip.start_device("Laptop").unwrap();
        rip.decide_device(&user, false);
        assert_eq!(rip.poll_device(&device), Poll::Refused);

        let (device, _) = rip.start_device("Old").unwrap();
        rip.expire_all();
        assert_eq!(rip.poll_device(&device), Poll::Expired);
    }

    #[test]
    fn redirect_building_and_base_url() {
        assert_eq!(
            redirect_with(
                "http://127.0.0.1:5/cb",
                &[("code", "a b"), ("state", "x&y")]
            ),
            "http://127.0.0.1:5/cb?code=a%20b&state=x%26y"
        );
        assert_eq!(
            redirect_with("http://127.0.0.1:5/cb?k=1", &[("error", "access_denied")]),
            "http://127.0.0.1:5/cb?k=1&error=access_denied"
        );
        let mut h = HeaderMap::new();
        h.insert("host", "library.example.org".parse().unwrap());
        assert_eq!(base_url(&h), "http://library.example.org");
        h.insert("x-forwarded-proto", "https".parse().unwrap());
        assert_eq!(base_url(&h), "https://library.example.org");
        h.insert("host", "evil\"<>".parse().unwrap());
        assert_eq!(base_url(&h), "https://localhost");
    }

    #[test]
    fn mbids() {
        assert!(is_mbid("b84ee12a-09ef-421b-82de-0441a926375b"));
        assert!(!is_mbid("b84ee12a09ef421b82de0441a926375b"));
        assert!(!is_mbid("not-an-id"));
    }
}
