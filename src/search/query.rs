//! The search language.
//!
//! ```text
//! sea*                     words starting with "sea"
//! gr?y                     one unknown letter
//! "the left hand"          exact phrase
//! le guin NOT film         AND is implied; AND, OR, NOT (upper case) and ( )
//! -film                    shorthand for NOT film
//! title:earth* author:"le guin" year:1960..1975 type:book
//! in:shed  loan:*  loan:sam  idea:"lonely lighthouse keepers"
//! type:(book OR film)      a field applies to everything in the brackets
//! ```
//!
//! Lower-case "and"/"or"/"not" are ordinary words, so
//! `the old man and the sea` searches for those words.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Any,
    Title,
    Creator,
    Publisher,
    Tag,
    Notes,
    Code,
    Year,
    Type,
    In,
    Loan,
    Idea,
}

impl Field {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "title" => Field::Title,
            "creator" | "author" | "artist" | "director" | "by" | "brand" | "maker" => {
                Field::Creator
            }
            "publisher" | "label" | "studio" => Field::Publisher,
            "tag" | "tags" | "subject" => Field::Tag,
            "notes" | "note" | "description" => Field::Notes,
            "isbn" | "code" | "barcode" => Field::Code,
            "year" | "date" | "released" => Field::Year,
            "type" | "kind" | "media" | "format" => Field::Type,
            "in" | "at" | "where" | "location" | "place" => Field::In,
            "loan" | "lent" | "borrower" | "out" => Field::Loan,
            "idea" | "about" | "like" => Field::Idea,
            _ => return None,
        })
    }

    /// True for fields searched through the full-text index.
    pub fn is_text(self) -> bool {
        matches!(
            self,
            Field::Any
                | Field::Title
                | Field::Creator
                | Field::Publisher
                | Field::Tag
                | Field::Notes
                | Field::Code
        )
    }

    pub fn name(self) -> &'static str {
        match self {
            Field::Any => "",
            Field::Title => "title",
            Field::Creator => "creator",
            Field::Publisher => "publisher",
            Field::Tag => "tag",
            Field::Notes => "notes",
            Field::Code => "isbn",
            Field::Year => "year",
            Field::Type => "type",
            Field::In => "in",
            Field::Loan => "loan",
            Field::Idea => "idea",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    pub field: Field,
    pub text: String,
    pub phrase: bool,
}

impl Term {
    pub fn has_wildcards(&self) -> bool {
        self.text.contains(['*', '?'])
    }

    /// A word ending in a single `*` and otherwise plain: FTS can do it as a
    /// prefix query.
    pub fn is_prefix(&self) -> bool {
        !self.phrase
            && self.text.len() > 1
            && self.text.ends_with('*')
            && !self.text[..self.text.len() - 1].contains(['*', '?'])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
    Term(Term),
    /// Matches everything (an empty query or brackets).
    All,
}

impl Node {
    /// Visits every term with whether it sits under an odd number of NOTs.
    pub fn walk_terms<'a>(&'a self, negated: bool, f: &mut impl FnMut(&'a Term, bool)) {
        match self {
            Node::And(v) | Node::Or(v) => v.iter().for_each(|n| n.walk_terms(negated, f)),
            Node::Not(n) => n.walk_terms(!negated, f),
            Node::Term(t) => f(t, negated),
            Node::All => {}
        }
    }

    /// Positive free-text terms: the words the user is "looking for", used
    /// for ranking, match explanations and meaning-based widening.
    pub fn positive_text_terms(&self) -> Vec<&Term> {
        let mut out = Vec::new();
        self.walk_terms(false, &mut |t, neg| {
            if !neg && t.field.is_text() {
                out.push(t);
            }
        });
        out
    }

    pub fn idea_texts(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.walk_terms(false, &mut |t, _| {
            if t.field == Field::Idea && !out.contains(&t.text) {
                out.push(t.text.clone());
            }
        });
        out
    }

    /// The query with positive free-text terms replaced by "anything":
    /// what remains are the structural constraints (type, year, place, NOTs).
    pub fn structural(&self) -> Node {
        fn go(n: &Node, negated: bool) -> Node {
            match n {
                Node::And(v) => simplify_and(v.iter().map(|c| go(c, negated)).collect()),
                Node::Or(v) => {
                    let kids: Vec<Node> = v.iter().map(|c| go(c, negated)).collect();
                    if kids.contains(&Node::All) {
                        Node::All
                    } else {
                        Node::Or(kids)
                    }
                }
                Node::Not(inner) => Node::Not(Box::new(go(inner, !negated))),
                Node::Term(t) if !negated && t.field.is_text() => Node::All,
                other => other.clone(),
            }
        }
        go(self, false)
    }
}

fn simplify_and(kids: Vec<Node>) -> Node {
    let kids: Vec<Node> = kids.into_iter().filter(|k| *k != Node::All).collect();
    match kids.len() {
        0 => Node::All,
        1 => kids.into_iter().next().expect("one"),
        _ => Node::And(kids),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Word(String),
    Phrase(String),
    Field(Field),
    LParen,
    RParen,
    And,
    Or,
    Not,
}

fn tokenize(input: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '(' {
            toks.push(Tok::LParen);
            i += 1;
        } else if c == ')' {
            toks.push(Tok::RParen);
            i += 1;
        } else if c == '"' || c == '“' || c == '”' {
            let mut j = i + 1;
            let mut s = String::new();
            while j < chars.len() && !matches!(chars[j], '"' | '“' | '”') {
                s.push(chars[j]);
                j += 1;
            }
            toks.push(Tok::Phrase(s.trim().to_owned()));
            i = j + 1;
        } else if c == '-'
            && toks.last().is_none_or(|t| !matches!(t, Tok::Field(_)))
            && chars.get(i + 1).is_some_and(|n| !n.is_whitespace())
        {
            toks.push(Tok::Not);
            i += 1;
        } else {
            let mut j = i;
            let mut s = String::new();
            while j < chars.len()
                && !chars[j].is_whitespace()
                && !matches!(chars[j], '(' | ')' | '"' | '“' | '”')
            {
                s.push(chars[j]);
                j += 1;
            }
            i = j;
            // field:value — the value may follow immediately or be a phrase/bracket.
            if let Some((name, rest)) = s.split_once(':')
                && let Some(field) = Field::from_name(name)
            {
                toks.push(Tok::Field(field));
                if !rest.is_empty() {
                    toks.push(Tok::Word(rest.to_owned()));
                }
                continue;
            }
            match s.as_str() {
                "AND" | "&&" => toks.push(Tok::And),
                "OR" | "||" | "|" => toks.push(Tok::Or),
                "NOT" => toks.push(Tok::Not),
                _ => {
                    let w = s
                        .trim_matches(|c: char| matches!(c, ',' | ';' | ':'))
                        .to_owned();
                    if !w.is_empty() {
                        toks.push(Tok::Word(w));
                    }
                }
            }
        }
    }
    toks
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    depth: usize,
}

const MAX_DEPTH: usize = 16;
const MAX_TERMS: usize = 40;

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn or(&mut self, field: Field) -> Node {
        let mut kids = vec![self.and(field)];
        while self.peek() == Some(&Tok::Or) {
            self.next();
            kids.push(self.and(field));
        }
        let kids: Vec<Node> = kids.into_iter().collect();
        if kids.contains(&Node::All) {
            // "x OR" with nothing after it: drop the empty side.
            let rest: Vec<Node> = kids.into_iter().filter(|k| *k != Node::All).collect();
            return match rest.len() {
                0 => Node::All,
                1 => rest.into_iter().next().expect("one"),
                _ => Node::Or(rest),
            };
        }
        if kids.len() == 1 {
            kids.into_iter().next().expect("one")
        } else {
            Node::Or(kids)
        }
    }

    fn and(&mut self, field: Field) -> Node {
        let mut kids = Vec::new();
        loop {
            match self.peek() {
                None | Some(Tok::Or) | Some(Tok::RParen) => break,
                Some(Tok::And) => {
                    self.next();
                }
                _ => kids.push(self.unary(field)),
            }
        }
        simplify_and(kids)
    }

    fn unary(&mut self, field: Field) -> Node {
        if self.peek() == Some(&Tok::Not) {
            self.next();
            return match self.unary(field) {
                Node::All => Node::All,
                n => Node::Not(Box::new(n)),
            };
        }
        self.atom(field)
    }

    fn atom(&mut self, field: Field) -> Node {
        match self.next() {
            Some(Tok::LParen) => {
                if self.depth >= MAX_DEPTH {
                    return Node::All;
                }
                self.depth += 1;
                let n = self.or(field);
                self.depth -= 1;
                if self.peek() == Some(&Tok::RParen) {
                    self.next();
                }
                n
            }
            Some(Tok::Field(f)) => match self.peek() {
                Some(Tok::LParen) => self.atom(f),
                Some(Tok::Word(_)) | Some(Tok::Phrase(_)) => self.atom(f),
                // "loan:" on its own means "anything on loan".
                _ if f == Field::Loan => term(f, "*", false),
                _ => Node::All,
            },
            Some(Tok::Word(w)) => {
                // Inside idea:( … ) or after idea: keep plain words together as one idea.
                term(field, &w, false)
            }
            Some(Tok::Phrase(p)) => {
                if p.is_empty() {
                    Node::All
                } else {
                    term(field, &p, true)
                }
            }
            // Stray operators or a lone ")".
            Some(Tok::And) | Some(Tok::Or) | Some(Tok::Not) | Some(Tok::RParen) | None => Node::All,
        }
    }
}

fn term(field: Field, text: &str, phrase: bool) -> Node {
    Node::Term(Term {
        field,
        text: text.to_owned(),
        phrase,
    })
}

/// Parses a query. Never fails: malformed input degrades to something sensible.
pub fn parse(input: &str) -> Node {
    let toks = tokenize(input);
    let mut p = Parser {
        toks,
        pos: 0,
        depth: 0,
    };
    let mut parts = vec![p.or(Field::Any)];
    // Unbalanced ")" stops the top level; keep going with what follows.
    while p.pos < p.toks.len() {
        p.next();
        parts.push(p.or(Field::Any));
    }
    let node = simplify_and(parts);
    limit_terms(node)
}

fn limit_terms(node: Node) -> Node {
    let mut n = 0;
    node.walk_terms(false, &mut |_, _| n += 1);
    if n <= MAX_TERMS {
        return node;
    }
    // Pathological input: keep only the first terms, ANDed.
    let mut kept = Vec::new();
    node.walk_terms(false, &mut |t, neg| {
        if kept.len() < MAX_TERMS {
            let n = Node::Term(t.clone());
            kept.push(if neg { Node::Not(Box::new(n)) } else { n });
        }
    });
    Node::And(kept)
}

/// Renders a node back to query text (used to show the advanced form's
/// combined query, and in tests).
pub fn render(node: &Node) -> String {
    fn go(n: &Node, top: bool) -> String {
        match n {
            Node::All => String::new(),
            Node::Term(t) => {
                let v = if t.phrase || t.text.contains(char::is_whitespace) {
                    format!("\"{}\"", t.text.replace('"', ""))
                } else {
                    t.text.clone()
                };
                if t.field == Field::Any {
                    v
                } else {
                    format!("{}:{v}", t.field.name())
                }
            }
            Node::Not(inner) => format!("NOT {}", go(inner, false)),
            Node::And(v) => {
                let s = v
                    .iter()
                    .map(|c| go(c, false))
                    .collect::<Vec<_>>()
                    .join(" AND ");
                if top { s } else { format!("({s})") }
            }
            Node::Or(v) => {
                let s = v
                    .iter()
                    .map(|c| go(c, false))
                    .collect::<Vec<_>>()
                    .join(" OR ");
                if top { s } else { format!("({s})") }
            }
        }
    }
    go(node, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(field: Field, text: &str) -> Node {
        term(field, text, false)
    }
    fn p(field: Field, text: &str) -> Node {
        term(field, text, true)
    }

    #[test]
    fn empty_and_whitespace() {
        assert_eq!(parse(""), Node::All);
        assert_eq!(parse("   "), Node::All);
        assert_eq!(parse("()"), Node::All);
    }

    #[test]
    fn implicit_and() {
        assert_eq!(
            parse("le guin"),
            Node::And(vec![t(Field::Any, "le"), t(Field::Any, "guin")])
        );
    }

    #[test]
    fn lowercase_operators_are_words() {
        let n = parse("the old man and the sea");
        let mut words = 0;
        n.walk_terms(false, &mut |_, _| words += 1);
        assert_eq!(words, 6);
    }

    #[test]
    fn precedence_and_binds_tighter_than_or() {
        assert_eq!(
            parse("a b OR c"),
            Node::Or(vec![
                Node::And(vec![t(Field::Any, "a"), t(Field::Any, "b")]),
                t(Field::Any, "c")
            ])
        );
        assert_eq!(
            parse("a (b OR c)"),
            Node::And(vec![
                t(Field::Any, "a"),
                Node::Or(vec![t(Field::Any, "b"), t(Field::Any, "c")])
            ])
        );
    }

    #[test]
    fn not_and_minus() {
        assert_eq!(
            parse("le guin NOT film"),
            Node::And(vec![
                t(Field::Any, "le"),
                t(Field::Any, "guin"),
                Node::Not(Box::new(t(Field::Any, "film")))
            ])
        );
        assert_eq!(parse("-film"), Node::Not(Box::new(t(Field::Any, "film"))));
        assert_eq!(
            parse("spider-man"),
            t(Field::Any, "spider-man"),
            "inner hyphen is part of the word"
        );
        assert_eq!(parse("year:-5"), t(Field::Year, "-5"));
    }

    #[test]
    fn phrases_including_curly_quotes() {
        assert_eq!(parse("\"the left hand\""), p(Field::Any, "the left hand"));
        assert_eq!(
            parse("“films for a rainy Sunday”"),
            p(Field::Any, "films for a rainy Sunday")
        );
        assert_eq!(
            parse("\"unterminated phrase"),
            p(Field::Any, "unterminated phrase")
        );
    }

    #[test]
    fn fields_and_synonyms() {
        assert_eq!(
            parse("title:earth* author:\"le guin\" year:1960..1975"),
            Node::And(vec![
                t(Field::Title, "earth*"),
                p(Field::Creator, "le guin"),
                t(Field::Year, "1960..1975")
            ])
        );
        assert_eq!(parse("director:miyazaki"), t(Field::Creator, "miyazaki"));
        assert_eq!(parse("in:shed"), t(Field::In, "shed"));
        assert_eq!(parse("loan:*"), t(Field::Loan, "*"));
        assert_eq!(parse("loan:"), t(Field::Loan, "*"));
        assert_eq!(
            parse("idea:\"lonely lighthouse keepers\""),
            p(Field::Idea, "lonely lighthouse keepers")
        );
    }

    #[test]
    fn field_applies_to_brackets() {
        assert_eq!(
            parse("type:(book OR film)"),
            Node::Or(vec![t(Field::Type, "book"), t(Field::Type, "film")])
        );
    }

    #[test]
    fn unknown_field_is_text() {
        assert_eq!(parse("foo:bar"), t(Field::Any, "foo:bar"));
        assert_eq!(
            parse("note: hello"),
            t(Field::Notes, "hello"),
            "space after the colon is fine"
        );
        assert_eq!(
            parse("title: OR x"),
            t(Field::Any, "x"),
            "field with nothing after it is dropped"
        );
    }

    #[test]
    fn malformed_input_degrades_gracefully() {
        assert_eq!(parse("a OR"), t(Field::Any, "a"));
        assert_eq!(parse("AND OR NOT"), Node::All);
        assert_eq!(
            parse("(a b"),
            Node::And(vec![t(Field::Any, "a"), t(Field::Any, "b")])
        );
        assert_eq!(
            parse("a) b"),
            Node::And(vec![t(Field::Any, "a"), t(Field::Any, "b")])
        );
        let deep = "(".repeat(100) + "x" + &")".repeat(100);
        let _ = parse(&deep); // must not overflow the stack
        let many = (0..200)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut n = 0;
        parse(&many).walk_terms(false, &mut |_, _| n += 1);
        assert_eq!(n, MAX_TERMS);
    }

    #[test]
    fn wildcard_kinds() {
        let Node::Term(prefix) = parse("sea*") else {
            panic!()
        };
        assert!(prefix.is_prefix() && prefix.has_wildcards());
        let Node::Term(inner) = parse("gr?y") else {
            panic!()
        };
        assert!(!inner.is_prefix() && inner.has_wildcards());
        let Node::Term(both) = parse("*essays*") else {
            panic!()
        };
        assert!(!both.is_prefix());
        let Node::Term(star) = parse("*") else {
            panic!()
        };
        assert!(!star.is_prefix());
    }

    #[test]
    fn structural_strips_positive_text_only() {
        let n = parse("ghibli type:film NOT ponyo year:1990..");
        let s = n.structural();
        assert_eq!(
            s,
            Node::And(vec![
                t(Field::Type, "film"),
                Node::Not(Box::new(t(Field::Any, "ponyo"))),
                t(Field::Year, "1990..")
            ])
        );
        assert_eq!(parse("sea").structural(), Node::All);
        assert_eq!(parse("sea OR type:film").structural(), Node::All);
    }

    #[test]
    fn term_collections() {
        let n = parse("sea title:old NOT whale idea:ocean idea:ocean in:shed");
        let texts: Vec<&str> = n
            .positive_text_terms()
            .iter()
            .map(|t| t.text.as_str())
            .collect();
        assert_eq!(texts, vec!["sea", "old"]);
        assert_eq!(n.idea_texts(), vec!["ocean"]);
    }

    #[test]
    fn render_round_trips() {
        for q in [
            "title:earth* AND creator:\"le guin\" AND year:1960..1975",
            "(a OR b) AND NOT c",
            "idea:\"islands and wizards\"",
        ] {
            assert_eq!(parse(&render(&parse(q))), parse(q), "{q}");
        }
    }
}
