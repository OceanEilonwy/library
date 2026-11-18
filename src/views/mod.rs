//! HTML views (maud). Pages share one layout; fragments returned to htmx
//! requests are the same functions the full pages use.

pub mod admin;
pub mod public;

use maud::{DOCTYPE, Markup, html};

use crate::db::settings::Branding;
use crate::db::types::MediaType;

/// Hand-drawn art the librarian has supplied (URLs), if any.
#[derive(Debug, Clone, Default)]
pub struct Art {
    pub hero: Option<String>,
    pub margin_left: Option<String>,
    pub margin_right: Option<String>,
    pub empty: Option<String>,
}

/// Everything a page needs to know about the request and the library.
#[derive(Clone)]
pub struct Ctx {
    pub admin: bool,
    pub admin_configured: bool,
    pub branding: Branding,
    pub types: Vec<MediaType>,
    pub asset_version: &'static str,
    pub art: Art,
    /// Path and query of this request, for "come back here" links.
    pub here: String,
}

pub enum Header {
    /// The big masthead, for the front page.
    Masthead,
    /// A compact bar with a search box (results, item pages).
    Search(String),
    /// A compact bar without search (admin pages).
    Plain,
}

const FAVICON: &str = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'%3E%3Crect width='32' height='32' fill='%23fbf8f2'/%3E%3Ctext x='16' y='24' font-family='Georgia,serif' font-size='24' font-weight='700' text-anchor='middle' fill='%231f2629'%3EL%3C/text%3E%3C/svg%3E";

/// Inline CSS allowed by hash in the Content-Security-Policy.
pub const CRITICAL_CSS: &str = "html{background:#fbf8f2;color:#1f2629;color-scheme:light}";

pub fn page(ctx: &Ctx, title: &str, header: Header, body: Markup) -> Markup {
    let full_title = if title.is_empty() {
        ctx.branding.title.clone()
    } else {
        format!("{title} · {}", ctx.branding.title)
    };
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="htmx-config" content=r#"{"includeIndicatorCSS":false,"defaultTimeout":20000}"#;
                meta name="color-scheme" content="light";
                // Paints the paper colour before the stylesheet arrives, so
                // navigating never flashes the browser's dark background.
                style { (maud::PreEscaped(CRITICAL_CSS)) }
                title { (full_title) }
                link rel="icon" href=(FAVICON);
                link rel="stylesheet" href="/static/fonts/fonts.css";
                link rel="stylesheet" href={ "/static/css/app.css?v=" (ctx.asset_version) };
                script src={ "/static/js/htmx.min.js?v=" (ctx.asset_version) } defer {}
                script src={ "/static/js/app.js?v=" (ctx.asset_version) } defer {}
            }
            body {
                a.skip href="#main" { "Skip to content" }
                div.wrap {
                    (topbar(ctx, &header))
                    @match &header {
                        Header::Masthead => (masthead(ctx)),
                        Header::Search(q) => (compact_head(ctx, Some(q))),
                        Header::Plain => (compact_head(ctx, None)),
                    }
                    main #main { (body) }
                    footer.site {
                        span { (ctx.branding.title) }
                        @if ctx.admin { a href="/admin" { "Librarian's desk" } }
                    }
                }
            }
        }
    }
}

fn topbar(ctx: &Ctx, header: &Header) -> Markup {
    let today = jiff::Zoned::now().strftime("%A %-d %B %Y").to_string();
    let show_topbar = matches!(header, Header::Masthead);
    html! {
        @if show_topbar {
            div.topbar {
                span { (today) }
                nav aria-label="Site" {
                    @if ctx.admin {
                        a href="/admin/items/new" { "New entry" }
                        span.badge-admin { "● Librarian" }
                    } @else {
                        (librarian_button(ctx))
                    }
                }
            }
        }
        @if !ctx.admin { (unlock_popover(ctx)) }
    }
}

fn librarian_button(ctx: &Ctx) -> Markup {
    html! {
        a.btn.small.caps href={ "/unlock?next=" (urlencode(&ctx.here)) }
            data-panel="unlock-pop" data-panel-always aria-expanded="false" aria-controls="unlock-pop" {
            "Librarian"
        }
    }
}

fn unlock_popover(ctx: &Ctx) -> Markup {
    html! {
        form #unlock-pop.unlock-pop method="post" action="/unlock" hidden aria-label="Librarian unlock" {
            div.head {
                span { "Librarian's desk" }
                button.x type="button" data-close="unlock-pop" aria-label="Close" { "×" }
            }
            input type="hidden" name="next" value=(ctx.here);
            div.field {
                label for="unlock-password" { "Admin password" }
                input #unlock-password.u-input type="password" name="password" autocomplete="current-password" required;
            }
            p.small.muted { "Editing stays unlocked until you close the browser." }
            button.btn.solid type="submit" { "Unlock editing" }
        }
    }
}

fn masthead(ctx: &Ctx) -> Markup {
    html! {
        header.masthead {
            @if let Some(hero) = &ctx.art.hero { img.art-hero src=(hero) alt=""; }
            h1 { (ctx.branding.title) }
            @if !ctx.branding.tagline.is_empty() { p { (ctx.branding.tagline) } }
        }
        nav.sections aria-label="Sections" {
            @for t in &ctx.types {
                a href={ "/search?type=" (t.slug) } { (t.section) }
            }
            a href="/search?sort=added" { "New arrivals" }
        }
    }
}

fn compact_head(ctx: &Ctx, search: Option<&String>) -> Markup {
    html! {
        header.compact-head {
            a.brand href="/" { (ctx.branding.title) }
            @if let Some(q) = search {
                form.grow.inline-search role="search" action="/search" method="get" {
                    label.visually-hidden for="top-q" { "Search the catalogue" }
                    input #top-q type="search" name="q" value=(q) placeholder="Search…" autocomplete="off";
                    button.go type="submit" aria-label="Search" { "→" }
                    a href={ "/advanced?q=" (urlencode(q)) } { "Advanced" }
                }
            }
            div.admin-tools {
                @if ctx.admin {
                    span.badge-admin { "● Librarian" }
                    // Admin pages have their own buttons; don't repeat it here.
                    @if search.is_some() {
                        a.btn.small.solid href="/admin/items/new" { "New entry" }
                    }
                } @else {
                    (librarian_button(ctx))
                }
            }
        }
    }
}

pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn zoned(ts: i64) -> jiff::Zoned {
    jiff::Timestamp::from_second(ts)
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
        .to_zoned(jiff::tz::TimeZone::system())
}

/// "18 November 2026"
pub fn date_long(ts: i64) -> String {
    zoned(ts).strftime("%-d %B %Y").to_string()
}

/// "29 October" this year, "29 October 2025" otherwise.
pub fn date_short(ts: i64) -> String {
    let z = zoned(ts);
    if z.year() == jiff::Zoned::now().year() {
        z.strftime("%-d %B").to_string()
    } else {
        z.strftime("%-d %B %Y").to_string()
    }
}

/// Ordinal-free label for a decade filter value.
pub fn decade_label(d: i64) -> String {
    format!("the {d}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencoding() {
        assert_eq!(urlencode("a b&c/ł"), "a+b%26c%2F%C5%82");
    }

    #[test]
    fn dates_render() {
        assert!(date_long(0).contains("1970") || date_long(0).contains("1969"));
        assert_eq!(decade_label(1980), "the 1980s");
    }
}
