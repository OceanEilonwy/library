//! Playlists: anyone with an account can make them, and everyone can see
//! them, but only the entries the viewer may play.
//!
//! The visibility rule (see docs/player.md): entries the viewer cannot play
//! are left out; a playlist left with no entries is hidden, except from its
//! owner. When the owner rewrites the entries, the ones hidden from them are
//! kept after the rest in their original order.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::items::Invalid;
use super::media_files::{self, MediaFile};
use super::now;
use crate::util;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistRow {
    pub id: i64,
    pub account_id: i64,
    pub owner: String,
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl PlaylistRow {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            account_id: r.get(1)?,
            owner: r.get(2)?,
            name: r.get(3)?,
            created_at: r.get(4)?,
            updated_at: r.get(5)?,
        })
    }
}

const SELECT: &str = "SELECT p.id, p.account_id, a.username, p.name, p.created_at, p.updated_at
    FROM playlists p JOIN accounts a ON a.id = p.account_id";

/// A playlist as one viewer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistView {
    pub row: PlaylistRow,
    pub mine: bool,
    /// Only the entries the viewer may play, in order.
    pub entries: Vec<MediaFile>,
}

/// Who is looking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Looker {
    pub account_id: i64,
    pub adult: bool,
}

fn clean_name(name: &str) -> Result<String> {
    let name = util::clean(name, 100);
    if name.is_empty() {
        let mut bad = Invalid::default();
        bad.add("name", "Give the playlist a name.");
        return Err(bad.into());
    }
    Ok(name)
}

pub fn create(conn: &Connection, account_id: i64, name: &str) -> Result<i64> {
    let name = clean_name(name)?;
    let ts = now();
    conn.execute(
        "INSERT INTO playlists (account_id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
        params![account_id, name, ts],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn row(conn: &Connection, id: i64) -> Result<Option<PlaylistRow>> {
    Ok(conn
        .prepare_cached(&format!("{SELECT} WHERE p.id = ?1"))?
        .query_row([id], PlaylistRow::from_row)
        .optional()?)
}

/// Entry media ids of a playlist, in order.
fn entry_ids(conn: &Connection, id: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT media_file_id FROM playlist_entries WHERE playlist_id = ?1 ORDER BY position",
    )?;
    let rows = stmt.query_map([id], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Applies the visibility rule to one playlist.
fn view_of(
    row: PlaylistRow,
    ids: &[i64],
    files: &HashMap<i64, MediaFile>,
    who: Looker,
) -> Option<PlaylistView> {
    let entries: Vec<MediaFile> = ids.iter().filter_map(|i| files.get(i).cloned()).collect();
    let mine = row.account_id == who.account_id;
    if entries.is_empty() && !mine {
        return None;
    }
    Some(PlaylistView { row, mine, entries })
}

/// One playlist, if the viewer may see it.
pub fn get_visible(conn: &Connection, id: i64, who: Looker) -> Result<Option<PlaylistView>> {
    let Some(row) = row(conn, id)? else {
        return Ok(None);
    };
    let ids = entry_ids(conn, id)?;
    let files = media_files::visible_by_ids(conn, &ids, who.adult)?;
    Ok(view_of(row, &ids, &files, who))
}

/// The playlists the viewer may see (or only their own), most recently
/// updated first.
pub fn list_visible(conn: &Connection, who: Looker, mine_only: bool) -> Result<Vec<PlaylistView>> {
    let rows: Vec<PlaylistRow> = conn
        .prepare_cached(&format!(
            "{SELECT} WHERE (?1 = 0 OR p.account_id = ?2) ORDER BY p.updated_at DESC, p.id DESC"
        ))?
        .query_map(params![mine_only, who.account_id], PlaylistRow::from_row)?
        .collect::<rusqlite::Result<_>>()?;
    let mut entries: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut stmt = conn.prepare_cached(
        "SELECT playlist_id, media_file_id FROM playlist_entries ORDER BY playlist_id, position",
    )?;
    for pair in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
        let (playlist, media) = pair?;
        entries.entry(playlist).or_default().push(media);
    }
    let all: Vec<i64> = entries
        .values()
        .flatten()
        .copied()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let files = media_files::visible_by_ids(conn, &all, who.adult)?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let ids = entries.get(&row.id).map(Vec::as_slice).unwrap_or(&[]);
            view_of(row, ids, &files, who)
        })
        .collect())
}

fn touch(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE playlists SET updated_at = ?2 WHERE id = ?1",
        params![id, now()],
    )?;
    Ok(())
}

pub fn rename(conn: &Connection, id: i64, name: &str) -> Result<()> {
    let name = clean_name(name)?;
    conn.execute(
        "UPDATE playlists SET name = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, name, now()],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM playlists WHERE id = ?1", [id])?;
    Ok(())
}

/// Adds files to the end. The caller has checked the owner may play them.
pub fn append(conn: &mut Connection, id: i64, media_ids: &[i64]) -> Result<()> {
    let tx = conn.transaction()?;
    let mut next: i64 = tx.query_row(
        "SELECT coalesce(max(position), 0) + 1 FROM playlist_entries WHERE playlist_id = ?1",
        [id],
        |r| r.get(0),
    )?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO playlist_entries (playlist_id, position, media_file_id) VALUES (?1, ?2, ?3)",
        )?;
        for m in media_ids {
            stmt.execute(params![id, next, m])?;
            next += 1;
        }
    }
    touch(&tx, id)?;
    tx.commit()?;
    Ok(())
}

/// Replaces the entries the owner can see with `visible` (in that order);
/// entries hidden from them stay, after these, in their original order.
/// The caller has checked the owner may play everything in `visible`.
pub fn replace_visible(conn: &mut Connection, id: i64, visible: &[i64], adult: bool) -> Result<()> {
    let tx = conn.transaction()?;
    let current = entry_ids(&tx, id)?;
    let playable = media_files::visible_by_ids(&tx, &current, adult)?;
    let hidden: Vec<i64> = current
        .into_iter()
        .filter(|m| !playable.contains_key(m))
        .collect();
    tx.execute("DELETE FROM playlist_entries WHERE playlist_id = ?1", [id])?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO playlist_entries (playlist_id, position, media_file_id) VALUES (?1, ?2, ?3)",
        )?;
        for (pos, m) in visible.iter().chain(hidden.iter()).enumerate() {
            stmt.execute(params![id, i64::try_from(pos + 1)?, m])?;
        }
    }
    touch(&tx, id)?;
    tx.commit()?;
    Ok(())
}

/// Every entry id, hidden ones included (for tests and the owner's checks).
#[cfg(test)]
pub fn all_entry_ids(conn: &Connection, id: i64) -> Result<Vec<i64>> {
    entry_ids(conn, id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::accounts::{self, Access};
    use crate::db::items::{self, tests::input};
    use crate::db::media_files::tests::new_file;
    use crate::db::testing::temp_db;

    #[test]
    fn visibility_rule_and_hidden_entries_survive_rewrites() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let adult = accounts::create(c, "grownup", "g@x", "h", Access::Adult)?;
            let child = accounts::create(c, "kiddo", "k@x", "h", Access::Child)?;
            let open = items::create(c, &input(c, "Songs", ""))?;
            let mut late = input(c, "Late", "");
            late.restricted = true;
            let late = items::create(c, &late)?;
            let a = media_files::add(c, &new_file(open, "A", Some(1)))?;
            let b = media_files::add(c, &new_file(open, "B", Some(2)))?;
            let x = media_files::add(c, &new_file(late, "X", Some(1)))?;
            let as_adult = Looker {
                account_id: adult.id,
                adult: true,
            };
            let as_child = Looker {
                account_id: child.id,
                adult: false,
            };

            // An adult's playlist mixing both.
            let mixed = create(c, adult.id, "Mixed")?;
            append(c, mixed, &[x, a, b])?;
            let v = get_visible(c, mixed, as_child)?.unwrap();
            assert_eq!(
                v.entries.iter().map(|m| m.id).collect::<Vec<_>>(),
                vec![a, b]
            );
            assert!(!v.mine);

            // Only restricted entries: hidden from the child entirely.
            let late_only = create(c, adult.id, "Late only")?;
            append(c, late_only, &[x])?;
            assert!(get_visible(c, late_only, as_child)?.is_none());
            assert!(get_visible(c, late_only, as_adult)?.is_some());

            // Empty playlists: hidden from others, shown to the owner.
            let empty = create(c, child.id, "Empty")?;
            assert!(get_visible(c, empty, as_adult)?.is_none());
            assert!(get_visible(c, empty, as_child)?.unwrap().mine);
            let names = |who, mine| -> Result<Vec<String>> {
                Ok(list_visible(c, who, mine)?
                    .into_iter()
                    .map(|v| v.row.name)
                    .collect())
            };
            let mut seen = names(as_child, false)?;
            seen.sort();
            assert_eq!(seen, vec!["Empty", "Mixed"]);
            assert_eq!(names(as_child, true)?, vec!["Empty"]);
            assert_eq!(
                names(as_adult, false)?.len(),
                2,
                "not the child's empty one"
            );

            // A child-owned playlist that holds a restricted file (e.g. the
            // child was demoted): rewriting keeps the hidden entry at the end.
            let own = create(c, child.id, "Own")?;
            append(c, own, &[a, x, b])?;
            replace_visible(c, own, &[b, a], false)?;
            assert_eq!(all_entry_ids(c, own)?, vec![b, a, x]);
            replace_visible(c, own, &[a], false)?;
            assert_eq!(all_entry_ids(c, own)?, vec![a, x]);

            rename(c, own, "Renamed")?;
            assert_eq!(row(c, own)?.unwrap().name, "Renamed");
            assert!(rename(c, own, "  ").is_err());
            delete(c, own)?;
            assert!(row(c, own)?.is_none());
            Ok(())
        })
        .unwrap();
    }
}
