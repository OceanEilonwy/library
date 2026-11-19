//! Favourite media files, per account.

use anyhow::Result;
use rusqlite::{Connection, params};

use super::media_files::{self, MediaFile};
use super::now;

/// Marks a file as a favourite (doing it twice is fine).
pub fn add(conn: &Connection, account_id: i64, media_file_id: i64) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO favourites (account_id, media_file_id, created_at) VALUES (?1, ?2, ?3)",
        params![account_id, media_file_id, now()],
    )?;
    Ok(())
}

pub fn remove(conn: &Connection, account_id: i64, media_file_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM favourites WHERE account_id = ?1 AND media_file_id = ?2",
        params![account_id, media_file_id],
    )?;
    Ok(())
}

/// An account's favourites that it may still play, newest first.
pub fn list(conn: &Connection, account_id: i64, adult: bool) -> Result<Vec<MediaFile>> {
    let ids: Vec<i64> = conn
        .prepare_cached(
            "SELECT media_file_id FROM favourites WHERE account_id = ?1
             ORDER BY created_at DESC, media_file_id DESC",
        )?
        .query_map([account_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut files = media_files::visible_by_ids(conn, &ids, adult)?;
    Ok(ids.iter().filter_map(|id| files.remove(id)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::accounts::{self, Access, Status};
    use crate::db::items::{self, tests::input};
    use crate::db::media_files::tests::new_file;
    use crate::db::testing::temp_db;

    #[test]
    fn add_list_remove() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let acc = accounts::create(c, "kiddo", "k@x", "h", Access::Child, Status::Approved)?;
            let open = items::create(c, &input(c, "Songs", ""))?;
            let mut late = input(c, "Late", "");
            late.restricted = true;
            let late = items::create(c, &late)?;
            let a = media_files::add(c, &new_file(open, "A", Some(1)))?;
            let x = media_files::add(c, &new_file(late, "X", Some(1)))?;
            add(c, acc.id, a)?;
            add(c, acc.id, a)?;
            add(c, acc.id, x)?;
            assert_eq!(list(c, acc.id, false)?.len(), 1, "restricted left out");
            assert_eq!(list(c, acc.id, true)?.len(), 2);
            remove(c, acc.id, a)?;
            assert!(list(c, acc.id, false)?.is_empty());
            Ok(())
        })
        .unwrap();
    }
}
