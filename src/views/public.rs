//! Pages anyone can see: the front page, search, items and lending.

use maud::{Markup, html};

use super::{Ctx, Header, date_long, date_short, decade_label, page, urlencode};
use crate::db::items::{Item, Summary};
use crate::db::loans::Loan;
use crate::db::locations::Location;
use crate::db::photos::Photo;
use crate::db::types::{FieldDef, FieldKind};
use crate::search::{Availability, Hit, Results, Sort};

/// Values for the "Showing … kept … from …" filter sentence.
#[derive(Debug, Clone, Default)]
pub struct FilterChoices {
    pub places: Vec<Location>,
    pub decades: Vec<i64>,
}

/// The current search, echoed back into forms and links.
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    pub q: String,
    pub type_slug: String,
    pub place: Option<i64>,
    pub decade: Option<i64>,
    pub availability: Availability,
    pub sort: Sort,
}

impl SearchState {
    pub fn query_string(&self, offset: usize) -> String {
        let mut parts = vec![format!("q={}", urlencode(&self.q))];
        if !self.type_slug.is_empty() {
            parts.push(format!("type={}", urlencode(&self.type_slug)));
        }
        if let Some(p) = self.place {
            parts.push(format!("place={p}"));
        }
        if let Some(d) = self.decade {
            parts.push(format!("decade={d}"));
        }
        match self.availability {
            Availability::Any => {}
            Availability::OnShelf => parts.push("avail=in".into()),
            Availability::OnLoan => parts.push("avail=out".into()),
        }
        if self.sort != Sort::Relevance {
            parts.push(format!("sort={}", self.sort.as_str()));
        }
        if offset > 0 {
            parts.push(format!("offset={offset}"));
        }
        parts.join("&")
    }
}

const GRAMMAR: &[(&str, &str)] = &[
    ("sea*", "sea, seas, seaside…"),
    ("gr?y", "grey or gray"),
    ("AND OR NOT", "combine; group with ( )"),
    ("\"…\"", "exact phrase"),
    ("year:", "1990, 1984..2001 or 1980s"),
    ("creator:", "author, artist, director"),
    ("type:", "book, music, film, camping…"),
    ("idea:", "matched by meaning"),
    ("in:", "a place, e.g. in:shed"),
    ("loan:*", "anything lent out; loan:sam"),
];

pub fn grammar() -> Markup {
    html! {
        aside.grammar aria-label="Search grammar" {
            h3.kicker { "Search grammar" }
            dl {
                @for (k, v) in GRAMMAR {
                    dt { (k) }
                    dd { (v) }
                }
            }
        }
    }
}

/// The advanced search form (inline on the front page, or its own page).
pub fn advanced_form(ctx: &Ctx, q: &str) -> Markup {
    html! {
        section.advanced aria-labelledby="adv-title" {
            form.main action="/search" method="get" {
                h2 #adv-title { "Advanced search" }
                div.grid-fields {
                    div.field {
                        label for="adv-title-in" { "Title" }
                        input #adv-title-in.u-input.adv-src name="title" placeholder="the * of *" autocomplete="off";
                    }
                    div.field {
                        label for="adv-creator" { "Author, artist or director" }
                        input #adv-creator.u-input.adv-src name="creator" placeholder="bowie OR eno" autocomplete="off";
                    }
                    div.field {
                        span.label #adv-year-label { "Released" }
                        div.checks role="group" aria-labelledby="adv-year-label" {
                            input.u-input.adv-src name="from" inputmode="numeric" placeholder="from" aria-label="From year" size="6";
                            i.serif { "to" }
                            input.u-input.adv-src name="to" inputmode="numeric" placeholder="to" aria-label="To year" size="6";
                        }
                    }
                    fieldset.field.dashed-box {
                        legend.label { "Media" }
                        div.checks {
                            @for t in &ctx.types {
                                label { input.adv-src type="checkbox" name="types" value=(t.slug); (t.section) }
                            }
                        }
                    }
                    div.field {
                        label for="adv-loan" { "On loan to" }
                        input #adv-loan.u-input.adv-src name="loan" placeholder="* for anyone" autocomplete="off";
                    }
                    div.field.full {
                        label for="adv-idea" { "Idea or mood — searched by meaning" }
                        input #adv-idea.u-input.italic.adv-src name="idea" placeholder="gentle, flying machines, growing up" autocomplete="off";
                    }
                    div.field.full {
                        label for="adv-q" { "Query, as text" }
                        (query_preview(q))
                        span.help { "The fields above write this for you. You can also edit it directly." }
                    }
                }
                div.row-actions {
                    button.btn.solid type="submit" { "Search" }
                    a.btn href="/advanced" { "Reset" }
                }
            }
            (grammar())
        }
    }
}

/// The live "Query, as text" box. Re-rendered as fields change.
pub fn query_preview(q: &str) -> Markup {
    html! {
        textarea #adv-q.box-input name="q" rows="2"
            hx-get="/advanced/preview"
            hx-include="closest form"
            hx-trigger="input changed delay:300ms from:.adv-src, change from:.adv-src"
            hx-target="this" hx-swap="outerHTML" { (q) }
    }
}

pub fn landing(ctx: &Ctx, recent: &[Summary]) -> Markup {
    let body = html! {
        section.hero-search {
            @if let Some(src) = &ctx.art.margin_left { img.margin-art src=(src) alt=""; }
            form.search-line role="search" action="/search" method="get" {
                label.kicker.accent for="q" { "Search the catalogue" }
                div.line {
                    input #q type="search" name="q" placeholder="What are you looking for?" autocomplete="off" autofocus;
                    button.go type="submit" aria-label="Search" { "→" }
                }
                div.hint {
                    span { "Words, names, years — or an idea: " i { "“films for a rainy Sunday”" } }
                    a.link-btn.accent href="/advanced" data-panel="adv-panel" aria-expanded="false" aria-controls="adv-panel" {
                        "Advanced search ↓"
                    }
                }
            }
            @if let Some(src) = &ctx.art.margin_right { img.margin-art src=(src) alt=""; }
        }
        div #adv-panel hidden { (advanced_form(ctx, "")) }
        @if !recent.is_empty() {
            section.see-also aria-labelledby="recent-title" {
                h2 #recent-title .kicker { "Recently added" }
                @for s in recent {
                    a href={ "/items/" (s.id) } {
                        strong { (s.title) }
                        span { (summary_meta(s)) }
                    }
                }
            }
        }
    };
    page(ctx, "", Header::Masthead, body)
}

pub fn advanced_page(ctx: &Ctx, q: &str) -> Markup {
    page(ctx, "Advanced search", Header::Plain, advanced_form(ctx, q))
}

fn summary_meta(s: &Summary) -> String {
    let mut parts = vec![];
    if !s.creator.is_empty() {
        parts.push(s.creator.clone());
    }
    if let Some(y) = s.year {
        parts.push(y.to_string());
    }
    parts.push(type_label(s));
    parts.join(" · ")
}

fn type_label(s: &Summary) -> String {
    if !s.format.is_empty() && s.format.len() <= 24 {
        s.format.clone()
    } else {
        s.type_name
            .rsplit('·')
            .next()
            .unwrap_or(&s.type_name)
            .trim()
            .to_owned()
    }
}

pub fn results_page(
    ctx: &Ctx,
    state: &SearchState,
    choices: &FilterChoices,
    results: &Results,
    offset: usize,
) -> Markup {
    let title = if state.q.is_empty() {
        "Catalogue".to_owned()
    } else {
        format!("“{}”", state.q)
    };
    page(
        ctx,
        &title,
        Header::Search(state.q.clone()),
        results_section(ctx, state, choices, results, offset),
    )
}

/// The whole results area: filter sentence, counts, list, see-also.
pub fn results_section(
    ctx: &Ctx,
    state: &SearchState,
    choices: &FilterChoices,
    results: &Results,
    offset: usize,
) -> Markup {
    html! {
        section #results aria-live="polite" {
            (filter_sentence(ctx, state, choices))
            p.counts { (counts_line(results)) }
            @if results.hits.is_empty() {
                div.empty {
                    @if let Some(src) = &ctx.art.empty { img src=(src) alt=""; }
                    p { "Nothing on the shelves matches that." }
                    p.small.muted { "Try fewer words, a wildcard like " span.mono { "sea*" } ", or describe the idea instead." }
                }
            } @else {
                ol #result-list.results {
                    (result_rows(ctx, results.hits.as_slice(), offset))
                }
                (more_button(state, results, offset))
            }
            @if !results.see_also.is_empty() {
                section.see-also aria-labelledby="see-also-title" {
                    h2 #see-also-title .kicker { "See also — related by idea, not by name" }
                    @for h in &results.see_also {
                        a href={ "/items/" (h.item.id) } {
                            strong { (h.item.title) }
                            span { (summary_meta(&h.item)) }
                        }
                    }
                }
            }
        }
    }
}

pub fn more_button(state: &SearchState, results: &Results, offset: usize) -> Markup {
    let next = offset + results.hits.len();
    html! {
        div #more.more {
            @if results.has_more {
                a.btn href={ "/search?" (state.query_string(next)) }
                    hx-get={ "/search?" (state.query_string(next)) }
                    hx-target="#result-list" hx-swap="beforeend" {
                    "Load more"
                }
            }
        }
    }
}

/// Rows appended by "Load more", plus the replacement button.
pub fn more_rows(ctx: &Ctx, state: &SearchState, results: &Results, offset: usize) -> Markup {
    html! {
        (result_rows(ctx, &results.hits, offset))
        hx-partial hx-target="#more" hx-swap="outerHTML" { (more_button(state, results, offset)) }
    }
}

fn filter_sentence(ctx: &Ctx, s: &SearchState, choices: &FilterChoices) -> Markup {
    html! {
        form.filters action="/search" method="get"
            hx-get="/search" hx-target="#results" hx-swap="outerHTML" hx-trigger="change" hx-push-url="true" {
            input type="hidden" name="q" value=(s.q);
            p.sentence {
                "Showing "
                select name="type" aria-label="Kind of thing" {
                    option value="" selected[s.type_slug.is_empty()] { "everything" }
                    @for t in &ctx.types {
                        option value=(t.slug) selected[s.type_slug == t.slug] { (t.section.to_lowercase()) }
                    }
                }
                " kept "
                select name="place" aria-label="Place" {
                    option value="" selected[s.place.is_none()] { "anywhere" }
                    @for l in &choices.places {
                        option value=(l.id) selected[s.place == Some(l.id)] { "in " (l.path) }
                    }
                }
                ", from "
                select name="decade" aria-label="Period" {
                    option value="" selected[s.decade.is_none()] { "any year" }
                    @for d in &choices.decades {
                        option value=(d) selected[s.decade == Some(*d)] { (decade_label(*d)) }
                    }
                }
                ", "
                select name="avail" aria-label="Availability" {
                    option value="" selected[s.availability == Availability::Any] { "in or out" }
                    option value="in" selected[s.availability == Availability::OnShelf] { "on the shelf" }
                    option value="out" selected[s.availability == Availability::OnLoan] { "on loan" }
                }
                ", sorted by "
                select name="sort" aria-label="Sort order" {
                    option value="relevance" selected[s.sort == Sort::Relevance] { "relevance" }
                    option value="title" selected[s.sort == Sort::Title] { "title" }
                    option value="year" selected[s.sort == Sort::Year] { "year" }
                    option value="added" selected[s.sort == Sort::Added] { "date added" }
                }
                "."
            }
            noscript { button.btn.small type="submit" { "Apply" } }
        }
    }
}

pub fn counts_line(r: &Results) -> String {
    let entries = if r.total == 1 {
        "1 entry".to_owned()
    } else {
        format!("{} entries", r.total)
    };
    if r.has_text && r.by_idea > 0 {
        format!("{entries} · {} by name, {} by idea", r.by_name, r.by_idea)
    } else {
        entries
    }
}

fn result_rows(ctx: &Ctx, hits: &[Hit], offset: usize) -> Markup {
    html! {
        @for (i, h) in hits.iter().enumerate() {
            (result_row(ctx, &h.item, h.reason.as_deref(), offset + i + 1, LoanUi::Closed))
        }
    }
}

/// One entry in a result list. `loan_open` shows the inline lending form.
/// Whether a result row shows the inline lending form (and any error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoanUi<'a> {
    Closed,
    Open(Option<&'a str>),
}

pub fn result_row(
    ctx: &Ctx,
    s: &Summary,
    reason: Option<&str>,
    number: usize,
    loan: LoanUi,
) -> Markup {
    let loan_open = matches!(loan, LoanUi::Open(_));
    let loan_error = match loan {
        LoanUi::Open(e) => e,
        LoanUi::Closed => None,
    };
    let mut meta = vec![type_label(s)];
    if let Some(y) = s.year {
        meta.push(y.to_string());
    }
    if let Some(p) = &s.location_path {
        meta.push(p.clone());
    }
    let id = s.id;
    html! {
        li.result #{ "item-" (id) } data-number=(number) {
            div.row {
                span.num aria-hidden="true" { (format!("{number:02}")) }
                div.body {
                    h3 { a href={ "/items/" (id) } { (s.title) } }
                    @if !s.creator.is_empty() { span.creator { (s.creator) } }
                    span.meta {
                        (meta.join(" · "))
                        @if let Some(r) = reason { " · " span.why { (r) } }
                    }
                    @if let Some(l) = &s.loan {
                        span.out { "Out — with " (l.borrower) " since " (date_short(l.since)) "." }
                    }
                }
                @if let Some(photo) = &s.first_photo {
                    a.thumb href={ "/items/" (id) "#photos" }
                        aria-label={ "Photos of " (s.title) (if s.first_photo_caption.is_empty() { String::new() } else { format!(" ({})", s.first_photo_caption) }) } {
                        img src={ "/photos/" (crate::media::thumb_name(photo)) } alt="" loading="lazy" width="54" height="74";
                        span.count aria-hidden="true" { (s.photo_count) }
                    }
                }
                div.actions {
                    @if s.loan.is_none() {
                        a.btn.caps href={ "/items/" (id) "/loan?n=" (number) }
                            hx-get={ "/items/" (id) "/loan?n=" (number) } hx-target={ "#item-" (id) } hx-swap="outerHTML"
                            .solid[loan_open] { "Loan" }
                    } @else {
                        form method="post" action={ "/items/" (id) "/return" }
                            hx-post={ "/items/" (id) "/return?n=" (number) } hx-target={ "#item-" (id) } hx-swap="outerHTML" {
                            button.btn.caps type="submit" { "Returned" }
                        }
                    }
                    @if ctx.admin {
                        a.edit href={ "/admin/items/" (id) "/edit" } { "Edit" }
                    }
                }
            }
            @if loan_open && s.loan.is_none() {
                (inline_loan_form(s, number, loan_error))
            }
        }
    }
}

pub fn inline_loan_form(s: &Summary, number: usize, error: Option<&str>) -> Markup {
    let id = s.id;
    html! {
        form.loan-form method="post" action={ "/items/" (id) "/loan" }
            hx-post={ "/items/" (id) "/loan?n=" (number) } hx-target={ "#item-" (id) } hx-swap="outerHTML" {
            span.title { "Lend " i { (s.title) } " to —" }
            div.field {
                label for={ "borrower-" (id) } { "Borrower's name" }
                input.u-input id={ "borrower-" (id) } name="borrower" autocomplete="name" required maxlength="80" autofocus;
                @if let Some(e) = error { span.error { (e) } }
            }
            span.dated { "dated " b { (date_long(crate::db::now())) } }
            button.btn.accent type="submit" { "Confirm loan" }
            a.link-btn href={ "/items/" (id) }
                hx-get={ "/items/" (id) "/row?n=" (number) } hx-target={ "#item-" (id) } hx-swap="outerHTML" { "Cancel" }
        }
    }
}

pub struct ItemView<'a> {
    pub item: &'a Item,
    pub fields: &'a [(FieldDef, String)],
    pub photos: &'a [Photo],
    pub loans: &'a [Loan],
    pub similar: &'a [Summary],
}

pub fn item_page(ctx: &Ctx, v: &ItemView, flash: Option<&str>) -> Markup {
    let it = v.item;
    let body = html! {
        @if let Some(f) = flash { p.flash role="status" { (f) } }
        article.item-page {
            div.main {
                span.kicker.accent { (it.type_name) }
                h1 { (it.title) }
                @if !it.creator.is_empty() { span.creator { (it.creator) } }
                dl.facts {
                    @if let Some(y) = it.year { dt { "Year" } dd { (y) } }
                    @if !it.publisher.is_empty() { dt { "Publisher" } dd { (it.publisher) } }
                    @if !it.format.is_empty() { dt { "Format" } dd { (it.format) } }
                    @if !it.identifier.is_empty() { dt { "ISBN / code" } dd.mono { (it.identifier) } }
                    dt { "Where" }
                    dd {
                        @if let Some(p) = &it.location_path {
                            a href={ "/search?q=" (urlencode(&format!("in:\"{p}\""))) } { (p) }
                        } @else { span.muted { "Not recorded" } }
                    }
                    @if !it.tags.is_empty() { dt { "Tags" } dd { (it.tags) } }
                    @for (def, value) in v.fields {
                        @if !value.is_empty() {
                            dt { (def.name) }
                            dd { (if def.kind == FieldKind::Bool { "Yes".to_owned() } else { value.clone() }) }
                        }
                    }
                    dt { "Added" } dd { (date_long(it.created_at)) }
                }
                @if !it.description.is_empty() { div.prose { (it.description) } }
                @if !it.notes.is_empty() {
                    section {
                        h2.kicker { "Notes" }
                        div.prose { (it.notes) }
                    }
                }
                @if !v.photos.is_empty() {
                    section #photos aria-labelledby="photos-title" {
                        h2 #photos-title .kicker { "Photos" }
                        div.gallery {
                            @for p in v.photos {
                                figure {
                                    a href={ "/photos/" (p.file) } {
                                        img src={ "/photos/" (crate::media::thumb_name(&p.file)) }
                                            alt=(if p.caption.is_empty() { format!("Photo of {}", it.title) } else { p.caption.clone() })
                                            loading="lazy";
                                    }
                                    @if !p.caption.is_empty() { figcaption { (p.caption) } }
                                    @if ctx.admin {
                                        div.photo-tools {
                                            form method="post" action={ "/admin/items/" (it.id) "/photos/" (p.id) "/first" } {
                                                button.link-btn.small type="submit" { "Show first" }
                                            }
                                            form method="post" action={ "/admin/items/" (it.id) "/photos/" (p.id) "/delete" } data-confirm="Remove this photo?" {
                                                button.link-btn.small type="submit" { "Remove" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                @if !v.similar.is_empty() {
                    section.see-also aria-labelledby="similar-title" {
                        h2 #similar-title .kicker { "Similar in spirit" }
                        @for s in v.similar {
                            a href={ "/items/" (s.id) } { strong { (s.title) } span { (summary_meta(s)) } }
                        }
                    }
                }
            }
            div.side {
                (loan_panel(it.id, &it.title, v.loans, None))
                @if ctx.admin {
                    section.panel aria-labelledby="admin-title" {
                        h2 #admin-title { "Librarian" }
                        a.btn.solid href={ "/admin/items/" (it.id) "/edit" } { "Edit this entry" }
                        form method="post" action={ "/admin/items/" (it.id) "/photos" } enctype="multipart/form-data" {
                            div.photo-drop {
                                label.drop for="more-photos" { "+ Photos" small { "camera or library" } }
                                input #more-photos type="file" name="photos" accept="image/*" multiple data-preview="more-previews";
                                div #more-previews.photo-previews {}
                            }
                            div.field {
                                label for="photo-caption" { "Caption (optional)" }
                                input #photo-caption.u-input name="caption" maxlength="120";
                            }
                            button.btn type="submit" { "Upload" }
                        }
                        form method="post" action={ "/admin/items/" (it.id) "/delete" } data-confirm="Delete this entry for good?" {
                            button.link-btn type="submit" { "Delete entry" }
                        }
                    }
                }
            }
        }
    };
    page(ctx, &it.title, Header::Search(String::new()), body)
}

/// Lending status and form on an item's page.
pub fn loan_panel(item_id: i64, title: &str, loans: &[Loan], error: Option<&str>) -> Markup {
    let active = loans.iter().find(|l| l.returned_at.is_none());
    let past: Vec<&Loan> = loans
        .iter()
        .filter(|l| l.returned_at.is_some())
        .take(10)
        .collect();
    html! {
        section #loan-panel.panel aria-labelledby="loan-title" {
            h2 #loan-title { "Lending" }
            @if let Some(l) = active {
                p.serif.italic { "Out — with " (l.borrower) " since " (date_long(l.loaned_at)) "." }
                form method="post" action={ "/items/" (item_id) "/return" }
                    hx-post={ "/items/" (item_id) "/return?ctx=panel" } hx-target="#loan-panel" hx-swap="outerHTML" {
                    button.btn.caps type="submit" { "Returned" }
                }
            } @else {
                form method="post" action={ "/items/" (item_id) "/loan" }
                    hx-post={ "/items/" (item_id) "/loan?ctx=panel" } hx-target="#loan-panel" hx-swap="outerHTML" {
                    div.field {
                        label for="panel-borrower" { "Lend " i { (title) } " to" }
                        input #panel-borrower.u-input name="borrower" autocomplete="name" required maxlength="80" placeholder="Borrower's name";
                        @if let Some(e) = error { span.error { (e) } }
                    }
                    p.small.muted { "Dated " (date_long(crate::db::now())) " automatically. Loans are visible to everyone." }
                    button.btn.accent.block type="submit" { "Confirm loan" }
                }
            }
            @if !past.is_empty() {
                h3.kicker { "History" }
                ul.history {
                    @for l in past {
                        li { (l.borrower) " — " (date_short(l.loaned_at)) " → " (l.returned_at.map(date_short).unwrap_or_default()) }
                    }
                }
            }
        }
    }
}

/// A full page with the lending form, for browsers without JavaScript.
pub fn loan_page(ctx: &Ctx, s: &Summary, error: Option<&str>) -> Markup {
    page(
        ctx,
        &format!("Lend {}", s.title),
        Header::Search(String::new()),
        html! { ol.results { li.result { (inline_loan_form(s, 1, error)) } } },
    )
}

pub fn unlock_page(ctx: &Ctx, next: &str, error: Option<&str>) -> Markup {
    page(
        ctx,
        "Librarian",
        Header::Plain,
        html! {
            form.unlock-page method="post" action="/unlock" {
                h1.page-title { "Librarian's desk" }
                @if !ctx.admin_configured {
                    p.flash.err { "No admin password is set yet. Run " span.mono { "library hash-password" } " and add the result to the config file." }
                }
                @if let Some(e) = error { p.flash.err role="alert" { (e) } }
                input type="hidden" name="next" value=(next);
                div.field {
                    label for="unlock-password-page" { "Admin password" }
                    input #unlock-password-page.u-input type="password" name="password" autocomplete="current-password" required autofocus;
                }
                p.small.muted { "Editing stays unlocked until you close the browser." }
                button.btn.solid type="submit" { "Unlock editing" }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_string_round_trip() {
        let s = SearchState {
            q: "le guin".into(),
            type_slug: "book".into(),
            place: Some(3),
            decade: Some(1960),
            availability: Availability::OnShelf,
            sort: Sort::Year,
        };
        assert_eq!(
            s.query_string(20),
            "q=le+guin&type=book&place=3&decade=1960&avail=in&sort=year&offset=20"
        );
        assert_eq!(SearchState::default().query_string(0), "q=");
    }

    #[test]
    fn counts_wording() {
        let mut r = Results {
            total: 7,
            by_name: 5,
            by_idea: 2,
            has_text: true,
            ..Default::default()
        };
        assert_eq!(counts_line(&r), "7 entries · 5 by name, 2 by idea");
        r.by_idea = 0;
        r.total = 1;
        assert_eq!(counts_line(&r), "1 entry");
    }
}
