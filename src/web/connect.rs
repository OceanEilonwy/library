// SPDX-License-Identifier: AGPL-3.0-or-later
//! The htmx site's side of Library Rip: the connect pages the librarian
//! says yes on (`/connect` for the browser handshake, `/connect/device` for
//! a typed code), disconnecting from the desk, and the "Get Library Rip"
//! download page (`/ripper`).

use std::path::PathBuf;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;

use super::rip::{self, loopback_redirect, redirect_with, valid_challenge};
use super::{Admin, AppError, AppState, Fields, Res, Viewer, html, html_status, see_other};
use crate::db::api_keys;
use crate::views::rip::{self as rv, Build, ConnectRequest, Os, RipperView};
use crate::views::urlencode;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/connect", get(connect_page).post(connect_answer))
        .route("/connect/device", get(device_page).post(device_answer))
        .route("/ripper", get(ripper_page))
        .route("/ripper/download/{file}", get(download))
        .route("/admin/rippers/{id}/disconnect", post(disconnect))
}

// ---------- the browser handshake ----------

/// The query (and later the form) of a connect link.
#[derive(Deserialize, Default)]
pub struct ConnectParams {
    #[serde(default)]
    client: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    redirect_uri: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    code_challenge: String,
    #[serde(default)]
    code_challenge_method: String,
}

/// Checks a connect link; the error is a plain-English reason.
pub fn check_request(
    client: &str,
    name: &str,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
    method: &str,
) -> Result<ConnectRequest, &'static str> {
    if client != rip::CLIENT {
        return Err("This link isn't from Library Rip.");
    }
    if !loopback_redirect(redirect_uri) {
        return Err(
            "This link would send the answer somewhere other than Library Rip on this computer, \
             so it was refused.",
        );
    }
    if method != "S256" || !valid_challenge(challenge) || state.is_empty() || state.len() > 500 {
        return Err("This link is missing its security check.");
    }
    Ok(ConnectRequest {
        name: api_keys::clean_name(name),
        redirect_uri: redirect_uri.to_owned(),
        state: state.to_owned(),
        code_challenge: challenge.to_owned(),
    })
}

/// `GET /connect`: asks the librarian (unlocking first) whether to connect.
pub async fn connect_page(
    State(st): State<AppState>,
    viewer: Viewer,
    Query(p): Query<ConnectParams>,
) -> Res {
    let ctx = st.ctx(&viewer).await?;
    let checked = check_request(
        &p.client,
        &p.name,
        &p.redirect_uri,
        &p.state,
        &p.code_challenge,
        &p.code_challenge_method,
    );
    let request = match checked {
        Ok(r) => r,
        Err(message) => {
            return Ok(html_status(
                StatusCode::BAD_REQUEST,
                rv::connect_problem(&ctx, message),
            ));
        }
    };
    if !viewer.admin {
        return Ok(see_other(&format!(
            "/unlock?next={}",
            urlencode(&viewer.here)
        )));
    }
    Ok(html(rv::connect_confirm(&ctx, &request)))
}

/// A 302 to the ripper's listener.
fn found(to: &str) -> Response {
    match HeaderValue::from_str(to) {
        Ok(v) => (StatusCode::FOUND, [(header::LOCATION, v)]).into_response(),
        Err(_) => AppError::BadRequest("That address can't be used.".into()).into_response(),
    }
}

/// `POST /connect`: Yes sends a one-time code back to the ripper, No says so.
pub async fn connect_answer(State(st): State<AppState>, Admin(viewer): Admin, form: Fields) -> Res {
    let checked = check_request(
        form.get("client"),
        form.get("name"),
        form.get("redirect_uri"),
        form.get("state"),
        form.get("code_challenge"),
        form.get("code_challenge_method"),
    );
    let request = match checked {
        Ok(r) => r,
        Err(message) => {
            let ctx = st.ctx(&viewer).await?;
            return Ok(html_status(
                StatusCode::BAD_REQUEST,
                rv::connect_problem(&ctx, message),
            ));
        }
    };
    if form.get("decision") != "yes" {
        tracing::info!(name = %request.name, "library rip connection refused");
        return Ok(found(&redirect_with(
            &request.redirect_uri,
            &[("error", "access_denied"), ("state", &request.state)],
        )));
    }
    let code = st.rip.issue_code(
        &request.name,
        &request.code_challenge,
        &request.redirect_uri,
    );
    Ok(found(&redirect_with(
        &request.redirect_uri,
        &[("code", &code), ("state", &request.state)],
    )))
}

// ---------- the code-typing handshake ----------

#[derive(Deserialize, Default)]
pub struct DeviceParams {
    #[serde(default)]
    code: String,
}

/// `GET /connect/device`: where the librarian types the ripper's code.
pub async fn device_page(
    State(st): State<AppState>,
    Admin(viewer): Admin,
    Query(p): Query<DeviceParams>,
) -> Res {
    let ctx = st.ctx(&viewer).await?;
    let code: String = p.code.chars().take(12).collect();
    Ok(html(rv::device_form(&ctx, &code, None)))
}

/// `POST /connect/device`: a typed code (then Yes or No).
pub async fn device_answer(State(st): State<AppState>, Admin(viewer): Admin, form: Fields) -> Res {
    let ctx = st.ctx(&viewer).await?;
    let typed: String = form.get("code").chars().take(20).collect();
    let unknown = || {
        html_status(
            StatusCode::UNPROCESSABLE_ENTITY,
            rv::device_form(
                &ctx,
                &typed,
                Some(
                    "That code isn't right, or it has run out. Check the code Library Rip shows, \
                     or start again there.",
                ),
            ),
        )
    };
    match form.get("decision") {
        "" => Ok(match st.rip.waiting_device(&typed) {
            Some(d) => html(rv::device_confirm(&ctx, &d.name, &d.user_code)),
            None => unknown(),
        }),
        decision => {
            let approve = decision == "yes";
            Ok(match st.rip.decide_device(&typed, approve) {
                Some(d) => {
                    tracing::info!(name = %d.name, approve, "library rip device code answered");
                    html(rv::device_done(&ctx, &d.name, approve))
                }
                None => unknown(),
            })
        }
    }
}

/// `POST /admin/rippers/{id}/disconnect`: revokes a ripper's key.
pub async fn disconnect(State(st): State<AppState>, Admin(_): Admin, Path(id): Path<i64>) -> Res {
    if !st.db.write(move |c| api_keys::revoke(c, id)).await? {
        return Err(AppError::NotFound);
    }
    tracing::info!(key = id, "library rip disconnected from the desk");
    Ok(see_other("/admin?msg=disconnected#rippers"))
}

// ---------- downloads ----------

/// A file name we are willing to serve from the downloads folder.
pub fn is_download_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
        && !name.starts_with('.')
        && !name.contains("..")
}

/// Files that are ripper builds (not checksums, partial copies or notes).
fn is_build(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    is_download_name(name)
        && ![
            ".sha256",
            ".sha256sum",
            ".part",
            ".tmp",
            ".txt",
            ".md",
            ".asc",
            ".sig",
        ]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// The builds in the downloads folders, with sizes and SHA-256s.
async fn builds(st: &AppState) -> Vec<Build> {
    let mut found: Vec<(String, PathBuf, u64, std::time::SystemTime)> = Vec::new();
    for dir in st.cfg.ripper_download_dirs() {
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if !is_build(&name) || found.iter().any(|f| f.0 == name) {
                continue;
            }
            let Ok(meta) = entry.metadata().await else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            found.push((name, entry.path(), meta.len(), modified));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = Vec::with_capacity(found.len());
    for (file, path, size, modified) in found {
        let sha256 = st.rip.download_sha256(path, size, modified).await;
        out.push(Build {
            os: Os::of_build(&file),
            file,
            size,
            sha256,
        });
    }
    out.sort_by(|a, b| a.os.cmp(&b.os).then_with(|| a.file.cmp(&b.file)));
    out
}

/// `GET /ripper`: "Get Library Rip".
pub async fn ripper_page(State(st): State<AppState>, viewer: Viewer, headers: HeaderMap) -> Res {
    let ctx = st.ctx(&viewer).await?;
    let visitor = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .and_then(Os::from_user_agent);
    let builds = builds(&st).await;
    Ok(html(rv::ripper_page(
        &ctx,
        &RipperView {
            builds: &builds,
            visitor,
            source_url: &st.cfg.source_url,
        },
    )))
}

/// `GET /ripper/download/{file}`
pub async fn download(
    State(st): State<AppState>,
    Path(file): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !is_build(&file) {
        return AppError::NotFound.into_response();
    }
    let mut found = None;
    for dir in st.cfg.ripper_download_dirs() {
        let path = dir.join(&file);
        if matches!(tokio::fs::metadata(&path).await, Ok(m) if m.is_file()) {
            found = Some(path);
            break;
        }
    }
    let Some(path) = found else {
        return AppError::NotFound.into_response();
    };
    let mut res =
        super::stream::serve_file(&path, "application/octet-stream", &file, &headers).await;
    super::stream::make_attachment(&mut res, &file);
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_links_are_checked() {
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        let ok = check_request(
            "library-rip",
            "Henry's desktop",
            "http://127.0.0.1:5000/callback",
            "abc",
            challenge,
            "S256",
        )
        .unwrap();
        assert_eq!(ok.name, "Henry's desktop");
        let bad = |client, uri, state, method| {
            check_request(client, "x", uri, state, challenge, method).is_err()
        };
        assert!(bad("other", "http://127.0.0.1:5000/cb", "s", "S256"));
        assert!(bad("library-rip", "https://evil.example/cb", "s", "S256"));
        assert!(bad("library-rip", "http://127.0.0.1:5000/cb", "", "S256"));
        assert!(bad("library-rip", "http://127.0.0.1:5000/cb", "s", "plain"));
    }

    #[test]
    fn download_names() {
        assert!(is_build("library-rip-0.1.0-x86_64.AppImage"));
        assert!(is_build("library-rip-0.1.0-linux-x86_64.tar.gz"));
        assert!(!is_build("library-rip.AppImage.sha256"));
        assert!(!is_build(".hidden"));
        assert!(!is_build("../secret"));
        assert!(!is_build("a/b"));
        assert!(!is_build(""));
    }
}
