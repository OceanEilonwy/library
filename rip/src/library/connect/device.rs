// SPDX-License-Identifier: AGPL-3.0-or-later
//! Device-code flow: for when the browser is on another device.

use std::time::Duration;

use reqwest::StatusCode;
use tokio::time::Instant;

use super::super::api::{DeviceRequest, DeviceResponse, TokenRequest, TokenResponse};
use super::post_token;
use crate::library::client::{LibraryClient, check, error_message, read_json};
use crate::library::{LibraryError, Result};

/// The shortest wait between polls, whatever the library asks for.
const MIN_INTERVAL: Duration = Duration::from_secs(1);
/// Added to the interval when the library says to slow down.
const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);

/// `POST /api/rip/device`: asks for a code to show the user.
pub async fn request_code(
    http: &reqwest::Client,
    base: &url::Url,
    computer_name: &str,
) -> Result<DeviceResponse> {
    let client = LibraryClient::new(http.clone(), base.clone(), None);
    let response = client
        .request(reqwest::Method::POST, client.endpoint("api/rip/device")?)
        .json(&DeviceRequest {
            name: computer_name.to_owned(),
        })
        .send()
        .await?;
    read_json(check(response).await?).await
}

/// Polls `POST /api/rip/token` every `interval` seconds until the librarian
/// approves (key), refuses ([`LibraryError::Denied`]) or the code expires
/// ([`LibraryError::Expired`]). Drop the future to cancel.
pub async fn poll_for_key(
    http: &reqwest::Client,
    base: &url::Url,
    device: &DeviceResponse,
) -> Result<TokenResponse> {
    let interval = Duration::from_secs(device.interval).max(MIN_INTERVAL);
    poll(http, base, device, interval).await
}

/// One poll's verdict.
#[derive(Debug, PartialEq, Eq)]
enum Pending {
    Wait,
    SlowDown,
}

/// The polling loop with an explicit first interval (tests use a short one).
async fn poll(
    http: &reqwest::Client,
    base: &url::Url,
    device: &DeviceResponse,
    mut interval: Duration,
) -> Result<TokenResponse> {
    let client = LibraryClient::new(http.clone(), base.clone(), None);
    let deadline = Instant::now() + Duration::from_secs(device.expires_in);
    let request = TokenRequest::DeviceCode {
        device_code: device.device_code.clone(),
    };
    loop {
        if Instant::now() + interval > deadline {
            return Err(LibraryError::Expired);
        }
        tokio::time::sleep(interval).await;
        let response = post_token(&client, &request).await?;
        if response.status().is_success() {
            return read_json(response).await;
        }
        match pending(response).await? {
            Pending::Wait => {}
            Pending::SlowDown => interval += SLOW_DOWN_STEP,
        }
    }
}

/// Interprets an unsuccessful poll: keep waiting, slow down, or an error.
/// Accepts both the contract's statuses (428/403/410/429) and the
/// RFC 8628 error codes in the body.
async fn pending(response: reqwest::Response) -> Result<Pending> {
    let status = response.status();
    let message = error_message(response).await;
    // `error_message` gives the body's `error` field when it has one.
    match (status, message.as_str()) {
        (StatusCode::PRECONDITION_REQUIRED, _) | (_, "authorization_pending") => Ok(Pending::Wait),
        (StatusCode::TOO_MANY_REQUESTS, _) | (_, "slow_down") => Ok(Pending::SlowDown),
        (StatusCode::FORBIDDEN, _) | (_, "access_denied") => Err(LibraryError::Denied),
        (StatusCode::GONE, _) | (_, "expired_token") => Err(LibraryError::Expired),
        (StatusCode::UNPROCESSABLE_ENTITY, _) => Err(LibraryError::Rejected(message)),
        (other, _) => Err(LibraryError::Status {
            status: other.as_u16(),
            message,
        }),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    //! Device polling against a local mock token endpoint.

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Json;
    use axum::Router;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;
    use serde_json::{Value, json};

    use super::*;

    /// Serves `router` on a random local port; returns its base URL.
    pub(crate) async fn serve(router: Router) -> url::Url {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        url::Url::parse(&format!("http://{address}/")).unwrap()
    }

    fn device(expires_in: u64) -> DeviceResponse {
        DeviceResponse {
            device_code: "dev".into(),
            user_code: "WXYZ-1234".into(),
            verification_uri: "http://x/connect/device".into(),
            interval: 0,
            expires_in,
        }
    }

    /// The mock's script and call counter.
    type Script = (Arc<Vec<(u16, Value)>>, Arc<AtomicUsize>);

    /// Answers each poll from `script` in turn (repeating the last).
    async fn scripted(script: Vec<(u16, Value)>) -> (url::Url, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let state = (Arc::new(script), Arc::clone(&calls));
        let router = Router::new()
            .route(
                "/api/rip/token",
                post(
                    |State((script, calls)): State<Script>,
                     Json(body): Json<Value>|
                     -> std::future::Ready<Response> {
                        assert_eq!(body["grantType"], "device_code");
                        assert_eq!(body["deviceCode"], "dev");
                        let call = calls.fetch_add(1, Ordering::SeqCst);
                        let (status, answer) = script.get(call).or(script.last()).cloned().unwrap();
                        std::future::ready(
                            (StatusCode::from_u16(status).unwrap(), Json(answer)).into_response(),
                        )
                    },
                ),
            )
            .with_state(state);
        (serve(router).await, calls)
    }

    fn token() -> Value {
        json!({"apiKey": "lrk_abc", "keyId": 3, "keyName": "Desk",
            "library": {"name": "The Library", "url": "http://x/"}})
    }

    fn pending_answer() -> (u16, Value) {
        (428, json!({"error": "authorization_pending"}))
    }

    #[tokio::test]
    async fn polls_until_approved() {
        let (base, calls) =
            scripted(vec![pending_answer(), pending_answer(), (200, token())]).await;
        let key = poll(
            &reqwest::Client::new(),
            &base,
            &device(60),
            Duration::from_millis(10),
        )
        .await
        .unwrap();
        assert_eq!(key.key_id, 3);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn refused_and_expired() {
        let (base, _) = scripted(vec![pending_answer(), (403, json!({"error": "No"}))]).await;
        let result = poll(
            &reqwest::Client::new(),
            &base,
            &device(60),
            Duration::from_millis(10),
        )
        .await;
        assert!(matches!(result, Err(LibraryError::Denied)));

        let (base, _) = scripted(vec![(410, json!({"error": "expired_token"}))]).await;
        let result = poll(
            &reqwest::Client::new(),
            &base,
            &device(60),
            Duration::from_millis(10),
        )
        .await;
        assert!(matches!(result, Err(LibraryError::Expired)));

        // RFC 8628 style: 400 with a code in the body.
        let (base, _) = scripted(vec![(400, json!({"error": "access_denied"}))]).await;
        let result = poll(
            &reqwest::Client::new(),
            &base,
            &device(60),
            Duration::from_millis(10),
        )
        .await;
        assert!(matches!(result, Err(LibraryError::Denied)));
    }

    #[tokio::test]
    async fn gives_up_at_the_local_deadline() {
        let (base, _) = scripted(vec![pending_answer()]).await;
        let result = poll(
            &reqwest::Client::new(),
            &base,
            &device(0),
            Duration::from_millis(10),
        )
        .await;
        assert!(matches!(result, Err(LibraryError::Expired)));
    }

    #[tokio::test]
    async fn slow_down_adds_to_the_interval() {
        let (base, calls) = scripted(vec![(429, json!({"error": "Too many requests"}))]).await;
        // 10 ms, then 5.01 s: the second poll would pass the 2 s deadline.
        let result = poll(
            &reqwest::Client::new(),
            &base,
            &device(2),
            Duration::from_millis(10),
        )
        .await;
        assert!(matches!(result, Err(LibraryError::Expired)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn public_poll_honours_the_server_interval() {
        let (base, calls) = scripted(vec![pending_answer(), (200, token())]).await;
        let started = std::time::Instant::now();
        let mut answer = device(30);
        answer.interval = 1;
        let key = poll_for_key(&reqwest::Client::new(), &base, &answer)
            .await
            .unwrap();
        assert_eq!(key.key_name, "Desk");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(started.elapsed() >= Duration::from_secs(2));
    }

    #[tokio::test]
    async fn request_code_sends_the_computer_name() {
        let router = Router::new().route(
            "/api/rip/device",
            post(|Json(body): Json<Value>| async move {
                assert_eq!(body, json!({"name": "Henry's desktop"}));
                Json(json!({"deviceCode": "dev", "userCode": "WXYZ-1234",
                    "verificationUri": "http://x/connect/device", "interval": 5, "expiresIn": 600}))
            }),
        );
        let base = serve(router).await;
        let answer = request_code(&reqwest::Client::new(), &base, "Henry's desktop")
            .await
            .unwrap();
        assert_eq!(answer.user_code, "WXYZ-1234");
        assert_eq!(answer.interval, 5);
    }
}
