//! The upload studio's "Fill from MusicBrainz" panel: finding the release a
//! CD was ripped from and matching the waiting files to its tracks.
//! Librarian only; JSON in and out (the panel's script draws it).
//!
//! - `POST /admin/items/{id}/media/musicbrainz/search` —
//!   `{query, barcode?, files}` → `{candidates, notes}`, ranked by how well
//!   each release fits the files.
//! - `GET /admin/items/{id}/media/musicbrainz/release/{mbid}` → the release
//!   with its discs and tracks.
//! - `POST /admin/items/{id}/media/musicbrainz/match` — `{release, files}` →
//!   `{release, matches, missing, fit}`.
//! - `POST /admin/items/{id}/media/musicbrainz/item` — `{creator, year}` fills
//!   the entry's artist and year where they are empty → `{creator, year}`.

use std::collections::HashMap;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::accounts::{ApiError, ApiResult};
use super::{Admin, AppState};
use crate::db::items::{self, Item};
use crate::musicbrainz::{self, NotFound, Query, Release, ReleaseSummary};
use crate::trackmatch::{self, Assignment, FileInfo, Fit};
use crate::util;

/// Most files one request may describe.
const MAX_FILES: usize = 400;
/// Most track lists fetched to rank one search (MusicBrainz allows one
/// request a second, so each costs the librarian a second).
const FETCH_LIMIT: usize = 4;
/// Most candidates shown.
const MAX_CANDIDATES: usize = 12;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/items/{id}/media/musicbrainz/search", post(search))
        .route(
            "/admin/items/{id}/media/musicbrainz/release/{mbid}",
            get(release),
        )
        .route("/admin/items/{id}/media/musicbrainz/match", post(matching))
        .route("/admin/items/{id}/media/musicbrainz/item", post(fill_item))
}

/// The search box's starting text: the entry's artist and title.
pub fn prefill_query(item: &Item) -> String {
    format!("{} {}", item.creator.trim(), item.title.trim())
        .trim()
        .to_owned()
}

/// The entry's identifier when it is a CD's barcode (EAN/UPC, not an ISBN).
pub fn item_barcode(item: &Item) -> Option<String> {
    barcode_of(&item.identifier)
}

fn barcode_of(s: &str) -> Option<String> {
    if !util::looks_like_code(s) {
        return None;
    }
    let code = util::normalize_identifier(s);
    let isbn = code.len() == 10 || (code.len() == 13 && code.starts_with("97"));
    (!isbn && matches!(code.len(), 8 | 12 | 13)).then_some(code)
}

fn switched_off() -> ApiError {
    ApiError::Forbidden(
        "Online lookups are switched off ([lookup] enabled = false in library.toml).".into(),
    )
}

fn json_body<T>(body: Result<Json<T>, JsonRejection>) -> ApiResult<T> {
    body.map(|Json(b)| b)
        .map_err(|e| ApiError::BadRequest(e.body_text()))
}

/// The entry, or 404.
async fn item(st: &AppState, id: i64) -> ApiResult<Item> {
    st.db
        .read(move |c| items::get(c, id))
        .await?
        .ok_or(ApiError::NotFound)
}

fn check_files(files: &[FileInfo]) -> ApiResult<()> {
    if files.len() > MAX_FILES {
        return Err(ApiError::BadRequest(format!(
            "Match at most {MAX_FILES} files at a time."
        )));
    }
    Ok(())
}

fn upstream(e: anyhow::Error) -> ApiError {
    if e.downcast_ref::<NotFound>().is_some() {
        return ApiError::Missing(NotFound.to_string());
    }
    tracing::info!(error = %e, "MusicBrainz lookup failed");
    ApiError::Upstream("MusicBrainz didn't answer. Try again in a moment.".into())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchBody {
    #[serde(default)]
    query: String,
    /// The entry's barcode, searched first when given.
    #[serde(default)]
    barcode: String,
    /// The music files waiting to upload, for ranking.
    #[serde(default)]
    files: Vec<FileInfo>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CandidateOut {
    #[serde(flatten)]
    release: ReleaseSummary,
    track_count: usize,
    /// Summed track lengths, when the track list was fetched.
    total_ms: Option<i64>,
    /// How well it fits the files (absent when no files were sent).
    fit: Option<Fit>,
    /// The release's page on MusicBrainz.
    url: String,
}

#[derive(Serialize)]
struct SearchOut {
    candidates: Vec<CandidateOut>,
    notes: Vec<String>,
}

async fn search(
    State(st): State<AppState>,
    Admin(_): Admin,
    Path(id): Path<i64>,
    body: Result<Json<SearchBody>, JsonRejection>,
) -> ApiResult<Json<SearchOut>> {
    let body = json_body(body)?;
    item(&st, id).await?;
    if !st.lookups.enabled() {
        return Err(switched_off());
    }
    check_files(&body.files)?;
    let lookups = &st.lookups;
    let mut notes = Vec::new();
    let mut found: Vec<ReleaseSummary> = Vec::new();
    let mut full: HashMap<String, Release> = HashMap::new();

    if let Some(mbid) = musicbrainz::mbid_in(&body.query) {
        // A pasted link or id: that release and nothing else.
        let r = lookups.release(&mbid).await.map_err(upstream)?;
        found.push(ReleaseSummary::from(&r));
        full.insert(r.id.clone(), r);
    } else {
        let mut queries = Vec::new();
        if let Some(code) = barcode_of(&body.barcode) {
            queries.push(Query::Barcode(code));
        }
        if let Some(q) = Query::parse(&body.query)
            && !queries.contains(&q)
        {
            queries.push(q);
        }
        if queries.is_empty() {
            return Err(ApiError::BadRequest(
                "Type an artist and album, a barcode, or a MusicBrainz link.".into(),
            ));
        }
        let mut failed = 0;
        for q in &queries {
            match lookups.release_search(q).await {
                Ok(list) => {
                    for r in list {
                        if !found.iter().any(|f| f.id == r.id) {
                            found.push(r);
                        }
                    }
                }
                Err(e) => {
                    tracing::info!(error = %e, "MusicBrainz search failed");
                    failed += 1;
                }
            }
        }
        if failed == queries.len() {
            return Err(upstream(anyhow::anyhow!("every search failed")));
        }
        if failed > 0 {
            notes.push("One of the searches didn't answer; the list may be short.".into());
        }
    }
    found.truncate(MAX_CANDIDATES);

    // Fetch the track lists of the releases whose track count is closest
    // to the number of files, to compare lengths.
    let files = body.files.len();
    if files > 0 {
        let mut order: Vec<(usize, usize)> = found
            .iter()
            .enumerate()
            .map(|(i, r)| (r.track_count().abs_diff(files), i))
            .collect();
        order.sort_unstable();
        for (_, i) in order.into_iter().take(FETCH_LIMIT) {
            let Some(summary) = found.get(i) else {
                continue;
            };
            if full.contains_key(&summary.id) {
                continue;
            }
            match lookups.release(&summary.id).await {
                Ok(r) => {
                    full.insert(summary.id.clone(), r);
                }
                Err(e) => tracing::info!(error = %e, "MusicBrainz release fetch failed"),
            }
        }
    }

    let mut candidates: Vec<CandidateOut> = found
        .into_iter()
        .map(|summary| {
            let release = full.get(&summary.id);
            let summary = release.map(ReleaseSummary::from).unwrap_or(summary);
            let tracks = summary.track_count();
            CandidateOut {
                fit: (files > 0).then(|| match release {
                    Some(r) => trackmatch::assign(&body.files, r).fit,
                    None => trackmatch::count_fit(files, tracks),
                }),
                track_count: tracks,
                total_ms: release.and_then(Release::total_ms),
                url: format!("https://musicbrainz.org/release/{}", summary.id),
                release: summary,
            }
        })
        .collect();
    // Best fit first; MusicBrainz's own order breaks ties (stable sort).
    candidates.sort_by_key(|c| std::cmp::Reverse(c.fit.as_ref().map_or(0, |f| f.score)));
    if candidates.is_empty() {
        notes.push("MusicBrainz found nothing for that. Try fewer words, or the barcode.".into());
    }
    Ok(Json(SearchOut { candidates, notes }))
}

async fn release(
    State(st): State<AppState>,
    Admin(_): Admin,
    Path((id, mbid)): Path<(i64, String)>,
) -> ApiResult<Json<Release>> {
    item(&st, id).await?;
    if !st.lookups.enabled() {
        return Err(switched_off());
    }
    if !musicbrainz::is_mbid(&mbid) {
        return Err(ApiError::Missing(NotFound.to_string()));
    }
    Ok(Json(st.lookups.release(&mbid).await.map_err(upstream)?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchBody {
    release: String,
    #[serde(default)]
    files: Vec<FileInfo>,
}

#[derive(Serialize)]
struct MatchOut {
    release: Release,
    #[serde(flatten)]
    assignment: Assignment,
}

async fn matching(
    State(st): State<AppState>,
    Admin(_): Admin,
    Path(id): Path<i64>,
    body: Result<Json<MatchBody>, JsonRejection>,
) -> ApiResult<Json<MatchOut>> {
    let body = json_body(body)?;
    item(&st, id).await?;
    if !st.lookups.enabled() {
        return Err(switched_off());
    }
    check_files(&body.files)?;
    let mbid = musicbrainz::mbid_in(&body.release)
        .ok_or_else(|| ApiError::Missing(NotFound.to_string()))?;
    let release = st.lookups.release(&mbid).await.map_err(upstream)?;
    let assignment = trackmatch::assign(&body.files, &release);
    Ok(Json(MatchOut {
        release,
        assignment,
    }))
}

#[derive(Deserialize)]
pub struct ItemBody {
    #[serde(default)]
    creator: String,
    #[serde(default)]
    year: Option<i64>,
}

#[derive(Serialize)]
struct ItemOut {
    creator: String,
    year: Option<i64>,
}

/// Fills the entry's artist and year from the release, only where empty.
async fn fill_item(
    State(st): State<AppState>,
    Admin(_): Admin,
    Path(id): Path<i64>,
    body: Result<Json<ItemBody>, JsonRejection>,
) -> ApiResult<Json<ItemOut>> {
    let body = json_body(body)?;
    let it = st
        .db
        .write(move |c| items::fill_empty(c, id, &body.creator, body.year))
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(ItemOut {
        creator: it.creator,
        year: it.year,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn barcodes_but_not_isbns() {
        assert_eq!(
            barcode_of("0 75992-74432 9").as_deref(),
            Some("075992744329")
        );
        assert_eq!(
            barcode_of("5012345678900").as_deref(),
            Some("5012345678900")
        );
        assert_eq!(barcode_of("9780261102958"), None);
        assert_eq!(barcode_of("0261102958"), None);
        assert_eq!(barcode_of("Blue"), None);
        assert_eq!(barcode_of(""), None);
    }
}
