// SPDX-License-Identifier: AGPL-3.0-or-later
//! MusicBrainz identification and Cover Art Archive (lane D).
//!
//! Requests are rate-limited to one per second across the whole process
//! and send a descriptive User-Agent (`LibraryRip/<version> ( <source url> )`),
//! per MusicBrainz rules; answers are cached in memory for an hour. Base
//! URLs are injectable so tests use a local mock server.

mod wire;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use tokio::time::Instant;
use url::Url;

use crate::model::{CoverArt, MSF_OFFSET_SECTORS, Mbid, ReleaseMetadata, Toc};

/// Default MusicBrainz web service base.
pub const DEFAULT_BASE: &str = "https://musicbrainz.org/ws/2/";
/// Default Cover Art Archive base.
pub const DEFAULT_COVER_BASE: &str = "https://coverartarchive.org/";
/// MusicBrainz allows one request per second per client.
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);
/// The largest cover image we download.
pub const MAX_COVER_BYTES: usize = 16 * 1024 * 1024;

/// How long answers stay cached.
const CACHE_TTL: Duration = Duration::from_secs(3600);
/// How many answers are cached.
const CACHE_ENTRIES: usize = 200;
/// Retries when MusicBrainz answers 503 (busy / rate limited).
const BUSY_RETRIES: usize = 2;
/// Whole-request limit for one JSON request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Whole-request limit for one cover download.
const COVER_TIMEOUT: Duration = Duration::from_secs(60);
/// Includes for disc lookups.
const DISC_INCLUDES: &str = "artist-credits recordings isrcs labels";
/// Includes for full release details.
const RELEASE_INCLUDES: &str = "artist-credits labels recordings isrcs release-groups discids";

/// The `User-Agent` sent to MusicBrainz and the Cover Art Archive.
pub fn user_agent() -> String {
    format!("LibraryRip/{} ( {} )", crate::VERSION, crate::SOURCE_URL)
}

/// Errors from MusicBrainz.
#[derive(Debug, thiserror::Error)]
pub enum MusicBrainzError {
    /// Network or HTTP error.
    #[error("could not reach MusicBrainz: {0}")]
    Http(#[from] reqwest::Error),
    /// The answer could not be understood.
    #[error("MusicBrainz sent something unexpected: {0}")]
    Unexpected(String),
    /// MusicBrainz kept saying it was busy.
    #[error("MusicBrainz is busy right now; please try again in a minute")]
    Busy,
    /// The release or medium does not exist.
    #[error("MusicBrainz has no {0}")]
    NotFound(String),
    /// The cover image is too big to download.
    #[error("the cover picture is too big (over {} MB)", MAX_COVER_BYTES / 1024 / 1024)]
    CoverTooBig,
    /// An address could not be built.
    #[error("bad MusicBrainz address: {0}")]
    Url(#[from] url::ParseError),
}

/// Result type of the musicbrainz module.
pub type Result<T, E = MusicBrainzError> = std::result::Result<T, E>;

/// A possible match shown on the Disc screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseCandidate {
    /// Release id.
    pub release_mbid: Mbid,
    /// Title.
    pub title: String,
    /// Artist credit.
    pub artist: String,
    /// Release date.
    pub date: Option<String>,
    /// Country code.
    pub country: Option<String>,
    /// Barcode.
    pub barcode: Option<String>,
    /// Format and disc, e.g. "CD 1 of 2".
    pub medium: String,
    /// Which medium of the release matches the disc (1-based).
    pub medium_position: u8,
    /// Tracks on that medium.
    pub track_count: u8,
    /// Whether the Cover Art Archive has a front image.
    pub has_front_cover: bool,
}

/// Spaces requests out so at most one starts per interval.
#[derive(Debug)]
struct RateLimiter {
    interval: Duration,
    next: tokio::sync::Mutex<Option<Instant>>,
}

impl RateLimiter {
    fn new(interval: Duration) -> Self {
        RateLimiter {
            interval,
            next: tokio::sync::Mutex::new(None),
        }
    }

    /// Waits for our turn. The lock is held while sleeping, so waiters
    /// queue up in order.
    async fn wait(&self) {
        let mut next = self.next.lock().await;
        if let Some(when) = *next {
            tokio::time::sleep_until(when).await;
        }
        *next = Some(Instant::now() + self.interval);
    }
}

/// The limiter every default client shares.
fn process_limiter() -> Arc<RateLimiter> {
    static LIMITER: OnceLock<Arc<RateLimiter>> = OnceLock::new();
    Arc::clone(LIMITER.get_or_init(|| Arc::new(RateLimiter::new(MIN_INTERVAL))))
}

/// Answers by URL, for an hour.
#[derive(Debug, Default)]
struct Cache {
    entries: HashMap<String, (Instant, Arc<[u8]>)>,
}

impl Cache {
    fn get(&mut self, key: &str) -> Option<Arc<[u8]>> {
        match self.entries.get(key) {
            Some((stored, body)) if stored.elapsed() < CACHE_TTL => Some(Arc::clone(body)),
            Some(_) => {
                self.entries.remove(key);
                None
            }
            None => None,
        }
    }

    fn put(&mut self, key: String, body: Arc<[u8]>) {
        if self.entries.len() >= CACHE_ENTRIES {
            self.entries
                .retain(|_, (stored, _)| stored.elapsed() < CACHE_TTL);
            if self.entries.len() >= CACHE_ENTRIES {
                let oldest = self
                    .entries
                    .iter()
                    .min_by_key(|(_, (stored, _))| *stored)
                    .map(|(key, _)| key.clone());
                if let Some(oldest) = oldest {
                    self.entries.remove(&oldest);
                }
            }
        }
        self.entries.insert(key, (Instant::now(), body));
    }
}

/// The MusicBrainz + Cover Art Archive client. Cheap to clone; clones
/// share the cache.
#[derive(Clone, Debug)]
pub struct MusicBrainzClient {
    http: reqwest::Client,
    base: Url,
    cover_base: Url,
    limiter: Arc<RateLimiter>,
    cache: Arc<Mutex<Cache>>,
}

impl MusicBrainzClient {
    /// A client using the given bases (see [`DEFAULT_BASE`] and
    /// [`DEFAULT_COVER_BASE`]), sharing the process-wide one-per-second
    /// rate limit.
    pub fn new(http: reqwest::Client, base: Url, cover_base: Url) -> Self {
        MusicBrainzClient {
            http,
            base: with_trailing_slash(base),
            cover_base: with_trailing_slash(cover_base),
            limiter: process_limiter(),
            cache: Arc::default(),
        }
    }

    /// A client for musicbrainz.org and coverartarchive.org.
    pub fn public(http: reqwest::Client) -> Result<Self> {
        Ok(Self::new(
            http,
            Url::parse(DEFAULT_BASE)?,
            Url::parse(DEFAULT_COVER_BASE)?,
        ))
    }

    /// The same client with its own limiter at `interval` instead of the
    /// shared one-per-second limit: for a local mirror or tests. Never
    /// use less than [`MIN_INTERVAL`] for musicbrainz.org.
    pub fn with_min_interval(mut self, interval: Duration) -> Self {
        self.limiter = Arc::new(RateLimiter::new(interval));
        self
    }

    /// `GET discid/{id}?toc=…&inc=artist-credits+recordings+isrcs+labels`:
    /// releases with this disc ID, or when MusicBrainz does not know the
    /// ID, releases whose TOC is close (fuzzy matches). Each candidate
    /// names the medium that matches the disc (by disc ID, else by track
    /// count). Unknown discs give an empty list.
    pub async fn lookup_disc(&self, disc_id: &str, toc: &Toc) -> Result<Vec<ReleaseCandidate>> {
        if disc_id.len() != 28
            || !disc_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(MusicBrainzError::Unexpected(format!(
                "\"{disc_id}\" is not a MusicBrainz disc ID"
            )));
        }
        let mut url = self.base.join(&format!("discid/{disc_id}"))?;
        url.query_pairs_mut()
            .append_pair("toc", &toc_parameter(toc))
            .append_pair("cdstubs", "no")
            .append_pair("inc", DISC_INCLUDES)
            .append_pair("fmt", "json");
        let Some(answer) = self.get_json::<wire::DiscAnswer>(url).await? else {
            return Ok(Vec::new());
        };
        let audio_tracks = toc.audio_tracks().count();
        Ok(answer
            .releases
            .iter()
            .filter_map(|release| release.candidate(Some(disc_id), audio_tracks))
            .collect())
    }

    /// Text search for releases (when the disc ID is unknown). A release
    /// with several discs gives one candidate per disc.
    pub async fn search(&self, query: &str) -> Result<Vec<ReleaseCandidate>> {
        let mut url = self.base.join("release")?;
        url.query_pairs_mut()
            .append_pair("query", query)
            .append_pair("limit", "25")
            .append_pair("fmt", "json");
        let Some(answer) = self.get_json::<wire::SearchAnswer>(url).await? else {
            return Ok(Vec::new());
        };
        Ok(answer
            .releases
            .iter()
            .flat_map(wire::Release::candidates_per_medium)
            .collect())
    }

    /// Full metadata for one medium of a release (no cover; see
    /// [`MusicBrainzClient::front_cover`]). `disc_id` is left `None`: the
    /// caller fills in the disc ID it matched with.
    pub async fn release(&self, release: &Mbid, medium_position: u8) -> Result<ReleaseMetadata> {
        let mut url = self.base.join(&format!("release/{release}"))?;
        url.query_pairs_mut()
            .append_pair("inc", RELEASE_INCLUDES)
            .append_pair("fmt", "json");
        let answer = self
            .get_json::<wire::Release>(url)
            .await?
            .ok_or_else(|| MusicBrainzError::NotFound(format!("release {release}")))?;
        answer.metadata(medium_position)
    }

    /// The front cover (`release/{id}/front-500`), `None` if there is none.
    pub async fn front_cover(&self, release: &Mbid) -> Result<Option<CoverArt>> {
        self.cover(release, "front-500").await
    }

    /// A small front cover for the match list (`front-250`).
    pub async fn front_thumbnail(&self, release: &Mbid) -> Result<Option<CoverArt>> {
        self.cover(release, "front-250").await
    }

    async fn cover(&self, release: &Mbid, size: &str) -> Result<Option<CoverArt>> {
        let url = self.cover_base.join(&format!("release/{release}/{size}"))?;
        // The Cover Art Archive redirects to the image on archive.org;
        // reqwest follows redirects by default.
        let mut response = self
            .http
            .get(url)
            .header(reqwest::header::USER_AGENT, user_agent())
            .timeout(COVER_TIMEOUT)
            .send()
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(MusicBrainzError::Unexpected(format!(
                "the Cover Art Archive answered {}",
                response.status()
            )));
        }
        let mime = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(|value| value.trim().to_ascii_lowercase())
            .unwrap_or_default();
        if !mime.starts_with("image/") {
            return Err(MusicBrainzError::Unexpected(format!(
                "the cover is not a picture ({mime})"
            )));
        }
        let declared = response.content_length().unwrap_or(0);
        if usize::try_from(declared).map_or(true, |declared| declared > MAX_COVER_BYTES) {
            return Err(MusicBrainzError::CoverTooBig);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len().saturating_add(chunk.len()) > MAX_COVER_BYTES {
                return Err(MusicBrainzError::CoverTooBig);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Some(CoverArt {
            bytes: bytes.into(),
            mime,
        }))
    }

    /// GETs JSON through the cache and rate limiter; `None` on 404.
    async fn get_json<T: DeserializeOwned>(&self, url: Url) -> Result<Option<T>> {
        let key = url.to_string();
        let cached = self.cache.lock().ok().and_then(|mut cache| cache.get(&key));
        let body = match cached {
            Some(body) => body,
            None => {
                let Some(body) = self.fetch(url).await? else {
                    return Ok(None);
                };
                if let Ok(mut cache) = self.cache.lock() {
                    cache.put(key, Arc::clone(&body));
                }
                body
            }
        };
        serde_json::from_slice(&body)
            .map(Some)
            .map_err(|error| MusicBrainzError::Unexpected(error.to_string()))
    }

    async fn fetch(&self, url: Url) -> Result<Option<Arc<[u8]>>> {
        for _ in 0..=BUSY_RETRIES {
            self.limiter.wait().await;
            let response = self
                .http
                .get(url.clone())
                .header(reqwest::header::USER_AGENT, user_agent())
                .header(reqwest::header::ACCEPT, "application/json")
                .timeout(REQUEST_TIMEOUT)
                .send()
                .await?;
            match response.status() {
                status if status.is_success() => {
                    return Ok(Some(response.bytes().await?.to_vec().into()));
                }
                StatusCode::NOT_FOUND => return Ok(None),
                StatusCode::SERVICE_UNAVAILABLE | StatusCode::TOO_MANY_REQUESTS => continue,
                status => {
                    return Err(MusicBrainzError::Unexpected(format!(
                        "MusicBrainz answered {status}"
                    )));
                }
            }
        }
        Err(MusicBrainzError::Busy)
    }
}

fn with_trailing_slash(mut url: Url) -> Url {
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    url
}

/// The `toc` parameter: first track, last track, lead-out and each track
/// offset, in sectors including the 150-sector lead-in, over the tracks
/// the MusicBrainz disc ID covers (trailing data tracks left out).
pub fn toc_parameter(toc: &Toc) -> String {
    let tracks = toc.tracks();
    let last_audio = tracks.iter().rposition(|track| track.is_audio);
    let covered = last_audio
        .and_then(|last| tracks.get(..=last))
        .unwrap_or(tracks);
    let first = covered.first().map_or(1, |track| track.number.get());
    let last = covered.last().map_or(1, |track| track.number.get());
    let lead_out = crate::core::discid::audio_lead_out(toc);
    let mut parts = vec![
        first.to_string(),
        last.to_string(),
        lead_out.0.saturating_add(MSF_OFFSET_SECTORS).to_string(),
    ];
    parts.extend(covered.iter().map(|track| {
        track
            .start_lba
            .0
            .saturating_add(MSF_OFFSET_SECTORS)
            .to_string()
    }));
    parts.join(" ")
}

#[cfg(test)]
mod tests;
