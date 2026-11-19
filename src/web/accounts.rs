//! Player accounts over JSON: registering, logging in and out, and the
//! [`SignedIn`] extractor every `/api` handler uses. Also the JSON error type
//! the API answers with.

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

use axum::Json;
use axum::extract::{FromRequest, FromRequestParts, Request, State};
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::http::{StatusCode, request::Parts};
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::AppState;
use super::auth::{self, Throttle};
use crate::db::accounts::{self, Access, Account, Status};
use crate::db::items::Invalid;
use crate::db::now;

pub const COOKIE: &str = "library_account";
const COOKIE_MAX_AGE: i64 = 31_536_000;
/// `last_seen` is rewritten at most this often per session.
const TOUCH_EVERY_SECS: i64 = 300;

/// Limits on guessing passwords and on making accounts.
pub struct Throttles {
    /// Wrong passwords, per login name and (key "*") overall.
    pub login: Throttle,
    /// New accounts, overall (hashing is deliberately slow).
    pub register: Throttle,
}

impl Default for Throttles {
    fn default() -> Self {
        Self {
            login: Throttle::new(10, Duration::from_secs(10 * 60)),
            register: Throttle::new(30, Duration::from_secs(60 * 60)),
        }
    }
}

const LOGIN_LIMIT_ALL: usize = 100;

/// Said when the right password is given for an account still waiting.
const AWAITING_APPROVAL: &str = "Your account is waiting for a librarian to approve it.";
/// Said after registering.
const REGISTERED: &str = "Thanks! A librarian needs to approve your account before you can sign in. \
     Try signing in again once they have.";

// ---------- errors ----------

/// An `/api` failure, answered as `{"error": "…"}` with its status.
#[derive(Debug)]
pub enum ApiError {
    Unauthorized,
    WrongPassword,
    /// The password was right but no librarian has approved the account.
    AwaitingApproval,
    Forbidden(String),
    NotFound,
    BadRequest(String),
    Invalid(Invalid),
    TooLarge(String),
    TooMany,
    /// 404 with a message of its own (something outside the library).
    Missing(String),
    /// 502: an outside service (MusicBrainz) didn't answer.
    Upstream(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        match e.downcast::<Invalid>() {
            Ok(bad) => ApiError::Invalid(bad),
            Err(e) => ApiError::Internal(e),
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    fields: Option<BTreeMap<String, String>>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error, fields) = match self {
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "Please sign in first.".to_owned(),
                None,
            ),
            ApiError::WrongPassword => (
                StatusCode::UNAUTHORIZED,
                "That username or password isn't right.".to_owned(),
                None,
            ),
            ApiError::AwaitingApproval => {
                (StatusCode::FORBIDDEN, AWAITING_APPROVAL.to_owned(), None)
            }
            ApiError::Forbidden(m) => (StatusCode::FORBIDDEN, m, None),
            ApiError::NotFound => (
                StatusCode::NOT_FOUND,
                "That isn't in the library.".to_owned(),
                None,
            ),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m, None),
            ApiError::Invalid(bad) => {
                let fields: BTreeMap<String, String> = bad.0.iter().cloned().collect();
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    bad.to_string(),
                    Some(fields),
                )
            }
            ApiError::TooLarge(m) => (StatusCode::PAYLOAD_TOO_LARGE, m, None),
            ApiError::TooMany => (
                StatusCode::TOO_MANY_REQUESTS,
                "Too many wrong attempts. Wait ten minutes and try again.".to_owned(),
                None,
            ),
            ApiError::Missing(m) => (StatusCode::NOT_FOUND, m, None),
            ApiError::Upstream(m) => (StatusCode::BAD_GATEWAY, m, None),
            ApiError::Internal(e) => {
                tracing::error!(error = ?e, "api request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Something went wrong. Please try again.".to_owned(),
                    None,
                )
            }
        };
        (status, Json(ErrorBody { error, fields })).into_response()
    }
}

pub type ApiResult<T = Response> = Result<T, ApiError>;

/// A JSON body whose problems are answered in the API's own error shape.
pub struct ApiJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ApiJson<T> {
    type Rejection = ApiError;
    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(v)| ApiJson(v))
            .map_err(|e| {
                ApiError::BadRequest(format!("That request didn't make sense: {}", e.body_text()))
            })
    }
}

// ---------- who is asking ----------

/// A request from a signed-in Player account.
pub struct SignedIn {
    pub account: Account,
}

impl SignedIn {
    /// May this account see restricted (adults only) items?
    pub fn adult(&self) -> bool {
        self.account.access.sees_restricted()
    }
}

impl FromRequestParts<AppState> for SignedIn {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = auth::cookie(&parts.headers, COOKIE).ok_or(ApiError::Unauthorized)?;
        let lookup = token.clone();
        let (account, last_seen) = state
            .db
            .read(move |c| accounts::session(c, &lookup))
            .await?
            .ok_or(ApiError::Unauthorized)?;
        // Sessions are only made for approved accounts; this is a second
        // line of defence.
        if !account.approved() {
            return Err(ApiError::Unauthorized);
        }
        if now() - last_seen > TOUCH_EVERY_SECS
            && let Err(e) = state
                .db
                .write(move |c| accounts::touch_session(c, &token))
                .await
        {
            tracing::warn!(error = %e, "could not record session use");
        }
        Ok(SignedIn { account })
    }
}

// ---------- JSON shapes ----------

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AccountJson {
    pub id: i64,
    pub username: String,
    pub email: String,
    pub access: Access,
}

impl From<&Account> for AccountJson {
    fn from(a: &Account) -> Self {
        Self {
            id: a.id,
            username: a.username.clone(),
            email: a.email.clone(),
            access: a.access,
        }
    }
}

/// The answer to registering: the account exists but waits for a librarian.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingJson {
    pub status: String,
    pub message: String,
}

#[derive(Deserialize)]
pub struct RegisterBody {
    #[serde(default)]
    username: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
}

#[derive(Deserialize)]
pub struct LoginBody {
    #[serde(default)]
    login: String,
    #[serde(default)]
    password: String,
}

// ---------- cookies ----------

fn session_cookie(token: &str, secure: bool) -> String {
    format!(
        "{COOKIE}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={COOKIE_MAX_AGE}{}",
        if secure { "; Secure" } else { "" }
    )
}

fn cleared_cookie(secure: bool) -> String {
    format!(
        "{COOKIE}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0{}",
        if secure { "; Secure" } else { "" }
    )
}

fn with_cookie(mut res: Response, cookie: &str) -> ApiResult {
    let value = HeaderValue::from_str(cookie)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("bad cookie header: {e}")))?;
    res.headers_mut().insert(header::SET_COOKIE, value);
    Ok(res)
}

/// A hash to check against when the login name doesn't exist, so a wrong
/// name takes as long as a wrong password.
fn dummy_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| auth::hash_password("not a real password").unwrap_or_default())
}

// ---------- handlers ----------

/// Makes a pending account. No session is started: a librarian approves
/// the account first.
pub async fn register(
    State(st): State<AppState>,
    ApiJson(body): ApiJson<RegisterBody>,
) -> ApiResult {
    if !st.cfg.accounts.open_registration {
        return Err(ApiError::Forbidden(
            "New accounts can't be made here. Ask the librarian to make one for you.".into(),
        ));
    }
    let bad = accounts::validate(body.username.trim(), &body.email, &body.password);
    if !bad.is_empty() {
        return Err(ApiError::Invalid(bad));
    }
    if st.throttles.register.blocked("*") {
        return Err(ApiError::Forbidden(
            "Lots of accounts have been made just now. Please try again in an hour.".into(),
        ));
    }
    let password = body.password;
    let hash = tokio::task::spawn_blocking(move || auth::hash_password(&password))
        .await
        .map_err(|e| ApiError::Internal(e.into()))??;
    let access = st.cfg.accounts.default_access;
    let (username, email) = (body.username, body.email);
    let account = st
        .db
        .write(move |c| accounts::create(c, &username, &email, &hash, access, Status::Pending))
        .await?;
    st.throttles.register.record("*");
    tracing::info!(username = %account.username, "account registered, waiting for approval");
    Ok((
        StatusCode::ACCEPTED,
        Json(PendingJson {
            status: Status::Pending.as_str().to_owned(),
            message: REGISTERED.to_owned(),
        }),
    )
        .into_response())
}

pub async fn login(
    State(st): State<AppState>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<LoginBody>,
) -> ApiResult {
    let key = body.login.trim().to_lowercase();
    if key.is_empty() || body.password.is_empty() {
        let mut bad = Invalid::default();
        if key.is_empty() {
            bad.add("login", "Enter your username or email.");
        }
        if body.password.is_empty() {
            bad.add("password", "Enter your password.");
        }
        return Err(ApiError::Invalid(bad));
    }
    let throttle = &st.throttles.login;
    if throttle.blocked(&key) || throttle.blocked_over("*", LOGIN_LIMIT_ALL) {
        return Err(ApiError::TooMany);
    }
    let login = key.clone();
    let found = st.db.read(move |c| accounts::for_login(c, &login)).await?;
    let password = body.password;
    let (account, ok) = tokio::task::spawn_blocking(move || match found {
        Some((account, hash)) => {
            let ok = auth::verify_password(&hash, &password);
            (Some(account), ok)
        }
        None => {
            auth::verify_password(dummy_hash(), &password);
            (None, false)
        }
    })
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    let Some(account) = account.filter(|_| ok) else {
        throttle.record(&key);
        throttle.record("*");
        return Err(ApiError::WrongPassword);
    };
    throttle.clear(&key);
    // Only said once the password is right, so it reveals nothing to
    // someone guessing.
    if !account.approved() {
        return Err(ApiError::AwaitingApproval);
    }
    let id = account.id;
    let token = st
        .db
        .write(move |c| accounts::create_session(c, id))
        .await?;
    with_cookie(
        Json(AccountJson::from(&account)).into_response(),
        &session_cookie(&token, super::is_https(&headers)),
    )
}

pub async fn logout(State(st): State<AppState>, headers: HeaderMap) -> ApiResult {
    if let Some(token) = auth::cookie(&headers, COOKIE) {
        st.db
            .write(move |c| accounts::delete_session(c, &token))
            .await?;
    }
    with_cookie(
        StatusCode::NO_CONTENT.into_response(),
        &cleared_cookie(super::is_https(&headers)),
    )
}

pub async fn me(who: SignedIn) -> Json<AccountJson> {
    Json(AccountJson::from(&who.account))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies() {
        let c = session_cookie("abc", false);
        assert!(c.starts_with("library_account=abc;"));
        assert!(
            c.contains("HttpOnly") && c.contains("SameSite=Lax") && c.contains("Max-Age=31536000")
        );
        assert!(!c.contains("Secure"));
        assert!(session_cookie("abc", true).ends_with("; Secure"));
        assert!(cleared_cookie(false).contains("Max-Age=0"));
    }
}
