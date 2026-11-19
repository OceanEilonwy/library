//! Filling in details from online catalogues.
//!
//! Each source turns a title or barcode into [`Candidate`]s. Sources are
//! queried concurrently with a timeout; one failing never blocks the rest.
//! Picked candidates are kept briefly in a [`CandidateCache`] so the add form
//! can be prefilled from a link without re-querying.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rand::Rng;
use serde_json::Value;

use crate::config::LookupConfig;
use crate::db::types::Lookup;
use crate::musicbrainz;
use crate::util;

/// MusicBrainz asks for at most one request a second. A different spacing
/// is only honoured for other servers (a local mirror, or tests).
fn musicbrainz_interval(cfg: &LookupConfig) -> Duration {
    let asked = Duration::from_millis(cfg.musicbrainz_interval_ms);
    if cfg.musicbrainz_url.contains("musicbrainz.org") {
        asked.max(Duration::from_secs(1))
    } else {
        asked
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    OpenLibrary,
    GoogleBooks,
    MusicBrainz,
    Tmdb,
    Upc,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::OpenLibrary => "Open Library",
            Source::GoogleBooks => "Google Books",
            Source::MusicBrainz => "MusicBrainz",
            Source::Tmdb => "TMDb",
            Source::Upc => "Barcode lookup",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Candidate {
    pub source: Option<Source>,
    pub title: String,
    pub creator: String,
    pub year: Option<i64>,
    pub publisher: String,
    pub format: String,
    pub identifier: String,
    pub description: String,
    pub tags: String,
    pub cover_url: Option<String>,
    /// Open Library work key ("/works/OL…W"), for fetching the description.
    pub work: Option<String>,
}

impl Candidate {
    /// "Tolkien · HarperCollins · 1995 · paperback"
    pub fn meta_line(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for s in [&self.creator, &self.publisher] {
            if !s.is_empty() {
                parts.push(s.clone());
            }
        }
        if let Some(y) = self.year {
            parts.push(y.to_string());
        }
        if !self.format.is_empty() {
            parts.push(self.format.clone());
        }
        parts.join(" · ")
    }
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub candidates: Vec<Candidate>,
    /// Sources that failed or were skipped, with a short reason.
    pub notes: Vec<String>,
}

#[derive(Clone)]
pub struct Lookups {
    cfg: LookupConfig,
    client: reqwest::Client,
    /// MusicBrainz's rate limiter and caches, shared by every clone.
    musicbrainz: Arc<musicbrainz::Shared>,
}

impl Lookups {
    pub fn new(cfg: LookupConfig) -> Result<Self> {
        let ua = match &cfg.contact {
            Some(c) if !c.trim().is_empty() => {
                format!(
                    "home-library/{} ( {} )",
                    env!("CARGO_PKG_VERSION"),
                    c.trim()
                )
            }
            _ => format!("home-library/{} ( self-hosted )", env!("CARGO_PKG_VERSION")),
        };
        let client = reqwest::Client::builder()
            .user_agent(ua)
            .timeout(Duration::from_secs(cfg.timeout_secs.max(1)))
            .connect_timeout(Duration::from_secs(5))
            .build()?;
        let musicbrainz = Arc::new(musicbrainz::Shared::new(musicbrainz_interval(&cfg)));
        Ok(Self {
            cfg,
            client,
            musicbrainz,
        })
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn config(&self) -> &LookupConfig {
        &self.cfg
    }

    /// Whether online lookups are switched on.
    pub fn enabled(&self) -> bool {
        self.cfg.enabled
    }

    pub(crate) fn musicbrainz_state(&self) -> &musicbrainz::Shared {
        &self.musicbrainz
    }

    /// Searches the sources suited to `kind` (or all sources for `None`).
    pub async fn search(&self, query: &str, kind: Option<Lookup>) -> Outcome {
        let query = query.trim();
        let mut out = Outcome::default();
        if !self.cfg.enabled {
            out.notes.push("Online lookups are switched off.".into());
            return out;
        }
        if query.is_empty() {
            return out;
        }
        let code = util::looks_like_code(query).then(|| util::normalize_identifier(query));
        let wants = |k: Lookup| kind.is_none_or(|x| x == k);
        // An ISBN-shaped code is a book whatever the chosen type.
        let isbn = code
            .as_ref()
            .filter(|c| c.len() == 10 || c.starts_with("978") || c.starts_with("979"));

        let mut set: tokio::task::JoinSet<(usize, Source, Result<Vec<Candidate>>)> =
            tokio::task::JoinSet::new();
        let isbn = isbn.cloned();
        let mut n = 0;
        let mut spawn = |source: Source| {
            let me = self.clone();
            let (q, code, isbn) = (query.to_owned(), code.clone(), isbn.clone());
            let order = n;
            n += 1;
            set.spawn(async move {
                let res = match source {
                    Source::OpenLibrary => me.open_library(&q, isbn.as_deref()).await,
                    Source::GoogleBooks => me.google_books(&q, isbn.as_deref()).await,
                    Source::MusicBrainz => me.musicbrainz(&q, code.as_deref()).await,
                    Source::Tmdb => me.tmdb(&q).await,
                    Source::Upc => me.upc(code.as_deref().unwrap_or_default()).await,
                };
                (order, source, res)
            });
        };
        if wants(Lookup::Book) || isbn.is_some() {
            spawn(Source::OpenLibrary);
            spawn(Source::GoogleBooks);
        }
        if wants(Lookup::Music) && isbn.is_none() {
            spawn(Source::MusicBrainz);
        }
        if wants(Lookup::Film) && code.is_none() {
            if self
                .cfg
                .tmdb_token
                .as_deref()
                .is_some_and(|t| !t.is_empty())
            {
                spawn(Source::Tmdb);
            } else if kind == Some(Lookup::Film) {
                out.notes
                    .push("Film lookups need a TMDb token in the config.".into());
            }
        }
        if (wants(Lookup::Product) || kind.is_none()) && isbn.is_none() && code.is_some() {
            spawn(Source::Upc);
        }
        let mut results = Vec::new();
        while let Some(r) = set.join_next().await {
            if let Ok(r) = r {
                results.push(r);
            }
        }
        // Keep a stable source order regardless of who answered first.
        results.sort_by_key(|(order, _, _)| *order);
        let results = results.into_iter().map(|(_, s, r)| (s, r));
        for (source, res) in results {
            match res {
                Ok(mut c) => {
                    for cand in &mut c {
                        cand.source = Some(source);
                    }
                    out.candidates.extend(c);
                }
                Err(e) => {
                    tracing::info!(source = source.label(), error = %e, "lookup failed");
                    out.notes.push(format!("{} didn't answer.", source.label()));
                }
            }
        }
        dedupe(&mut out.candidates);
        out
    }

    async fn get_json(&self, req: reqwest::RequestBuilder) -> Result<Value> {
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            bail!("HTTP {status}");
        }
        Ok(resp.json().await?)
    }

    async fn open_library(&self, query: &str, isbn: Option<&str>) -> Result<Vec<Candidate>> {
        let url = format!(
            "{}/search.json",
            self.cfg.open_library_url.trim_end_matches('/')
        );
        let fields =
            "key,title,subtitle,author_name,first_publish_year,publisher,isbn,subject,cover_i";
        let req = match isbn {
            Some(code) => {
                self.client
                    .get(url)
                    .query(&[("isbn", code), ("fields", fields), ("limit", "8")])
            }
            None => self
                .client
                .get(url)
                .query(&[("q", query), ("fields", fields), ("limit", "8")]),
        };
        let json = self.get_json(req).await?;
        Ok(parse_open_library(&json, isbn))
    }

    async fn google_books(&self, query: &str, isbn: Option<&str>) -> Result<Vec<Candidate>> {
        let url = format!(
            "{}/volumes",
            self.cfg.google_books_url.trim_end_matches('/')
        );
        let q = match isbn {
            Some(code) => format!("isbn:{code}"),
            None => query.to_owned(),
        };
        let json = self
            .get_json(self.client.get(url).query(&[
                ("q", q.as_str()),
                ("maxResults", "8"),
                ("printType", "books"),
            ]))
            .await?;
        Ok(parse_google_books(&json))
    }

    async fn musicbrainz(&self, query: &str, code: Option<&str>) -> Result<Vec<Candidate>> {
        let url = format!(
            "{}/release/",
            self.cfg.musicbrainz_url.trim_end_matches('/')
        );
        let q = match code {
            Some(c) => format!("barcode:{c}"),
            None => query.to_owned(),
        };
        let json = self
            .musicbrainz_json(self.client.get(url).query(&[
                ("query", q.as_str()),
                ("fmt", "json"),
                ("limit", "8"),
            ]))
            .await?;
        Ok(parse_musicbrainz(&json))
    }

    async fn tmdb(&self, query: &str) -> Result<Vec<Candidate>> {
        let token = self.cfg.tmdb_token.clone().unwrap_or_default();
        let url = format!("{}/search/movie", self.cfg.tmdb_url.trim_end_matches('/'));
        let json = self
            .get_json(
                self.client
                    .get(url)
                    .bearer_auth(token)
                    .query(&[("query", query), ("include_adult", "false")]),
            )
            .await?;
        Ok(parse_tmdb(&json))
    }

    async fn upc(&self, code: &str) -> Result<Vec<Candidate>> {
        let url = format!("{}/lookup", self.cfg.upc_url.trim_end_matches('/'));
        let json = self
            .get_json(self.client.get(url).query(&[("upc", code)]))
            .await?;
        Ok(parse_upc(&json))
    }

    /// Fills in a missing description for a picked candidate: from the Open
    /// Library work record, else from Google Books by ISBN. Best effort.
    pub async fn enrich(&self, c: &mut Candidate) {
        if !self.cfg.enabled || !c.description.trim().is_empty() {
            return;
        }
        if let Some(work) = c.work.clone() {
            let url = format!(
                "{}{work}.json",
                self.cfg.open_library_url.trim_end_matches('/')
            );
            match self.get_json(self.client.get(url)).await {
                Ok(json) => c.description = parse_ol_description(&json),
                Err(e) => tracing::info!(error = %e, "Open Library work lookup failed"),
            }
        }
        let code = util::normalize_identifier(&c.identifier);
        if c.description.is_empty() && (code.len() == 10 || code.len() == 13) {
            match self.google_books("", Some(&code)).await {
                Ok(found) => {
                    if let Some(d) = found
                        .into_iter()
                        .map(|x| x.description)
                        .find(|d| !d.is_empty())
                    {
                        c.description = d;
                    }
                }
                Err(e) => tracing::info!(error = %e, "Google Books description lookup failed"),
            }
        }
    }

    /// Downloads a cover image (max 8 MB).
    pub async fn fetch_image(&self, url: &str) -> Result<Vec<u8>> {
        if !url.starts_with("https://") && !url.starts_with("http://") {
            bail!("not a web address");
        }
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .context("fetching cover")?;
        if !resp.status().is_success() {
            bail!("cover returned {}", resp.status());
        }
        let is_image = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("image/"));
        if !is_image {
            bail!("cover is not an image");
        }
        if resp.content_length().is_some_and(|n| n > 8 << 20) {
            bail!("cover is too large");
        }
        let bytes = resp.bytes().await?;
        if bytes.len() > 8 << 20 {
            bail!("cover is too large");
        }
        Ok(bytes.to_vec())
    }
}

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned()
}

fn strs(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn year_of(date: &str) -> Option<i64> {
    date.get(..4)
        .and_then(|y| y.parse().ok())
        .filter(|y| (1000..=3000).contains(y))
}

fn https(url: String) -> String {
    match url.strip_prefix("http://") {
        Some(rest) => format!("https://{rest}"),
        None => url,
    }
}

pub fn parse_open_library(json: &Value, isbn: Option<&str>) -> Vec<Candidate> {
    let Some(docs) = json.get("docs").and_then(Value::as_array) else {
        return Vec::new();
    };
    docs.iter()
        .filter(|d| !s(d, "title").is_empty())
        .map(|d| {
            let mut title = s(d, "title");
            let sub = s(d, "subtitle");
            if !sub.is_empty() {
                title = format!("{title}: {sub}");
            }
            let isbns = strs(d, "isbn");
            let identifier = isbn
                .map(str::to_owned)
                .or_else(|| isbns.iter().find(|i| i.len() == 13).cloned())
                .or_else(|| isbns.first().cloned())
                .unwrap_or_default();
            Candidate {
                source: None,
                title,
                creator: strs(d, "author_name").join(", "),
                year: d.get("first_publish_year").and_then(Value::as_i64),
                publisher: strs(d, "publisher").into_iter().next().unwrap_or_default(),
                format: String::new(),
                identifier,
                description: String::new(),
                tags: strs(d, "subject")
                    .into_iter()
                    .take(6)
                    .collect::<Vec<_>>()
                    .join(", "),
                cover_url: d
                    .get("cover_i")
                    .and_then(Value::as_i64)
                    .map(|id| format!("https://covers.openlibrary.org/b/id/{id}-L.jpg")),
                work: Some(s(d, "key")).filter(|k| k.starts_with("/works/")),
            }
        })
        .collect()
}

pub fn parse_google_books(json: &Value) -> Vec<Candidate> {
    let Some(items) = json.get("items").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|it| it.get("volumeInfo"))
        .filter(|v| !s(v, "title").is_empty())
        .map(|v| {
            let mut title = s(v, "title");
            let sub = s(v, "subtitle");
            if !sub.is_empty() {
                title = format!("{title}: {sub}");
            }
            let ids = v.get("industryIdentifiers").and_then(Value::as_array);
            let pick = |kind: &str| {
                ids.and_then(|a| {
                    a.iter()
                        .find(|i| s(i, "type") == kind)
                        .map(|i| s(i, "identifier"))
                })
            };
            let mut desc = strip_html(&s(v, "description"));
            if desc.chars().count() > 600 {
                desc = desc.chars().take(600).collect::<String>() + "…";
            }
            Candidate {
                source: None,
                work: None,
                title,
                creator: strs(v, "authors").join(", "),
                year: year_of(&s(v, "publishedDate")),
                publisher: s(v, "publisher"),
                format: String::new(),
                identifier: pick("ISBN_13")
                    .or_else(|| pick("ISBN_10"))
                    .unwrap_or_default(),
                description: desc,
                tags: strs(v, "categories").join(", "),
                cover_url: v
                    .get("imageLinks")
                    .and_then(|l| l.get("thumbnail").or_else(|| l.get("smallThumbnail")))
                    .and_then(Value::as_str)
                    .map(|u| https(u.to_owned())),
            }
        })
        .collect()
}

pub fn parse_musicbrainz(json: &Value) -> Vec<Candidate> {
    let Some(releases) = json.get("releases").and_then(Value::as_array) else {
        return Vec::new();
    };
    releases
        .iter()
        .filter(|r| !s(r, "title").is_empty())
        .map(|r| {
            let artist = musicbrainz::artist_credit(r);
            let label = r
                .get("label-info")
                .and_then(Value::as_array)
                .and_then(|l| l.first())
                .and_then(|l| l.get("label"))
                .map(|l| s(l, "name"))
                .unwrap_or_default();
            let media = r.get("media").and_then(Value::as_array);
            let format = media
                .and_then(|m| m.first())
                .map(|m| s(m, "format"))
                .unwrap_or_default();
            let tracks: i64 = media
                .map(|m| {
                    m.iter()
                        .filter_map(|x| x.get("track-count").and_then(Value::as_i64))
                        .sum()
                })
                .unwrap_or(0);
            let format = match (format.is_empty(), tracks) {
                (false, n) if n > 0 => format!("{format}, {n} tracks"),
                _ => format,
            };
            let id = s(r, "id");
            Candidate {
                source: None,
                work: None,
                title: s(r, "title"),
                creator: artist,
                year: year_of(&s(r, "date")),
                publisher: label,
                format,
                identifier: s(r, "barcode"),
                description: String::new(),
                tags: String::new(),
                cover_url: (!id.is_empty())
                    .then(|| format!("https://coverartarchive.org/release/{id}/front-500")),
            }
        })
        .collect()
}

pub fn parse_tmdb(json: &Value) -> Vec<Candidate> {
    let Some(results) = json.get("results").and_then(Value::as_array) else {
        return Vec::new();
    };
    results
        .iter()
        .take(8)
        .filter(|r| !s(r, "title").is_empty())
        .map(|r| Candidate {
            source: None,
            work: None,
            title: s(r, "title"),
            creator: String::new(),
            year: year_of(&s(r, "release_date")),
            publisher: String::new(),
            format: "Film".into(),
            identifier: String::new(),
            description: s(r, "overview"),
            tags: String::new(),
            cover_url: r
                .get("poster_path")
                .and_then(Value::as_str)
                .map(|p| format!("https://image.tmdb.org/t/p/w342{p}")),
        })
        .collect()
}

pub fn parse_upc(json: &Value) -> Vec<Candidate> {
    let Some(items) = json.get("items").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|i| !s(i, "title").is_empty())
        .map(|i| Candidate {
            source: None,
            work: None,
            title: s(i, "title"),
            creator: s(i, "brand"),
            year: None,
            publisher: String::new(),
            format: s(i, "model"),
            identifier: {
                let ean = s(i, "ean");
                if ean.is_empty() { s(i, "upc") } else { ean }
            },
            description: s(i, "description").chars().take(600).collect(),
            tags: s(i, "category").replace(" > ", ", "),
            cover_url: strs(i, "images")
                .into_iter()
                .find(|u| u.starts_with("http"))
                .map(https),
        })
        .collect()
}

/// Plain text from the bits of HTML some catalogues put in descriptions:
/// tags dropped (paragraph and line breaks kept), common entities decoded.
pub fn strip_html(s: &str) -> String {
    if !s.contains('<') && !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        match rest[i..].find('>') {
            Some(j) => {
                let tag = rest[i + 1..i + j]
                    .trim_start_matches('/')
                    .to_ascii_lowercase();
                if tag.starts_with("br")
                    || tag.starts_with("p") && (tag.len() == 1 || tag.as_bytes()[1] == b' ')
                {
                    out.push('\n');
                }
                rest = &rest[i + j + 1..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    let mut text = String::with_capacity(out.len());
    let mut it = out.as_str();
    while let Some(i) = it.find('&') {
        text.push_str(&it[..i]);
        let tail = &it[i..];
        let end = tail.find(';').filter(|e| *e <= 8);
        let decoded = end.and_then(|e| match &tail[1..e] {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            n if n.starts_with("#x") => u32::from_str_radix(&n[2..], 16)
                .ok()
                .and_then(char::from_u32),
            n if n.starts_with('#') => n[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        });
        match (decoded, end) {
            (Some(c), Some(e)) => {
                text.push(c);
                it = &tail[e + 1..];
            }
            _ => {
                text.push('&');
                it = &tail[1..];
            }
        }
    }
    text.push_str(it);
    // Collapse the blank lines left by markup.
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut cleaned = Vec::new();
    for l in lines {
        if l.is_empty() && cleaned.last().is_none_or(|p: &&str| p.is_empty()) {
            continue;
        }
        cleaned.push(l);
    }
    cleaned.join("\n").trim().to_owned()
}

/// The description from an Open Library work record, without the
/// "([source][1])" footnotes and link lists it often carries.
pub fn parse_ol_description(json: &Value) -> String {
    let raw = match json.get("description") {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Object(o)) => o
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        _ => return String::new(),
    };
    let mut text = strip_html(&raw.replace("\r\n", "\n"));
    for cut in ["\n----", "\n\n**Contains", "([source]", "\n[1]:"] {
        if let Some(i) = text.find(cut) {
            text.truncate(i);
        }
    }
    let text = text.trim();
    if text.chars().count() > 1000 {
        let mut t: String = text.chars().take(1000).collect();
        if let Some(i) = t.rfind(". ") {
            t.truncate(i + 1);
        } else {
            t.push('…');
        }
        t
    } else {
        text.to_owned()
    }
}

/// Drops near-identical candidates from different sources (same title,
/// creator and year), keeping the first (richer sources come first).
fn dedupe(cands: &mut Vec<Candidate>) {
    let mut seen = std::collections::HashSet::new();
    cands.retain(|c| {
        seen.insert((
            util::title_key(&c.title),
            util::normalize(&c.creator),
            c.year,
            util::normalize_identifier(&c.identifier),
        ))
    });
}

/// Short-lived store of looked-up candidates, keyed by random ids.
#[derive(Default)]
pub struct CandidateCache {
    map: Mutex<HashMap<String, (Instant, Candidate)>>,
}

const CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const CACHE_MAX: usize = 1000;

impl CandidateCache {
    pub fn put(&self, c: Candidate) -> String {
        let id: String = {
            let mut rng = rand::thread_rng();
            (0..16)
                .map(|_| format!("{:x}", rng.gen_range(0..16u8)))
                .collect()
        };
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        map.retain(|_, (t, _)| now.duration_since(*t) < CACHE_TTL);
        if map.len() >= CACHE_MAX
            && let Some(oldest) = map
                .iter()
                .min_by_key(|(_, (t, _))| *t)
                .map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
        map.insert(id.clone(), (now, c));
        id
    }

    pub fn get(&self, id: &str) -> Option<Candidate> {
        let map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        map.get(id)
            .filter(|(t, _)| t.elapsed() < CACHE_TTL)
            .map(|(_, c)| c.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn open_library_parsing() {
        let j = json!({"docs": [
            {"title": "The Hobbit", "subtitle": "or There and Back Again", "author_name": ["J.R.R. Tolkien"],
             "first_publish_year": 1937, "publisher": ["HarperCollins", "Unwin"], "isbn": ["0261102958", "9780261102958"],
             "subject": ["Fantasy", "Dragons"], "cover_i": 123},
            {"title": ""}
        ]});
        let c = parse_open_library(&j, None);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].title, "The Hobbit: or There and Back Again");
        assert_eq!(c[0].creator, "J.R.R. Tolkien");
        assert_eq!(c[0].year, Some(1937));
        assert_eq!(c[0].identifier, "9780261102958");
        assert_eq!(c[0].tags, "Fantasy, Dragons");
        assert_eq!(
            c[0].cover_url.as_deref(),
            Some("https://covers.openlibrary.org/b/id/123-L.jpg")
        );
        assert_eq!(
            parse_open_library(&j, Some("0261102958"))[0].identifier,
            "0261102958"
        );
        assert!(parse_open_library(&json!({}), None).is_empty());
    }

    #[test]
    fn google_books_parsing() {
        let j = json!({"items": [{"volumeInfo": {
            "title": "Pride and Prejudice", "authors": ["Jane Austen"], "publisher": "Penguin",
            "publishedDate": "2003-04-29", "description": "x".repeat(700), "categories": ["Fiction"],
            "industryIdentifiers": [{"type": "ISBN_10", "identifier": "0141439513"}, {"type": "ISBN_13", "identifier": "9780141439518"}],
            "imageLinks": {"thumbnail": "http://books.google.com/x.jpg"}}}]});
        let c = parse_google_books(&j);
        assert_eq!(c[0].year, Some(2003));
        assert_eq!(c[0].identifier, "9780141439518");
        assert_eq!(
            c[0].cover_url.as_deref(),
            Some("https://books.google.com/x.jpg")
        );
        assert!(c[0].description.ends_with('…'));
        assert_eq!(c[0].description.chars().count(), 601);
    }

    #[test]
    fn musicbrainz_parsing() {
        let j = json!({"releases": [{
            "id": "abc", "title": "Blue", "date": "1971-06-22", "barcode": "075992744329",
            "artist-credit": [{"name": "Joni Mitchell", "joinphrase": ""}],
            "label-info": [{"label": {"name": "Reprise"}}],
            "media": [{"format": "CD", "track-count": 10}]}]});
        let c = parse_musicbrainz(&j);
        assert_eq!(c[0].creator, "Joni Mitchell");
        assert_eq!(c[0].publisher, "Reprise");
        assert_eq!(c[0].format, "CD, 10 tracks");
        assert_eq!(c[0].year, Some(1971));
        assert_eq!(
            c[0].cover_url.as_deref(),
            Some("https://coverartarchive.org/release/abc/front-500")
        );
        let duo = json!({"releases": [{"title": "X", "artist-credit": [
            {"name": "Bowie", "joinphrase": " & "}, {"artist": {"name": "Eno"}}]}]});
        assert_eq!(parse_musicbrainz(&duo)[0].creator, "Bowie & Eno");
    }

    #[test]
    fn tmdb_and_upc_parsing() {
        let t = parse_tmdb(
            &json!({"results": [{"title": "Spirited Away", "release_date": "2001-07-20", "overview": "A girl", "poster_path": "/p.jpg"}]}),
        );
        assert_eq!(t[0].year, Some(2001));
        assert_eq!(
            t[0].cover_url.as_deref(),
            Some("https://image.tmdb.org/t/p/w342/p.jpg")
        );
        let u = parse_upc(
            &json!({"items": [{"title": "Trangia 25-1 UL", "brand": "Trangia", "model": "25-1", "ean": "7315081251100",
            "category": "Sporting Goods > Camping", "images": ["http://img/x.jpg"]}]}),
        );
        assert_eq!(u[0].creator, "Trangia");
        assert_eq!(u[0].tags, "Sporting Goods, Camping");
        assert_eq!(u[0].cover_url.as_deref(), Some("https://img/x.jpg"));
        assert_eq!(year_of("19"), None);
    }

    #[test]
    fn html_is_stripped_from_descriptions() {
        assert_eq!(
            strip_html(
                "<p><i>Orthodoxy</i> is <a href=\"https://x\">a book</a> by G.&nbsp;K. Chesterton &amp; more.</p><p>Second &#8220;para&#8221;.</p>"
            ),
            "Orthodoxy is a book by G. K. Chesterton & more.\n\nSecond \u{201c}para\u{201d}."
        );
        assert_eq!(strip_html("No markup, 3 < 4"), "No markup, 3 < 4");
        assert_eq!(strip_html("Tom &amp Jerry & co"), "Tom &amp Jerry & co");
        assert_eq!(strip_html("<b>bold</b><br/>next"), "bold\nnext");
        assert_eq!(
            parse_ol_description(&json!({"description": "<p>A <b>tale</b>.</p>"})),
            "A tale."
        );
    }

    #[test]
    fn open_library_work_descriptions() {
        assert_eq!(
            parse_ol_description(&json!({"description": "A hobbit goes there and back."})),
            "A hobbit goes there and back."
        );
        assert_eq!(
            parse_ol_description(
                &json!({"description": {"type": "/type/text", "value": "Bilbo travels.\r\n\n([source][1])\n\n[1]: http://x"}})
            ),
            "Bilbo travels."
        );
        assert_eq!(
            parse_ol_description(&json!({"description": "Story.\n----------\nAlso contained in:"})),
            "Story."
        );
        assert_eq!(parse_ol_description(&json!({})), "");
        let long = "Sentence one. ".repeat(100);
        let d = parse_ol_description(&json!({ "description": long }));
        assert!(d.chars().count() <= 1000 && d.ends_with('.'));
        let c = parse_open_library(
            &json!({"docs": [{"title": "X", "key": "/works/OL1W"}]}),
            None,
        );
        assert_eq!(c[0].work.as_deref(), Some("/works/OL1W"));
    }

    #[test]
    fn dedupe_keeps_first() {
        let mut v = vec![
            Candidate {
                title: "The Hobbit".into(),
                creator: "Tolkien".into(),
                year: Some(1937),
                source: Some(Source::OpenLibrary),
                ..Default::default()
            },
            Candidate {
                title: "Hobbit".into(),
                creator: "tolkien".into(),
                year: Some(1937),
                source: Some(Source::GoogleBooks),
                ..Default::default()
            },
            Candidate {
                title: "The Hobbit".into(),
                creator: "Tolkien".into(),
                year: Some(1966),
                ..Default::default()
            },
        ];
        dedupe(&mut v);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].source, Some(Source::OpenLibrary));
    }

    #[test]
    fn cache_round_trip() {
        let cache = CandidateCache::default();
        let id = cache.put(Candidate {
            title: "X".into(),
            ..Default::default()
        });
        assert_eq!(id.len(), 16);
        assert_eq!(cache.get(&id).unwrap().title, "X");
        assert!(cache.get("nope").is_none());
    }

    #[test]
    fn meta_line() {
        let c = Candidate {
            creator: "Tolkien".into(),
            publisher: "HarperCollins".into(),
            year: Some(1995),
            format: "paperback".into(),
            ..Default::default()
        };
        assert_eq!(c.meta_line(), "Tolkien · HarperCollins · 1995 · paperback");
    }

    /// End to end against a local stand-in server: routing by kind, the
    /// user agent, and one source failing without spoiling the others.
    #[tokio::test]
    async fn search_queries_sources_concurrently() {
        use axum::{Json, Router, http::HeaderMap, routing::get};
        let app = Router::new()
            .route(
                "/ol/search.json",
                get(|h: HeaderMap| async move {
                    let ua = h.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
                    assert!(ua.starts_with("home-library/"), "{ua}");
                    Json(json!({"docs": [{"title": "The Hobbit", "key": "/works/OL27482W", "author_name": ["Tolkien"], "first_publish_year": 1937}]}))
                }),
            )
            .route("/ol/works/OL27482W.json", get(|| async { Json(json!({"description": {"value": "Bilbo Baggins is swept into a quest."}})) }))
            .route("/gb/volumes", get(|| async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom") }))
            .route(
                "/mb/release/",
                get(|| async { Json(json!({"releases": [{"title": "Blue", "artist-credit": [{"name": "Joni Mitchell"}]}]})) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base = format!("http://{addr}");
        let cfg = LookupConfig {
            open_library_url: format!("{base}/ol"),
            google_books_url: format!("{base}/gb"),
            musicbrainz_url: format!("{base}/mb"),
            tmdb_url: format!("{base}/tmdb"),
            upc_url: format!("{base}/upc"),
            ..Default::default()
        };
        let l = Lookups::new(cfg).unwrap();
        let books = l.search("hobbit", Some(Lookup::Book)).await;
        assert_eq!(books.candidates.len(), 1);
        assert_eq!(books.candidates[0].source, Some(Source::OpenLibrary));
        assert_eq!(books.notes, vec!["Google Books didn't answer."]);
        let mut picked = books.candidates[0].clone();
        l.enrich(&mut picked).await;
        assert_eq!(picked.description, "Bilbo Baggins is swept into a quest.");
        let music = l.search("blue", Some(Lookup::Music)).await;
        assert_eq!(music.candidates[0].creator, "Joni Mitchell");
        let films = l.search("totoro", Some(Lookup::Film)).await;
        assert!(films.candidates.is_empty());
        assert!(films.notes[0].contains("TMDb token"));
        let off = Lookups::new(LookupConfig {
            enabled: false,
            ..Default::default()
        })
        .unwrap();
        assert!(off.search("x", None).await.notes[0].contains("switched off"));
    }
}
