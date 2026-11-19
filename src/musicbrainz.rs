//! Releases and their track lists from MusicBrainz, for the upload studio's
//! "Fill from MusicBrainz" panel.
//!
//! MusicBrainz asks clients to send at most one request a second and to say
//! who they are. Every request to it from this process (the add form's
//! search too) waits its turn at one [`RateLimiter`], goes out with the
//! configured user agent, and answers are kept for an hour in a small
//! [`TtlCache`] (releases by id, searches by query) so a librarian trying a
//! few candidates never asks twice.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tokio::time::Instant;

use crate::util;

/// One track of a medium.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    /// 1-based position on its medium.
    pub position: u32,
    /// The printed number ("7", or "B2" on a record).
    pub number: String,
    pub title: String,
    pub length_ms: Option<i64>,
    /// The track's artist, only when it differs from the release's.
    pub artist: Option<String>,
}

/// One disc (or side, tape, DVD…) of a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Medium {
    /// 1-based disc number.
    pub position: u32,
    /// "CD", "DVD-Video", "12\" Vinyl"…
    pub format: String,
    /// The disc's own title, if it has one.
    pub title: String,
    pub tracks: Vec<Track>,
}

/// A release with its full track list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    /// The MusicBrainz release id (MBID).
    pub id: String,
    pub title: String,
    pub artist: String,
    pub year: Option<i64>,
    pub date: String,
    pub country: String,
    pub barcode: String,
    pub media: Vec<Medium>,
}

impl Release {
    /// Tracks on each disc, in disc order.
    pub fn disc_tracks(&self) -> Vec<u32> {
        self.media
            .iter()
            .map(|m| u32::try_from(m.tracks.len()).unwrap_or(u32::MAX))
            .collect()
    }

    pub fn track_count(&self) -> usize {
        self.media.iter().map(|m| m.tracks.len()).sum()
    }

    /// The summed length of the tracks MusicBrainz knows the length of.
    pub fn total_ms(&self) -> Option<i64> {
        let lengths: Vec<i64> = self
            .media
            .iter()
            .flat_map(|m| &m.tracks)
            .filter_map(|t| t.length_ms)
            .collect();
        (!lengths.is_empty()).then(|| lengths.iter().sum())
    }

    /// "2×CD", "CD + DVD-Video".
    pub fn format_label(&self) -> String {
        format_label(self.media.iter().map(|m| m.format.as_str()))
    }
}

/// A release as a search lists it: no tracks yet, only their counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSummary {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub year: Option<i64>,
    pub date: String,
    pub country: String,
    pub barcode: String,
    pub format: String,
    /// Tracks on each disc.
    pub disc_tracks: Vec<u32>,
}

impl ReleaseSummary {
    pub fn track_count(&self) -> usize {
        self.disc_tracks
            .iter()
            .map(|n| usize::try_from(*n).unwrap_or(0))
            .sum()
    }
}

impl From<&Release> for ReleaseSummary {
    fn from(r: &Release) -> Self {
        Self {
            id: r.id.clone(),
            title: r.title.clone(),
            artist: r.artist.clone(),
            year: r.year,
            date: r.date.clone(),
            country: r.country.clone(),
            barcode: r.barcode.clone(),
            format: r.format_label(),
            disc_tracks: r.disc_tracks(),
        }
    }
}

/// What to ask the release search for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    /// Free text: artist and album, usually.
    Text(String),
    /// A barcode (EAN/UPC), digits only.
    Barcode(String),
}

impl Query {
    /// Reads what a librarian typed: a barcode, or free text.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            None
        } else if util::looks_like_code(s) {
            Some(Self::Barcode(util::normalize_identifier(s)))
        } else {
            Some(Self::Text(s.chars().take(200).collect()))
        }
    }

    /// The Lucene query string MusicBrainz's search takes.
    pub fn lucene(&self) -> String {
        match self {
            Self::Barcode(code) => format!("barcode:{code}"),
            Self::Text(text) => escape_lucene(text),
        }
    }

    /// Key for the search cache.
    fn cache_key(&self) -> String {
        match self {
            Self::Barcode(code) => format!("barcode:{code}"),
            Self::Text(text) => format!("text:{}", util::normalize(text)),
        }
    }
}

/// Escapes the characters Lucene treats as syntax, so "AC/DC: Back in Black"
/// is searched as words.
pub fn escape_lucene(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if matches!(
            c,
            '+' | '-'
                | '&'
                | '|'
                | '!'
                | '('
                | ')'
                | '{'
                | '}'
                | '['
                | ']'
                | '^'
                | '"'
                | '~'
                | '*'
                | '?'
                | ':'
                | '\\'
                | '/'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Finds a MusicBrainz release id in what was typed: a bare MBID or a
/// `https://musicbrainz.org/release/<mbid>` link. Lower-cased.
pub fn mbid_in(s: &str) -> Option<String> {
    let s = s.trim();
    let bytes = s.as_bytes();
    (0..bytes.len().saturating_sub(35)).find_map(|start| {
        let candidate = s.get(start..start + 36)?;
        let shape_ok = candidate.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        });
        let bounded = |i: usize| {
            bytes
                .get(i)
                .is_none_or(|b| !b.is_ascii_alphanumeric() && *b != b'-')
        };
        let before_ok = start == 0 || bounded(start - 1);
        (shape_ok && before_ok && bounded(start + 36)).then(|| candidate.to_ascii_lowercase())
    })
}

/// True if `s` is exactly a release id (as the studio sends it).
pub fn is_mbid(s: &str) -> bool {
    s.len() == 36 && mbid_in(s).is_some()
}

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned()
}

fn year_of(date: &str) -> Option<i64> {
    date.get(..4)
        .and_then(|y| y.parse().ok())
        .filter(|y| (1000..=3000).contains(y))
}

fn position(v: &Value, key: &str) -> Option<u32> {
    v.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

fn length(v: &Value) -> Option<i64> {
    v.get("length").and_then(Value::as_i64).filter(|ms| *ms > 0)
}

/// "Bowie & Eno" from an `artist-credit` array (credited names and join
/// phrases, falling back to the artist's own name).
pub fn artist_credit(v: &Value) -> String {
    v.get("artist-credit")
        .and_then(Value::as_array)
        .map(|credits| {
            credits
                .iter()
                .map(|c| {
                    let name = s(c, "name");
                    let name = if name.is_empty() {
                        c.get("artist").map(|a| s(a, "name")).unwrap_or_default()
                    } else {
                        name
                    };
                    name + c.get("joinphrase").and_then(Value::as_str).unwrap_or("")
                })
                .collect::<String>()
        })
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// Joins disc formats: two CDs are "2×CD", a CD and a DVD "CD + DVD-Video".
pub fn format_label<'a>(formats: impl Iterator<Item = &'a str>) -> String {
    let mut groups: Vec<(&str, usize)> = Vec::new();
    for f in formats {
        let f = if f.trim().is_empty() {
            "disc"
        } else {
            f.trim()
        };
        match groups.last_mut() {
            Some((last, n)) if *last == f => *n += 1,
            _ => groups.push((f, 1)),
        }
    }
    if groups.iter().all(|(f, _)| *f == "disc") {
        return String::new();
    }
    groups
        .iter()
        .map(|(f, n)| {
            if *n > 1 {
                format!("{n}×{f}")
            } else {
                (*f).to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Parses `GET /release/{mbid}?inc=recordings+media+artist-credits&fmt=json`.
/// `None` when the answer has no release id.
pub fn parse_release(json: &Value) -> Option<Release> {
    let id = s(json, "id");
    if id.is_empty() {
        return None;
    }
    let artist = artist_credit(json);
    let mut media: Vec<Medium> = json
        .get("media")
        .and_then(Value::as_array)
        .map(|media| {
            media
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let fallback = u32::try_from(i + 1).unwrap_or(u32::MAX);
                    let mut tracks: Vec<Track> = m
                        .get("tracks")
                        .and_then(Value::as_array)
                        .map(|tracks| {
                            tracks
                                .iter()
                                .enumerate()
                                .map(|(j, t)| {
                                    let recording = t.get("recording");
                                    let mut title = s(t, "title");
                                    if title.is_empty() {
                                        title =
                                            recording.map(|r| s(r, "title")).unwrap_or_default();
                                    }
                                    let credit = artist_credit(t);
                                    let credit = if credit.is_empty() {
                                        recording.map(artist_credit).unwrap_or_default()
                                    } else {
                                        credit
                                    };
                                    let fallback = u32::try_from(j + 1).unwrap_or(u32::MAX);
                                    Track {
                                        position: position(t, "position").unwrap_or(fallback),
                                        number: {
                                            let n = s(t, "number");
                                            if n.is_empty() {
                                                fallback.to_string()
                                            } else {
                                                n
                                            }
                                        },
                                        title,
                                        length_ms: length(t).or_else(|| recording.and_then(length)),
                                        artist: (!credit.is_empty()
                                            && util::normalize(&credit)
                                                != util::normalize(&artist))
                                        .then_some(credit),
                                    }
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    tracks.sort_by_key(|t| t.position);
                    Medium {
                        position: position(m, "position").unwrap_or(fallback),
                        format: s(m, "format"),
                        title: s(m, "title"),
                        tracks,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    media.sort_by_key(|m| m.position);
    let date = s(json, "date");
    Some(Release {
        id,
        title: s(json, "title"),
        artist,
        year: year_of(&date),
        date,
        country: s(json, "country"),
        barcode: s(json, "barcode"),
        media,
    })
}

/// Parses a release search (`GET /release/?query=…&fmt=json`).
pub fn parse_release_search(json: &Value) -> Vec<ReleaseSummary> {
    let Some(releases) = json.get("releases").and_then(Value::as_array) else {
        return Vec::new();
    };
    releases
        .iter()
        .filter(|r| !s(r, "id").is_empty() && !s(r, "title").is_empty())
        .map(|r| {
            let media = r.get("media").and_then(Value::as_array);
            let date = s(r, "date");
            ReleaseSummary {
                id: s(r, "id"),
                title: s(r, "title"),
                artist: artist_credit(r),
                year: year_of(&date),
                date,
                country: s(r, "country"),
                barcode: s(r, "barcode"),
                format: format_label(
                    media
                        .into_iter()
                        .flatten()
                        .map(|m| m.get("format").and_then(Value::as_str).unwrap_or("")),
                ),
                disc_tracks: media
                    .into_iter()
                    .flatten()
                    .map(|m| position(m, "track-count").unwrap_or(0))
                    .collect(),
            }
        })
        .collect()
}

/// Lets one request through per `interval`, across every task that shares
/// it. Waiters queue on the lock in arrival order.
#[derive(Debug)]
pub struct RateLimiter {
    interval: Duration,
    next: tokio::sync::Mutex<Option<Instant>>,
}

impl RateLimiter {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            next: tokio::sync::Mutex::new(None),
        }
    }

    /// Waits until a request may go out, and books the slot after it.
    pub async fn wait(&self) {
        let mut next = self.next.lock().await;
        if let Some(at) = *next
            && at > Instant::now()
        {
            tokio::time::sleep_until(at).await;
        }
        *next = Some(Instant::now() + self.interval);
    }
}

/// A small map whose entries expire after `ttl`; the oldest entry makes
/// way when it is full.
#[derive(Debug)]
pub struct TtlCache<K, V> {
    ttl: Duration,
    max: usize,
    map: Mutex<HashMap<K, (Instant, V)>>,
}

impl<K: Eq + Hash + Clone, V: Clone> TtlCache<K, V> {
    pub fn new(ttl: Duration, max: usize) -> Self {
        Self {
            ttl,
            max: max.max(1),
            map: Mutex::new(HashMap::new()),
        }
    }

    pub fn get(&self, key: &K) -> Option<V> {
        let map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        map.get(key)
            .filter(|(t, _)| t.elapsed() < self.ttl)
            .map(|(_, v)| v.clone())
    }

    pub fn put(&self, key: K, value: V) {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let ttl = self.ttl;
        map.retain(|_, (t, _)| t.elapsed() < ttl);
        if map.len() >= self.max
            && !map.contains_key(&key)
            && let Some(oldest) = map
                .iter()
                .min_by_key(|(_, (t, _))| *t)
                .map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
        map.insert(key, (Instant::now(), value));
    }
}

/// How long answers from MusicBrainz are reused.
pub const CACHE_TTL: Duration = Duration::from_secs(60 * 60);

/// The process-wide MusicBrainz state: the rate limiter and the caches.
#[derive(Debug)]
pub struct Shared {
    pub limiter: RateLimiter,
    pub releases: TtlCache<String, Release>,
    pub searches: TtlCache<String, Vec<ReleaseSummary>>,
}

impl Shared {
    pub fn new(interval: Duration) -> Self {
        Self {
            limiter: RateLimiter::new(interval),
            releases: TtlCache::new(CACHE_TTL, 200),
            searches: TtlCache::new(CACHE_TTL, 200),
        }
    }
}

/// MusicBrainz answered 404: no such release.
#[derive(Debug)]
pub struct NotFound;

impl std::fmt::Display for NotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MusicBrainz has no release with that id.")
    }
}

impl std::error::Error for NotFound {}

impl crate::lookup::Lookups {
    /// Searches MusicBrainz releases. Cached by query.
    pub async fn release_search(&self, query: &Query) -> anyhow::Result<Vec<ReleaseSummary>> {
        let key = query.cache_key();
        if let Some(hit) = self.musicbrainz_state().searches.get(&key) {
            return Ok(hit);
        }
        let url = format!(
            "{}/release/",
            self.config().musicbrainz_url.trim_end_matches('/')
        );
        let lucene = query.lucene();
        let json = self
            .musicbrainz_json(self.client().get(url).query(&[
                ("query", lucene.as_str()),
                ("fmt", "json"),
                ("limit", "10"),
            ]))
            .await?;
        let found = parse_release_search(&json);
        self.musicbrainz_state().searches.put(key, found.clone());
        Ok(found)
    }

    /// One release with its media and tracks. Cached by id.
    pub async fn release(&self, mbid: &str) -> anyhow::Result<Release> {
        let mbid = mbid.to_ascii_lowercase();
        if !is_mbid(&mbid) {
            return Err(NotFound.into());
        }
        if let Some(hit) = self.musicbrainz_state().releases.get(&mbid) {
            return Ok(hit);
        }
        let url = format!(
            "{}/release/{mbid}",
            self.config().musicbrainz_url.trim_end_matches('/')
        );
        let json = self
            .musicbrainz_json(
                self.client()
                    .get(url)
                    .query(&[("inc", "recordings+media+artist-credits"), ("fmt", "json")]),
            )
            .await?;
        let release = parse_release(&json).ok_or(NotFound)?;
        self.musicbrainz_state()
            .releases
            .put(mbid.clone(), release.clone());
        if release.id != mbid {
            // A merged release answers with its new id; remember both.
            self.musicbrainz_state()
                .releases
                .put(release.id.clone(), release.clone());
        }
        Ok(release)
    }

    /// Sends a request to MusicBrainz once the rate limiter allows it.
    pub(crate) async fn musicbrainz_json(
        &self,
        req: reqwest::RequestBuilder,
    ) -> anyhow::Result<Value> {
        self.musicbrainz_state().limiter.wait().await;
        let resp = req.send().await?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(NotFound.into());
        }
        if !status.is_success() {
            anyhow::bail!("HTTP {status}");
        }
        Ok(resp.json().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release_json() -> Value {
        json!({
            "id": "6f1a3e4c-2b9d-4c11-9b0a-1d2e3f405162",
            "title": "Night Songs",
            "date": "1998-03-02",
            "country": "GB",
            "barcode": "5012345678900",
            "artist-credit": [{"name": "The Lanterns", "joinphrase": ""}],
            "media": [
                {"position": 2, "format": "CD", "title": "Bonus", "track-count": 1, "tracks": [
                    {"position": 1, "number": "1", "title": "Encore", "length": 200000,
                     "artist-credit": [{"name": "The Lanterns", "joinphrase": " feat. "}, {"name": "Ada"}]}
                ]},
                {"position": 1, "format": "CD", "title": "", "track-count": 2, "tracks": [
                    {"position": 2, "number": "2", "title": "Second", "length": null,
                     "recording": {"title": "Second", "length": 181000},
                     "artist-credit": [{"name": "The Lanterns"}]},
                    {"position": 1, "number": "1", "title": "First", "length": 245000,
                     "artist-credit": [{"artist": {"name": "the lanterns"}}]}
                ]}
            ]
        })
    }

    #[test]
    fn release_parsing() {
        let r = parse_release(&release_json()).unwrap();
        assert_eq!(r.title, "Night Songs");
        assert_eq!(r.artist, "The Lanterns");
        assert_eq!(r.year, Some(1998));
        assert_eq!(r.country, "GB");
        assert_eq!(r.disc_tracks(), vec![2, 1], "discs sorted by position");
        assert_eq!(r.media[0].tracks[0].title, "First", "tracks sorted");
        assert_eq!(
            r.media[0].tracks[1].length_ms,
            Some(181_000),
            "the recording's length when the track has none"
        );
        assert_eq!(r.media[0].tracks[0].artist, None, "same artist, any case");
        assert_eq!(
            r.media[1].tracks[0].artist.as_deref(),
            Some("The Lanterns feat. Ada")
        );
        assert_eq!(r.media[1].title, "Bonus");
        assert_eq!(r.track_count(), 3);
        assert_eq!(r.total_ms(), Some(626_000));
        assert_eq!(r.format_label(), "2×CD");
        assert!(parse_release(&json!({"title": "No id"})).is_none());
        let bare =
            parse_release(&json!({"id": "x", "media": [{"tracks": [{"title": "T"}]}]})).unwrap();
        assert_eq!(bare.media[0].position, 1);
        assert_eq!(bare.media[0].tracks[0].position, 1);
        assert_eq!(bare.media[0].tracks[0].number, "1");
        assert_eq!(bare.total_ms(), None);
    }

    #[test]
    fn search_parsing() {
        let found = parse_release_search(&json!({"releases": [
            {"id": "a", "title": "Blue", "date": "1971", "country": "US",
             "artist-credit": [{"name": "Joni Mitchell"}],
             "media": [{"format": "CD", "track-count": 10}]},
            {"id": "b", "title": "Deluxe", "media": [
                {"format": "CD", "track-count": 12}, {"format": "DVD-Video", "track-count": 3}]},
            {"id": "", "title": "No id"}
        ]}));
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].artist, "Joni Mitchell");
        assert_eq!(found[0].year, Some(1971));
        assert_eq!(found[0].disc_tracks, vec![10]);
        assert_eq!(found[1].format, "CD + DVD-Video");
        assert_eq!(found[1].track_count(), 15);
        assert!(parse_release_search(&json!({})).is_empty());
    }

    #[test]
    fn queries_and_ids() {
        assert_eq!(
            Query::parse(" 5012345 678900 "),
            Some(Query::Barcode("5012345678900".into()))
        );
        assert_eq!(
            Query::parse("AC/DC: Back in Black").unwrap().lucene(),
            "AC\\/DC\\: Back in Black"
        );
        assert_eq!(Query::parse("  "), None);
        let id = "6f1a3e4c-2b9d-4c11-9b0a-1d2e3f405162";
        assert_eq!(mbid_in(id).as_deref(), Some(id));
        assert_eq!(
            mbid_in(&format!(
                "https://musicbrainz.org/release/{}",
                id.to_uppercase()
            ))
            .as_deref(),
            Some(id)
        );
        assert_eq!(mbid_in("not an id"), None);
        assert_eq!(mbid_in(&format!("{id}0")), None);
        assert!(is_mbid(id));
        assert!(!is_mbid(&format!(" {id}")));
        assert_eq!(format_label(["", ""].into_iter()), "");
        assert_eq!(format_label(["CD", "CD", "DVD"].into_iter()), "2×CD + DVD");
    }

    #[tokio::test]
    async fn rate_limiter_spaces_requests() {
        let limiter = std::sync::Arc::new(RateLimiter::new(Duration::from_millis(120)));
        let start = std::time::Instant::now();
        let mut tasks = Vec::new();
        for _ in 0..3 {
            let l = limiter.clone();
            tasks.push(tokio::spawn(async move {
                l.wait().await;
                std::time::Instant::now()
            }));
        }
        let mut times = Vec::new();
        for t in tasks {
            times.push(t.await.unwrap());
        }
        times.sort();
        assert!(times[0].duration_since(start) < Duration::from_millis(100));
        for pair in times.windows(2) {
            assert!(
                pair[1].duration_since(pair[0]) >= Duration::from_millis(110),
                "{:?}",
                pair[1].duration_since(pair[0])
            );
        }
    }

    #[test]
    fn cache_expires_and_evicts() {
        let cache: TtlCache<String, i32> = TtlCache::new(Duration::from_secs(60), 2);
        cache.put("a".into(), 1);
        cache.put("b".into(), 2);
        cache.put("c".into(), 3);
        assert_eq!(cache.get(&"a".into()), None, "oldest evicted");
        assert_eq!(cache.get(&"c".into()), Some(3));
        let short: TtlCache<String, i32> = TtlCache::new(Duration::ZERO, 2);
        short.put("a".into(), 1);
        assert_eq!(short.get(&"a".into()), None, "expired");
    }

    /// Against a stand-in server: the user agent, the cache (a second
    /// question never reaches the server), 404s and the rate limiter.
    #[tokio::test]
    async fn fetches_through_cache_and_limiter() {
        use crate::config::LookupConfig;
        use axum::extract::{Path, Query as Q, State};
        use axum::{Json, Router, http::HeaderMap, http::StatusCode, routing::get};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let hits = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/mb/release/",
                get(
                    |State(h): State<Arc<AtomicUsize>>,
                     Q(q): Q<HashMap<String, String>>| async move {
                        h.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(q["query"], "barcode:5012345678900");
                        Json(json!({"releases": [{"id": "6f1a3e4c-2b9d-4c11-9b0a-1d2e3f405162", "title": "Night Songs",
                            "media": [{"format": "CD", "track-count": 2}]}]}))
                    },
                ),
            )
            .route(
                "/mb/release/{id}",
                get(
                    |State(h): State<Arc<AtomicUsize>>,
                     Path(id): Path<String>,
                     Q(q): Q<HashMap<String, String>>,
                     headers: HeaderMap| async move {
                        h.fetch_add(1, Ordering::SeqCst);
                        let ua = headers["user-agent"].to_str().unwrap().to_owned();
                        assert!(ua.contains("( librarian@example.org )"), "{ua}");
                        assert_eq!(q["inc"], "recordings+media+artist-credits");
                        if id == "6f1a3e4c-2b9d-4c11-9b0a-1d2e3f405162" {
                            Ok(Json(release_json()))
                        } else {
                            Err(StatusCode::NOT_FOUND)
                        }
                    },
                ),
            )
            .with_state(hits.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let l = crate::lookup::Lookups::new(LookupConfig {
            musicbrainz_url: format!("http://{addr}/mb"),
            contact: Some("librarian@example.org".into()),
            musicbrainz_interval_ms: 150,
            ..Default::default()
        })
        .unwrap();
        let start = std::time::Instant::now();
        let id = "6f1a3e4c-2b9d-4c11-9b0a-1D2E3F405162";
        let r = l.release(id).await.unwrap();
        assert_eq!(r.title, "Night Songs");
        let again = l.release(&id.to_lowercase()).await.unwrap();
        assert_eq!(again, r);
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "the second came from the cache"
        );
        let q = Query::parse("5012345678900").unwrap();
        assert_eq!(l.release_search(&q).await.unwrap()[0].disc_tracks, vec![2]);
        assert_eq!(l.release_search(&q).await.unwrap().len(), 1);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        let missing = l
            .release("00000000-0000-0000-0000-000000000000")
            .await
            .unwrap_err();
        assert!(missing.downcast_ref::<NotFound>().is_some());
        assert!(l.release("../etc").await.is_err(), "never sent");
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        assert!(
            start.elapsed() >= Duration::from_millis(290),
            "three requests are spaced out: {:?}",
            start.elapsed()
        );
    }
}
