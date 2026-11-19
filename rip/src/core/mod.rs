// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pure audio-CD logic, testable without a drive (lane B).
//!
//! - [`discid`]: MusicBrainz, freedb, AccurateRip and CTDB ids from a TOC.
//! - [`accuraterip`]: v1/v2 track checksums and the `dBAR` database.
//! - [`ctdb`]: CUETools database lookup and comparison.
//! - [`pressing`]: matching other pressings (AccurateRip and CTDB checksums
//!   over a window of sample shifts, streamed from the track files).
//! - [`offset`]: AccurateRip drive offset database and sample-exact shifting.
//! - [`cue`]: cue sheet.
//! - [`log`]: the human-readable rip log.
//! - [`report_json`]: the machine-readable `rip.json`.
//!
//! Network fetches take a base [`url::Url`] so tests point them at a local
//! mock server, and a whole-request timeout (normally [`SERVICE_TIMEOUT`]).

pub mod accuraterip;
pub mod ctdb;
pub mod cue;
pub mod discid;
pub mod log;
pub mod offset;
pub mod pressing;
pub mod report_json;

use std::time::Duration;

use url::Url;

/// How long one AccurateRip, CTDB or drive-list request may take in all
/// before it is given up ("it took too long"), so a database that accepts
/// the connection and then never answers cannot stall a rip.
pub const SERVICE_TIMEOUT: Duration = Duration::from_secs(30);

/// Errors from the `core` module.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// Input data is malformed (bad database file, PCM length, …).
    #[error("{0}")]
    Invalid(String),
    /// A database could not be reached.
    #[error("could not reach {service}: {source}")]
    Network {
        /// Plain name of the service, e.g. "the AccurateRip database".
        service: &'static str,
        /// The underlying error.
        source: reqwest::Error,
    },
    /// JSON encoding failed.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// A ripped track's audio file could not be read back.
    #[error("could not read {path}: {source}")]
    Io {
        /// The file.
        path: std::path::PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
}

/// Result type of the `core` module.
pub type Result<T, E = CoreError> = std::result::Result<T, E>;

/// `base` with a trailing `/` on its path, so that [`Url::join`] appends
/// to it instead of replacing its last segment.
pub(crate) fn with_trailing_slash(base: &Url) -> Url {
    let mut url = base.clone();
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    url
}

#[cfg(test)]
pub(crate) mod testserver {
    //! A local HTTP server for network tests (never the real internet).

    use url::Url;

    /// Serves `app` on a random loopback port and returns its base URL.
    pub(crate) async fn serve(app: axum::Router) -> Url {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Url::parse(&format!("http://{address}/")).unwrap()
    }

    /// A client that ignores proxy settings from the environment.
    pub(crate) fn client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }
}
