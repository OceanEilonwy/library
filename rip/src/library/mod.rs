// SPDX-License-Identifier: AGPL-3.0-or-later
//! Talking to The Library (lane D): the rip API client, the connect
//! handshake and where the API key is kept. See docs/rip.md.
//!
//! The usual sequence for the app:
//!
//! 1. [`http_client`] once per process;
//! 2. [`LibraryClient::new`] without a key and [`LibraryClient::info`] to
//!    check the address;
//! 3. [`connect::loopback::connect`] (or [`connect::device`]) for a key;
//! 4. [`keystore::default_store`] and [`keystore::KeyStore::save`] with
//!    [`keystore::StoredConnection::from_token`];
//! 5. [`LibraryClient::with_key`] for the rip API, and [`disconnect`] to
//!    revoke and forget the key.

pub mod api;
pub mod client;
pub mod connect;
pub mod keystore;

use std::time::Duration;

pub use api::ApiKey;
pub use client::LibraryClient;

/// The `User-Agent` every request to the library sends.
pub fn user_agent() -> String {
    format!("LibraryRip/{}", crate::VERSION)
}

/// How long to wait for a TCP connection to the library.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a connection may stay silent before the request fails.
pub const READ_TIMEOUT: Duration = Duration::from_secs(120);
/// Whole-request limit for small JSON requests (not uploads).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The one HTTP client the whole app shares (the library, MusicBrainz,
/// the Cover Art Archive, AccurateRip and CTDB): rustls, a default
/// user agent that identifies Library Rip with a contact address as
/// MusicBrainz asks ([`crate::musicbrainz::user_agent`]; library requests
/// still send [`user_agent`]), connect and read timeouts so a server that
/// goes silent fails the request (no overall timeout, so long uploads are
/// not cut off; small requests add their own). Build it once and clone
/// it; clones share connections.
pub fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(crate::musicbrainz::user_agent())
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()?)
}

/// What [`disconnect`] managed to do.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Disconnected {
    /// The library revoked the key and it is forgotten here.
    Revoked,
    /// The library had already revoked the key; it is forgotten here.
    AlreadyRevoked,
    /// The library could not be reached, so the key is only forgotten
    /// here; the librarian can still disconnect it from the desk.
    ForgottenOnly,
}

/// Disconnects this computer: `DELETE /api/rip/token`, then forgets the
/// key in `store` whatever the library said (the user asked to
/// disconnect). Only a failure to forget the key locally is an error.
///
/// `store` is blocking (the OS keyring); this call is short, but a GUI
/// should still run it off the UI thread.
pub async fn disconnect(
    client: &LibraryClient,
    store: &dyn keystore::KeyStore,
) -> Result<Disconnected> {
    let outcome = match client.revoke().await {
        Ok(()) => Disconnected::Revoked,
        Err(LibraryError::Unauthorised) => Disconnected::AlreadyRevoked,
        Err(error) => {
            tracing::warn!(%error, "could not revoke the key on the library");
            Disconnected::ForgottenOnly
        }
    };
    store.clear()?;
    Ok(outcome)
}

/// Errors talking to the library.
#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    /// The address does not look like a library.
    #[error("{0} does not look like a library")]
    NotALibrary(String),
    /// This ripper is older than the library allows.
    #[error("this library needs Library Rip {required} or newer; you have {current}")]
    TooOld {
        /// `minRipperVersion` from `/api/rip/info`.
        required: String,
        /// Our version.
        current: String,
    },
    /// The key was revoked or is wrong (HTTP 401): this computer is
    /// disconnected and must connect again.
    #[error("this computer is no longer connected to the library; please connect again")]
    Unauthorised,
    /// The request needs a key and the client has none.
    #[error("this computer is not connected to a library yet")]
    NotConnected,
    /// The librarian said no.
    #[error("the librarian did not allow the connection")]
    Denied,
    /// The device code or one-time code expired.
    #[error("the connection request expired; please try again")]
    Expired,
    /// Nobody answered in the browser in time.
    #[error("nobody answered in the browser in time; please try again")]
    TimedOut,
    /// The connect callback did not match (wrong `state`).
    #[error("the browser sent back an unexpected answer; please try again")]
    StateMismatch,
    /// The browser came back with an error other than "no".
    #[error("the library could not connect this computer: {0}")]
    ConnectFailed(String),
    /// The browser could not be opened; show `url` for the user to open.
    #[error("could not open your web browser ({source}); please open {url} yourself")]
    Browser {
        /// The `/connect` address to open by hand.
        url: url::Url,
        /// Why it failed.
        source: std::io::Error,
    },
    /// The local listener for the browser's answer could not start.
    #[error("could not listen for the browser's answer: {0}")]
    Listener(std::io::Error),
    /// The file is too big for the library (HTTP 413).
    #[error("the library will not take a file this big: {0}")]
    TooLarge(String),
    /// The library refused the request as invalid (HTTP 422).
    #[error("the library did not accept it: {0}")]
    Rejected(String),
    /// A file changed on disk while it was being uploaded.
    #[error("{} changed while it was being uploaded; please rip again", .0.display())]
    FileChanged(std::path::PathBuf),
    /// The server answered with an unexpected status.
    #[error("the library answered {status}: {message}")]
    Status {
        /// HTTP status.
        status: u16,
        /// Message from the body, if any.
        message: String,
    },
    /// The answer could not be understood.
    #[error("the library sent an answer Library Rip does not understand: {0}")]
    Unexpected(String),
    /// Network or HTTP error.
    #[error("could not reach the library: {0}")]
    Http(#[from] reqwest::Error),
    /// Bad URL.
    #[error("that is not a web address: {0}")]
    Url(#[from] url::ParseError),
    /// Key storage failed.
    #[error("could not store the connection: {0}")]
    KeyStore(String),
    /// File system error.
    #[error("could not use {path}: {source}")]
    Io {
        /// The file involved.
        path: std::path::PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
}

/// Result type of the library module.
pub type Result<T, E = LibraryError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests;
