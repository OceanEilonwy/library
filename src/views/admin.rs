//! Librarian-only pages: adding and editing items, lookups, types, places
//! and settings.

use maud::{Markup, html};

use super::{Ctx, Header, page, urlencode};
use crate::db::items::{Invalid, ItemInput};
use crate::db::locations::Location;
use crate::db::types::{FieldDef, FieldKind, Lookup, MediaType};
use crate::dupes::Possible;
use crate::lookup::Candidate;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    New,
    Edit(i64),
}

pub struct FormView<'a> {
    pub mode: Mode,
    pub input: &'a ItemInput,
    pub current_type: &'a MediaType,
    pub fields: &'a [FieldDef],
    pub errors: &'a Invalid,
    pub location_paths: &'a [String],
    pub lookup_q: &'a str,
    /// The candidate the form was prefilled from, with its cache id.
    pub picked: Option<(&'a str, &'a Candidate)>,
    pub dupes: &'a [Possible],
    pub flash: Option<&'a str>,
}

pub fn item_form_page(ctx: &Ctx, v: &FormView) -> Markup {
    let (title, sub) = match v.mode {
        Mode::New => ("A new entry", "Look it up first, or fill in the details yourself."),
        Mode::Edit(_) => ("Edit entry", "Changes are saved when you press Save."),
    };
    let body = html! {
        h1.page-title { (title) " — " i { (v.current_type.section) } }
        p.page-sub { (sub) }
        @if let Some(f) = v.flash { p.flash role="status" { (f) } }
        div.two-col {
            @if v.mode == Mode::New {
                section.left aria-labelledby="lookup-title" {
                    h2 #lookup-title .kicker { "I. Look it up" }
                    (lookup_form(v.current_type, v.lookup_q))
                    div #lookup-results {
                        @if let Some((cid, c)) = v.picked {
                            div.picked-row {
                                span.serif aria-hidden="true" { "✓" }
                                div {
                                    strong.serif { (c.title) }
                                    br;
                                    span.small.muted { (c.meta_line()) " · " (c.source.map(|s| s.label()).unwrap_or("")) }
                                }
                                a.small href={ "/admin/lookup?q=" (urlencode(v.lookup_q)) "&type=" (v.current_type.id) } data-cand=(cid) { "Change" }
                            }
                        }
                    }
                }
            }
            section.right aria-labelledby="details-title" {
                h2 #details-title .kicker { @if v.mode == Mode::New { "II. " } "The details" }
                (details_form(ctx, v))
            }
        }
    };
    page(ctx, title, Header::Plain, body)
}

fn lookup_form(t: &MediaType, q: &str) -> Markup {
    html! {
        form.scan-panel action="/admin/lookup" method="get" data-mobile-page
            hx-get="/admin/lookup" hx-target="#lookup-results" hx-swap="innerHTML" {
            input type="hidden" name="type" value=(t.id);
            div.inline-search {
                label.visually-hidden for="lookup-q" { "Title, ISBN or barcode" }
                input #lookup-q type="search" name="q" value=(q) placeholder="Title, ISBN or barcode" autocomplete="off";
                button.btn.solid type="submit" { "Find" }
            }
            div.row-actions {
                button.btn type="button" data-scan { "Scan barcode" }
                span #scan-status.small.muted role="status" {}
                input #scan-file type="file" accept="image/*" capture="environment" hidden;
            }
            div #scan-panel hidden {
                video playsinline muted {}
                button.btn.small type="button" data-scan-stop { "Stop camera" }
            }
            span.small.muted { (lookup_sources(t.lookup)) }
        }
    }
}

fn lookup_sources(l: Lookup) -> &'static str {
    match l {
        Lookup::Book => "From Open Library and Google Books — or scan the barcode on the back.",
        Lookup::Music => "From MusicBrainz — or scan the barcode.",
        Lookup::Film => "From TMDb — or scan the barcode.",
        Lookup::Product => "Barcode product lookup — kit often has no match, and that's fine: type it in.",
        Lookup::None => "No online source for this type: fill in the details yourself.",
    }
}

/// The list of lookup matches (inline on wide screens).
pub fn candidates(cands: &[(String, Candidate)], notes: &[String], q: &str, type_id: i64) -> Markup {
    html! {
        @if cands.is_empty() && !q.trim().is_empty() {
            p.notice { "No matches. Fill in the details yourself — that works just as well." }
        }
        @if !cands.is_empty() {
            p.small.muted { (cands.len()) " match" @if cands.len() != 1 { "es" } " · pick one to fill in the form" }
        }
        ol.candidates {
            @for (id, c) in cands {
                li {
                    a href={ "/admin/items/new?cand=" (id) "&type=" (type_id) "&q=" (urlencode(q)) } {
                        span.mark aria-hidden="true" { "○" }
                        span {
                            strong { (c.title) }
                            span.meta { (c.meta_line()) }
                            span.src { (c.source.map(|s| s.label()).unwrap_or("")) }
                        }
                    }
                }
            }
        }
        @for n in notes { p.small.muted { (n) } }
    }
}

/// Full-page lookup results (phones), with the search pinned at the top.
pub fn lookup_page(ctx: &Ctx, t: &MediaType, q: &str, cands: &[(String, Candidate)], notes: &[String]) -> Markup {
    page(
        ctx,
        "Look it up",
        Header::Plain,
        html! {
            h1.page-title { "Look it up" }
            div.row-actions {
                a href={ "/admin/items/new?type=" (t.id) } { "← Back to the new entry" }
                a href={ "/admin/items/new?type=" (t.id) } { "Type it myself" }
            }
            (lookup_form(t, q))
            div #lookup-results { (candidates(cands, notes, q, t.id)) }
        },
    )
}

fn err(errors: &Invalid, field: &str) -> Markup {
    html! { @if let Some(e) = errors.get(field) { span.error role="alert" { (e) } } }
}

fn details_form(ctx: &Ctx, v: &FormView) -> Markup {
    let i = v.input;
    let action = match v.mode {
        Mode::New => "/admin/items".to_owned(),
        Mode::Edit(id) => format!("/admin/items/{id}"),
    };
    html! {
        form #item-form method="post" action=(action) enctype="multipart/form-data" {
            @if let Some((cid, _)) = v.picked { input type="hidden" name="cand" value=(cid); }
            input type="hidden" name="lookup_q" value=(v.lookup_q);
            @if let Mode::Edit(id) = v.mode { input #f-item-id type="hidden" name="item_id" value=(id); }
            div #dupes hx-get="/admin/duplicates" hx-include=".dup-watch, #f-item-id"
                hx-trigger="change delay:400ms from:.dup-watch" hx-swap="innerHTML" {
                (dupe_note(v.dupes))
            }
            div.grid-fields {
                div.field.full {
                    label for="f-title" { "Title" }
                    input #f-title.u-input.big.dup-watch name="title" value=(i.title) required maxlength="300";
                    (err(v.errors, "title"))
                }
                div.field {
                    label for="f-type" { "Type" }
                    select #f-type.u-select name="type_id"
                        hx-get="/admin/items/type-fields" hx-include="#f-creator, #f-item-id"
                        hx-target="#type-fields" hx-swap="outerHTML" {
                        @for t in &ctx.types {
                            option value=(t.id) selected[t.id == v.current_type.id] { (t.name) }
                        }
                    }
                    a.small href={ "/admin/types/new?next=" (urlencode(&ctx.here)) } { "+ New type (e.g. Equipment · Sports)" }
                    (err(v.errors, "type_id"))
                }
                div.field {
                    label for="f-year" { "Year" }
                    input #f-year.u-input.dup-watch name="year" value=(i.year) inputmode="numeric" maxlength="4";
                    (err(v.errors, "year"))
                }
                (type_fields(v.current_type, v.fields, i, v.errors, matches!(v.mode, Mode::Edit(_))))
                div.field {
                    label for="f-publisher" { "Publisher / label" }
                    input #f-publisher.u-input.dup-watch name="publisher" value=(i.publisher) maxlength="200";
                }
                div.field {
                    label for="f-format" { "Format / edition" }
                    input #f-format.u-input.dup-watch name="format" value=(i.format) maxlength="200" placeholder="paperback, CD, Blu-ray…";
                }
                div.field {
                    label for="f-identifier" { "ISBN / barcode" }
                    input #f-identifier.u-input.code.dup-watch name="identifier" value=(i.identifier) maxlength="40" inputmode="numeric";
                }
                div.field.full {
                    label for="f-description" { "Description & themes — helps search by idea" }
                    textarea #f-description.u-textarea.italic name="description" rows="2" maxlength="4000" { (i.description) }
                }
                div.field {
                    label for="f-tags" { "Tags" }
                    input #f-tags.u-input name="tags" value=(i.tags) maxlength="500" placeholder="classic, romance";
                }
                div.field.full {
                    label for="f-location" { "Location — type a path, use › or > for inside" }
                    input #f-location.u-input name="location" value=(i.location) list="location-paths" placeholder="Lounge › Bookcase 1 › Shelf 2" autocomplete="off";
                    datalist #location-paths { @for p in v.location_paths { option value=(p) {} } }
                    span.help { "New places are created as you save." }
                }
                div.field.full {
                    label for="f-notes" { "Notes" }
                    textarea #f-notes.u-textarea name="notes" rows="2" maxlength="4000" { (i.notes) }
                }
                div.full.photo-drop {
                    label.drop for="f-photos" { "+ Photo" small { "optional" } }
                    input #f-photos type="file" name="photos" accept="image/*" multiple data-preview="f-previews";
                    div #f-previews.photo-previews {}
                    @if let Some((_, c)) = v.picked && c.cover_url.is_some() {
                        label { input type="checkbox" name="use_cover" value="1"; " Use the cover from " (c.source.map(|s| s.label()).unwrap_or("the lookup")) }
                    }
                }
            }
            div.save-bar {
                @if let Mode::Edit(id) = v.mode {
                    a.btn href={ "/items/" (id) } { "Cancel" }
                } @else {
                    button.btn type="submit" name="then" value="another" { "Save & another" }
                }
                button.btn.accent type="submit" { @if v.mode == Mode::New { "Add to catalogue" } @else { "Save" } }
            }
        }
    }
}

/// The parts of the form that depend on the type: the creator label and
/// the librarian's own fields.
pub fn type_fields(t: &MediaType, fields: &[FieldDef], input: &ItemInput, errors: &Invalid, editing: bool) -> Markup {
    let value_of = |id: i64| input.fields.iter().find(|(f, _)| *f == id).map(|(_, v)| v.as_str()).unwrap_or("");
    html! {
        div #type-fields.full.grid-fields {
            div.field {
                label for="f-creator" { (t.creator_label) }
                input #f-creator.u-input.italic.dup-watch name="creator" value=(input.creator) maxlength="200";
            }
            fieldset.full.dashed-box.grid-fields {
                legend.label { "Your own fields — saved for every " (t.name) }
                @for f in fields {
                    @let name = format!("field_{}", f.id);
                    @let value = value_of(f.id);
                    div.field {
                        @match f.kind {
                            FieldKind::Bool => {
                                span.label { (f.name) @if f.item_id.is_some() { " (this item)" } }
                                label.checks {
                                    input type="hidden" name=(name) value="no";
                                    input type="checkbox" name=(name) value="yes" checked[value == "yes"];
                                    " Yes"
                                }
                            }
                            FieldKind::Choice => {
                                label for=(name) { (f.name) @if f.item_id.is_some() { " (this item)" } }
                                select.u-select id=(name) name=(name) {
                                    option value="" selected[value.is_empty()] { "—" }
                                    @for o in &f.options { option value=(o) selected[value == o] { (o) } }
                                }
                            }
                            kind => {
                                label for=(name) { (f.name) @if f.item_id.is_some() { " (this item)" } }
                                input.u-input id=(name) name=(name) value=(value)
                                    type=(match kind { FieldKind::Date => "date", _ => "text" })
                                    inputmode=(if kind == FieldKind::Number { "decimal" } else { "text" });
                            }
                        }
                        (err(errors, &name))
                    }
                }
                details.add-field.full {
                    summary { "+ Add a field" }
                    div.new-field {
                        div.field {
                            label for="nf-name" { "Name" }
                            input #nf-name.u-input name="new_field_name" placeholder="Last checked" maxlength="60";
                        }
                        div.field {
                            label for="nf-kind" { "Kind" }
                            select #nf-kind.u-select name="new_field_kind" {
                                @for k in FieldKind::ALL { option value=(k.as_str()) { (k.label()) } }
                            }
                        }
                        div.field {
                            label for="nf-options" { "Choices (for a choice list)" }
                            input #nf-options.u-input name="new_field_options" placeholder="Meths, Gas, None";
                        }
                        @if editing {
                            fieldset.checks {
                                legend.visually-hidden { "Applies to" }
                                label { input type="radio" name="new_field_scope" value="type" checked; " All " (t.section) " items" }
                                label { input type="radio" name="new_field_scope" value="item"; " Only this item" }
                            }
                        }
                        (err(errors, "new_field"))
                        button.btn.solid type="button"
                            hx-post="/admin/items/add-field" hx-target="#type-fields" hx-swap="outerHTML" {
                            "Add field"
                        }
                    }
                }
            }
        }
    }
}

/// The "possible duplicate" note.
pub fn dupe_note(dupes: &[Possible]) -> Markup {
    html! {
        @if !dupes.is_empty() {
            aside.dupe-note role="status" {
                span.kicker.accent { "Editor's note — possible duplicate" }
                @if dupes.len() == 1 {
                    @let d = &dupes[0];
                    p {
                        "The catalogue already lists " i { (d.title) }
                        @if !d.creator.is_empty() { " by " (d.creator) }
                        @if let Some(l) = &d.location { ", in " b { (l) } }
                        ". " (capitalise(&d.why)) "."
                    }
                } @else {
                    p { "These look similar:" }
                    ul {
                        @for d in dupes {
                            li { a href={ "/items/" (d.id) } target="_blank" { i { (d.title) } } " — " (d.why) }
                        }
                    }
                }
                div.row-actions {
                    label { input type="checkbox" name="keep_duplicate" value="1"; " It's a different copy — keep both" }
                    @if dupes.len() == 1 {
                        a.btn.small href={ "/items/" (dupes[0].id) } target="_blank" { "View the existing entry" }
                    }
                }
            }
        }
    }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

pub fn new_type_page(ctx: &Ctx, next: &str, name: &str, error: Option<&str>) -> Markup {
    page(
        ctx,
        "New type",
        Header::Plain,
        html! {
            h1.page-title { "A new type" }
            p.page-sub { "Types group what's in the library: “Equipment · Sports”, “Board games”, “Vinyl”…" }
            @if let Some(e) = error { p.flash.err role="alert" { (e) } }
            form.unlock-page method="post" action="/admin/types" {
                input type="hidden" name="next" value=(next);
                div.field {
                    label for="t-name" { "Name" }
                    input #t-name.u-input name="name" value=(name) required maxlength="60" placeholder="Equipment · Sports";
                }
                div.field {
                    label for="t-section" { "Short label for the front page" }
                    input #t-section.u-input name="section" maxlength="40" placeholder="Sports";
                    span.help { "Left blank, it is taken from the name." }
                }
                div.field {
                    label for="t-creator" { "What to call the creator" }
                    input #t-creator.u-input name="creator_label" maxlength="30" placeholder="Brand";
                }
                div.field {
                    label for="t-lookup" { "Look details up in" }
                    select #t-lookup.u-select name="lookup" {
                        @for l in Lookup::ALL { option value=(l.as_str()) selected[l == Lookup::Product] { (l.label()) } }
                    }
                }
                button.btn.solid type="submit" { "Create type" }
            }
        },
    )
}

pub struct DeskView<'a> {
    pub item_count: i64,
    pub on_loan: i64,
    pub pending_embeddings: i64,
    pub semantic: &'a str,
    pub title: &'a str,
    pub tagline: &'a str,
    pub places: &'a [(Location, i64)],
    pub types: &'a [(MediaType, Vec<FieldDef>)],
    pub flash: Option<&'a str>,
}

/// The librarian's desk: settings, places, types and fields.
pub fn desk(ctx: &Ctx, v: &DeskView) -> Markup {
    page(
        ctx,
        "Librarian's desk",
        Header::Plain,
        html! {
            h1.page-title { "Librarian's desk" }
            p.page-sub {
                (v.item_count) " entries · " (v.on_loan) " out on loan · meaning search: " (v.semantic)
                @if v.pending_embeddings > 0 { " (" (v.pending_embeddings) " waiting to be indexed)" }
            }
            @if let Some(f) = v.flash { p.flash role="status" { (f) } }
            div.row-actions { a.btn.solid href="/admin/items/new" { "New entry" } a.btn href="/admin/types/new?next=/admin" { "New type" } }
            div.two-col {
                section.left aria-labelledby="settings-title" {
                    h2 #settings-title .kicker { "The library" }
                    form method="post" action="/admin/settings" {
                        div.field {
                            label for="s-title" { "Title" }
                            input #s-title.u-input.big name="title" value=(v.title) maxlength="80";
                        }
                        div.field {
                            label for="s-tagline" { "Tagline" }
                            input #s-tagline.u-input.italic name="tagline" value=(v.tagline) maxlength="160";
                        }
                        button.btn.solid type="submit" { "Save" }
                    }
                    h2.kicker { "Types & your own fields" }
                    ul.admin-list {
                        @for (t, fields) in v.types {
                            li {
                                div.grow {
                                    strong.serif { (t.name) }
                                    span.small.muted { " · " (t.lookup.label()) }
                                    @if !fields.is_empty() {
                                        ul.history {
                                            @for f in fields {
                                                li {
                                                    form method="post" action={ "/admin/fields/" (f.id) "/delete" } data-confirm="Remove this field and its values?" {
                                                        span { (f.name) " — " (f.kind.label()) }
                                                        button.link-btn.small type="submit" { "remove" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                section.right aria-labelledby="places-title" {
                    h2 #places-title .kicker { "Places" }
                    @if v.places.is_empty() { p.muted { "Places are created when you give an item a location." } }
                    ul.admin-list {
                        @for (l, n) in v.places {
                            li {
                                form.grow method="post" action={ "/admin/places/" (l.id) "/rename" } {
                                    span.small.muted { (l.path.rsplit_once(" › ").map(|(p, _)| format!("{p} ›")).unwrap_or_default()) }
                                    label.visually-hidden for={ "place-" (l.id) } { "Name of " (l.path) }
                                    input type="text" id={ "place-" (l.id) } name="name" value=(l.name) maxlength="60";
                                    button.btn.small type="submit" { "Rename" }
                                    a.small href={ "/search?place=" (l.id) } { (n) " item" @if *n != 1 { "s" } }
                                }
                                @if *n == 0 {
                                    form method="post" action={ "/admin/places/" (l.id) "/delete" } {
                                        button.link-btn.small type="submit" { "remove" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capitalise_first_letter() {
        assert_eq!(capitalise("same ISBN"), "Same ISBN");
        assert_eq!(capitalise(""), "");
    }
}
