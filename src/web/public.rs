//! Handlers anyone can use: browsing, searching and lending.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use serde::Deserialize;

use super::auth::{self, Login};
use super::{AppError, AppState, Fields, Res, Viewer, html, html_status, see_other};
use crate::db::{items, loans, locations, photos};
use crate::embed;
use crate::search::{self, Advanced, Availability, Filters, Sort};
use crate::views::public::{self as pv, FilterChoices, ItemView, LoanUi, SearchState};

const PAGE: usize = 25;

pub async fn landing(State(st): State<AppState>, viewer: Viewer) -> Res {
    let ctx = st.ctx(&viewer).await?;
    let recent = st
        .db
        .read(|c| {
            let ids: Vec<i64> = c
                .prepare_cached("SELECT id FROM items ORDER BY created_at DESC, id DESC LIMIT 4")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            items::summaries(c, &ids)
        })
        .await?;
    Ok(html(pv::landing(&ctx, &recent)))
}

#[derive(Debug, Default, Deserialize)]
pub struct SearchParams {
    #[serde(default)]
    q: String,
    #[serde(default, rename = "type")]
    type_slug: String,
    #[serde(default)]
    place: String,
    #[serde(default)]
    decade: String,
    #[serde(default)]
    avail: String,
    #[serde(default)]
    sort: String,
    #[serde(default)]
    offset: String,
}

/// Advanced-form fields, if the request came from that form.
fn advanced_from(pairs: &[(String, String)]) -> Option<Advanced> {
    let get = |k: &str| pairs.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.trim().to_owned()).unwrap_or_default();
    let keys = ["title", "creator", "from", "to", "types", "loan", "idea"];
    if !pairs.iter().any(|(k, v)| keys.contains(&k.as_str()) && !v.trim().is_empty()) {
        return None;
    }
    Some(Advanced {
        title: get("title"),
        creator: get("creator"),
        year_from: get("from"),
        year_to: get("to"),
        types: pairs.iter().filter(|(k, _)| k == "types").map(|(_, v)| v.clone()).collect(),
        loan: get("loan"),
        idea: get("idea"),
        words: String::new(),
    })
}

pub async fn search(
    State(st): State<AppState>,
    viewer: Viewer,
    Query(raw): Query<Vec<(String, String)>>,
    Query(p): Query<SearchParams>,
) -> Res {
    // The advanced form submits its fields: turn them into one query and
    // redirect, so the address bar shows the query the fields wrote.
    if let Some(adv) = advanced_from(&raw) {
        let typed = raw.iter().rev().find(|(k, _)| k == "q").map(|(_, v)| v.trim().to_owned()).unwrap_or_default();
        let composed = adv.to_query();
        let q = if typed.is_empty() { composed } else { typed };
        return Ok(see_other(&format!("/search?q={}", crate::views::urlencode(&q))));
    }
    let offset = p.offset.parse::<usize>().unwrap_or(0).min(10_000);
    let state = SearchState {
        q: p.q.trim().chars().take(500).collect(),
        type_slug: p.type_slug.clone(),
        place: p.place.parse().ok(),
        decade: p.decade.parse().ok().filter(|d: &i64| d % 10 == 0 && (0..=3000).contains(d)),
        availability: match p.avail.as_str() {
            "in" => Availability::OnShelf,
            "out" => Availability::OnLoan,
            _ => Availability::Any,
        },
        sort: Sort::parse(&p.sort),
    };
    let req = search::Request {
        q: state.q.clone(),
        filters: Filters {
            type_slug: (!state.type_slug.is_empty()).then(|| state.type_slug.clone()),
            place: state.place,
            decade: state.decade,
            availability: state.availability,
        },
        sort: state.sort,
        offset,
        limit: PAGE,
    };
    let results = search::search(&st.db, &st.semantic, req).await?;
    let ctx = st.ctx(&viewer).await?;

    if viewer.htmx && viewer.hx_target.ends_with("#result-list") {
        return Ok(html(pv::more_rows(&ctx, &state, &results, offset)));
    }
    let choices = st.db.read(filter_choices).await?;
    if viewer.htmx && viewer.hx_target.ends_with("#results") {
        return Ok(html(pv::results_section(&ctx, &state, &choices, &results, offset)));
    }
    Ok(html(pv::results_page(&ctx, &state, &choices, &results, offset)))
}

fn filter_choices(c: &rusqlite::Connection) -> anyhow::Result<FilterChoices> {
    let places = locations::list(c)?.into_iter().filter(|l| l.path.matches(" › ").count() < 2).collect();
    let decades: Vec<i64> = c
        .prepare_cached("SELECT DISTINCT (year / 10) * 10 FROM items WHERE year IS NOT NULL ORDER BY 1")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(FilterChoices { places, decades })
}

#[derive(Deserialize)]
pub struct QParam {
    #[serde(default)]
    q: String,
}

pub async fn advanced(State(st): State<AppState>, viewer: Viewer, Query(p): Query<QParam>) -> Res {
    let ctx = st.ctx(&viewer).await?;
    Ok(html(pv::advanced_page(&ctx, &p.q)))
}

/// Re-renders the "Query, as text" box from the advanced fields.
pub async fn advanced_preview(Query(raw): Query<Vec<(String, String)>>) -> Res {
    let q = advanced_from(&raw).map(|a| a.to_query()).unwrap_or_default();
    Ok(html(pv::query_preview(&q)))
}

#[derive(Deserialize, Default)]
pub struct ItemParams {
    #[serde(default)]
    saved: Option<String>,
}

pub async fn item(State(st): State<AppState>, viewer: Viewer, Path(id): Path<i64>, Query(p): Query<ItemParams>) -> Res {
    let data = st
        .db
        .read(move |c| {
            let Some(item) = items::get(c, id)? else { return Ok(None) };
            let fields = items::field_values(c, id)?;
            let photos = photos::for_item(c, id)?;
            let loans = loans::history(c, id)?;
            // "Similar in spirit": nearest neighbours by meaning.
            let similar = match embed::item_vector(c, id)? {
                Some(v) => {
                    let ids: Vec<i64> = embed::neighbours(c, &v, 5)?
                        .into_iter()
                        .filter(|(other, sim)| *other != id && *sim > 0.0)
                        .map(|(i, _)| i)
                        .take(3)
                        .collect();
                    items::summaries(c, &ids)?
                }
                None => Vec::new(),
            };
            Ok(Some((item, fields, photos, loans, similar)))
        })
        .await?;
    let Some((item, fields, photos, loans, similar)) = data else { return Err(AppError::NotFound) };
    let ctx = st.ctx(&viewer).await?;
    let flash = p.saved.as_deref().map(|s| match s {
        "photos" => "Photos added.",
        "returned" => "Marked as returned.",
        "lent" => "Lent out — the date has been recorded.",
        _ => "Saved.",
    });
    Ok(html(pv::item_page(
        &ctx,
        &ItemView { item: &item, fields: &fields, photos: &photos, loans: &loans, similar: &similar },
        flash,
    )))
}

#[derive(Deserialize, Default)]
pub struct RowParams {
    #[serde(default)]
    n: Option<usize>,
    #[serde(default)]
    ctx: Option<String>,
}

async fn summary(st: &AppState, id: i64) -> Res<items::Summary> {
    st.db.read(move |c| items::summary(c, id)).await?.ok_or(AppError::NotFound)
}

/// A result row, closed (used by "Cancel").
pub async fn item_row(State(st): State<AppState>, viewer: Viewer, Path(id): Path<i64>, Query(p): Query<RowParams>) -> Res {
    let s = summary(&st, id).await?;
    let ctx = st.ctx(&viewer).await?;
    Ok(html(pv::result_row(&ctx, &s, None, p.n.unwrap_or(1), LoanUi::Closed)))
}

/// The lending form: inline in a result row (htmx) or as a page.
pub async fn loan_form(State(st): State<AppState>, viewer: Viewer, Path(id): Path<i64>, Query(p): Query<RowParams>) -> Res {
    let s = summary(&st, id).await?;
    let ctx = st.ctx(&viewer).await?;
    if viewer.htmx {
        Ok(html(pv::result_row(&ctx, &s, None, p.n.unwrap_or(1), LoanUi::Open(None))))
    } else {
        Ok(html(pv::loan_page(&ctx, &s, None)))
    }
}

pub async fn lend(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(id): Path<i64>,
    Query(p): Query<RowParams>,
    form: Fields,
) -> Res {
    let borrower = form.get("borrower").to_owned();
    let res = st.db.write(move |c| loans::lend(c, id, &borrower)).await;
    let error = match &res {
        Ok(_) => None,
        Err(e) => match e.downcast_ref::<loans::LoanError>() {
            Some(loans::LoanError::NoSuchItem) => return Err(AppError::NotFound),
            Some(le) => Some(le.to_string()),
            None => return Err(AppError::Internal(res.unwrap_err())),
        },
    };
    respond_after_loan(&st, &viewer, id, &p, error).await
}

pub async fn give_back(State(st): State<AppState>, viewer: Viewer, Path(id): Path<i64>, Query(p): Query<RowParams>) -> Res {
    st.db.write(move |c| loans::mark_returned(c, id)).await?;
    respond_after_loan(&st, &viewer, id, &p, None).await
}

async fn respond_after_loan(st: &AppState, viewer: &Viewer, id: i64, p: &RowParams, error: Option<String>) -> Res {
    if !viewer.htmx {
        if let Some(e) = error {
            let s = summary(st, id).await?;
            let ctx = st.ctx(viewer).await?;
            return Ok(html_status(StatusCode::UNPROCESSABLE_ENTITY, pv::loan_page(&ctx, &s, Some(&e))));
        }
        return Ok(see_other(&format!("/items/{id}")));
    }
    if p.ctx.as_deref() == Some("panel") {
        let (title, history) = st
            .db
            .read(move |c| {
                let title = items::get(c, id)?.map(|i| i.title).unwrap_or_default();
                Ok((title, loans::history(c, id)?))
            })
            .await?;
        return Ok(html(pv::loan_panel(id, &title, &history, error.as_deref())));
    }
    let s = summary(st, id).await?;
    let ctx = st.ctx(viewer).await?;
    let n = p.n.unwrap_or(1);
    let ui = match &error {
        Some(e) if s.loan.is_none() => LoanUi::Open(Some(e.as_str())),
        _ => LoanUi::Closed,
    };
    Ok(html(pv::result_row(&ctx, &s, None, n, ui)))
}

#[derive(Deserialize, Default)]
pub struct NextParam {
    #[serde(default)]
    next: String,
}

pub async fn unlock_page(State(st): State<AppState>, viewer: Viewer, Query(p): Query<NextParam>) -> Res {
    let ctx = st.ctx(&viewer).await?;
    let next = auth::safe_next(&p.next);
    if viewer.admin {
        return Ok(see_other(&next));
    }
    Ok(html(pv::unlock_page(&ctx, &next, None)))
}

pub async fn unlock(State(st): State<AppState>, viewer: Viewer, headers: HeaderMap, form: Fields) -> Res {
    let next = auth::safe_next(form.get("next"));
    let password = form.get("password").to_owned();
    let a = st.auth.clone();
    let outcome = tokio::task::spawn_blocking(move || a.login(&password))
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    let (error, status) = match outcome {
        Login::Ok(cookie) => {
            let mut r = see_other(&next);
            r.headers_mut().insert(
                header::SET_COOKIE,
                auth::session_cookie(&cookie, super::is_https(&headers)).parse().expect("valid cookie header"),
            );
            return Ok(r);
        }
        Login::Wrong => ("That isn't the admin password.", StatusCode::UNAUTHORIZED),
        Login::TooMany => ("Too many wrong attempts. Wait ten minutes and try again.", StatusCode::TOO_MANY_REQUESTS),
        Login::NotConfigured => ("No admin password has been set up on the server yet.", StatusCode::UNAUTHORIZED),
    };
    let ctx = st.ctx(&viewer).await?;
    Ok(html_status(status, pv::unlock_page(&ctx, &next, Some(error))))
}
