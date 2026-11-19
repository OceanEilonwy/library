//! Digital copies of items: media files (tracks, films, documents) and the
//! subtitles attached to videos.
//!
//! Every read that serves the Player takes `adult`: a child account never
//! receives a file of a restricted item. Files of the `archive` kind (rip
//! logs, cue sheets, masters) are for the librarian only: no Player read
//! returns them.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use super::now;
use crate::util;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Audio,
    Video,
    Document,
    /// Kept for the record, never shown or streamed in the Player.
    Archive,
}

impl MediaKind {
    pub const ALL: [MediaKind; 4] = [
        MediaKind::Audio,
        MediaKind::Video,
        MediaKind::Document,
        MediaKind::Archive,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Audio => "audio",
            MediaKind::Video => "video",
            MediaKind::Document => "document",
            MediaKind::Archive => "archive",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "audio" => Some(MediaKind::Audio),
            "video" => Some(MediaKind::Video),
            "document" => Some(MediaKind::Document),
            "archive" => Some(MediaKind::Archive),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MediaKind::Audio => "Audio",
            MediaKind::Video => "Video",
            MediaKind::Document => "Document",
            MediaKind::Archive => "Archive",
        }
    }

    /// Does the Player ever see files of this kind?
    pub fn playable(self) -> bool {
        match self {
            MediaKind::Audio | MediaKind::Video | MediaKind::Document => true,
            MediaKind::Archive => false,
        }
    }
}

/// SQL condition (on `m`) for files the Player may see at all.
const PLAYABLE: &str = "m.kind <> 'archive'";

/// A media file with the item details the Player shows alongside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaFile {
    pub id: i64,
    pub item_id: i64,
    pub kind: MediaKind,
    pub title: String,
    pub disc: i64,
    pub track: Option<i64>,
    pub file: String,
    pub original_name: String,
    pub mime: String,
    pub size_bytes: i64,
    pub duration_ms: Option<i64>,
    pub position: i64,
    pub created_at: i64,
    pub item_title: String,
    pub creator: String,
    pub year: Option<i64>,
    pub restricted: bool,
    /// File name of the item's first photo.
    pub cover: Option<String>,
    /// Lower-case hex SHA-256 of the stored bytes (older uploads may lack it).
    pub sha256: Option<String>,
}

const SELECT: &str = "SELECT m.id, m.item_id, m.kind, m.title, m.disc, m.track, m.file,
        m.original_name, m.mime, m.size_bytes, m.duration_ms, m.position, m.created_at,
        i.title, i.creator, i.year, i.restricted,
        (SELECT p.file FROM photos p WHERE p.item_id = i.id ORDER BY p.position, p.id LIMIT 1),
        m.sha256
    FROM media_files m JOIN items i ON i.id = m.item_id";

/// The order files of an item play in.
pub const ORDER: &str = "m.disc, m.track IS NULL, m.track, m.position, m.id";

impl MediaFile {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            item_id: r.get(1)?,
            // Anything unknown is treated as an archive file: never played.
            kind: MediaKind::parse(&r.get::<_, String>(2)?).unwrap_or(MediaKind::Archive),
            title: r.get(3)?,
            disc: r.get(4)?,
            track: r.get(5)?,
            file: r.get(6)?,
            original_name: r.get(7)?,
            mime: r.get(8)?,
            size_bytes: r.get(9)?,
            duration_ms: r.get(10)?,
            position: r.get(11)?,
            created_at: r.get(12)?,
            item_title: r.get(13)?,
            creator: r.get(14)?,
            year: r.get(15)?,
            restricted: r.get(16)?,
            cover: r.get(17)?,
            sha256: r.get(18)?,
        })
    }

    /// May a Player viewer with this much access play it? Never for
    /// archive files.
    pub fn visible_to(&self, adult: bool) -> bool {
        self.kind.playable() && (adult || !self.restricted)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subtitle {
    pub id: i64,
    pub media_file_id: i64,
    pub language: String,
    pub label: String,
    pub file: String,
}

/// What an upload records.
#[derive(Debug, Clone)]
pub struct NewMediaFile {
    pub item_id: i64,
    pub kind: MediaKind,
    pub title: String,
    pub disc: i64,
    pub track: Option<i64>,
    pub file: String,
    pub original_name: String,
    pub mime: String,
    pub size_bytes: i64,
    pub duration_ms: Option<i64>,
    /// Lower-case hex SHA-256 of the stored bytes.
    pub sha256: Option<String>,
}

/// Cleans a title, falling back to `fallback` when empty.
pub fn clean_title(title: &str, fallback: &str) -> String {
    match util::clean(title, 300) {
        t if t.is_empty() => util::clean(fallback, 300),
        t => t,
    }
}

/// Keeps disc and track numbers in a sensible range.
pub fn clean_numbers(disc: Option<i64>, track: Option<i64>) -> (i64, Option<i64>) {
    (
        disc.filter(|d| (1..=999).contains(d)).unwrap_or(1),
        track.filter(|t| (0..=9999).contains(t)),
    )
}

/// Is this 64 lower-case hex digits?
pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn add(conn: &Connection, m: &NewMediaFile) -> Result<i64> {
    let (disc, track) = clean_numbers(Some(m.disc), m.track);
    conn.execute(
        "INSERT INTO media_files (item_id, kind, title, disc, track, file, original_name, mime,
                                  size_bytes, duration_ms, position, created_at, sha256)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                 (SELECT coalesce(max(position), 0) + 1 FROM media_files WHERE item_id = ?1), ?11,
                 ?12)",
        params![
            m.item_id,
            m.kind.as_str(),
            clean_title(&m.title, &m.original_name),
            disc,
            track,
            m.file,
            util::clean(&m.original_name, 300),
            m.mime,
            m.size_bytes,
            m.duration_ms.filter(|d| *d >= 0),
            now(),
            m.sha256.as_deref().filter(|h| is_sha256_hex(h)),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// One file, whatever its restriction (callers check [`MediaFile::visible_to`]).
pub fn get(conn: &Connection, id: i64) -> Result<Option<MediaFile>> {
    Ok(conn
        .prepare_cached(&format!("{SELECT} WHERE m.id = ?1"))?
        .query_row([id], MediaFile::from_row)
        .optional()?)
}

/// An item's files the Player may show (no archive files), in playing order.
pub fn playable_for_item(conn: &Connection, item_id: i64) -> Result<Vec<MediaFile>> {
    let mut stmt = conn.prepare_cached(&format!(
        "{SELECT} WHERE m.item_id = ?1 AND {PLAYABLE} ORDER BY {ORDER}"
    ))?;
    let rows = stmt.query_map([item_id], MediaFile::from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Every file of an item, archive files included, in playing order (for
/// the librarian and Library Rip).
pub fn for_item(conn: &Connection, item_id: i64) -> Result<Vec<MediaFile>> {
    let mut stmt =
        conn.prepare_cached(&format!("{SELECT} WHERE m.item_id = ?1 ORDER BY {ORDER}"))?;
    let rows = stmt.query_map([item_id], MediaFile::from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Files by id that the viewer may play, keyed by id (missing ids and
/// archive files skipped).
pub fn visible_by_ids(
    conn: &Connection,
    ids: &[i64],
    adult: bool,
) -> Result<HashMap<i64, MediaFile>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let json = serde_json::to_string(ids)?;
    let mut stmt = conn.prepare_cached(&format!(
        "{SELECT} WHERE m.id IN (SELECT value FROM json_each(?1)) AND (?2 OR i.restricted = 0)
         AND {PLAYABLE}"
    ))?;
    let rows = stmt.query_map(params![json, adult], MediaFile::from_row)?;
    rows.map(|r| r.map(|m| (m.id, m)))
        .collect::<rusqlite::Result<_>>()
        .map_err(Into::into)
}

/// Subtitles of the given files, by file id, in label order.
pub fn subtitles_for(conn: &Connection, media_ids: &[i64]) -> Result<HashMap<i64, Vec<Subtitle>>> {
    let mut out: HashMap<i64, Vec<Subtitle>> = HashMap::new();
    if media_ids.is_empty() {
        return Ok(out);
    }
    let json = serde_json::to_string(media_ids)?;
    let mut stmt = conn.prepare_cached(
        "SELECT id, media_file_id, language, label, file FROM subtitles
         WHERE media_file_id IN (SELECT value FROM json_each(?1)) ORDER BY label, id",
    )?;
    let rows = stmt.query_map([json], |r| {
        Ok(Subtitle {
            id: r.get(0)?,
            media_file_id: r.get(1)?,
            language: r.get(2)?,
            label: r.get(3)?,
            file: r.get(4)?,
        })
    })?;
    for s in rows {
        let s = s?;
        out.entry(s.media_file_id).or_default().push(s);
    }
    Ok(out)
}

/// A subtitle and whether its item is restricted.
pub fn subtitle(conn: &Connection, id: i64) -> Result<Option<(Subtitle, bool)>> {
    Ok(conn
        .prepare_cached(
            "SELECT s.id, s.media_file_id, s.language, s.label, s.file, i.restricted
             FROM subtitles s JOIN media_files m ON m.id = s.media_file_id
             JOIN items i ON i.id = m.item_id WHERE s.id = ?1",
        )?
        .query_row([id], |r| {
            Ok((
                Subtitle {
                    id: r.get(0)?,
                    media_file_id: r.get(1)?,
                    language: r.get(2)?,
                    label: r.get(3)?,
                    file: r.get(4)?,
                },
                r.get(5)?,
            ))
        })
        .optional()?)
}

pub fn add_subtitle(
    conn: &Connection,
    media_file_id: i64,
    language: &str,
    label: &str,
    file: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO subtitles (media_file_id, language, label, file, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![media_file_id, language, label, file, now()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Removes a subtitle of a file of `item_id`; returns its stored file.
pub fn remove_subtitle(
    conn: &Connection,
    item_id: i64,
    subtitle_id: i64,
) -> Result<Option<String>> {
    let file: Option<String> = conn
        .query_row(
            "SELECT s.file FROM subtitles s JOIN media_files m ON m.id = s.media_file_id
             WHERE s.id = ?1 AND m.item_id = ?2",
            params![subtitle_id, item_id],
            |r| r.get(0),
        )
        .optional()?;
    if file.is_some() {
        conn.execute("DELETE FROM subtitles WHERE id = ?1", [subtitle_id])?;
    }
    Ok(file)
}

/// Renames and renumbers a file of `item_id`. False if it isn't one.
pub fn update(
    conn: &Connection,
    item_id: i64,
    id: i64,
    title: &str,
    disc: Option<i64>,
    track: Option<i64>,
) -> Result<bool> {
    let Some(current) = get(conn, id)?.filter(|m| m.item_id == item_id) else {
        return Ok(false);
    };
    let (disc, track) = clean_numbers(disc, track);
    conn.execute(
        "UPDATE media_files SET title = ?2, disc = ?3, track = ?4 WHERE id = ?1",
        params![id, clean_title(title, &current.title), disc, track],
    )?;
    Ok(true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

/// Moves a file one place up or down its item's list. Positions are made
/// dense first; when both files have track numbers on the same disc those
/// swap too, so the playing order follows.
pub fn shift(conn: &mut Connection, item_id: i64, id: i64, dir: Direction) -> Result<()> {
    let tx = conn.transaction()?;
    let order: Vec<(i64, i64, Option<i64>)> = tx
        .prepare(&format!(
            "SELECT m.id, m.disc, m.track FROM media_files m WHERE m.item_id = ?1 ORDER BY {ORDER}"
        ))?
        .query_map([item_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let Some(at) = order.iter().position(|(m, _, _)| *m == id) else {
        return Ok(());
    };
    let other = match dir {
        Direction::Up => at.checked_sub(1),
        Direction::Down => Some(at + 1).filter(|i| *i < order.len()),
    };
    let Some(other) = other else {
        return Ok(());
    };
    let mut ids: Vec<i64> = order.iter().map(|(m, _, _)| *m).collect();
    ids.swap(at, other);
    for (pos, m) in ids.iter().enumerate() {
        tx.execute(
            "UPDATE media_files SET position = ?2 WHERE id = ?1",
            params![m, i64::try_from(pos).unwrap_or(i64::MAX)],
        )?;
    }
    if let (Some(&(a, disc_a, Some(track_a))), Some(&(b, disc_b, Some(track_b)))) =
        (order.get(at), order.get(other))
        && disc_a == disc_b
    {
        tx.execute(
            "UPDATE media_files SET track = ?2 WHERE id = ?1",
            params![a, track_b],
        )?;
        tx.execute(
            "UPDATE media_files SET track = ?2 WHERE id = ?1",
            params![b, track_a],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Removes a file of `item_id`; returns the stored files to delete (the
/// media file and its subtitles).
pub fn remove(conn: &mut Connection, item_id: i64, id: i64) -> Result<Option<Vec<String>>> {
    let tx = conn.transaction()?;
    let file: Option<String> = tx
        .query_row(
            "SELECT file FROM media_files WHERE id = ?1 AND item_id = ?2",
            params![id, item_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(file) = file else {
        return Ok(None);
    };
    let mut files: Vec<String> = tx
        .prepare("SELECT file FROM subtitles WHERE media_file_id = ?1")?
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    files.push(file);
    tx.execute("DELETE FROM media_files WHERE id = ?1", [id])?;
    tx.commit()?;
    Ok(Some(files))
}

/// Every stored file (media and subtitles) belonging to an item.
pub fn files_for_item(conn: &Connection, item_id: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT file FROM media_files WHERE item_id = ?1
         UNION ALL
         SELECT s.file FROM subtitles s JOIN media_files m ON m.id = s.media_file_id
         WHERE m.item_id = ?1",
    )?;
    let rows = stmt.query_map([item_id], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Which items with digital copies to list for the Player.
#[derive(Debug, Clone, Default)]
pub struct CatalogueQuery {
    /// Words to find in the title or creator.
    pub q: Option<String>,
    pub kind: Option<MediaKind>,
    pub type_slug: Option<String>,
    pub item_id: Option<i64>,
    pub adult: bool,
    /// Alphabetical instead of newest first.
    pub by_title: bool,
    pub offset: i64,
    pub limit: i64,
}

/// An item that has digital copies, summarised for the Player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemWithMedia {
    pub id: i64,
    pub title: String,
    pub creator: String,
    pub year: Option<i64>,
    pub type_name: String,
    pub type_slug: String,
    pub restricted: bool,
    pub cover: Option<String>,
    pub kinds: Vec<MediaKind>,
    pub file_count: i64,
    pub total_duration_ms: Option<i64>,
    pub description: String,
}

/// Escapes `%`, `_` and `\` for a LIKE pattern.
pub(crate) fn like_pattern(q: &str) -> String {
    let mut out = String::with_capacity(q.len() + 2);
    out.push('%');
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// Items with at least one file the viewer may play, and how many match.
pub fn catalogue(conn: &Connection, q: &CatalogueQuery) -> Result<(Vec<ItemWithMedia>, i64)> {
    // Archive files never count: an item with only those isn't listed.
    const FROM: &str = "FROM items i JOIN media_types t ON t.id = i.type_id
        WHERE EXISTS (SELECT 1 FROM media_files m WHERE m.item_id = i.id
                      AND m.kind <> 'archive'
                      AND (:kind IS NULL OR m.kind = :kind))
          AND (:adult OR i.restricted = 0)
          AND (:item IS NULL OR i.id = :item)
          AND (:type IS NULL OR t.slug = :type)
          AND (:q IS NULL OR i.title LIKE :q ESCAPE '\\' OR i.creator LIKE :q ESCAPE '\\')";
    let pattern =
        q.q.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(like_pattern);
    let kind = q.kind.map(MediaKind::as_str);
    let params = rusqlite::named_params! {
        ":kind": kind,
        ":adult": q.adult,
        ":item": q.item_id,
        ":type": q.type_slug.as_deref().filter(|s| !s.is_empty()),
        ":q": pattern,
    };
    let total: i64 = conn
        .prepare_cached(&format!("SELECT count(*) {FROM}"))?
        .query_row(params, |r| r.get(0))?;
    let order = if q.by_title {
        "i.title COLLATE NOCASE, i.id"
    } else {
        "(SELECT max(m.created_at) FROM media_files m WHERE m.item_id = i.id
          AND m.kind <> 'archive') DESC, i.id DESC"
    };
    let sql = format!(
        "SELECT i.id, i.title, i.creator, i.year, t.name, t.slug, i.restricted,
            (SELECT p.file FROM photos p WHERE p.item_id = i.id ORDER BY p.position, p.id LIMIT 1),
            (SELECT group_concat(DISTINCT m.kind) FROM media_files m
             WHERE m.item_id = i.id AND {PLAYABLE}),
            (SELECT count(*) FROM media_files m WHERE m.item_id = i.id AND {PLAYABLE}),
            (SELECT sum(m.duration_ms) FROM media_files m WHERE m.item_id = i.id AND {PLAYABLE}),
            i.description
         {FROM} ORDER BY {order} LIMIT {} OFFSET {}",
        q.limit.clamp(1, 200),
        q.offset.max(0)
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params, |r| {
        let kinds: Option<String> = r.get(8)?;
        let mut kinds: Vec<MediaKind> = kinds
            .unwrap_or_default()
            .split(',')
            .filter_map(MediaKind::parse)
            .filter(|k| k.playable())
            .collect();
        kinds.sort_by_key(|k| MediaKind::ALL.iter().position(|a| a == k));
        Ok(ItemWithMedia {
            id: r.get(0)?,
            title: r.get(1)?,
            creator: r.get(2)?,
            year: r.get(3)?,
            type_name: r.get(4)?,
            type_slug: r.get(5)?,
            restricted: r.get(6)?,
            cover: r.get(7)?,
            kinds,
            file_count: r.get(9)?,
            total_duration_ms: r.get(10)?,
            description: r.get(11)?,
        })
    })?;
    Ok((rows.collect::<rusqlite::Result<_>>()?, total))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::db::items::{self, tests::input};
    use crate::db::testing::temp_db;

    pub fn new_file(item_id: i64, title: &str, track: Option<i64>) -> NewMediaFile {
        NewMediaFile {
            item_id,
            kind: MediaKind::Audio,
            title: title.into(),
            disc: 1,
            track,
            file: format!("{}.mp3", title.to_lowercase().replace(' ', "-")),
            original_name: format!("{title}.mp3"),
            mime: "audio/mpeg".into(),
            size_bytes: 10,
            duration_ms: Some(1000),
            sha256: None,
        }
    }

    #[test]
    fn add_order_update_shift_remove() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let item = items::create(c, &input(c, "Blue", "Joni Mitchell"))?;
            let b = add(c, &new_file(item, "Carey", Some(2)))?;
            let a = add(c, &new_file(item, "All I Want", Some(1)))?;
            let ids = |c: &Connection| -> Result<Vec<i64>> {
                Ok(for_item(c, item)?.iter().map(|m| m.id).collect())
            };
            assert_eq!(ids(c)?, vec![a, b], "track order wins");
            let m = get(c, a)?.unwrap();
            assert_eq!(
                (m.item_title.as_str(), m.creator.as_str()),
                ("Blue", "Joni Mitchell")
            );
            assert!(m.visible_to(false));

            assert!(update(c, item, a, "  ", Some(0), Some(3))?);
            let m = get(c, a)?.unwrap();
            assert_eq!(m.title, "All I Want", "blank title keeps the old one");
            assert_eq!((m.disc, m.track), (1, Some(3)));
            assert!(!update(c, item + 1, a, "x", None, None)?);
            assert_eq!(ids(c)?, vec![b, a]);

            shift(c, item, a, Direction::Up)?;
            assert_eq!(ids(c)?, vec![a, b], "moving swaps track numbers");
            shift(c, item, a, Direction::Up)?;
            assert_eq!(ids(c)?, vec![a, b], "already first");

            let sub = add_subtitle(c, a, "en", "English", "s.vtt")?;
            assert_eq!(subtitles_for(c, &[a])?[&a].len(), 1);
            assert_eq!(subtitle(c, sub)?.unwrap().0.label, "English");
            let mut all = files_for_item(c, item)?;
            all.sort();
            assert_eq!(all, vec!["all-i-want.mp3", "carey.mp3", "s.vtt"]);

            assert_eq!(remove(c, item + 1, a)?, None, "must belong to the item");
            let mut gone = remove(c, item, a)?.unwrap();
            gone.sort();
            assert_eq!(gone, vec!["all-i-want.mp3", "s.vtt"]);
            assert!(subtitle(c, sub)?.is_none());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn catalogue_filters() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let blue = items::create(c, &input(c, "Blue", "Joni Mitchell"))?;
            items::create(c, &input(c, "No copy", ""))?;
            let mut late = input(c, "100% Late_night", "");
            late.restricted = true;
            let late = items::create(c, &late)?;
            add(c, &new_file(blue, "Carey", Some(1)))?;
            let mut film = new_file(late, "Film", None);
            film.kind = MediaKind::Video;
            film.duration_ms = None;
            add(c, &film)?;
            let q = |f: &dyn Fn(&mut CatalogueQuery)| -> Result<Vec<i64>> {
                let mut query = CatalogueQuery {
                    limit: 50,
                    ..Default::default()
                };
                f(&mut query);
                Ok(catalogue(c, &query)?.0.iter().map(|i| i.id).collect())
            };
            assert_eq!(
                q(&|_| {})?,
                vec![blue],
                "child: no restricted, no copy-less"
            );
            assert_eq!(
                q(&|x| x.adult = true)?,
                vec![late, blue],
                "newest copy first"
            );
            assert_eq!(
                q(&|x| {
                    x.adult = true;
                    x.by_title = true
                })?,
                vec![late, blue]
            );
            assert_eq!(
                q(&|x| {
                    x.adult = true;
                    x.kind = Some(MediaKind::Video)
                })?,
                vec![late]
            );
            assert_eq!(q(&|x| x.q = Some("joni".into()))?, vec![blue]);
            assert_eq!(
                q(&|x| {
                    x.adult = true;
                    x.q = Some("0% L".into())
                })?,
                vec![late]
            );
            assert!(
                q(&|x| {
                    x.adult = true;
                    x.q = Some("%".into())
                })?
                .contains(&late)
            );
            assert!(
                q(&|x| {
                    x.adult = true;
                    x.q = Some("_".into())
                })?
                .contains(&late)
            );
            assert_eq!(
                q(&|x| x.q = Some("x%".into()))?,
                Vec::<i64>::new(),
                "% is literal"
            );
            assert_eq!(
                q(&|x| x.type_slug = Some("film".into()))?,
                Vec::<i64>::new()
            );
            let (rows, total) = catalogue(
                c,
                &CatalogueQuery {
                    adult: true,
                    limit: 1,
                    ..Default::default()
                },
            )?;
            assert_eq!((rows.len(), total), (1, 2));
            assert_eq!(rows[0].kinds, vec![MediaKind::Video]);
            assert_eq!(rows[0].total_duration_ms, None);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn restricted_files_are_hidden_from_children() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let mut inp = input(c, "Late film", "");
            inp.restricted = true;
            let item = items::create(c, &inp)?;
            let m = add(c, &new_file(item, "Film", None))?;
            assert!(visible_by_ids(c, &[m], false)?.is_empty());
            assert!(visible_by_ids(c, &[m], true)?.contains_key(&m));
            assert!(!get(c, m)?.unwrap().visible_to(false));
            Ok(())
        })
        .unwrap();
    }
}
