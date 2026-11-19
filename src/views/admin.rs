//! Librarian-only pages: adding and editing items, lookups, types, places
//! and settings.

use maud::{Markup, html};

use std::collections::HashMap;

use super::{Ctx, Header, date_long, page, urlencode};
use crate::db::accounts::{Access, Account};
use crate::db::items::{Invalid, Item, ItemInput};
use crate::db::locations::Location;
use crate::db::media_files::{MediaFile, MediaKind, Subtitle};
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
        Mode::New => (
            "A new entry",
            "Look it up first, or fill in the details yourself.",
        ),
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
        Lookup::Product => {
            "Barcode product lookup — kit often has no match, and that's fine: type it in."
        }
        Lookup::None => "No online source for this type: fill in the details yourself.",
    }
}

/// The list of lookup matches (inline on wide screens).
pub fn candidates(
    cands: &[(String, Candidate)],
    notes: &[String],
    q: &str,
    type_id: i64,
) -> Markup {
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
pub fn lookup_page(
    ctx: &Ctx,
    t: &MediaType,
    q: &str,
    cands: &[(String, Candidate)],
    notes: &[String],
) -> Markup {
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
                div.field {
                    span.label { "Digital copies" }
                    label.checks {
                        input type="checkbox" name="restricted" value="1" checked[i.restricted];
                        " Restricted (adults only)"
                    }
                    span.help { "Only adult Player accounts can see or play this item's files." }
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
pub fn type_fields(
    t: &MediaType,
    fields: &[FieldDef],
    input: &ItemInput,
    errors: &Invalid,
    editing: bool,
) -> Markup {
    let value_of = |id: i64| {
        input
            .fields
            .iter()
            .find(|(f, _)| *f == id)
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    };
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
                    input #t-name.u-input name="name" value=(name) required maxlength="60" placeholder="Equipment > Sports";
                    span.help { "Type > between parts; it becomes “Equipment · Sports”." }
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
            div.row-actions.desk-actions {
                a.btn.solid href="/admin/items/new" { "New entry" }
                a.btn href="/admin/types/new?next=/admin" { "New type" }
                a.btn href="/admin/accounts" { "Player accounts" }
                a.btn href="/player/" { "Open the Player" }
            }
            div.two-col {
                section.left aria-labelledby="settings-title" {
                    h2 #settings-title .kicker { "The library" }
                    form.stack method="post" action="/admin/settings" {
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

// ---------- Player accounts ----------

/// The Player accounts page.
pub fn accounts_page(ctx: &Ctx, accounts: &[Account], flash: Option<&str>) -> Markup {
    page(
        ctx,
        "Player accounts",
        Header::Plain,
        html! {
            h1.page-title { "Player accounts" }
            p.page-sub {
                "People who listen and watch in the "
                a href="/player/" { "Player" }
                ". Children never see items marked “Restricted (adults only)”."
            }
            @if let Some(f) = flash { p.flash role="status" { (f) } }
            div.row-actions.desk-actions { a.btn href="/admin" { "← Librarian's desk" } }
            @if accounts.is_empty() {
                div.empty {
                    p { "No accounts yet." }
                    p.small.muted {
                        "People can make one from the Player, or run "
                        span.mono { "library create-account <name> <email> --adult" }
                        " on the server."
                    }
                }
            } @else {
                ul.admin-list.accounts {
                    @for a in accounts { (account_row(a, None)) }
                }
            }
        },
    )
}

/// One account, with its access select and delete button.
pub fn account_row(a: &Account, note: Option<&str>) -> Markup {
    html! {
        li #{ "account-" (a.id) } {
            div.grow {
                strong.serif { (a.username) }
                span.small.muted { " · " (a.email) " · joined " (date_long(a.created_at)) }
            }
            form method="post" action={ "/admin/accounts/" (a.id) "/access" }
                hx-post={ "/admin/accounts/" (a.id) "/access" } hx-trigger="change"
                hx-target={ "#account-" (a.id) } hx-swap="outerHTML" {
                label.visually-hidden for={ "access-" (a.id) } { "Access for " (a.username) }
                select.u-select.access-select id={ "access-" (a.id) } name="access" {
                    @for level in [Access::Child, Access::Adult] {
                        option value=(level.as_str()) selected[a.access == level] { (level.label()) }
                    }
                }
                noscript { button.btn.small type="submit" { "Change" } }
                @if let Some(n) = note { span.small.ok role="status" { (n) } }
            }
            form method="post" action={ "/admin/accounts/" (a.id) "/delete" }
                data-confirm={ "Delete " (a.username) "'s account, playlists and favourites?" } {
                button.link-btn.small type="submit" { "Delete" }
            }
        }
    }
}

// ---------- the upload studio ----------

/// What the upload studio shows.
pub struct StudioData {
    pub item: Item,
    pub files: Vec<MediaFile>,
    pub subtitles: HashMap<i64, Vec<Subtitle>>,
}

/// "1:02:03" or "4:05".
pub fn duration_label(ms: i64) -> String {
    let total = ms.max(0) / 1000;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// "4.2 MB", "1.3 GB".
pub fn size_label(bytes: i64) -> String {
    const UNITS: [&str; 4] = ["bytes", "KB", "MB", "GB"];
    let mut value = bytes.max(0) as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} bytes")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn studio_page(ctx: &Ctx, d: &StudioData, flash: Option<&str>, max_bytes: u64) -> Markup {
    let it = &d.item;
    let accept = crate::digital::ACCEPTED
        .iter()
        .chain(["srt", "vtt"].iter())
        .map(|e| format!(".{e}"))
        .collect::<Vec<_>>()
        .join(",");
    page(
        ctx,
        &format!("Digital copy of {}", it.title),
        Header::Plain,
        html! {
            h1.page-title { "Digital copy — " i { (it.title) } }
            p.page-sub {
                "Drop in the files ripped from a CD or DVD. They are stored exactly as they are — nothing is converted."
                @if it.restricted { " This item is " b { "restricted" } ": only adult accounts will see it." }
            }
            @if let Some(f) = flash { p.flash role="status" { (f) } }
            div.row-actions.desk-actions {
                a.btn href={ "/items/" (it.id) } { "← Back to the entry" }
                a.btn href="/player/" { "Open the Player" }
            }
            section #studio.studio data-item=(it.id) data-max-bytes=(max_bytes) aria-labelledby="studio-title" {
                h2 #studio-title .kicker { "I. Add files" }
                div #drop-zone.drop-zone tabindex="0" {
                    p.drop-title { "Drop a folder or files here" }
                    p.small.muted {
                        "Music (MP3, M4A, FLAC, Ogg, Opus, WAV), films (MP4, WebM, MKV, MOV), documents (PDF, EPUB) "
                        "and subtitles (.srt, .vtt). Up to " (max_bytes / (1024 * 1024)) " MB a file."
                    }
                    div.row-actions.center {
                        label.btn.solid for="pick-folder" { "Choose a folder" }
                        label.btn for="pick-files" { "Choose files" }
                    }
                    input #pick-folder.picker type="file" webkitdirectory multiple;
                    input #pick-files.picker type="file" multiple accept=(accept);
                }
                noscript { p.flash.err { "The upload studio needs JavaScript switched on." } }
                div #pending.pending hidden {
                    div.pending-head {
                        h3.kicker { "Ready to upload" }
                        span #pending-summary.small.muted {}
                    }
                    p.small.muted.pending-hint { "Check the titles and numbers — they were read from the file names. Drag rows by their handle, or use the arrows, to reorder." }
                    div.table-scroll {
                        table.pending-table {
                            thead {
                                tr {
                                    th scope="col" { span.visually-hidden { "Order" } }
                                    th scope="col" { "Disc" }
                                    th scope="col" { "Track" }
                                    th scope="col" { "Title" }
                                    th scope="col" { "Kind" }
                                    th scope="col" { "Length" }
                                    th scope="col" { "Size" }
                                    th scope="col" { "Subtitles" }
                                    th scope="col" { "Progress" }
                                    th scope="col" { span.visually-hidden { "Remove" } }
                                }
                            }
                            tbody #pending-rows {}
                        }
                    }
                    p #pending-notes.small.muted {}
                    div.save-bar {
                        span #upload-status.grow.small role="status" {}
                        button #clear-pending.btn type="button" { "Clear the list" }
                        button #upload-all.btn.accent type="button" { "Upload all" }
                    }
                }
            }
            section #existing.studio-existing aria-labelledby="existing-title" {
                h2 #existing-title .kicker { "II. In the library" }
                (media_list(d))
            }
            script src={ "/static/js/media-upload.js?v=" (ctx.asset_version) } defer {}
        },
    )
}

/// The files already uploaded, with inline editing. Re-rendered by htmx
/// after each change.
pub fn media_list(d: &StudioData) -> Markup {
    let id = d.item.id;
    let last = d.files.len().saturating_sub(1);
    html! {
        div #media-list data-count=(d.files.len()) {
            @if d.files.is_empty() {
                p.muted.serif.italic { "No files yet. Add some above." }
            } @else {
                p.small.muted {
                    (d.files.len()) " file" @if d.files.len() != 1 { "s" }
                    @let total: i64 = d.files.iter().filter_map(|m| m.duration_ms).sum();
                    @if total > 0 { " · " (duration_label(total)) }
                    " · " (size_label(d.files.iter().map(|m| m.size_bytes).sum()))
                }
                ol.media-files {
                    @for (n, m) in d.files.iter().enumerate() {
                        li.media-file #{ "media-" (m.id) } {
                            div.media-main {
                                span.kind-badge.{ "kind-" (m.kind.as_str()) } { (m.kind.label()) }
                                form.media-edit method="post" action={ "/admin/items/" (id) "/media/" (m.id) }
                                    hx-post={ "/admin/items/" (id) "/media/" (m.id) }
                                    hx-target="#media-list" hx-swap="outerHTML" {
                                    label.mini { "Disc"
                                        input.u-input.num name="disc" value=(m.disc) inputmode="numeric" size="2" maxlength="3";
                                    }
                                    label.mini { "Track"
                                        input.u-input.num name="track" value=(m.track.map(|t| t.to_string()).unwrap_or_default()) inputmode="numeric" size="3" maxlength="4";
                                    }
                                    label.mini.grow { "Title"
                                        input.u-input name="title" value=(m.title) maxlength="300" required;
                                    }
                                    button.btn.small type="submit" { "Save" }
                                }
                                div.media-tools {
                                    span.small.muted.mono {
                                        @if let Some(ms) = m.duration_ms { (duration_label(ms)) " · " }
                                        (size_label(m.size_bytes)) " · " (m.original_name)
                                    }
                                    div.row-actions {
                                        form method="post" action={ "/admin/items/" (id) "/media/" (m.id) "/move" }
                                            hx-post={ "/admin/items/" (id) "/media/" (m.id) "/move" }
                                            hx-target="#media-list" hx-swap="outerHTML" {
                                            button.btn.small type="submit" name="dir" value="up" disabled[n == 0] aria-label={ "Move " (m.title) " up" } { "↑" }
                                            button.btn.small type="submit" name="dir" value="down" disabled[n == last] aria-label={ "Move " (m.title) " down" } { "↓" }
                                        }
                                        form method="post" action={ "/admin/items/" (id) "/media/" (m.id) "/delete" }
                                            hx-post={ "/admin/items/" (id) "/media/" (m.id) "/delete" }
                                            hx-confirm={ "Delete “" (m.title) "” for good?" }
                                            hx-target="#media-list" hx-swap="outerHTML" {
                                            button.link-btn.small type="submit" { "Delete" }
                                        }
                                    }
                                }
                            }
                            @if m.kind == MediaKind::Video {
                                div.subtitles {
                                    span.label { "Subtitles" }
                                    @let subs = d.subtitles.get(&m.id).map(Vec::as_slice).unwrap_or(&[]);
                                    @if subs.is_empty() { span.small.muted { "None" } }
                                    @for s in subs {
                                        form.sub-chip method="post" action={ "/admin/items/" (id) "/subtitles/" (s.id) "/delete" }
                                            hx-post={ "/admin/items/" (id) "/subtitles/" (s.id) "/delete" }
                                            hx-confirm={ "Remove the " (s.label) " subtitles?" }
                                            hx-target="#media-list" hx-swap="outerHTML" {
                                            span { (s.label) " " span.mono.small { "(" (s.language) ")" } }
                                            button.x type="submit" aria-label={ "Remove " (s.label) " subtitles" } { "×" }
                                        }
                                    }
                                    form.sub-add method="post" action={ "/admin/items/" (id) "/media/" (m.id) "/subtitles" } enctype="multipart/form-data" {
                                        label.visually-hidden for={ "sub-file-" (m.id) } { "Subtitle file" }
                                        input id={ "sub-file-" (m.id) } type="file" name="file" accept=".srt,.vtt" required;
                                        label.visually-hidden for={ "sub-lang-" (m.id) } { "Language code" }
                                        input.u-input.num id={ "sub-lang-" (m.id) } name="language" placeholder="en" size="5" maxlength="12";
                                        label.visually-hidden for={ "sub-label-" (m.id) } { "Label" }
                                        input.u-input id={ "sub-label-" (m.id) } name="label" placeholder="English (SDH)" maxlength="60";
                                        button.btn.small type="submit" { "Add subtitles" }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_for_lengths_and_sizes() {
        assert_eq!(duration_label(65_000), "1:05");
        assert_eq!(duration_label(3_725_000), "1:02:05");
        assert_eq!(size_label(999), "999 bytes");
        assert_eq!(size_label(4_200_000), "4.2 MB");
        assert_eq!(size_label(1_300_000_000), "1.3 GB");
    }

    #[test]
    fn capitalise_first_letter() {
        assert_eq!(capitalise("same ISBN"), "Same ISBN");
        assert_eq!(capitalise(""), "");
    }
}
