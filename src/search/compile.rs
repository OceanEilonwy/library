//! Turns a parsed query into a SQL boolean expression over `items i`.

use std::collections::HashMap;

use rusqlite::types::Value;

use super::query::{Field, Node, Term};

/// SQL text plus positional parameters (`?` placeholders, in order).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Sql {
    pub text: String,
    pub params: Vec<Value>,
}

/// Item ids that matched each `idea:` text (computed beforehand with the
/// embedding model). A missing entry matches nothing.
pub type IdeaMatches = HashMap<String, Vec<i64>>;

pub fn compile(node: &Node, ideas: &IdeaMatches) -> Sql {
    let mut sql = Sql::default();
    emit(node, ideas, &mut sql);
    sql
}

fn emit(node: &Node, ideas: &IdeaMatches, out: &mut Sql) {
    match node {
        Node::All => out.text.push_str("1"),
        Node::And(kids) | Node::Or(kids) => {
            let op = if matches!(node, Node::And(_)) { " AND " } else { " OR " };
            out.text.push('(');
            for (i, k) in kids.iter().enumerate() {
                if i > 0 {
                    out.text.push_str(op);
                }
                emit(k, ideas, out);
            }
            out.text.push(')');
        }
        Node::Not(inner) => {
            out.text.push_str("NOT (");
            emit(inner, ideas, out);
            out.text.push(')');
        }
        Node::Term(t) => term(t, ideas, out),
    }
}

fn push(out: &mut Sql, text: &str, params: impl IntoIterator<Item = Value>) {
    out.text.push_str(text);
    out.params.extend(params);
}

fn term(t: &Term, ideas: &IdeaMatches, out: &mut Sql) {
    match t.field {
        f if f.is_text() => text_term(t, out),
        Field::Year => year_term(&t.text, out),
        Field::Type => {
            let v = t.text.to_lowercase();
            if t.has_wildcards() {
                let pat = glob_escape_keep_wildcards(&v);
                push(
                    out,
                    "(i.type_id IN (SELECT id FROM media_types WHERE slug GLOB ? OR lower(name) GLOB ? OR lower(section) GLOB ?) OR lower(i.format) GLOB ?)",
                    [pat.clone(), pat.clone(), pat.clone(), format!("*{pat}*")].map(Value::Text),
                );
            } else {
                let singular = v.strip_suffix('s').unwrap_or(&v).to_owned();
                let contains = format!("*{}*", glob_escape(&singular));
                push(
                    out,
                    "(i.type_id IN (SELECT id FROM media_types WHERE slug = ? OR slug = ? OR lower(section) = ? OR lower(name) = ? OR lower(name) GLOB ?) OR lower(i.format) GLOB ?)",
                    [v.clone(), singular.clone(), v.clone(), v.clone(), contains.clone(), contains]
                        .map(Value::Text),
                );
            }
        }
        Field::In => {
            let parts: Vec<String> = t
                .text
                .to_lowercase()
                .split(['/', '>', '›'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(glob_escape_keep_wildcards)
                .collect();
            let pat = format!("*{}*", parts.join("*›*"));
            push(
                out,
                "i.location_id IN (SELECT id FROM locations WHERE lower(path) GLOB ?)",
                [Value::Text(pat)],
            );
        }
        Field::Loan => {
            let v = t.text.trim().to_lowercase();
            match v.as_str() {
                "" | "*" | "yes" | "out" | "any" => out.text.push_str(
                    "EXISTS (SELECT 1 FROM loans ln WHERE ln.item_id = i.id AND ln.returned_at IS NULL)",
                ),
                "no" | "none" | "in" => out.text.push_str(
                    "NOT EXISTS (SELECT 1 FROM loans ln WHERE ln.item_id = i.id AND ln.returned_at IS NULL)",
                ),
                name => push(
                    out,
                    "EXISTS (SELECT 1 FROM loans ln WHERE ln.item_id = i.id AND ln.returned_at IS NULL AND lower(ln.borrower) GLOB ?)",
                    [Value::Text(format!("*{}*", glob_escape_keep_wildcards(name)))],
                ),
            }
        }
        Field::Idea => {
            let ids = ideas.get(&t.text).cloned().unwrap_or_default();
            if ids.is_empty() {
                out.text.push('0');
            } else {
                push(
                    out,
                    "i.id IN (SELECT value FROM json_each(?))",
                    [Value::Text(serde_json::to_string(&ids).unwrap_or_else(|_| "[]".into()))],
                );
            }
        }
        _ => out.text.push('1'),
    }
}

/// FTS5 column filter for a text field.
fn fts_columns(field: Field) -> &'static str {
    match field {
        Field::Title => "{title} : ",
        Field::Creator => "{creator} : ",
        Field::Publisher => "{publisher} : ",
        Field::Tag => "{tags description} : ",
        Field::Notes => "{notes description extra} : ",
        Field::Code => "{identifier} : ",
        _ => "",
    }
}

fn glob_columns(field: Field) -> &'static str {
    match field {
        Field::Title => "i.title",
        Field::Creator => "i.creator",
        Field::Publisher => "i.publisher",
        Field::Tag => "i.tags || ' ' || i.description",
        Field::Notes => "i.notes || ' ' || i.description",
        Field::Code => "i.identifier",
        _ => "i.title || ' ' || i.creator || ' ' || i.publisher || ' ' || i.tags || ' ' || i.description || ' ' || i.notes || ' ' || i.format",
    }
}

/// The FTS5 query string for a text term, or `None` if it needs GLOB
/// (wildcards in the middle) or has nothing searchable in it.
pub fn fts_expr(t: &Term) -> Option<String> {
    if !t.text.chars().any(char::is_alphanumeric) {
        return None;
    }
    let cols = fts_columns(t.field);
    if t.is_prefix() {
        let stem = &t.text[..t.text.len() - 1];
        return Some(format!("{cols}\"{}\" *", stem.replace('"', "\"\"")));
    }
    if t.has_wildcards() {
        return None;
    }
    Some(format!("{cols}\"{}\"", t.text.replace('"', "\"\"")))
}

fn text_term(t: &Term, out: &mut Sql) {
    if t.field == Field::Code {
        let digits = crate::util::normalize_identifier(&t.text);
        if !digits.is_empty() && !t.has_wildcards() {
            let isbn13 = crate::util::isbn13(&digits);
            push(
                out,
                "(replace(replace(upper(i.identifier), '-', ''), ' ', '') IN (?, ?))",
                [Value::Text(digits), Value::Text(isbn13)],
            );
            return;
        }
    }
    if let Some(expr) = fts_expr(t) {
        push(
            out,
            "i.id IN (SELECT rowid FROM items_fts WHERE items_fts MATCH ?)",
            [Value::Text(expr)],
        );
        return;
    }
    if !t.text.chars().any(char::is_alphanumeric) {
        // "*" or punctuation only: matches everything rather than erroring.
        out.text.push('1');
        return;
    }
    let pat = format!("*{}*", glob_escape_keep_wildcards(&t.text.to_lowercase()));
    push(
        out,
        &format!("lower({}) GLOB ?", glob_columns(t.field)),
        [Value::Text(pat)],
    );
}

fn year_term(v: &str, out: &mut Sql) {
    let v = v.trim();
    let num = |s: &str| s.trim().parse::<i64>().ok().filter(|n| (0..=3000).contains(n));
    if let Some((a, b)) = v.split_once("..") {
        match (num(a), num(b)) {
            (Some(a), Some(b)) => push(
                out,
                "(i.year BETWEEN ? AND ?)",
                [Value::Integer(a.min(b)), Value::Integer(a.max(b))],
            ),
            (Some(a), None) if b.trim().is_empty() => push(out, "(i.year >= ?)", [Value::Integer(a)]),
            (None, Some(b)) if a.trim().is_empty() => push(out, "(i.year <= ?)", [Value::Integer(b)]),
            _ => out.text.push('0'),
        }
    } else if let Some(decade) = v.strip_suffix('s').and_then(num) {
        // "1980s"
        push(out, "(i.year BETWEEN ? AND ?)", [Value::Integer(decade), Value::Integer(decade + 9)]);
    } else if v.contains(['*', '?']) {
        push(
            out,
            "(CAST(i.year AS TEXT) GLOB ?)",
            [Value::Text(glob_escape_keep_wildcards(v))],
        );
    } else if let Some(y) = num(v) {
        push(out, "(i.year = ?)", [Value::Integer(y)]);
    } else {
        out.text.push('0');
    }
}

/// Escapes GLOB metacharacters, including the user's wildcards.
pub fn glob_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '*' => out.push_str("[*]"),
            '?' => out.push_str("[?]"),
            '[' => out.push_str("[[]"),
            other => out.push(other),
        }
    }
    out
}

/// Escapes `[` but keeps `*` and `?` as wildcards.
pub fn glob_escape_keep_wildcards(s: &str) -> String {
    s.replace('[', "[[]")
}

#[cfg(test)]
mod tests {
    use super::super::query::parse;
    use super::*;

    fn c(q: &str) -> Sql {
        compile(&parse(q), &IdeaMatches::new())
    }

    #[test]
    fn empty_query_is_true() {
        assert_eq!(c("").text, "1");
    }

    #[test]
    fn words_use_fts_with_quoting() {
        let s = c("hobbit");
        assert!(s.text.contains("items_fts MATCH ?"));
        assert_eq!(s.params, vec![Value::Text("\"hobbit\"".into())]);
        let s = c("title:\"say \"\"hi\"");
        assert!(matches!(&s.params[0], Value::Text(t) if t.starts_with("{title} : ")));
    }

    #[test]
    fn prefix_wildcard_uses_fts_others_use_glob() {
        assert_eq!(c("sea*").params, vec![Value::Text("\"sea\" *".into())]);
        let s = c("gr?y");
        assert!(s.text.contains("GLOB"));
        assert_eq!(s.params, vec![Value::Text("*gr?y*".into())]);
        assert_eq!(c("*").text, "1");
    }

    #[test]
    fn year_forms() {
        assert_eq!(c("year:1990").params, vec![Value::Integer(1990)]);
        assert_eq!(c("year:1975..1960").params, vec![Value::Integer(1960), Value::Integer(1975)]);
        assert!(c("year:1990..").text.contains(">="));
        assert!(c("year:..1990").text.contains("<="));
        assert_eq!(c("year:1980s").params, vec![Value::Integer(1980), Value::Integer(1989)]);
        assert!(c("year:197*").text.contains("GLOB"));
        assert_eq!(c("year:soon").text, "0");
    }

    #[test]
    fn loan_forms() {
        assert!(c("loan:*").text.starts_with("EXISTS"));
        assert!(c("loan:no").text.starts_with("NOT EXISTS"));
        assert_eq!(c("loan:Sam").params, vec![Value::Text("*sam*".into())]);
    }

    #[test]
    fn location_path_pattern() {
        assert_eq!(c("in:shed/box*").params, vec![Value::Text("*shed*›*box**".into())]);
    }

    #[test]
    fn idea_uses_supplied_ids() {
        let mut ideas = IdeaMatches::new();
        ideas.insert("ocean".into(), vec![3, 1]);
        let s = compile(&parse("idea:ocean"), &ideas);
        assert_eq!(s.params, vec![Value::Text("[3,1]".into())]);
        assert_eq!(c("idea:ocean").text, "0");
    }

    #[test]
    fn boolean_structure() {
        let s = c("a OR NOT b");
        assert!(s.text.starts_with("(") && s.text.contains(" OR NOT ("));
        assert_eq!(s.params.len(), 2);
    }

    #[test]
    fn glob_escaping() {
        assert_eq!(glob_escape("a*b?[c"), "a[*]b[?][[]c");
        assert_eq!(glob_escape_keep_wildcards("a*[b"), "a*[[]b");
    }
}
