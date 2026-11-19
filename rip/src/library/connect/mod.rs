// SPDX-License-Identifier: AGPL-3.0-or-later
//! The connect handshake: loopback redirect with PKCE, and the device-code
//! fallback (docs/rip.md, "Connect handshake").

pub mod device;
pub mod loopback;
pub mod pkce;

use reqwest::Response;

use super::Result;
use super::api::{TokenRequest, TokenResponse};
use super::client::LibraryClient;

/// Opens a URL in the user's browser. The app uses [`SystemBrowser`];
/// tests supply a fake that follows the redirect itself.
pub trait BrowserOpener: Send + Sync {
    /// Opens `url`.
    fn open(&self, url: &url::Url) -> std::io::Result<()>;
}

/// Opens URLs with the operating system's default browser.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemBrowser;

impl BrowserOpener for SystemBrowser {
    fn open(&self, url: &url::Url) -> std::io::Result<()> {
        open::that(url.as_str())
    }
}

/// This computer's name, used as the key name ("Henry's desktop").
pub fn computer_name() -> String {
    hostname::get()
        .ok()
        .and_then(|name| name.into_string().ok())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "This computer".to_owned())
}

/// The outcome of a successful handshake.
pub type Connected = TokenResponse;

/// `POST /api/rip/token` (no key); the caller interprets the status.
pub(crate) async fn post_token(client: &LibraryClient, request: &TokenRequest) -> Result<Response> {
    Ok(client
        .request(reqwest::Method::POST, client.endpoint("api/rip/token")?)
        .json(request)
        .send()
        .await?)
}
