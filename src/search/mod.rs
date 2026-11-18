//! Hybrid search: the boolean query language over SQLite (FTS5 + structured
//! filters), widened and re-ranked with meaning-based (embedding) matches.
//!
//! Ranking uses reciprocal rank fusion of the FTS5 bm25 order and the vector
//! similarity order, so an item that is both a strong word match and close
//! in meaning comes first.

pub mod compile;
pub mod query;

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::Connection;
use rusqlite::types::Value;

use crate::db::items::{self, Summary};
use crate::db::{Db, locations};
use crate::embed::{self, Semantic};
use crate::util;
use compile::{IdeaMatches, Sql, compile};
use query::{Field, Node, Term};

/// Neighbours fetched per query: enough to measure the query's baseline.
const NEIGHBOURS: usize = 200;
const MAX_MATCHES: i64 = 5000;
const RRF_K: f64 = 60.0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Availability {
    #[default]
    Any,
    OnShelf,
    OnLoan,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Sort {
    #[default]
    Relevance,
    Title,
    Year,
    Added,
}

impl Sort {
    pub fn parse(s: &str) -> Self {
        match s {
            "title" => Sort::Title,
            "year" => Sort::Year,
            "added" => Sort::Added,
            _ => Sort::Relevance,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Sort::Relevance => "relevance",
            Sort::Title => "title",
            Sort::Year => "year",
            Sort::Added => "added",
        }
    }
}

/// The result-page filters (the "Showing everything kept anywhere…" sentence).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filters {
    pub type_slug: Option<String>,
    pub place: Option<i64>,
    pub decade: Option<i64>,
    pub availability: Availability,
}

#[derive(Debug, Clone, Default)]
pub struct Request {
    pub q: String,
    pub filters: Filters,
    pub sort: Sort,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub item: Summary,
    /// Why it matched: "title", "author", "by idea"… (None for plain listings).
    pub reason: Option<String>,
    pub by_idea: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Results {
    pub total: usize,
    pub by_name: usize,
    pub by_idea: usize,
    pub hits: Vec<Hit>,
    pub see_also: Vec<Hit>,
    pub has_more: bool,
    /// True when the query had words to look for (affects the default sort).
    pub has_text: bool,
}

pub async fn search(db: &Db, semantic: &Semantic, req: Request) -> Result<Results> {
    let node = query::parse(&req.q);
    let text_terms: Vec<Term> = node.positive_text_terms().into_iter().cloned().collect();
    let free_text: String = text_terms
        .iter()
        // Wildcards mean "spelled like this", so they don't widen by meaning.
        .filter(|t| !t.has_wildcards())
        .map(|t| t.text.as_str())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    // Embed the free text and each idea: term up front (off the DB thread).
    let query_vec = if free_text.chars().any(char::is_alphanumeric) {
        semantic.embed_query(&free_text).await
    } else {
        None
    };
    let mut idea_vecs = Vec::new();
    for idea in node.idea_texts() {
        let v = semantic.embed_query(&idea).await;
        idea_vecs.push((idea, v));
    }

    let th = semantic.thresholds();
    db.read(move |c| run(c, &node, &text_terms, query_vec, idea_vecs, th, &req))
        .await
}

fn run(
    c: &Connection,
    node: &Node,
    text_terms: &[Term],
    query_vec: Option<Vec<f32>>,
    idea_vecs: Vec<(String, Option<Vec<f32>>)>,
    th: embed::Thresholds,
    req: &Request,
) -> Result<Results> {
    // Resolve idea: terms to item ids.
    let mut ideas = IdeaMatches::new();
    let mut idea_rank: HashMap<i64, usize> = HashMap::new();
    for (text, v) in idea_vecs {
        let Some(v) = v else { continue };
        let all = embed::neighbours(c, &v, NEIGHBOURS)?;
        let sims: Vec<f32> = all.iter().map(|(_, s)| *s).collect();
        let cut = (th.baseline_for(&sims) + th.idea).max(th.floor);
        let near: Vec<(i64, f32)> = all.into_iter().filter(|(_, s)| *s >= cut).collect();
        for (rank, (id, _)) in near.iter().enumerate() {
            let e = idea_rank.entry(*id).or_insert(rank);
            *e = (*e).min(rank);
        }
        ideas.insert(text, near.into_iter().map(|(id, _)| id).collect());
    }

    let filters = filters_sql(c, &req.filters)?;
    let matched = ids_where(c, &compile(node, &ideas), &filters)?;
    let matched_set: HashSet<i64> = matched.iter().copied().collect();

    // FTS rank of the positive words, for ordering.
    let fts_rank = fts_ranks(c, text_terms)?;

    // Meaning-based widening for free text.
    let mut vec_rank: HashMap<i64, usize> = HashMap::new();
    let mut idea_only: Vec<i64> = Vec::new();
    let mut see_also_ids: Vec<i64> = Vec::new();
    if let Some(qv) = &query_vec {
        let near = embed::neighbours(c, qv, NEIGHBOURS)?;
        let sims: Vec<f32> = near.iter().map(|(_, s)| *s).collect();
        let base = th.baseline_for(&sims);
        let (main_cut, see_cut) = (
            (base + th.main).max(th.floor),
            (base + th.see_also).max(th.floor - 0.05),
        );
        for (rank, (id, _)) in near.iter().enumerate() {
            vec_rank.insert(*id, rank);
        }
        let structural = node.structural();
        let candidates: Vec<i64> = near
            .iter()
            .filter(|(id, s)| *s >= see_cut && !matched_set.contains(id))
            .map(|(id, _)| *id)
            .collect();
        let allowed: HashSet<i64> =
            ids_in(c, &candidates, &compile(&structural, &ideas), &filters)?
                .into_iter()
                .collect();
        for (id, sim) in &near {
            if !allowed.contains(id) {
                continue;
            }
            if *sim >= main_cut {
                idea_only.push(*id);
            } else if see_also_ids.len() < 3 {
                see_also_ids.push(*id);
            }
        }
    }

    let has_text = !text_terms.is_empty() || !ideas.is_empty();
    let mut all: Vec<i64> = matched.clone();
    all.extend(idea_only.iter().copied());

    let sort = match req.sort {
        Sort::Relevance if !has_text => Sort::Title,
        s => s,
    };
    let ordered = match sort {
        Sort::Relevance => {
            let score = |id: &i64| -> f64 {
                let mut s = 0.0;
                if let Some(r) = fts_rank.get(id) {
                    s += 1.0 / (RRF_K + *r as f64);
                }
                if let Some(r) = vec_rank.get(id) {
                    s += 1.0 / (RRF_K + *r as f64);
                }
                if let Some(r) = idea_rank.get(id) {
                    s += 1.0 / (RRF_K + *r as f64);
                }
                // Word matches without an FTS rank (wildcards via GLOB) still beat
                // meaning-only matches slightly.
                if matched_set.contains(id) && !fts_rank.contains_key(id) {
                    s += 0.5 / (RRF_K + 50.0);
                }
                s
            };
            let mut scored: Vec<(i64, f64)> = all.iter().map(|id| (*id, score(id))).collect();
            scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            scored.into_iter().map(|(id, _)| id).collect()
        }
        other => order_by(c, &all, other)?,
    };

    let total = ordered.len();
    let page: Vec<i64> = ordered
        .iter()
        .skip(req.offset)
        .take(req.limit.max(1))
        .copied()
        .collect();
    let idea_set: HashSet<i64> = idea_only.iter().copied().collect();
    let hits = items::summaries(c, &page)?
        .into_iter()
        .map(|s| {
            let by_idea = idea_set.contains(&s.id);
            let reason = if by_idea || (idea_rank.contains_key(&s.id) && text_terms.is_empty()) {
                Some("by idea".to_owned())
            } else {
                reason_for(&s, text_terms)
            };
            Hit {
                item: s,
                reason,
                by_idea,
            }
        })
        .collect();
    let see_also = items::summaries(c, &see_also_ids)?
        .into_iter()
        .map(|s| Hit {
            item: s,
            reason: Some("related idea".into()),
            by_idea: true,
        })
        .collect();

    Ok(Results {
        total,
        by_name: matched.len(),
        by_idea: idea_only.len(),
        hits,
        see_also,
        has_more: req.offset + req.limit < total,
        has_text,
    })
}

fn filters_sql(c: &Connection, f: &Filters) -> Result<Sql> {
    let mut sql = Sql {
        text: "1".into(),
        params: Vec::new(),
    };
    if let Some(slug) = &f.type_slug {
        sql.text
            .push_str(" AND i.type_id IN (SELECT id FROM media_types WHERE slug = ?)");
        sql.params.push(Value::Text(slug.clone()));
    }
    if let Some(place) = f.place
        && let Some(loc) = locations::get(c, place)?
    {
        sql.text.push_str(
            " AND i.location_id IN (SELECT id FROM locations WHERE id = ? OR path LIKE ? ESCAPE '\\')",
        );
        sql.params.push(Value::Integer(loc.id));
        let escaped = loc
            .path
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        sql.params
            .push(Value::Text(format!("{escaped}{}%", locations::SEP)));
    }
    if let Some(d) = f.decade {
        sql.text.push_str(" AND i.year BETWEEN ? AND ?");
        sql.params.push(Value::Integer(d));
        sql.params.push(Value::Integer(d + 9));
    }
    match f.availability {
        Availability::Any => {}
        Availability::OnShelf => sql.text.push_str(
            " AND NOT EXISTS (SELECT 1 FROM loans ln WHERE ln.item_id = i.id AND ln.returned_at IS NULL)",
        ),
        Availability::OnLoan => sql.text.push_str(
            " AND EXISTS (SELECT 1 FROM loans ln WHERE ln.item_id = i.id AND ln.returned_at IS NULL)",
        ),
    }
    Ok(sql)
}

fn ids_where(c: &Connection, cond: &Sql, filters: &Sql) -> Result<Vec<i64>> {
    let text = format!(
        "SELECT i.id FROM items i WHERE ({}) AND ({}) ORDER BY i.id LIMIT {MAX_MATCHES}",
        cond.text, filters.text
    );
    let params: Vec<&Value> = cond.params.iter().chain(&filters.params).collect();
    let mut stmt = c.prepare(&text)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn ids_in(c: &Connection, ids: &[i64], cond: &Sql, filters: &Sql) -> Result<Vec<i64>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let text = format!(
        "SELECT i.id FROM items i WHERE i.id IN (SELECT value FROM json_each(?)) AND ({}) AND ({})",
        cond.text, filters.text
    );
    let json = Value::Text(serde_json::to_string(ids)?);
    let params: Vec<&Value> = std::iter::once(&json)
        .chain(&cond.params)
        .chain(&filters.params)
        .collect();
    let mut stmt = c.prepare(&text)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn fts_ranks(c: &Connection, terms: &[Term]) -> Result<HashMap<i64, usize>> {
    let exprs: Vec<String> = terms.iter().filter_map(compile::fts_expr).collect();
    if exprs.is_empty() {
        return Ok(HashMap::new());
    }
    let expr = exprs.join(" OR ");
    // Title and creator hits weigh more than notes.
    let mut stmt = c.prepare_cached(
        "SELECT rowid FROM items_fts WHERE items_fts MATCH ?1
         ORDER BY bm25(items_fts, 10.0, 6.0, 2.0, 2.0, 3.0, 1.0, 4.0, 1.0) LIMIT 2000",
    )?;
    let rows = stmt.query_map([expr], |r| r.get::<_, i64>(0))?;
    let mut out = HashMap::new();
    for (rank, id) in rows.enumerate() {
        out.insert(id?, rank);
    }
    Ok(out)
}

fn order_by(c: &Connection, ids: &[i64], sort: Sort) -> Result<Vec<i64>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let order = match sort {
        Sort::Year => "i.year IS NULL, i.year DESC, i.title COLLATE NOCASE",
        Sort::Added => "i.created_at DESC, i.id DESC",
        _ => {
            "CASE WHEN lower(i.title) LIKE 'the %' THEN substr(i.title, 5)
                  WHEN lower(i.title) LIKE 'a %' THEN substr(i.title, 3)
                  WHEN lower(i.title) LIKE 'an %' THEN substr(i.title, 4)
                  ELSE i.title END COLLATE NOCASE, i.id"
        }
    };
    let mut stmt = c.prepare_cached(&format!(
        "SELECT i.id FROM items i WHERE i.id IN (SELECT value FROM json_each(?1)) ORDER BY {order}"
    ))?;
    let rows = stmt.query_map([serde_json::to_string(ids)?], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// A short explanation of which part of an item matched the words.
fn reason_for(s: &Summary, terms: &[Term]) -> Option<String> {
    if terms.is_empty() {
        return None;
    }
    let title = util::normalize(&s.title);
    let creator = util::normalize(&s.creator);
    let tags = util::normalize(&s.tags);
    let contains = |hay: &str, t: &Term| {
        if t.has_wildcards() {
            wildcard_match(hay, &t.text.to_lowercase())
        } else {
            // Whole words only: "le" should not explain a match on "left".
            let needle = util::normalize(&t.text);
            !needle.is_empty() && format!(" {hay} ").contains(&format!(" {needle} "))
        }
    };
    let mut reasons: Vec<String> = Vec::new();
    for t in terms {
        let r = match t.field {
            Field::Code => "isbn".to_owned(),
            _ if contains(&title, t) && matches!(t.field, Field::Any | Field::Title) => {
                "title".to_owned()
            }
            _ if contains(&creator, t) && matches!(t.field, Field::Any | Field::Creator) => {
                s.creator_label.to_lowercase()
            }
            _ if contains(&tags, t) => "subject".to_owned(),
            Field::Publisher => "publisher".to_owned(),
            _ => "details".to_owned(),
        };
        if !reasons.contains(&r) {
            reasons.push(r);
        }
    }
    if reasons.len() > 1 {
        reasons.retain(|r| r != "details");
    }
    Some(reasons.join(" · "))
}

/// Shell-style `*`/`?` match anywhere in `hay` (both already lower-case).
fn wildcard_match(hay: &str, pattern: &str) -> bool {
    fn m(p: &[char], s: &[char]) -> bool {
        match (p.first(), s.first()) {
            (None, _) => true,
            (Some('*'), _) => m(&p[1..], s) || (!s.is_empty() && m(p, &s[1..])),
            (Some('?'), Some(_)) => m(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => m(&p[1..], &s[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = hay.chars().collect();
    (0..=s.len()).any(|start| m(&p, &s[start..]))
}

/// Builds a query string from the advanced search form's fields.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Advanced {
    pub title: String,
    pub creator: String,
    pub year_from: String,
    pub year_to: String,
    pub types: Vec<String>,
    pub loan: String,
    pub idea: String,
    pub words: String,
}

impl Advanced {
    pub fn to_query(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !self.words.trim().is_empty() {
            let w = query::render(&query::parse(&self.words));
            parts.push(if w.contains(" OR ") {
                format!("({w})")
            } else {
                w
            });
        }
        let quote = |field: &str, v: &str| {
            let v = v.trim().replace('"', "");
            if v.contains(char::is_whitespace) {
                format!("{field}:\"{v}\"")
            } else {
                format!("{field}:{v}")
            }
        };
        if !self.title.trim().is_empty() {
            parts.push(quote("title", &self.title));
        }
        if !self.creator.trim().is_empty() {
            // "bowie OR eno" in the creator box means either.
            let c = self.creator.trim();
            if c.contains(" OR ") {
                let alts: Vec<String> = c.split(" OR ").map(|a| quote("creator", a)).collect();
                parts.push(format!("({})", alts.join(" OR ")));
            } else {
                parts.push(quote("creator", c));
            }
        }
        let (from, to) = (self.year_from.trim(), self.year_to.trim());
        match (from.is_empty(), to.is_empty()) {
            (false, false) => parts.push(format!("year:{from}..{to}")),
            (false, true) => parts.push(format!("year:{from}..")),
            (true, false) => parts.push(format!("year:..{to}")),
            (true, true) => {}
        }
        let types: Vec<&String> = self.types.iter().filter(|t| !t.is_empty()).collect();
        match types.len() {
            0 => {}
            1 => parts.push(format!("type:{}", types[0])),
            _ => parts.push(format!(
                "type:({})",
                types
                    .iter()
                    .map(|t| t.as_str())
                    .collect::<Vec<_>>()
                    .join(" OR ")
            )),
        }
        if !self.loan.trim().is_empty() {
            parts.push(quote("loan", &self.loan));
        }
        if !self.idea.trim().is_empty() {
            parts.push(format!("idea:\"{}\"", self.idea.trim().replace('"', "")));
        }
        parts.join(" AND ")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::db::items::{ItemInput, create};
    use crate::db::testing::temp_db;
    use crate::db::{loans, types};
    use crate::embed::fake::FakeEmbedder;

    struct Lib {
        db: Db,
        _dir: tempfile::TempDir,
        ids: HashMap<&'static str, i64>,
    }

    fn add(
        c: &mut Connection,
        slug: &str,
        title: &str,
        creator: &str,
        year: Option<i64>,
        desc: &str,
        loc: &str,
    ) -> i64 {
        let t = types::by_slug(c, slug).unwrap().unwrap();
        create(
            c,
            &ItemInput {
                type_id: t.id,
                title: title.into(),
                creator: creator.into(),
                year: year.map(|y| y.to_string()).unwrap_or_default(),
                description: desc.into(),
                location: loc.into(),
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn library() -> Lib {
        let (db, dir) = temp_db();
        let mut ids = HashMap::new();
        db.write_blocking(|c| {
            ids.insert(
                "earthsea",
                add(
                    c,
                    "book",
                    "A Wizard of Earthsea",
                    "Ursula K. Le Guin",
                    Some(1968),
                    "wizard school, islands",
                    "Lounge > Bookcase 1 > Shelf 2",
                ),
            );
            ids.insert(
                "lefthand",
                add(
                    c,
                    "book",
                    "The Left Hand of Darkness",
                    "Ursula K. Le Guin",
                    Some(1969),
                    "winter planet",
                    "Shed > Books Box 1",
                ),
            );
            ids.insert(
                "lathe",
                add(
                    c,
                    "book",
                    "The Lathe of Heaven",
                    "Ursula K. Le Guin",
                    Some(1971),
                    "dreams",
                    "Shed > Books Box 1",
                ),
            );
            ids.insert(
                "oldman",
                add(
                    c,
                    "book",
                    "The Old Man and the Sea",
                    "Ernest Hemingway",
                    Some(1952),
                    "fisherman",
                    "Lounge > Bookcase 1 > Shelf 1",
                ),
            );
            ids.insert(
                "moby",
                add(
                    c,
                    "book",
                    "Moby-Dick",
                    "Herman Melville",
                    Some(1851),
                    "whaling voyage",
                    "Shed > Books Box 2",
                ),
            );
            ids.insert(
                "totoro",
                add(
                    c,
                    "film",
                    "My Neighbour Totoro",
                    "Hayao Miyazaki",
                    Some(1988),
                    "forest spirits",
                    "Lounge > TV cabinet",
                ),
            );
            ids.insert(
                "tales",
                add(
                    c,
                    "film",
                    "Tales from Earthsea",
                    "Gorō Miyazaki",
                    Some(2006),
                    "dragons",
                    "Lounge > TV cabinet",
                ),
            );
            ids.insert(
                "tent",
                add(
                    c,
                    "camping",
                    "Tent, 2-person",
                    "MSR",
                    None,
                    "hiking camping",
                    "Shed > Camping shelf",
                ),
            );
            ids.insert(
                "grey",
                add(
                    c,
                    "book",
                    "The Grey King",
                    "Susan Cooper",
                    Some(1975),
                    "",
                    "",
                ),
            );
            Ok(())
        })
        .unwrap();
        let lefthand = ids["lefthand"];
        db.write_blocking(|c| loans::lend(c, lefthand, "Sam"))
            .unwrap();
        Lib { db, _dir: dir, ids }
    }

    async fn q(lib: &Lib, sem: &Semantic, query: &str) -> Vec<i64> {
        let r = search(
            &lib.db,
            sem,
            Request {
                q: query.into(),
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        r.hits.iter().map(|h| h.item.id).collect()
    }

    fn set(ids: &[i64]) -> HashSet<i64> {
        ids.iter().copied().collect()
    }

    #[tokio::test]
    async fn words_fields_and_booleans() {
        let lib = library();
        let none = Semantic::disabled();
        let id = |k: &str| lib.ids[k];
        assert_eq!(
            set(&q(&lib, &none, "le guin").await),
            set(&[id("earthsea"), id("lefthand"), id("lathe")])
        );
        assert_eq!(
            set(&q(&lib, &none, "earth*").await),
            set(&[id("earthsea"), id("tales")])
        );
        assert_eq!(
            q(&lib, &none, "title:earth* type:book").await,
            vec![id("earthsea")]
        );
        assert_eq!(
            set(&q(&lib, &none, "earthsea NOT type:film").await),
            set(&[id("earthsea")])
        );
        assert_eq!(
            set(&q(&lib, &none, "earthsea -film").await),
            set(&[id("earthsea")])
        );
        assert_eq!(
            set(&q(&lib, &none, "miyazaki OR melville").await),
            set(&[id("totoro"), id("tales"), id("moby")])
        );
        assert_eq!(q(&lib, &none, "\"old man\"").await, vec![id("oldman")]);
        assert_eq!(q(&lib, &none, "gr?y").await, vec![id("grey")]);
        assert_eq!(
            set(&q(&lib, &none, "author:\"le guin\" year:1960..1970").await),
            set(&[id("earthsea"), id("lefthand")])
        );
        assert_eq!(q(&lib, &none, "year:1980s").await, vec![id("totoro")]);
        assert_eq!(
            set(&q(&lib, &none, "in:shed type:book").await),
            set(&[id("lefthand"), id("lathe"), id("moby")])
        );
        assert_eq!(
            set(&q(&lib, &none, "in:shed/books*").await),
            set(&[id("lefthand"), id("lathe"), id("moby")])
        );
        assert_eq!(q(&lib, &none, "loan:*").await, vec![id("lefthand")]);
        assert_eq!(q(&lib, &none, "loan:sam").await, vec![id("lefthand")]);
        assert!(!q(&lib, &none, "loan:no").await.contains(&id("lefthand")));
        assert_eq!(q(&lib, &none, "type:camping").await, vec![id("tent")]);
        assert_eq!(q(&lib, &none, "type:equipment").await, vec![id("tent")]);
        assert_eq!(
            set(&q(&lib, &none, "type:films").await),
            set(&[id("totoro"), id("tales")])
        );
        assert_eq!(q(&lib, &none, "nothing-like-this").await, Vec::<i64>::new());
        // Diacritics fold: "Goro" finds "Gorō".
        assert_eq!(q(&lib, &none, "goro").await, vec![id("tales")]);
    }

    #[tokio::test]
    async fn empty_query_lists_everything_sorted_by_title() {
        let lib = library();
        let r = search(
            &lib.db,
            &Semantic::disabled(),
            Request {
                limit: 3,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(r.total, 9);
        assert!(r.has_more);
        assert!(!r.has_text);
        // "The Grey King" sorts under G, "The Lathe…" under L; "Earthsea" (A Wizard) under W.
        let titles: Vec<&str> = r.hits.iter().map(|h| h.item.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "The Grey King",
                "The Lathe of Heaven",
                "The Left Hand of Darkness"
            ]
        );
    }

    #[tokio::test]
    async fn title_matches_rank_above_other_fields_and_reasons_explain() {
        let lib = library();
        let r = search(
            &lib.db,
            &Semantic::disabled(),
            Request {
                q: "earthsea".into(),
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(r.hits[0].reason.as_deref(), Some("title"));
        let r = search(
            &lib.db,
            &Semantic::disabled(),
            Request {
                q: "miyazaki".into(),
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(
            r.hits
                .iter()
                .all(|h| h.reason.as_deref() == Some("director"))
        );
    }

    #[tokio::test]
    async fn filters_and_sorts() {
        let lib = library();
        let none = Semantic::disabled();
        let place = lib
            .db
            .read_blocking(|c| {
                Ok(locations::list(c)?
                    .into_iter()
                    .find(|l| l.path == "Shed")
                    .unwrap()
                    .id)
            })
            .unwrap();
        let r = search(
            &lib.db,
            &none,
            Request {
                q: "".into(),
                filters: Filters {
                    place: Some(place),
                    availability: Availability::OnShelf,
                    ..Default::default()
                },
                sort: Sort::Year,
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let titles: Vec<&str> = r.hits.iter().map(|h| h.item.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["The Lathe of Heaven", "Moby-Dick", "Tent, 2-person"]
        );
        let r = search(
            &lib.db,
            &none,
            Request {
                filters: Filters {
                    type_slug: Some("film".into()),
                    decade: Some(2000),
                    ..Default::default()
                },
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.hits[0].item.title, "Tales from Earthsea");
    }

    #[tokio::test]
    async fn meaning_widens_results_and_fills_see_also() {
        let lib = library();
        crate::embed::index_all_blocking(&lib.db, &FakeEmbedder).unwrap();
        let sem = Semantic::with(Arc::new(FakeEmbedder));
        let r = search(
            &lib.db,
            &sem,
            Request {
                q: "ocean".into(),
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // No item contains the word "ocean", but Moby-Dick's description is about the sea.
        assert_eq!(r.by_name, 0);
        assert!(r.hits.iter().any(|h| h.item.id == lib.ids["moby"]
            && h.by_idea
            && h.reason.as_deref() == Some("by idea")));

        // Explicit idea: term combines with structure.
        let ids = q(&lib, &sem, "idea:dragons type:film").await;
        assert_eq!(ids, vec![lib.ids["tales"]]);
        let ids = q(&lib, &sem, "idea:dragons NOT type:film").await;
        assert!(!ids.contains(&lib.ids["tales"]));

        // Structural constraints also limit meaning matches.
        let r = search(
            &lib.db,
            &sem,
            Request {
                q: "ocean type:film".into(),
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(r.hits.iter().all(|h| h.item.type_slug == "film"));
    }

    #[tokio::test]
    async fn hostile_input_never_errors() {
        let lib = library();
        let none = Semantic::disabled();
        for qs in [
            "\"",
            "*",
            "**",
            "NOT",
            "(((",
            "title:\"",
            "a\"b",
            "year:abc",
            "in:",
            "type:*",
            "idea:",
            "\" OR \"",
            "[",
            "%_",
            "NEAR(a b)",
            "a:b:c",
            "{title}: x",
        ] {
            search(
                &lib.db,
                &none,
                Request {
                    q: qs.into(),
                    limit: 10,
                    ..Default::default()
                },
            )
            .await
            .unwrap_or_else(|e| panic!("{qs}: {e}"));
        }
    }

    #[test]
    fn wildcard_matching() {
        assert!(wildcard_match("the grey king", "gr?y"));
        assert!(wildcard_match("collected essays vol 1", "*essays*"));
        assert!(!wildcard_match("the grey king", "gr?yy"));
    }

    #[test]
    fn advanced_form_builds_queries() {
        let a = Advanced {
            title: "the * of *".into(),
            creator: "bowie OR eno".into(),
            year_from: "1975".into(),
            year_to: "1980".into(),
            types: vec!["music".into(), "film".into()],
            loan: "".into(),
            idea: "ambient, Berlin".into(),
            words: "".into(),
        };
        assert_eq!(
            a.to_query(),
            "title:\"the * of *\" AND (creator:bowie OR creator:eno) AND year:1975..1980 AND type:(music OR film) AND idea:\"ambient, Berlin\""
        );
        let parsed = query::parse(&a.to_query());
        assert_eq!(parsed.idea_texts(), vec!["ambient, Berlin"]);
        assert_eq!(
            Advanced {
                loan: "*".into(),
                ..Default::default()
            }
            .to_query(),
            "loan:*"
        );
        assert_eq!(Advanced::default().to_query(), "");
    }
}
