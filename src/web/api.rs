//! The Player's JSON API (`/api`): the catalogue of digital copies,
//! playlists and favourites. Every handler takes [`SignedIn`]; a child
//! account never receives anything from a restricted item, and gets 404 when
//! asking for one directly. The shapes follow docs/player.md.

use std::collections::HashSet;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::AppState;
use super::accounts::{ApiError, ApiJson, ApiResult, SignedIn};
use crate::db::favourites;
use crate::db::media_files::{self, CatalogueQuery, ItemWithMedia, MediaFile, MediaKind, Subtitle};
use crate::db::playlists::{self, Looker, PlaylistView};

/// Most ids accepted in one request.
const MAX_IDS: usize = 5000;

// ---------- JSON shapes ----------

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleJson {
    pub id: i64,
    pub language: String,
    pub label: String,
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MediaFileJson {
    pub id: i64,
    pub item_id: i64,
    pub kind: MediaKind,
    pub title: String,
    pub disc: i64,
    pub track: Option<i64>,
    pub mime: String,
    pub size_bytes: i64,
    pub duration_ms: Option<i64>,
    pub stream_url: String,
    pub subtitles: Vec<SubtitleJson>,
    pub item_title: String,
    pub creator: String,
    pub year: Option<i64>,
    pub cover: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ItemSummaryJson {
    pub id: i64,
    pub title: String,
    pub creator: String,
    pub year: Option<i64>,
    #[serde(rename = "type")]
    pub type_name: String,
    pub type_slug: String,
    pub restricted: bool,
    pub cover: Option<String>,
    pub kinds: Vec<MediaKind>,
    pub file_count: i64,
    pub total_duration_ms: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ItemDetailJson {
    #[serde(flatten)]
    pub summary: ItemSummaryJson,
    pub description: String,
    pub media: Vec<MediaFileJson>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ItemListJson {
    pub items: Vec<ItemSummaryJson>,
    pub total: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistSummaryJson {
    pub id: i64,
    pub name: String,
    pub owner: String,
    pub mine: bool,
    pub entry_count: usize,
    pub total_duration_ms: Option<i64>,
    pub covers: Vec<String>,
    pub updated_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistJson {
    #[serde(flatten)]
    pub summary: PlaylistSummaryJson,
    pub entries: Vec<MediaFileJson>,
}

fn photo_url(file: &str) -> String {
    format!("/photos/{file}")
}

fn subtitle_json(s: &Subtitle) -> SubtitleJson {
    SubtitleJson {
        id: s.id,
        language: s.language.clone(),
        label: s.label.clone(),
        url: format!("/api/subtitles/{}", s.id),
    }
}

fn media_json(m: &MediaFile, subs: Option<&Vec<Subtitle>>) -> MediaFileJson {
    MediaFileJson {
        id: m.id,
        item_id: m.item_id,
        kind: m.kind,
        title: m.title.clone(),
        disc: m.disc,
        track: m.track,
        mime: m.mime.clone(),
        size_bytes: m.size_bytes,
        duration_ms: m.duration_ms,
        stream_url: format!("/api/media/{}/stream", m.id),
        subtitles: subs
            .map(|v| v.iter().map(subtitle_json).collect())
            .unwrap_or_default(),
        item_title: m.item_title.clone(),
        creator: m.creator.clone(),
        year: m.year,
        cover: m.cover.as_deref().map(photo_url),
    }
}

/// JSON for files, with their subtitles (one query for all of them).
fn media_list_json(conn: &Connection, files: &[MediaFile]) -> anyhow::Result<Vec<MediaFileJson>> {
    let ids: Vec<i64> = files.iter().map(|m| m.id).collect();
    let subs = media_files::subtitles_for(conn, &ids)?;
    Ok(files
        .iter()
        .map(|m| media_json(m, subs.get(&m.id)))
        .collect())
}

fn summary_json(i: ItemWithMedia) -> (ItemSummaryJson, String) {
    (
        ItemSummaryJson {
            id: i.id,
            title: i.title,
            creator: i.creator,
            year: i.year,
            type_name: i.type_name,
            type_slug: i.type_slug,
            restricted: i.restricted,
            cover: i.cover.as_deref().map(photo_url),
            kinds: i.kinds,
            file_count: i.file_count,
            total_duration_ms: i.total_duration_ms,
        },
        i.description,
    )
}

fn sum_durations(files: &[MediaFile]) -> Option<i64> {
    files
        .iter()
        .filter_map(|m| m.duration_ms)
        .fold(None, |acc, d| Some(acc.unwrap_or(0).saturating_add(d)))
}

fn playlist_summary(v: &PlaylistView) -> PlaylistSummaryJson {
    let mut covers: Vec<String> = Vec::new();
    for c in v.entries.iter().filter_map(|m| m.cover.as_deref()) {
        let url = photo_url(c);
        if !covers.contains(&url) {
            covers.push(url);
            if covers.len() == 4 {
                break;
            }
        }
    }
    PlaylistSummaryJson {
        id: v.row.id,
        name: v.row.name.clone(),
        owner: v.row.owner.clone(),
        mine: v.mine,
        entry_count: v.entries.len(),
        total_duration_ms: sum_durations(&v.entries),
        covers,
        updated_at: v.row.updated_at,
    }
}

fn playlist_json(conn: &Connection, v: &PlaylistView) -> anyhow::Result<PlaylistJson> {
    Ok(PlaylistJson {
        summary: playlist_summary(v),
        entries: media_list_json(conn, &v.entries)?,
    })
}

fn looker(who: &SignedIn) -> Looker {
    Looker {
        account_id: who.account.id,
        adult: who.adult(),
    }
}

// ---------- about the library ----------

/// `GET /api/info` (no account needed): the library's name, the server's
/// version and where its source code is (the AGPL's offer, linked from the
/// Player's about screen).
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InfoJson {
    pub name: String,
    pub version: String,
    pub source_url: String,
}

pub async fn info(State(st): State<AppState>) -> ApiResult<Json<InfoJson>> {
    let default_title = st.cfg.default_title.clone();
    let name = st
        .db
        .read(move |c| Ok(crate::db::settings::branding(c, &default_title)?.title))
        .await?;
    Ok(Json(InfoJson {
        name,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        source_url: st.cfg.source_url.clone(),
    }))
}

// ---------- catalogue ----------

#[derive(Debug, Default, Deserialize)]
pub struct ItemsParams {
    #[serde(default)]
    q: String,
    #[serde(default)]
    kind: String,
    #[serde(default, rename = "type")]
    type_slug: String,
    #[serde(default)]
    offset: String,
    #[serde(default)]
    limit: String,
    #[serde(default)]
    sort: String,
}

pub async fn items(
    State(st): State<AppState>,
    who: SignedIn,
    Query(p): Query<ItemsParams>,
) -> ApiResult<Json<ItemListJson>> {
    let kind = match p.kind.trim() {
        "" => None,
        k => Some(
            MediaKind::parse(k)
                .filter(|k| k.playable())
                .ok_or_else(|| {
                    ApiError::BadRequest("Kind should be audio, video or document.".into())
                })?,
        ),
    };
    let query = CatalogueQuery {
        q: Some(p.q.chars().take(200).collect()),
        kind,
        type_slug: Some(p.type_slug),
        item_id: None,
        adult: who.adult(),
        by_title: p.sort == "title",
        offset: p.offset.parse().unwrap_or(0),
        limit: p.limit.parse().unwrap_or(50),
    };
    let (rows, total) = st
        .db
        .read(move |c| media_files::catalogue(c, &query))
        .await?;
    Ok(Json(ItemListJson {
        items: rows.into_iter().map(|i| summary_json(i).0).collect(),
        total,
    }))
}

pub async fn item(
    State(st): State<AppState>,
    who: SignedIn,
    Path(id): Path<i64>,
) -> ApiResult<Json<ItemDetailJson>> {
    let adult = who.adult();
    let detail = st
        .db
        .read(move |c| {
            let query = CatalogueQuery {
                item_id: Some(id),
                adult,
                limit: 1,
                ..Default::default()
            };
            let Some(row) = media_files::catalogue(c, &query)?.0.pop() else {
                return Ok(None);
            };
            let files = media_files::playable_for_item(c, id)?;
            let media = media_list_json(c, &files)?;
            let (summary, description) = summary_json(row);
            Ok(Some(ItemDetailJson {
                summary,
                description,
                media,
            }))
        })
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(detail))
}

/// A file the viewer may play, or 404.
pub(super) async fn visible_media(st: &AppState, who: &SignedIn, id: i64) -> ApiResult<MediaFile> {
    let adult = who.adult();
    st.db
        .read(move |c| media_files::get(c, id))
        .await?
        .filter(|m| m.visible_to(adult))
        .ok_or(ApiError::NotFound)
}

pub async fn media(
    State(st): State<AppState>,
    who: SignedIn,
    Path(id): Path<i64>,
) -> ApiResult<Json<MediaFileJson>> {
    let m = visible_media(&st, &who, id).await?;
    let json = st
        .db
        .read(move |c| media_list_json(c, std::slice::from_ref(&m)))
        .await?
        .pop()
        .ok_or(ApiError::NotFound)?;
    Ok(Json(json))
}

#[derive(Deserialize)]
pub struct IdsBody {
    #[serde(default)]
    ids: Vec<i64>,
}

#[derive(Serialize)]
pub struct AllowedJson {
    allowed: Vec<i64>,
}

/// Which of the given files still exist and may be played (the Player uses
/// it to remove downloads that are no longer allowed).
pub async fn check(
    State(st): State<AppState>,
    who: SignedIn,
    ApiJson(body): ApiJson<IdsBody>,
) -> ApiResult<Json<AllowedJson>> {
    if body.ids.len() > MAX_IDS {
        return Err(ApiError::BadRequest("Too many ids at once.".into()));
    }
    let adult = who.adult();
    let ids = body.ids;
    let allowed = st
        .db
        .read(move |c| {
            let found = media_files::visible_by_ids(c, &ids, adult)?;
            let mut seen = HashSet::new();
            Ok(ids
                .into_iter()
                .filter(|id| found.contains_key(id) && seen.insert(*id))
                .collect())
        })
        .await?;
    Ok(Json(AllowedJson { allowed }))
}

// ---------- playlists ----------

#[derive(Debug, Default, Deserialize)]
pub struct PlaylistsParams {
    #[serde(default)]
    mine: String,
}

pub async fn playlists(
    State(st): State<AppState>,
    who: SignedIn,
    Query(p): Query<PlaylistsParams>,
) -> ApiResult<Json<Vec<PlaylistSummaryJson>>> {
    let mine = matches!(p.mine.as_str(), "1" | "true" | "yes");
    let looker = looker(&who);
    let list = st
        .db
        .read(move |c| playlists::list_visible(c, looker, mine))
        .await?;
    Ok(Json(list.iter().map(playlist_summary).collect()))
}

#[derive(Deserialize)]
pub struct NameBody {
    #[serde(default)]
    name: String,
}

pub async fn create_playlist(
    State(st): State<AppState>,
    who: SignedIn,
    ApiJson(body): ApiJson<NameBody>,
) -> ApiResult {
    let looker = looker(&who);
    let json = st
        .db
        .write(move |c| {
            let id = playlists::create(c, looker.account_id, &body.name)?;
            let view = playlists::get_visible(c, id, looker)?
                .ok_or_else(|| anyhow::anyhow!("new playlist not visible"))?;
            playlist_json(c, &view)
        })
        .await?;
    Ok((StatusCode::CREATED, Json(json)).into_response())
}

pub async fn playlist(
    State(st): State<AppState>,
    who: SignedIn,
    Path(id): Path<i64>,
) -> ApiResult<Json<PlaylistJson>> {
    let looker = looker(&who);
    let json = st
        .db
        .read(move |c| {
            playlists::get_visible(c, id, looker)?
                .map(|v| playlist_json(c, &v))
                .transpose()
        })
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(json))
}

/// What an owner-only change found.
enum Owned<T> {
    Done(T),
    Missing,
    NotOwner,
}

impl<T> Owned<T> {
    fn into_result(self) -> ApiResult<T> {
        match self {
            Owned::Done(v) => Ok(v),
            Owned::Missing => Err(ApiError::NotFound),
            Owned::NotOwner => Err(ApiError::Forbidden(
                "Only the person who made this playlist can change it.".into(),
            )),
        }
    }
}

/// Runs `change` on a playlist the viewer owns (404 if they can't see it,
/// 403 if it's someone else's), then returns the playlist as they see it.
async fn change_owned<F>(st: &AppState, who: &SignedIn, id: i64, change: F) -> ApiResult<Response>
where
    F: FnOnce(&mut Connection, Looker) -> anyhow::Result<bool> + Send + 'static,
{
    let looker = looker(who);
    let outcome = st
        .db
        .write(move |c| {
            let Some(view) = playlists::get_visible(c, id, looker)? else {
                return Ok(Owned::Missing);
            };
            if !view.mine {
                return Ok(Owned::NotOwner);
            }
            if !change(c, looker)? {
                return Ok(Owned::Missing);
            }
            match playlists::get_visible(c, id, looker)? {
                Some(v) => Ok(Owned::Done(Some(playlist_json(c, &v)?))),
                None => Ok(Owned::Done(None)),
            }
        })
        .await?
        .into_result()?;
    Ok(match outcome {
        Some(json) => Json(json).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    })
}

pub async fn rename_playlist(
    State(st): State<AppState>,
    who: SignedIn,
    Path(id): Path<i64>,
    ApiJson(body): ApiJson<NameBody>,
) -> ApiResult {
    change_owned(&st, &who, id, move |c, _| {
        playlists::rename(c, id, &body.name)?;
        Ok(true)
    })
    .await
}

pub async fn delete_playlist(
    State(st): State<AppState>,
    who: SignedIn,
    Path(id): Path<i64>,
) -> ApiResult {
    change_owned(&st, &who, id, move |c, _| {
        playlists::delete(c, id)?;
        Ok(true)
    })
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntriesBody {
    #[serde(default)]
    media_ids: Vec<i64>,
}

/// True if the viewer may play every one of `ids`.
fn all_visible(c: &Connection, ids: &[i64], adult: bool) -> anyhow::Result<bool> {
    let found = media_files::visible_by_ids(c, ids, adult)?;
    Ok(ids.iter().all(|id| found.contains_key(id)))
}

pub async fn append_entries(
    State(st): State<AppState>,
    who: SignedIn,
    Path(id): Path<i64>,
    ApiJson(body): ApiJson<EntriesBody>,
) -> ApiResult {
    if body.media_ids.len() > MAX_IDS {
        return Err(ApiError::BadRequest("Too many files at once.".into()));
    }
    change_owned(&st, &who, id, move |c, looker| {
        if !all_visible(c, &body.media_ids, looker.adult)? {
            return Ok(false);
        }
        playlists::append(c, id, &body.media_ids)?;
        Ok(true)
    })
    .await
}

pub async fn replace_entries(
    State(st): State<AppState>,
    who: SignedIn,
    Path(id): Path<i64>,
    ApiJson(body): ApiJson<EntriesBody>,
) -> ApiResult {
    if body.media_ids.len() > MAX_IDS {
        return Err(ApiError::BadRequest("Too many files at once.".into()));
    }
    change_owned(&st, &who, id, move |c, looker| {
        if !all_visible(c, &body.media_ids, looker.adult)? {
            return Ok(false);
        }
        playlists::replace_visible(c, id, &body.media_ids, looker.adult)?;
        Ok(true)
    })
    .await
}

// ---------- favourites ----------

pub async fn favourites(
    State(st): State<AppState>,
    who: SignedIn,
) -> ApiResult<Json<Vec<MediaFileJson>>> {
    let (account, adult) = (who.account.id, who.adult());
    let list = st
        .db
        .read(move |c| {
            let files = favourites::list(c, account, adult)?;
            media_list_json(c, &files)
        })
        .await?;
    Ok(Json(list))
}

pub async fn add_favourite(
    State(st): State<AppState>,
    who: SignedIn,
    Path(media_id): Path<i64>,
) -> ApiResult {
    visible_media(&st, &who, media_id).await?;
    let account = who.account.id;
    st.db
        .write(move |c| favourites::add(c, account, media_id))
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub async fn remove_favourite(
    State(st): State<AppState>,
    who: SignedIn,
    Path(media_id): Path<i64>,
) -> ApiResult {
    let account = who.account.id;
    st.db
        .write(move |c| favourites::remove(c, account, media_id))
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
