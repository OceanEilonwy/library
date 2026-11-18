//! The tree of places things live: "Shed › Camping shelf › Crate 1".

use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::util;

pub const SEP: &str = " › ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub path: String,
}

impl Location {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            parent_id: r.get(1)?,
            name: r.get(2)?,
            path: r.get(3)?,
        })
    }
}

/// Splits user input such as "Shed > Books Box 1", "Shed / Box" or
/// "Shed › Box" into clean segments.
pub fn split_path(input: &str) -> Vec<String> {
    input
        .split(['›', '>', '/', '\\'])
        .map(|s| util::clean(s, 60))
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn list(conn: &Connection) -> Result<Vec<Location>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, parent_id, name, path FROM locations ORDER BY path COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], Location::from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<Location>> {
    Ok(conn
        .prepare_cached("SELECT id, parent_id, name, path FROM locations WHERE id = ?1")?
        .query_row([id], Location::from_row)
        .optional()?)
}

#[cfg(test)]
pub fn children(conn: &Connection, parent: Option<i64>) -> Result<Vec<Location>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, parent_id, name, path FROM locations
         WHERE parent_id IS ?1 ORDER BY name COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([parent], Location::from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Finds the location for a typed path, creating any missing parts.
/// Returns `None` for an empty path. Matching is case-insensitive, so
/// "shed › books box 1" finds "Shed › Books Box 1".
pub fn ensure_path(conn: &Connection, input: &str) -> Result<Option<i64>> {
    let segments = split_path(input);
    if segments.is_empty() {
        return Ok(None);
    }
    if segments.len() > 8 {
        bail!("A location can be at most 8 levels deep.");
    }
    let mut parent: Option<i64> = None;
    let mut path = String::new();
    for seg in segments {
        let found: Option<(i64, String)> = conn
            .prepare_cached(
                "SELECT id, path FROM locations
                 WHERE parent_id IS ?1 AND name = ?2 COLLATE NOCASE",
            )?
            .query_row(params![parent, seg], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let id = match found {
            Some((id, p)) => {
                path = p;
                id
            }
            None => {
                path = if path.is_empty() {
                    seg.clone()
                } else {
                    format!("{path}{SEP}{seg}")
                };
                conn.execute(
                    "INSERT INTO locations (parent_id, name, path) VALUES (?1, ?2, ?3)",
                    params![parent, seg, path],
                )?;
                conn.last_insert_rowid()
            }
        };
        parent = Some(id);
    }
    Ok(parent)
}

/// Renames a location and rewrites the stored paths of it and everything
/// beneath it.
pub fn rename(conn: &Connection, id: i64, new_name: &str) -> Result<()> {
    let name = util::clean(&new_name.replace(['›', '>', '/'], " "), 60);
    if name.is_empty() {
        bail!("A place needs a name.");
    }
    let Some(loc) = get(conn, id)? else {
        bail!("No such place.")
    };
    let clash: bool = conn.query_row(
        "SELECT count(*) > 0 FROM locations
         WHERE parent_id IS ?1 AND name = ?2 COLLATE NOCASE AND id <> ?3",
        params![loc.parent_id, name, id],
        |r| r.get(0),
    )?;
    if clash {
        bail!("There is already a place called “{name}” there.");
    }
    let new_path = match loc.parent_id.and_then(|p| get(conn, p).ok().flatten()) {
        Some(parent) => format!("{}{SEP}{name}", parent.path),
        None => name.clone(),
    };
    conn.execute(
        "UPDATE locations SET name = ?1 WHERE id = ?2",
        params![name, id],
    )?;
    repath_subtree(conn, id, &loc.path, &new_path)?;
    Ok(())
}

fn repath_subtree(conn: &Connection, id: i64, old: &str, new: &str) -> Result<()> {
    conn.execute(
        "WITH RECURSIVE sub(id) AS (
             SELECT ?1 UNION ALL SELECT l.id FROM locations l JOIN sub ON l.parent_id = sub.id
         )
         UPDATE locations SET path = ?3 || substr(path, length(?2) + 1)
         WHERE id IN (SELECT id FROM sub)",
        params![id, old, new],
    )?;
    Ok(())
}

/// Deletes a place that holds no items and no other places.
pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    let (kids, items): (i64, i64) = conn.query_row(
        "SELECT (SELECT count(*) FROM locations WHERE parent_id = ?1),
                (SELECT count(*) FROM items WHERE location_id = ?1)",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if kids > 0 || items > 0 {
        bail!(
            "Only empty places can be removed: this one holds {items} item(s) and {kids} place(s)."
        );
    }
    conn.execute("DELETE FROM locations WHERE id = ?1", [id])?;
    Ok(())
}

/// Item counts per location, including everything in nested places.
pub fn counts(conn: &Connection) -> Result<Vec<(Location, i64)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT l.id, l.parent_id, l.name, l.path,
                (SELECT count(*) FROM items i JOIN locations d ON d.id = i.location_id
                 WHERE d.id = l.id OR d.path LIKE l.path || ' › %')
         FROM locations l ORDER BY l.path COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], |r| Ok((Location::from_row(r)?, r.get(4)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testing::temp_db;

    #[test]
    fn split_accepts_several_separators() {
        assert_eq!(
            split_path("Shed > Books Box 1"),
            vec!["Shed", "Books Box 1"]
        );
        assert_eq!(
            split_path("Lounge › CD rack / Row 2"),
            vec!["Lounge", "CD rack", "Row 2"]
        );
        assert!(split_path("  › ").is_empty());
    }

    #[test]
    fn ensure_path_reuses_and_creates() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let a = ensure_path(c, "Shed > Books Box 1")?.unwrap();
            let b = ensure_path(c, "shed › books box 1")?.unwrap();
            assert_eq!(a, b);
            let crate1 = ensure_path(c, "Shed > Camping shelf > Crate 1")?.unwrap();
            assert_eq!(
                get(c, crate1)?.unwrap().path,
                "Shed › Camping shelf › Crate 1"
            );
            assert_eq!(list(c)?.len(), 4);
            assert_eq!(ensure_path(c, "")?, None);
            assert_eq!(children(c, None)?.len(), 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn rename_rewrites_descendant_paths() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let deep = ensure_path(c, "Lounge > Bookcase 1 > Shelf 2")?.unwrap();
            ensure_path(c, "Lounge > Bookcase 10")?;
            let bc = get(c, get(c, deep)?.unwrap().parent_id.unwrap())?.unwrap();
            rename(c, bc.id, "Oak bookcase")?;
            assert_eq!(
                get(c, deep)?.unwrap().path,
                "Lounge › Oak bookcase › Shelf 2"
            );
            let paths: Vec<String> = list(c)?.into_iter().map(|l| l.path).collect();
            assert!(
                paths.contains(&"Lounge › Bookcase 10".to_owned()),
                "sibling with shared prefix untouched"
            );
            assert!(rename(c, bc.id, "bookcase 10").is_err());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn delete_only_when_empty() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let leaf = ensure_path(c, "Loft > Toy box")?.unwrap();
            let loft = get(c, leaf)?.unwrap().parent_id.unwrap();
            assert!(delete(c, loft).is_err());
            delete(c, leaf)?;
            delete(c, loft)?;
            assert!(list(c)?.is_empty());
            Ok(())
        })
        .unwrap();
    }
}
