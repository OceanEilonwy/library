//! Duplicate detection when adding an item. Never a hard block: it explains
//! what looks similar so the librarian can keep both (a second edition) or
//! go to the existing entry.

use anyhow::Result;
use rusqlite::Connection;

use crate::util;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Possible {
    pub id: i64,
    pub title: String,
    pub creator: String,
    pub year: Option<i64>,
    pub publisher: String,
    pub format: String,
    pub location: Option<String>,
    /// "same ISBN", "same title & author, different publisher"…
    pub why: String,
    pub strong: bool,
}

pub struct Probe<'a> {
    pub title: &'a str,
    pub creator: &'a str,
    pub identifier: &'a str,
    pub publisher: &'a str,
    pub year: Option<i64>,
    pub format: &'a str,
    /// The item being edited, which must not match itself.
    pub exclude: Option<i64>,
}

pub fn find(conn: &Connection, p: &Probe) -> Result<Vec<Possible>> {
    let title_key = util::title_key(p.title);
    let code = util::normalize_identifier(p.identifier);
    if title_key.is_empty() && code.len() < 8 {
        return Ok(Vec::new());
    }
    // Candidates: same identifier, or a title sharing its words (via FTS).
    let mut ids: Vec<i64> = Vec::new();
    if code.len() >= 8 {
        // ISBN-10 and ISBN-13 share a 9-digit core; match on it, then confirm.
        let thirteen = util::isbn13(&code);
        let core = if thirteen.len() == 13
            && (thirteen.starts_with("978") || thirteen.starts_with("979"))
        {
            thirteen[3..12].to_owned()
        } else {
            code.clone()
        };
        let mut stmt = conn.prepare_cached(
            "SELECT id, identifier FROM items WHERE identifier <> ''
             AND replace(replace(upper(identifier), '-', ''), ' ', '') LIKE '%' || ?1 || '%'",
        )?;
        for row in stmt.query_map([core], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })? {
            let (id, other) = row?;
            if util::isbn13(&util::normalize_identifier(&other)) == thirteen {
                ids.push(id);
            }
        }
    }
    let words: Vec<String> = title_key
        .split(' ')
        .filter(|w| w.len() > 1)
        .map(|w| format!("\"{w}\""))
        .collect();
    if !words.is_empty() {
        let expr = format!("{{title}} : ({})", words.join(" AND "));
        let mut stmt = conn.prepare_cached(
            "SELECT rowid FROM items_fts WHERE items_fts MATCH ?1 ORDER BY rank LIMIT 25",
        )?;
        if let Ok(rows) = stmt.query_map([expr], |r| r.get::<_, i64>(0)) {
            for id in rows.flatten() {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
    }
    let creator_key = util::normalize(p.creator);
    let mut out = Vec::new();
    let mut stmt = conn.prepare_cached(
        "SELECT i.id, i.title, i.creator, i.year, i.publisher, i.format, i.identifier, l.path
         FROM items i LEFT JOIN locations l ON l.id = i.location_id WHERE i.id = ?1",
    )?;
    for id in ids {
        if Some(id) == p.exclude {
            continue;
        }
        let row = stmt.query_row([id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        });
        let Ok((id, title, creator, year, publisher, format, identifier, location)) = row else {
            continue;
        };
        let same_code = code.len() >= 8 && {
            let other = util::normalize_identifier(&identifier);
            other == code || util::isbn13(&other) == util::isbn13(&code)
        };
        let t_sim = util::similarity(&util::title_key(&title), &title_key);
        let other_creator = util::normalize(&creator);
        let creator_match = creator_key.is_empty()
            || other_creator.is_empty()
            || util::similarity(&other_creator, &creator_key) > 0.8
            || other_creator.contains(&creator_key)
            || creator_key.contains(&other_creator);
        let why = if same_code {
            "same ISBN / barcode".to_owned()
        } else if t_sim >= 0.85 && creator_match {
            let mut diffs = Vec::new();
            if !p.publisher.trim().is_empty()
                && util::normalize(p.publisher) != util::normalize(&publisher)
                && !publisher.is_empty()
            {
                diffs.push("publisher");
            }
            if p.year.is_some() && year.is_some() && p.year != year {
                diffs.push("year");
            }
            if !p.format.trim().is_empty()
                && !format.is_empty()
                && util::normalize(p.format) != util::normalize(&format)
            {
                diffs.push("format");
            }
            let base = if creator_key.is_empty() || other_creator.is_empty() {
                "same title"
            } else {
                "same title & creator"
            };
            if diffs.is_empty() {
                base.to_owned()
            } else {
                format!("{base}, different {}", join_and(&diffs))
            }
        } else {
            continue;
        };
        out.push(Possible {
            id,
            title,
            creator,
            year,
            publisher,
            format,
            location,
            strong: same_code || why == "same title & creator" || why == "same title",
            why,
        });
    }
    out.sort_by_key(|p| !p.strong);
    out.truncate(5);
    Ok(out)
}

fn join_and(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [a] => (*a).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::items::{ItemInput, create, tests::book};
    use crate::db::testing::temp_db;

    fn probe<'a>(title: &'a str, creator: &'a str) -> Probe<'a> {
        Probe {
            title,
            creator,
            identifier: "",
            publisher: "",
            year: None,
            format: "",
            exclude: None,
        }
    }

    #[test]
    fn finds_same_title_and_explains_differences() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let id = create(
                c,
                &ItemInput {
                    type_id: book(c),
                    title: "The Hobbit".into(),
                    creator: "J. R. R. Tolkien".into(),
                    publisher: "Unwin".into(),
                    year: "1966".into(),
                    location: "Shed > Books Box 1".into(),
                    ..Default::default()
                },
            )?;
            create(
                c,
                &ItemInput {
                    type_id: book(c),
                    title: "The Hobbit Companion".into(),
                    creator: "David Day".into(),
                    ..Default::default()
                },
            )?;
            let mut p = probe("Hobbit", "Tolkien");
            p.publisher = "HarperCollins";
            p.year = Some(1995);
            let found = find(c, &p)?;
            assert_eq!(found.len(), 1, "{found:?}");
            assert_eq!(found[0].id, id);
            assert_eq!(
                found[0].why,
                "same title & creator, different publisher and year"
            );
            assert_eq!(found[0].location.as_deref(), Some("Shed › Books Box 1"));
            // Editing the item itself is not a duplicate of itself.
            p.exclude = Some(id);
            assert!(find(c, &p)?.is_empty());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn same_isbn_in_either_form() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            create(
                c,
                &ItemInput {
                    type_id: book(c),
                    title: "Different Title".into(),
                    identifier: "0-261-10295-8".into(),
                    ..Default::default()
                },
            )?;
            let mut p = probe("Whatever", "");
            p.identifier = "9780261102958";
            let found = find(c, &p)?;
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].why, "same ISBN / barcode");
            assert!(found[0].strong);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn different_creator_is_not_a_duplicate() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            create(
                c,
                &ItemInput {
                    type_id: book(c),
                    title: "Blue".into(),
                    creator: "Joni Mitchell".into(),
                    ..Default::default()
                },
            )?;
            assert!(find(c, &probe("Blue", "Maggie Nelson"))?.is_empty());
            assert_eq!(find(c, &probe("Blue", ""))?.len(), 1);
            assert!(find(c, &probe("", ""))?.is_empty());
            assert!(find(c, &probe("\"", "x"))?.is_empty());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn join_words() {
        assert_eq!(join_and(&["a"]), "a");
        assert_eq!(join_and(&["a", "b", "c"]), "a, b and c");
    }
}
