//! Photo records. The image files themselves live in `<data_dir>/photos`.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use super::now;
use crate::util;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Photo {
    pub id: i64,
    pub item_id: i64,
    pub file: String,
    pub width: i64,
    pub height: i64,
    pub caption: String,
}

pub fn add(conn: &Connection, item_id: i64, file: &str, width: u32, height: u32, caption: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO photos (item_id, file, width, height, caption, created_at, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6,
                 (SELECT coalesce(max(position), 0) + 1 FROM photos WHERE item_id = ?1))",
        params![item_id, file, width, height, util::clean(caption, 120), now()],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn for_item(conn: &Connection, item_id: i64) -> Result<Vec<Photo>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, item_id, file, width, height, caption FROM photos
         WHERE item_id = ?1 ORDER BY position, id",
    )?;
    let rows = stmt.query_map([item_id], |r| {
        Ok(Photo {
            id: r.get(0)?,
            item_id: r.get(1)?,
            file: r.get(2)?,
            width: r.get(3)?,
            height: r.get(4)?,
            caption: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Removes a photo record, returning its file name so the caller can delete it.
pub fn remove(conn: &Connection, item_id: i64, photo_id: i64) -> Result<Option<String>> {
    let file: Option<String> = conn
        .query_row(
            "SELECT file FROM photos WHERE id = ?1 AND item_id = ?2",
            params![photo_id, item_id],
            |r| r.get(0),
        )
        .optional()?;
    if file.is_some() {
        conn.execute("DELETE FROM photos WHERE id = ?1", [photo_id])?;
    }
    Ok(file)
}

/// Moves a photo to the front, making it the one shown in results.
pub fn make_first(conn: &Connection, item_id: i64, photo_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE photos SET position = (SELECT coalesce(min(position), 0) - 1 FROM photos WHERE item_id = ?1)
         WHERE id = ?2 AND item_id = ?1",
        params![item_id, photo_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::items::{self, tests::input};
    use crate::db::testing::temp_db;

    #[test]
    fn add_list_reorder_remove() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let id = items::create(c, &input(c, "Tent", ""))?;
            let a = add(c, id, "a.jpg", 10, 20, "front")?;
            let b = add(c, id, "b.jpg", 10, 20, "")?;
            assert_eq!(for_item(c, id)?.iter().map(|p| p.id).collect::<Vec<_>>(), vec![a, b]);
            make_first(c, id, b)?;
            assert_eq!(for_item(c, id)?[0].id, b);
            assert_eq!(remove(c, id, a)?.as_deref(), Some("a.jpg"));
            assert_eq!(remove(c, id + 1, b)?, None, "photo must belong to the item");
            assert_eq!(for_item(c, id)?.len(), 1);
            Ok(())
        })
        .unwrap();
    }
}
