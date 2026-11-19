// SPDX-License-Identifier: AGPL-3.0-or-later
//! Loopback flow: listen on `127.0.0.1:<random>`, open `/connect` in the
//! browser, receive `?code=&state=` on `/callback`, exchange it for a key.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use reqwest::Response;
use serde::Deserialize;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use url::Url;

use super::pkce::{Pkce, random_state};
use super::{BrowserOpener, post_token};
use crate::library::api::{TokenRequest, TokenResponse};
use crate::library::client::{LibraryClient, error_from, normalise_base, read_json};
use crate::library::{LibraryError, Result};

/// How long to wait for the librarian before giving up.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

/// How long to let the "you can close this tab" page finish sending.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// The `client` value sent to `/connect`.
const CLIENT_NAME: &str = "library-rip";

/// Runs the whole loopback flow against the library at `base`: starts the
/// listener, opens the browser, waits up to `timeout` for the answer and
/// exchanges the code for a key.
///
/// If the browser cannot be opened the error is
/// [`LibraryError::Browser`], which carries the address for the user to
/// open by hand; use [`LoopbackSession`] directly to show it while
/// waiting instead.
pub async fn connect(
    http: &reqwest::Client,
    base: &Url,
    computer_name: &str,
    browser: &dyn BrowserOpener,
    timeout: Duration,
) -> Result<TokenResponse> {
    let session = LoopbackSession::start(base, computer_name).await?;
    if let Err(source) = browser.open(session.connect_url()) {
        return Err(LibraryError::Browser {
            url: session.connect_url().clone(),
            source,
        });
    }
    session.finish(http, timeout).await
}

/// A started loopback handshake: the listener is up and the `/connect`
/// address is ready to open. Dropping it stops the listener.
#[derive(Debug)]
pub struct LoopbackSession {
    base: Url,
    connect_url: Url,
    redirect_uri: String,
    pkce: Pkce,
    answer: oneshot::Receiver<Answer>,
    shutdown: Option<oneshot::Sender<()>>,
    server: Option<JoinHandle<()>>,
}

impl LoopbackSession {
    /// Starts listening on `127.0.0.1:<random port>` and prepares the
    /// `/connect` address with a fresh `state` and PKCE challenge.
    pub async fn start(base: &Url, computer_name: &str) -> Result<Self> {
        let base = normalise_base(base.clone());
        let listener = tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .map_err(LibraryError::Listener)?;
        let port = listener
            .local_addr()
            .map_err(LibraryError::Listener)?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");
        let state = random_state();
        let pkce = Pkce::generate();

        let mut connect_url = base.join("connect")?;
        connect_url
            .query_pairs_mut()
            .append_pair("client", CLIENT_NAME)
            .append_pair("name", computer_name)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("state", &state)
            .append_pair("code_challenge", pkce.challenge())
            .append_pair("code_challenge_method", "S256");

        let (answer_tx, answer) = oneshot::channel();
        let (shutdown, shutdown_rx) = oneshot::channel::<()>();
        let callback = Arc::new(Callback {
            state,
            answer: Mutex::new(Some(answer_tx)),
        });
        let router = Router::new()
            .route("/callback", get(callback_page))
            .with_state(callback);
        let server = axum::serve(listener, router).with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });
        let server = tokio::spawn(async move {
            if let Err(error) = server.await {
                tracing::warn!(%error, "the connect listener stopped");
            }
        });

        Ok(LoopbackSession {
            base,
            connect_url,
            redirect_uri,
            pkce,
            answer,
            shutdown: Some(shutdown),
            server: Some(server),
        })
    }

    /// The address to open in the browser.
    pub fn connect_url(&self) -> &Url {
        &self.connect_url
    }

    /// The `redirect_uri` the library will send the browser back to.
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Waits up to `timeout` for the browser's answer, stops the
    /// listener and exchanges the code for a key.
    pub async fn finish(
        mut self,
        http: &reqwest::Client,
        timeout: Duration,
    ) -> Result<TokenResponse> {
        let answer = match tokio::time::timeout(timeout, &mut self.answer).await {
            Err(_) => Err(LibraryError::TimedOut),
            Ok(Err(_)) => Err(LibraryError::ConnectFailed(
                "the listener stopped before the browser answered".to_owned(),
            )),
            Ok(Ok(answer)) => Ok(answer),
        };
        self.stop().await;
        let code = match answer? {
            Answer::Code(code) => code,
            Answer::Denied => return Err(LibraryError::Denied),
            Answer::StateMismatch => return Err(LibraryError::StateMismatch),
            Answer::Failed(message) => return Err(LibraryError::ConnectFailed(message)),
        };

        let client = LibraryClient::new(http.clone(), self.base.clone(), None);
        let request = TokenRequest::AuthorizationCode {
            code,
            code_verifier: self.pkce.verifier().to_owned(),
            redirect_uri: self.redirect_uri.clone(),
        };
        token_answer(post_token(&client, &request).await?).await
    }

    /// Stops the listener, letting the last page finish sending.
    async fn stop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(mut server) = self.server.take()
            && tokio::time::timeout(SHUTDOWN_GRACE, &mut server)
                .await
                .is_err()
        {
            // A browser holding a keep-alive connection open; stop anyway.
            server.abort();
        }
    }
}

impl Drop for LoopbackSession {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(server) = self.server.take() {
            server.abort();
        }
    }
}

/// Interprets the token endpoint's answer to an authorisation code.
async fn token_answer(response: Response) -> Result<TokenResponse> {
    match response.status() {
        status if status.is_success() => read_json(response).await,
        reqwest::StatusCode::FORBIDDEN => Err(LibraryError::Denied),
        reqwest::StatusCode::GONE => Err(LibraryError::Expired),
        _ => Err(error_from(response).await),
    }
}

/// What the browser brought back.
#[derive(Debug, PartialEq, Eq)]
enum Answer {
    Code(String),
    Denied,
    StateMismatch,
    Failed(String),
}

/// Shared with the callback handler.
struct Callback {
    state: String,
    answer: Mutex<Option<oneshot::Sender<Answer>>>,
}

/// `/callback` query parameters.
#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

impl CallbackQuery {
    fn answer(self, expected_state: &str) -> Answer {
        if self.state.as_deref() != Some(expected_state) {
            return Answer::StateMismatch;
        }
        match (self.error, self.code) {
            (Some(error), _) if error == "access_denied" => Answer::Denied,
            (Some(error), _) => Answer::Failed(self.error_description.unwrap_or(error)),
            (None, Some(code)) if !code.is_empty() => Answer::Code(code),
            (None, _) => Answer::Failed("the library sent no code".to_owned()),
        }
    }
}

async fn callback_page(
    State(callback): State<Arc<Callback>>,
    Query(query): Query<CallbackQuery>,
) -> impl IntoResponse {
    let answer = query.answer(&callback.state);
    let (status, heading, text) = match &answer {
        Answer::Code(_) => (
            StatusCode::OK,
            "Library Rip is connecting",
            "You can close this tab and go back to Library Rip.",
        ),
        Answer::Denied => (
            StatusCode::OK,
            "Not connected",
            "The connection was not allowed. You can close this tab.",
        ),
        Answer::StateMismatch => (
            StatusCode::BAD_REQUEST,
            "Something did not match",
            "This answer was not for this Library Rip. Close this tab and try connecting again.",
        ),
        Answer::Failed(_) => (
            StatusCode::BAD_REQUEST,
            "Not connected",
            "The library could not connect Library Rip. Close this tab and try again.",
        ),
    };
    let sender = callback.answer.lock().ok().and_then(|mut slot| slot.take());
    let page = match sender {
        Some(sender) => {
            let _ = sender.send(answer);
            page(heading, text)
        }
        None => page(
            "Already answered",
            "Library Rip already has its answer. You can close this tab.",
        ),
    };
    (status, Html(page))
}

/// A small self-contained page (no user input is echoed).
fn page(heading: &str, text: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{heading} – Library Rip</title><style>\
         body{{font-family:system-ui,sans-serif;background:#f7f4ee;color:#2b2620;\
         display:grid;place-items:center;min-height:100vh;margin:0;padding:1rem}}\
         main{{max-width:28rem;text-align:center}}h1{{font-size:1.5rem}}\
         @media (prefers-color-scheme:dark){{body{{background:#1d1a16;color:#ece6dc}}}}\
         </style></head><body><main><h1>{heading}</h1><p>{text}</p></main></body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(code: Option<&str>, state: Option<&str>, error: Option<&str>) -> CallbackQuery {
        CallbackQuery {
            code: code.map(str::to_owned),
            state: state.map(str::to_owned),
            error: error.map(str::to_owned),
            error_description: None,
        }
    }

    #[test]
    fn callback_answers() {
        assert_eq!(
            query(Some("c"), Some("s"), None).answer("s"),
            Answer::Code("c".into())
        );
        assert_eq!(
            query(None, Some("s"), Some("access_denied")).answer("s"),
            Answer::Denied
        );
        assert_eq!(
            query(Some("c"), Some("other"), None).answer("s"),
            Answer::StateMismatch
        );
        assert_eq!(
            query(Some("c"), None, None).answer("s"),
            Answer::StateMismatch
        );
        assert!(matches!(
            query(None, Some("s"), Some("server_error")).answer("s"),
            Answer::Failed(_)
        ));
        assert!(matches!(
            query(None, Some("s"), None).answer("s"),
            Answer::Failed(_)
        ));
    }
}
