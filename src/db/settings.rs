//! Small key/value store for things the librarian can change at runtime.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

pub const TITLE: &str = "library_title";
pub const TAGLINE: &str = "library_tagline";

pub fn get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()?)
}

pub fn set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [key, value],
    )?;
    Ok(())
}

/// The title and tagline shown in the masthead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branding {
    pub title: String,
    pub tagline: String,
}

pub fn branding(conn: &Connection, default_title: &str) -> Result<Branding> {
    Ok(Branding {
        title: get(conn, TITLE)?
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| default_title.to_owned()),
        tagline: get(conn, TAGLINE)?.unwrap_or_else(|| {
            "A catalogue of everything on the shelves, in the shed and in the loft".to_owned()
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testing::temp_db;

    #[test]
    fn set_overwrites_and_branding_falls_back() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            assert_eq!(branding(c, "Default")?.title, "Default");
            set(c, TITLE, "Tallow Lane")?;
            set(c, TITLE, "Tallow Lane Library")?;
            assert_eq!(branding(c, "Default")?.title, "Tallow Lane Library");
            set(c, TITLE, "   ")?;
            assert_eq!(branding(c, "Default")?.title, "Default");
            Ok(())
        })
        .unwrap();
    }
}
