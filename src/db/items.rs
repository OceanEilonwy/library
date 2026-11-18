//! Catalogue items: create, update, read, and keep the full-text index in step.

use std::collections::HashMap;
use std::fmt;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use super::types::{self, FieldDef};
use super::{locations, now};
use crate::util;

/// Validation problems, keyed by form field name.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Invalid(pub Vec<(String, String)>);

impl Invalid {
    pub fn add(&mut self, field: &str, msg: impl Into<String>) {
        self.0.push((field.to_owned(), msg.into()));
    }
    pub fn get(&self, field: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(f, _)| f == field)
            .map(|(_, m)| m.as_str())
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msgs: Vec<&str> = self.0.iter().map(|(_, m)| m.as_str()).collect();
        write!(f, "{}", msgs.join(" "))
    }
}

impl std::error::Error for Invalid {}

/// What the add/edit form submits.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ItemInput {
    pub type_id: i64,
    pub title: String,
    pub creator: String,
    pub year: String,
    pub publisher: String,
    pub format: String,
    pub identifier: String,
    pub description: String,
    pub tags: String,
    pub notes: String,
    pub location: String,
    /// Raw values for user-defined fields, by field id.
    pub fields: Vec<(i64, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: i64,
    pub type_id: i64,
    pub type_name: String,
    pub type_slug: String,
    pub creator_label: String,
    pub title: String,
    pub creator: String,
    pub year: Option<i64>,
    pub publisher: String,
    pub format: String,
    pub identifier: String,
    pub description: String,
    pub tags: String,
    pub notes: String,
    pub location_id: Option<i64>,
    pub location_path: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Item {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            type_id: r.get(1)?,
            type_name: r.get(2)?,
            type_slug: r.get(3)?,
            creator_label: r.get(4)?,
            title: r.get(5)?,
            creator: r.get(6)?,
            year: r.get(7)?,
            publisher: r.get(8)?,
            format: r.get(9)?,
            identifier: r.get(10)?,
            description: r.get(11)?,
            tags: r.get(12)?,
            notes: r.get(13)?,
            location_id: r.get(14)?,
            location_path: r.get(15)?,
            created_at: r.get(16)?,
            updated_at: r.get(17)?,
        })
    }

    /// The form contents that would recreate this item.
    pub fn to_input(&self, fields: &[(FieldDef, String)]) -> ItemInput {
        ItemInput {
            type_id: self.type_id,
            title: self.title.clone(),
            creator: self.creator.clone(),
            year: self.year.map(|y| y.to_string()).unwrap_or_default(),
            publisher: self.publisher.clone(),
            format: self.format.clone(),
            identifier: self.identifier.clone(),
            description: self.description.clone(),
            tags: self.tags.clone(),
            notes: self.notes.clone(),
            location: self.location_path.clone().unwrap_or_default(),
            fields: fields.iter().map(|(d, v)| (d.id, v.clone())).collect(),
        }
    }
}

const ITEM_SELECT: &str =
    "SELECT i.id, i.type_id, t.name, t.slug, t.creator_label, i.title, i.creator, i.year,
        i.publisher, i.format, i.identifier, i.description, i.tags, i.notes, i.location_id,
        l.path, i.created_at, i.updated_at
    FROM items i JOIN media_types t ON t.id = i.type_id
    LEFT JOIN locations l ON l.id = i.location_id";

/// An item as shown in a result list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub id: i64,
    pub title: String,
    pub creator: String,
    pub year: Option<i64>,
    pub format: String,
    pub type_name: String,
    pub type_slug: String,
    pub creator_label: String,
    pub tags: String,
    pub created_at: i64,
    pub location_path: Option<String>,
    pub loan: Option<ActiveLoan>,
    pub photo_count: i64,
    pub first_photo: Option<String>,
    pub first_photo_caption: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveLoan {
    pub borrower: String,
    pub since: i64,
}

struct Valid {
    title: String,
    creator: String,
    year: Option<i64>,
    publisher: String,
    format: String,
    identifier: String,
    description: String,
    tags: String,
    notes: String,
    fields: Vec<(i64, String)>,
}

fn validate(conn: &Connection, input: &ItemInput, item_id: Option<i64>) -> Result<Valid> {
    let mut bad = Invalid::default();
    let title = util::clean(&input.title, 300);
    if title.is_empty() {
        bad.add("title", "Every item needs a title.");
    }
    if types::get(conn, input.type_id)?.is_none() {
        bad.add("type_id", "Choose what kind of thing this is.");
    }
    let year = match input.year.trim() {
        "" => None,
        y => match y.parse::<i64>() {
            Ok(n) if (0..=3000).contains(&n) => Some(n),
            _ => {
                bad.add("year", "The year should be a number like 1971.");
                None
            }
        },
    };
    let defs = types::fields_for(conn, input.type_id, item_id)?;
    let mut fields = Vec::new();
    for (field_id, raw) in &input.fields {
        let Some(def) = defs.iter().find(|d| d.id == *field_id) else {
            continue;
        };
        match def.kind.validate(raw, &def.options) {
            Ok(Some(v)) => fields.push((def.id, v)),
            Ok(None) => {}
            Err(e) => bad.add(&format!("field_{}", def.id), e.to_string()),
        }
    }
    if !bad.is_empty() {
        return Err(bad.into());
    }
    Ok(Valid {
        title,
        creator: util::clean(&input.creator, 200),
        year,
        publisher: util::clean(&input.publisher, 200),
        format: util::clean(&input.format, 200),
        identifier: util::clean(&input.identifier, 40),
        description: util::clean(&input.description, 4000),
        tags: util::clean(&input.tags, 500),
        notes: util::clean(&input.notes, 4000),
        fields,
    })
}

pub fn create(conn: &mut Connection, input: &ItemInput) -> Result<i64> {
    let tx = conn.transaction()?;
    let v = validate(&tx, input, None)?;
    let location = locations::ensure_path(&tx, &input.location)?;
    let ts = now();
    tx.execute(
        "INSERT INTO items (type_id, title, creator, year, publisher, format, identifier,
                            description, tags, notes, location_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
        params![
            input.type_id,
            v.title,
            v.creator,
            v.year,
            v.publisher,
            v.format,
            v.identifier,
            v.description,
            v.tags,
            v.notes,
            location,
            ts
        ],
    )?;
    let id = tx.last_insert_rowid();
    write_fields(&tx, id, &v.fields, true)?;
    sync_fts(&tx, id)?;
    tx.commit()?;
    Ok(id)
}

pub fn update(conn: &mut Connection, id: i64, input: &ItemInput) -> Result<()> {
    let tx = conn.transaction()?;
    let v = validate(&tx, input, Some(id))?;
    let location = locations::ensure_path(&tx, &input.location)?;
    let changed = tx.execute(
        "UPDATE items SET type_id = ?2, title = ?3, creator = ?4, year = ?5, publisher = ?6,
                format = ?7, identifier = ?8, description = ?9, tags = ?10, notes = ?11,
                location_id = ?12, updated_at = ?13, embed_hash = NULL
         WHERE id = ?1",
        params![
            id,
            input.type_id,
            v.title,
            v.creator,
            v.year,
            v.publisher,
            v.format,
            v.identifier,
            v.description,
            v.tags,
            v.notes,
            location,
            now()
        ],
    )?;
    if changed == 0 {
        anyhow::bail!("No such item.");
    }
    write_fields(&tx, id, &v.fields, false)?;
    sync_fts(&tx, id)?;
    tx.commit()?;
    Ok(())
}

fn write_fields(tx: &Transaction, id: i64, fields: &[(i64, String)], fresh: bool) -> Result<()> {
    if !fresh {
        // Values for fields that are no longer submitted (cleared, or the
        // item changed type) are removed; item-only field defs stay.
        tx.execute("DELETE FROM item_fields WHERE item_id = ?1", [id])?;
    }
    let mut stmt = tx
        .prepare_cached("INSERT INTO item_fields (item_id, field_id, value) VALUES (?1, ?2, ?3)")?;
    for (field, value) in fields {
        stmt.execute(params![id, field, value])?;
    }
    Ok(())
}

/// Rewrites the full-text row for an item from the current data.
pub fn sync_fts(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM items_fts WHERE rowid = ?1", [id])?;
    conn.execute(
        "INSERT INTO items_fts (rowid, title, creator, publisher, description, tags, notes, identifier, extra)
         SELECT i.id, i.title, i.creator, i.publisher, i.description, i.tags, i.notes,
                i.identifier || ' ' || replace(i.identifier, '-', ''),
                coalesce((SELECT group_concat(d.name || ' ' || f.value, ' ')
                          FROM item_fields f JOIN field_defs d ON d.id = f.field_id
                          WHERE f.item_id = i.id), '') || ' ' || coalesce(i.year, '') || ' ' || i.format
                || ' ' || t.name || ' ' || t.section
         FROM items i JOIN media_types t ON t.id = i.type_id WHERE i.id = ?1",
        [id],
    )?;
    Ok(())
}

/// Deletes an item; returns the photo files that should be removed from disk.
pub fn delete(conn: &mut Connection, id: i64) -> Result<Vec<String>> {
    let tx = conn.transaction()?;
    let files: Vec<String> = tx
        .prepare("SELECT file FROM photos WHERE item_id = ?1")?
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    tx.execute("DELETE FROM items_fts WHERE rowid = ?1", [id])?;
    if super::vector_table_exists(&tx)? {
        tx.execute("DELETE FROM item_vectors WHERE rowid = ?1", [id])?;
    }
    tx.execute("DELETE FROM field_defs WHERE item_id = ?1", [id])?;
    tx.execute("DELETE FROM items WHERE id = ?1", [id])?;
    tx.commit()?;
    Ok(files)
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<Item>> {
    Ok(conn
        .prepare_cached(&format!("{ITEM_SELECT} WHERE i.id = ?1"))?
        .query_row([id], Item::from_row)
        .optional()?)
}

/// User-defined field values for an item, with their definitions.
pub fn field_values(conn: &Connection, id: i64) -> Result<Vec<(FieldDef, String)>> {
    let Some(item) = get(conn, id)? else {
        return Ok(Vec::new());
    };
    let defs = types::fields_for(conn, item.type_id, Some(id))?;
    let values: HashMap<i64, String> = conn
        .prepare_cached("SELECT field_id, value FROM item_fields WHERE item_id = ?1")?
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(defs
        .into_iter()
        .map(|d| {
            let v = values.get(&d.id).cloned().unwrap_or_default();
            (d, v)
        })
        .collect())
}

const SUMMARY_SELECT: &str = "SELECT i.id, i.title, i.creator, i.year, i.format, t.name, t.slug, l.path,
        ln.borrower, ln.loaned_at,
        (SELECT count(*) FROM photos p WHERE p.item_id = i.id),
        (SELECT p.file FROM photos p WHERE p.item_id = i.id ORDER BY p.position, p.id LIMIT 1),
        coalesce((SELECT p.caption FROM photos p WHERE p.item_id = i.id ORDER BY p.position, p.id LIMIT 1), ''),
        t.creator_label, i.tags, i.created_at
    FROM items i JOIN media_types t ON t.id = i.type_id
    LEFT JOIN locations l ON l.id = i.location_id
    LEFT JOIN loans ln ON ln.item_id = i.id AND ln.returned_at IS NULL";

fn summary_from_row(r: &Row) -> rusqlite::Result<Summary> {
    let borrower: Option<String> = r.get(8)?;
    let since: Option<i64> = r.get(9)?;
    Ok(Summary {
        id: r.get(0)?,
        title: r.get(1)?,
        creator: r.get(2)?,
        year: r.get(3)?,
        format: r.get(4)?,
        type_name: r.get(5)?,
        type_slug: r.get(6)?,
        creator_label: r.get(13)?,
        tags: r.get(14)?,
        created_at: r.get(15)?,
        location_path: r.get(7)?,
        loan: borrower
            .zip(since)
            .map(|(borrower, since)| ActiveLoan { borrower, since }),
        photo_count: r.get(10)?,
        first_photo: r.get(11)?,
        first_photo_caption: r.get(12)?,
    })
}

/// Summaries for the given ids, in the same order (missing ids are skipped).
pub fn summaries(conn: &Connection, ids: &[i64]) -> Result<Vec<Summary>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let json = serde_json::to_string(ids)?;
    let mut stmt = conn.prepare_cached(&format!(
        "{SUMMARY_SELECT} WHERE i.id IN (SELECT value FROM json_each(?1))"
    ))?;
    let mut by_id: HashMap<i64, Summary> = stmt
        .query_map([json], summary_from_row)?
        .map(|r| r.map(|s| (s.id, s)))
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

pub fn summary(conn: &Connection, id: i64) -> Result<Option<Summary>> {
    Ok(summaries(conn, &[id])?.pop())
}

pub fn count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT count(*) FROM items", [], |r| r.get(0))?)
}

/// The text fed to the embedding model for an item.
pub fn embedding_text(conn: &Connection, id: i64) -> Result<Option<String>> {
    let Some(item) = get(conn, id)? else {
        return Ok(None);
    };
    let fields = field_values(conn, id)?;
    let mut parts = vec![item.title.clone()];
    if !item.creator.is_empty() {
        parts.push(format!("{}: {}", item.creator_label, item.creator));
    }
    parts.push(item.type_name.replace('·', " "));
    for s in [&item.description, &item.tags, &item.format, &item.publisher] {
        if !s.is_empty() {
            parts.push(s.clone());
        }
    }
    if let Some(y) = item.year {
        parts.push(y.to_string());
    }
    for (def, v) in fields {
        if !v.is_empty() {
            parts.push(format!("{}: {v}", def.name));
        }
    }
    Ok(Some(parts.join(". ")))
}

/// Items still waiting to be embedded.
pub fn needing_embedding(conn: &Connection, limit: usize) -> Result<Vec<i64>> {
    let mut stmt =
        conn.prepare_cached("SELECT id FROM items WHERE embed_hash IS NULL ORDER BY id LIMIT ?1")?;
    let rows = stmt.query_map([limit as i64], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::db::testing::temp_db;
    use crate::db::types::{FieldKind, FieldScope};

    pub fn book(conn: &Connection) -> i64 {
        types::by_slug(conn, "book").unwrap().unwrap().id
    }

    pub fn input(conn: &Connection, title: &str, creator: &str) -> ItemInput {
        ItemInput {
            type_id: book(conn),
            title: title.into(),
            creator: creator.into(),
            ..Default::default()
        }
    }

    #[test]
    fn create_get_update_delete() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let mut inp = input(c, "The Hobbit", "J. R. R. Tolkien");
            inp.year = "1995".into();
            inp.location = "Lounge > Bookcase 1 > Shelf 4".into();
            inp.tags = "fantasy, dragons".into();
            let id = create(c, &inp)?;
            let item = get(c, id)?.unwrap();
            assert_eq!(item.title, "The Hobbit");
            assert_eq!(item.year, Some(1995));
            assert_eq!(
                item.location_path.as_deref(),
                Some("Lounge › Bookcase 1 › Shelf 4")
            );
            assert_eq!(item.creator_label, "Author");

            let hits: i64 = c.query_row(
                "SELECT count(*) FROM items_fts WHERE items_fts MATCH 'dragons'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(hits, 1);

            inp.title = "The Hobbit (illustrated)".into();
            inp.tags = "fantasy".into();
            update(c, id, &inp)?;
            let hits: i64 = c.query_row(
                "SELECT count(*) FROM items_fts WHERE items_fts MATCH 'dragons'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(hits, 0, "fts follows updates");
            assert_eq!(get(c, id)?.unwrap().title, "The Hobbit (illustrated)");

            delete(c, id)?;
            assert!(get(c, id)?.is_none());
            let rows: i64 = c.query_row("SELECT count(*) FROM items_fts", [], |r| r.get(0))?;
            assert_eq!(rows, 0);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn validation_reports_each_problem() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let mut inp = input(c, "  ", "");
            inp.year = "nineteen".into();
            let err = create(c, &inp).unwrap_err();
            let bad = err.downcast_ref::<Invalid>().expect("Invalid");
            assert!(bad.get("title").is_some());
            assert!(bad.get("year").is_some());
            inp.type_id = 999;
            inp.title = "x".into();
            inp.year = "".into();
            let err = create(c, &inp).unwrap_err();
            assert!(
                err.downcast_ref::<Invalid>()
                    .unwrap()
                    .get("type_id")
                    .is_some()
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn custom_fields_are_validated_stored_and_indexed() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let b = book(c);
            let series =
                types::create_field(c, FieldScope::Type(b), "Series", FieldKind::Text, "")?;
            let signed =
                types::create_field(c, FieldScope::Type(b), "Signed", FieldKind::Bool, "")?;
            let pages =
                types::create_field(c, FieldScope::Type(b), "Pages", FieldKind::Number, "")?;
            let mut inp = input(c, "Mort", "Terry Pratchett");
            inp.fields = vec![
                (series.id, "Discworld".into()),
                (signed.id, "on".into()),
                (pages.id, "".into()),
            ];
            let id = create(c, &inp)?;
            let vals = field_values(c, id)?;
            assert_eq!(vals.len(), 3);
            assert_eq!(
                vals.iter().find(|(d, _)| d.id == series.id).unwrap().1,
                "Discworld"
            );
            assert_eq!(
                vals.iter().find(|(d, _)| d.id == signed.id).unwrap().1,
                "yes"
            );
            let hits: i64 = c.query_row(
                "SELECT count(*) FROM items_fts WHERE items_fts MATCH 'discworld'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(hits, 1);

            inp.fields = vec![(pages.id, "many".into())];
            let err = update(c, id, &inp).unwrap_err();
            assert!(
                err.downcast_ref::<Invalid>()
                    .unwrap()
                    .get(&format!("field_{}", pages.id))
                    .is_some()
            );

            // Item-only field.
            let only =
                types::create_field(c, FieldScope::Item(id), "Inscription", FieldKind::Text, "")?;
            inp.fields = vec![(only.id, "To Kasia".into())];
            update(c, id, &inp)?;
            let vals = field_values(c, id)?;
            assert_eq!(
                vals.iter().find(|(d, _)| d.id == only.id).unwrap().1,
                "To Kasia"
            );
            assert!(
                types::fields_for(c, b, None)?
                    .iter()
                    .all(|d| d.id != only.id)
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn summaries_keep_order_and_show_loans_and_photos() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let a = create(c, &input(c, "A", ""))?;
            let b = create(c, &input(c, "B", ""))?;
            c.execute("INSERT INTO loans (item_id, borrower, loaned_at) VALUES (?1, 'Sam', 100)", [a])?;
            c.execute(
                "INSERT INTO photos (item_id, file, width, height, created_at, caption) VALUES (?1, 'x.jpg', 1, 1, 0, 'Sleeve')",
                [b],
            )?;
            let s = summaries(c, &[b, 999, a])?;
            assert_eq!(s.iter().map(|s| s.id).collect::<Vec<_>>(), vec![b, a]);
            assert_eq!(s[1].loan.as_ref().unwrap().borrower, "Sam");
            assert_eq!(s[0].photo_count, 1);
            assert_eq!(s[0].first_photo.as_deref(), Some("x.jpg"));
            assert_eq!(s[0].first_photo_caption, "Sleeve");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn embedding_text_and_queue() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let mut inp = input(c, "Blue", "Joni Mitchell");
            inp.description = "confessional folk".into();
            let id = create(c, &inp)?;
            let text = embedding_text(c, id)?.unwrap();
            assert!(
                text.contains("Blue")
                    && text.contains("Author: Joni Mitchell")
                    && text.contains("folk")
            );
            assert_eq!(needing_embedding(c, 10)?, vec![id]);
            Ok(())
        })
        .unwrap();
    }
}
