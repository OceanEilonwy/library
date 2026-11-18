//! Media types ("Book", "Equipment · Camping") and user-defined fields.

use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::util;

#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub enum Lookup {
    Book,
    Music,
    Film,
    Product,
    None,
}

impl Lookup {
    pub fn as_str(self) -> &'static str {
        match self {
            Lookup::Book => "book",
            Lookup::Music => "music",
            Lookup::Film => "film",
            Lookup::Product => "product",
            Lookup::None => "none",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "book" => Lookup::Book,
            "music" => Lookup::Music,
            "film" => Lookup::Film,
            "product" => Lookup::Product,
            _ => Lookup::None,
        }
    }

    pub const ALL: [Lookup; 5] = [
        Lookup::Book,
        Lookup::Music,
        Lookup::Film,
        Lookup::Product,
        Lookup::None,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Lookup::Book => "Books (Open Library, Google Books)",
            Lookup::Music => "Music (MusicBrainz)",
            Lookup::Film => "Films (TMDb)",
            Lookup::Product => "Products (barcode lookup)",
            Lookup::None => "No online lookup",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaType {
    pub id: i64,
    pub name: String,
    pub slug: String,
    pub section: String,
    pub lookup: Lookup,
    pub creator_label: String,
}

impl MediaType {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            name: r.get(1)?,
            slug: r.get(2)?,
            section: r.get(3)?,
            lookup: Lookup::parse(&r.get::<_, String>(4)?),
            creator_label: r.get(5)?,
        })
    }
}

const COLS: &str = "id, name, slug, section, lookup, creator_label";

pub fn list(conn: &Connection) -> Result<Vec<MediaType>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLS} FROM media_types ORDER BY position, name"
    ))?;
    let rows = stmt.query_map([], MediaType::from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<MediaType>> {
    Ok(conn
        .prepare_cached(&format!("SELECT {COLS} FROM media_types WHERE id = ?1"))?
        .query_row([id], MediaType::from_row)
        .optional()?)
}

pub fn by_slug(conn: &Connection, slug: &str) -> Result<Option<MediaType>> {
    Ok(conn
        .prepare_cached(&format!("SELECT {COLS} FROM media_types WHERE slug = ?1"))?
        .query_row([slug], MediaType::from_row)
        .optional()?)
}

pub struct NewType<'a> {
    pub name: &'a str,
    pub section: &'a str,
    pub lookup: Lookup,
    pub creator_label: &'a str,
}

pub fn create(conn: &Connection, t: &NewType) -> Result<MediaType> {
    let name = util::clean(t.name, 60);
    if name.is_empty() {
        bail!("A type needs a name.");
    }
    let slug = util::slugify(&name);
    if slug.is_empty() {
        bail!("A type name needs at least one letter or digit.");
    }
    let section = match util::clean(t.section, 40) {
        s if s.is_empty() => name
            .rsplit_once('·')
            .map(|(_, s)| s.trim().to_owned())
            .unwrap_or_else(|| name.clone()),
        s => s,
    };
    let creator_label = match util::clean(t.creator_label, 30) {
        s if s.is_empty() => "Creator".to_owned(),
        s => s,
    };
    let exists: bool = conn.query_row(
        "SELECT count(*) > 0 FROM media_types WHERE name = ?1 OR slug = ?2",
        params![name, slug],
        |r| r.get(0),
    )?;
    if exists {
        bail!("There is already a type called “{name}”.");
    }
    let position: i64 = conn.query_row(
        "SELECT coalesce(max(position), 0) + 10 FROM media_types",
        [],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO media_types (name, slug, section, lookup, creator_label, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![name, slug, section, t.lookup.as_str(), creator_label, position],
    )?;
    Ok(get(conn, conn.last_insert_rowid())?.expect("just inserted"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Number,
    Date,
    Choice,
    Bool,
}

impl FieldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldKind::Text => "text",
            FieldKind::Number => "number",
            FieldKind::Date => "date",
            FieldKind::Choice => "choice",
            FieldKind::Bool => "bool",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "text" => FieldKind::Text,
            "number" => FieldKind::Number,
            "date" => FieldKind::Date,
            "choice" => FieldKind::Choice,
            "bool" => FieldKind::Bool,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            FieldKind::Text => "Text",
            FieldKind::Number => "Number",
            FieldKind::Date => "Date",
            FieldKind::Choice => "Choice list",
            FieldKind::Bool => "Yes / no",
        }
    }

    pub const ALL: [FieldKind; 5] = [
        FieldKind::Text,
        FieldKind::Number,
        FieldKind::Date,
        FieldKind::Choice,
        FieldKind::Bool,
    ];

    /// Checks and canonicalises a value entered for a field of this kind.
    /// Empty input means "no value".
    pub fn validate(self, raw: &str, options: &[String]) -> Result<Option<String>> {
        let v = raw.trim();
        if v.is_empty() {
            return Ok(None);
        }
        Ok(Some(match self {
            FieldKind::Text => util::clean(v, 500),
            FieldKind::Number => {
                let n: f64 = v
                    .replace(',', ".")
                    .parse()
                    .map_err(|_| anyhow::anyhow!("“{v}” is not a number."))?;
                if !n.is_finite() {
                    bail!("“{v}” is not a number.");
                }
                let s = n.to_string();
                s
            }
            FieldKind::Date => {
                let d: jiff::civil::Date = v
                    .parse()
                    .map_err(|_| anyhow::anyhow!("“{v}” is not a date (use YYYY-MM-DD)."))?;
                d.to_string()
            }
            FieldKind::Choice => {
                if !options.iter().any(|o| o == v) {
                    bail!("“{v}” is not one of the choices.");
                }
                v.to_owned()
            }
            FieldKind::Bool => match v {
                "yes" | "true" | "on" | "1" => "yes".to_owned(),
                "no" | "false" | "off" | "0" => return Ok(None),
                _ => bail!("“{v}” should be yes or no."),
            },
        }))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldDef {
    pub id: i64,
    pub type_id: Option<i64>,
    pub item_id: Option<i64>,
    pub name: String,
    pub kind: FieldKind,
    pub options: Vec<String>,
}

impl FieldDef {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        let opts: String = r.get(5)?;
        Ok(Self {
            id: r.get(0)?,
            type_id: r.get(1)?,
            item_id: r.get(2)?,
            name: r.get(3)?,
            kind: FieldKind::parse(&r.get::<_, String>(4)?).unwrap_or(FieldKind::Text),
            options: parse_options(&opts),
        })
    }
}

pub fn parse_options(s: &str) -> Vec<String> {
    s.split(['\n', ','])
        .map(str::trim)
        .filter(|o| !o.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Fields that apply to an item of `type_id` (and, if given, those defined
/// for that one item).
pub fn fields_for(conn: &Connection, type_id: i64, item_id: Option<i64>) -> Result<Vec<FieldDef>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, type_id, item_id, name, kind, options FROM field_defs
         WHERE type_id = ?1 OR (?2 IS NOT NULL AND item_id = ?2)
         ORDER BY item_id IS NOT NULL, position, id",
    )?;
    let rows = stmt.query_map(params![type_id, item_id], FieldDef::from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get_field(conn: &Connection, id: i64) -> Result<Option<FieldDef>> {
    Ok(conn
        .query_row(
            "SELECT id, type_id, item_id, name, kind, options FROM field_defs WHERE id = ?1",
            [id],
            FieldDef::from_row,
        )
        .optional()?)
}

pub enum FieldScope {
    Type(i64),
    Item(i64),
}

pub fn create_field(
    conn: &Connection,
    scope: FieldScope,
    name: &str,
    kind: FieldKind,
    options: &str,
) -> Result<FieldDef> {
    let name = util::clean(name, 60);
    if name.is_empty() {
        bail!("A field needs a name.");
    }
    let opts = parse_options(options);
    if kind == FieldKind::Choice && opts.is_empty() {
        bail!("A choice list needs at least one choice.");
    }
    let (type_id, item_id) = match scope {
        FieldScope::Type(t) => (Some(t), None),
        FieldScope::Item(i) => (None, Some(i)),
    };
    let clash: bool = conn.query_row(
        "SELECT count(*) > 0 FROM field_defs
         WHERE name = ?1 COLLATE NOCASE AND (type_id IS ?2 AND item_id IS ?3)",
        params![name, type_id, item_id],
        |r| r.get(0),
    )?;
    if clash {
        bail!("There is already a field called “{name}”.");
    }
    conn.execute(
        "INSERT INTO field_defs (type_id, item_id, name, kind, options, position)
         VALUES (?1, ?2, ?3, ?4, ?5,
                 (SELECT coalesce(max(position), 0) + 1 FROM field_defs WHERE type_id IS ?1 AND item_id IS ?2))",
        params![type_id, item_id, name, kind.as_str(), opts.join("\n")],
    )?;
    Ok(get_field(conn, conn.last_insert_rowid())?.expect("just inserted"))
}

pub fn delete_field(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM field_defs WHERE id = ?1", [id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testing::temp_db;

    #[test]
    fn create_type_derives_slug_and_section() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let t = create(
                c,
                &NewType { name: "Equipment · Sports", section: "", lookup: Lookup::Product, creator_label: "" },
            )?;
            assert_eq!(t.slug, "equipment-sports");
            assert_eq!(t.section, "Sports");
            assert_eq!(t.creator_label, "Creator");
            assert!(by_slug(c, "equipment-sports")?.is_some());
            let dup = create(
                c,
                &NewType { name: "equipment · sports", section: "", lookup: Lookup::None, creator_label: "" },
            );
            assert!(dup.is_err());
            assert!(create(c, &NewType { name: " ", section: "", lookup: Lookup::None, creator_label: "" }).is_err());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn list_is_ordered() {
        let (db, _d) = temp_db();
        let names: Vec<String> = db
            .read_blocking(|c| Ok(list(c)?.into_iter().map(|t| t.name).collect()))
            .unwrap();
        assert_eq!(names[0], "Book");
        assert_eq!(names.len(), 6);
    }

    #[test]
    fn fields_scope_and_validation() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let book = by_slug(c, "book")?.unwrap();
            create_field(c, FieldScope::Type(book.id), "Signed", FieldKind::Bool, "")?;
            assert!(create_field(c, FieldScope::Type(book.id), "signed", FieldKind::Text, "").is_err());
            assert!(create_field(c, FieldScope::Type(book.id), "Fuel", FieldKind::Choice, "").is_err());
            let fuel = create_field(c, FieldScope::Type(book.id), "Fuel", FieldKind::Choice, "Meths, Gas")?;
            assert_eq!(fuel.options, vec!["Meths", "Gas"]);
            let fields = fields_for(c, book.id, None)?;
            assert_eq!(fields.len(), 2);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn field_value_validation() {
        let opts = vec!["Meths".to_owned(), "Gas".to_owned()];
        assert_eq!(FieldKind::Number.validate("1,5", &[]).unwrap().as_deref(), Some("1.5"));
        assert!(FieldKind::Number.validate("heavy", &[]).is_err());
        assert_eq!(FieldKind::Date.validate("2026-03-01", &[]).unwrap().as_deref(), Some("2026-03-01"));
        assert!(FieldKind::Date.validate("March", &[]).is_err());
        assert_eq!(FieldKind::Choice.validate("Gas", &opts).unwrap().as_deref(), Some("Gas"));
        assert!(FieldKind::Choice.validate("Petrol", &opts).is_err());
        assert_eq!(FieldKind::Bool.validate("on", &[]).unwrap().as_deref(), Some("yes"));
        assert_eq!(FieldKind::Bool.validate("no", &[]).unwrap(), None);
        assert_eq!(FieldKind::Text.validate("   ", &[]).unwrap(), None);
    }
}
