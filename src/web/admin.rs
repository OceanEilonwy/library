//! Librarian-only handlers: adding/editing items, lookups, photos, types,
//! places and settings. Every handler takes [`Admin`], which rejects
//! visitors who have not unlocked editing.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use serde::Deserialize;

use super::{Admin, AppError, AppState, Fields, Res, Viewer, html, html_status, see_other};
use crate::db::items::{self, Invalid, ItemInput};
use crate::db::types::{self, FieldKind, FieldScope, Lookup, NewType};
use crate::db::{locations, photos, settings};
use crate::dupes::{self, Possible, Probe};
use crate::lookup::Candidate;
use crate::media;
use crate::views::admin::{self as av, DeskView, FormView, Mode};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin", get(desk))
        .route("/admin/settings", post(save_settings))
        .route("/admin/items", post(create))
        .route("/admin/items/new", get(new_item))
        .route("/admin/items/type-fields", get(type_fields))
        .route("/admin/items/add-field", post(add_field))
        .route("/admin/items/{id}", post(update))
        .route("/admin/items/{id}/edit", get(edit))
        .route("/admin/items/{id}/delete", post(delete))
        .route("/admin/items/{id}/photos", post(upload_photos))
        .route("/admin/items/{id}/photos/{pid}/delete", post(delete_photo))
        .route("/admin/items/{id}/photos/{pid}/first", post(first_photo))
        .route("/admin/lookup", get(lookup))
        .route("/admin/duplicates", get(duplicates))
        .route("/admin/barcode", post(barcode))
        .route("/admin/types", post(create_type))
        .route("/admin/types/new", get(new_type))
        .route("/admin/fields/{id}/delete", post(delete_field))
        .route("/admin/places/{id}/rename", post(rename_place))
        .route("/admin/places/{id}/delete", post(delete_place))
}

/// Reads the add/edit form into an [`ItemInput`].
pub fn input_from(f: &Fields) -> ItemInput {
    let mut fields: Vec<(i64, String)> = Vec::new();
    for (k, v) in f.pairs() {
        let Some(id) = k.strip_prefix("field_").and_then(|n| n.parse::<i64>().ok()) else {
            continue;
        };
        match fields.iter_mut().find(|(fid, _)| *fid == id) {
            // A checkbox sends a hidden "no" then "yes" when ticked.
            Some((_, existing)) => {
                if v == "yes" || existing.is_empty() || existing == "no" {
                    *existing = v.clone();
                }
            }
            None => fields.push((id, v.clone())),
        }
    }
    ItemInput {
        type_id: f.get("type_id").parse().unwrap_or(0),
        title: f.get("title").to_owned(),
        creator: f.get("creator").to_owned(),
        year: f.get("year").to_owned(),
        publisher: f.get("publisher").to_owned(),
        format: f.get("format").to_owned(),
        identifier: f.get("identifier").to_owned(),
        description: f.get("description").to_owned(),
        tags: f.get("tags").to_owned(),
        notes: f.get("notes").to_owned(),
        location: f.get("location").to_owned(),
        fields,
    }
}

struct FormState<'a> {
    mode: Mode,
    input: ItemInput,
    errors: Invalid,
    lookup_q: String,
    picked: Option<(String, Candidate)>,
    dupes: Vec<Possible>,
    flash: Option<&'a str>,
}

async fn render_form(st: &AppState, viewer: &Viewer, f: FormState<'_>, status: StatusCode) -> Res {
    let ctx = st.ctx(viewer).await?;
    let type_id = f.input.type_id;
    let item_id = match f.mode {
        Mode::Edit(id) => Some(id),
        Mode::New => None,
    };
    let (current, fields, paths) = st
        .db
        .read(move |c| {
            let t = match types::get(c, type_id)? {
                Some(t) => t,
                None => types::list(c)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("no media types"))?,
            };
            let fields = types::fields_for(c, t.id, item_id)?;
            let paths: Vec<String> = locations::list(c)?.into_iter().map(|l| l.path).collect();
            Ok((t, fields, paths))
        })
        .await?;
    let picked = f.picked.as_ref().map(|(id, c)| (id.as_str(), c));
    let view = FormView {
        mode: f.mode,
        input: &f.input,
        current_type: &current,
        fields: &fields,
        errors: &f.errors,
        location_paths: &paths,
        lookup_q: &f.lookup_q,
        picked,
        dupes: &f.dupes,
        flash: f.flash,
    };
    Ok(html_status(status, av::item_form_page(&ctx, &view)))
}

#[derive(Deserialize, Default)]
pub struct NewParams {
    #[serde(default, rename = "type")]
    type_id: Option<String>,
    #[serde(default)]
    cand: Option<String>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    saved: Option<String>,
}

pub async fn new_item(
    State(st): State<AppState>,
    Admin(viewer): Admin,
    Query(p): Query<NewParams>,
) -> Res {
    let mut type_id: i64 = p
        .type_id
        .as_deref()
        .and_then(|t| t.parse().ok())
        .unwrap_or(0);
    if type_id == 0 {
        type_id = st
            .db
            .read(|c| Ok(types::list(c)?.first().map(|t| t.id).unwrap_or(0)))
            .await?;
    }
    let mut input = ItemInput {
        type_id,
        ..Default::default()
    };
    let picked = p
        .cand
        .as_deref()
        .and_then(|id| st.candidates.get(id).map(|c| (id.to_owned(), c)));
    let mut dupes = Vec::new();
    if let Some((_, c)) = &picked {
        input.title = c.title.clone();
        input.creator = c.creator.clone();
        input.year = c.year.map(|y| y.to_string()).unwrap_or_default();
        input.publisher = c.publisher.clone();
        input.format = c.format.clone();
        input.identifier = c.identifier.clone();
        input.description = c.description.clone();
        input.tags = c.tags.clone();
        dupes = find_dupes(&st, &input, None).await?;
    }
    let flash = match (p.saved.as_deref(), p.cand.is_some() && picked.is_none()) {
        (Some(_), _) => Some("Saved. Ready for the next one."),
        (None, true) => Some("That lookup has expired; search again or fill in the details."),
        _ => None,
    };
    render_form(
        &st,
        &viewer,
        FormState {
            mode: Mode::New,
            input,
            errors: Invalid::default(),
            lookup_q: p.q.unwrap_or_default(),
            picked,
            dupes,
            flash,
        },
        StatusCode::OK,
    )
    .await
}

async fn find_dupes(st: &AppState, input: &ItemInput, exclude: Option<i64>) -> Res<Vec<Possible>> {
    let i = input.clone();
    Ok(st
        .db
        .read(move |c| {
            dupes::find(
                c,
                &Probe {
                    title: &i.title,
                    creator: &i.creator,
                    identifier: &i.identifier,
                    publisher: &i.publisher,
                    year: i.year.trim().parse().ok(),
                    format: &i.format,
                    exclude,
                },
            )
        })
        .await?)
}

pub async fn create(State(st): State<AppState>, Admin(viewer): Admin, form: Fields) -> Res {
    let input = input_from(&form);
    let picked = form
        .has("cand")
        .then(|| form.get("cand").to_owned())
        .and_then(|id| st.candidates.get(&id).map(|c| (id, c)));
    let lookup_q = form.get("lookup_q").to_owned();
    if !form.has("keep_duplicate") {
        let dupes = find_dupes(&st, &input, None).await?;
        if !dupes.is_empty() {
            return render_form(
                &st,
                &viewer,
                FormState {
                    mode: Mode::New,
                    input,
                    errors: Invalid::default(),
                    lookup_q,
                    picked,
                    dupes,
                    flash: Some("This might already be in the catalogue. Tick “keep both” to save it anyway (choose any photos again)."),
                },
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await;
        }
    }
    let to_save = input.clone();
    let id = match st.db.write(move |c| items::create(c, &to_save)).await {
        Ok(id) => id,
        Err(e) => {
            return invalid_or_error(&st, &viewer, e, Mode::New, input, lookup_q, picked).await;
        }
    };
    save_uploads(&st, id, &form, "").await;
    if form.has("use_cover")
        && let Some((_, c)) = &picked
        && let Some(url) = &c.cover_url
    {
        match st.lookups.fetch_image(url).await {
            Ok(bytes) => save_photo_bytes(&st, id, bytes, "Cover").await,
            Err(e) => tracing::info!(error = %e, "could not fetch cover"),
        }
    }
    st.semantic.poke();
    if form.get("then") == "another" {
        return Ok(see_other(&format!(
            "/admin/items/new?type={}&saved={id}",
            input.type_id
        )));
    }
    Ok(see_other(&format!("/items/{id}?saved=1")))
}

async fn invalid_or_error(
    st: &AppState,
    viewer: &Viewer,
    e: anyhow::Error,
    mode: Mode,
    input: ItemInput,
    lookup_q: String,
    picked: Option<(String, Candidate)>,
) -> Res {
    match e.downcast::<Invalid>() {
        Ok(errors) => {
            render_form(
                st,
                viewer,
                FormState {
                    mode,
                    input,
                    errors,
                    lookup_q,
                    picked,
                    dupes: Vec::new(),
                    flash: Some("Please check the highlighted fields."),
                },
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        Err(e) => Err(AppError::Internal(e)),
    }
}

async fn save_uploads(st: &AppState, id: i64, form: &Fields, caption: &str) -> usize {
    let mut n = 0;
    for up in form.files.iter().filter(|u| u.name == "photos") {
        save_photo_bytes(st, id, up.bytes.clone(), caption).await;
        n += 1;
    }
    n
}

async fn save_photo_bytes(st: &AppState, id: i64, bytes: Vec<u8>, caption: &str) {
    let dir = st.photos_dir();
    let saved = tokio::task::spawn_blocking(move || media::save_photo(&dir, &bytes)).await;
    match saved {
        Ok(Ok(s)) => {
            let caption = caption.to_owned();
            if let Err(e) = st
                .db
                .write(move |c| photos::add(c, id, &s.file, s.width, s.height, &caption))
                .await
            {
                tracing::warn!(error = %e, "could not record photo");
            }
        }
        Ok(Err(e)) => tracing::info!(error = %e, "photo rejected"),
        Err(e) => tracing::warn!(error = %e, "photo task failed"),
    }
}

pub async fn edit(State(st): State<AppState>, Admin(viewer): Admin, Path(id): Path<i64>) -> Res {
    let input = st
        .db
        .read(move |c| {
            let Some(item) = items::get(c, id)? else {
                return Ok(None);
            };
            let fields = items::field_values(c, id)?;
            Ok(Some(item.to_input(&fields)))
        })
        .await?
        .ok_or(AppError::NotFound)?;
    render_form(
        &st,
        &viewer,
        FormState {
            mode: Mode::Edit(id),
            input,
            errors: Invalid::default(),
            lookup_q: String::new(),
            picked: None,
            dupes: Vec::new(),
            flash: None,
        },
        StatusCode::OK,
    )
    .await
}

pub async fn update(
    State(st): State<AppState>,
    Admin(viewer): Admin,
    Path(id): Path<i64>,
    form: Fields,
) -> Res {
    let input = input_from(&form);
    let to_save = input.clone();
    if let Err(e) = st.db.write(move |c| items::update(c, id, &to_save)).await {
        if e.to_string() == "No such item." {
            return Err(AppError::NotFound);
        }
        return invalid_or_error(&st, &viewer, e, Mode::Edit(id), input, String::new(), None).await;
    }
    save_uploads(&st, id, &form, "").await;
    st.semantic.poke();
    Ok(see_other(&format!("/items/{id}?saved=1")))
}

pub async fn delete(State(st): State<AppState>, Admin(_): Admin, Path(id): Path<i64>) -> Res {
    let files = st.db.write(move |c| items::delete(c, id)).await?;
    let dir = st.photos_dir();
    tokio::task::spawn_blocking(move || files.iter().for_each(|f| media::delete_photo(&dir, f)));
    Ok(see_other("/search?sort=added"))
}

pub async fn upload_photos(
    State(st): State<AppState>,
    Admin(_): Admin,
    Path(id): Path<i64>,
    form: Fields,
) -> Res {
    let exists = st
        .db
        .read(move |c| Ok(items::get(c, id)?.is_some()))
        .await?;
    if !exists {
        return Err(AppError::NotFound);
    }
    let caption = form.get("caption").to_owned();
    save_uploads(&st, id, &form, &caption).await;
    Ok(see_other(&format!("/items/{id}?saved=photos#photos")))
}

pub async fn delete_photo(
    State(st): State<AppState>,
    Admin(_): Admin,
    Path((id, pid)): Path<(i64, i64)>,
) -> Res {
    if let Some(file) = st.db.write(move |c| photos::remove(c, id, pid)).await? {
        let dir = st.photos_dir();
        tokio::task::spawn_blocking(move || media::delete_photo(&dir, &file));
    }
    Ok(see_other(&format!("/items/{id}#photos")))
}

pub async fn first_photo(
    State(st): State<AppState>,
    Admin(_): Admin,
    Path((id, pid)): Path<(i64, i64)>,
) -> Res {
    st.db.write(move |c| photos::make_first(c, id, pid)).await?;
    Ok(see_other(&format!("/items/{id}#photos")))
}

#[derive(Deserialize, Default)]
pub struct LookupParams {
    #[serde(default)]
    q: String,
    #[serde(default, rename = "type")]
    type_id: String,
}

pub async fn lookup(
    State(st): State<AppState>,
    Admin(viewer): Admin,
    Query(p): Query<LookupParams>,
) -> Res {
    let type_id: i64 = p.type_id.parse().unwrap_or(0);
    let t = st
        .db
        .read(move |c| match types::get(c, type_id)? {
            Some(t) => Ok(t),
            None => types::list(c)?
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("no media types")),
        })
        .await?;
    let q: String = p.q.trim().chars().take(200).collect();
    let kind = (t.lookup != Lookup::None).then_some(t.lookup);
    let outcome = if q.is_empty() || t.lookup == Lookup::None && !crate::util::looks_like_code(&q) {
        Default::default()
    } else {
        st.lookups.search(&q, kind).await
    };
    let cands: Vec<(String, Candidate)> = outcome
        .candidates
        .into_iter()
        .take(20)
        .map(|c| (st.candidates.put(c.clone()), c))
        .collect();
    if viewer.htmx {
        return Ok(html(av::candidates(&cands, &outcome.notes, &q, t.id)));
    }
    let ctx = st.ctx(&viewer).await?;
    Ok(html(av::lookup_page(&ctx, &t, &q, &cands, &outcome.notes)))
}

#[derive(Deserialize, Default)]
pub struct DupeParams {
    #[serde(default)]
    title: String,
    #[serde(default)]
    creator: String,
    #[serde(default)]
    identifier: String,
    #[serde(default)]
    publisher: String,
    #[serde(default)]
    year: String,
    #[serde(default)]
    format: String,
    #[serde(default)]
    item_id: String,
}

pub async fn duplicates(
    State(st): State<AppState>,
    Admin(_): Admin,
    Query(p): Query<DupeParams>,
) -> Res {
    let input = ItemInput {
        title: p.title,
        creator: p.creator,
        identifier: p.identifier,
        publisher: p.publisher,
        year: p.year,
        format: p.format,
        ..Default::default()
    };
    let found = find_dupes(&st, &input, p.item_id.parse().ok()).await?;
    Ok(html(av::dupe_note(&found)))
}

#[derive(Deserialize, Default)]
pub struct TypeFieldParams {
    #[serde(default)]
    type_id: String,
    #[serde(default)]
    creator: String,
    #[serde(default)]
    item_id: String,
}

/// Re-renders the type-dependent part of the form after the type changes.
pub async fn type_fields(
    State(st): State<AppState>,
    Admin(_): Admin,
    Query(p): Query<TypeFieldParams>,
) -> Res {
    let type_id: i64 = p.type_id.parse().unwrap_or(0);
    let item_id: Option<i64> = p.item_id.parse().ok();
    let (t, fields, values) = st
        .db
        .read(move |c| {
            let t = types::get(c, type_id)?.ok_or_else(|| anyhow::anyhow!("no such type"))?;
            let fields = types::fields_for(c, t.id, item_id)?;
            let values = match item_id {
                Some(id) => items::field_values(c, id)?
                    .into_iter()
                    .map(|(d, v)| (d.id, v))
                    .collect(),
                None => Vec::new(),
            };
            Ok((t, fields, values))
        })
        .await
        .map_err(|_| AppError::BadRequest("Unknown type.".into()))?;
    let input = ItemInput {
        type_id,
        creator: p.creator,
        fields: values,
        ..Default::default()
    };
    Ok(html(av::type_fields(
        &t,
        &fields,
        &input,
        &Invalid::default(),
        item_id.is_some(),
    )))
}

/// Creates a user-defined field from the form and re-renders the
/// type-dependent part, keeping the values already typed.
pub async fn add_field(State(st): State<AppState>, Admin(_): Admin, form: Fields) -> Res {
    let input = input_from(&form);
    let item_id: Option<i64> = form.get("item_id").parse().ok();
    let name = form.get("new_field_name").to_owned();
    let kind = FieldKind::parse(form.get("new_field_kind")).unwrap_or(FieldKind::Text);
    let options = form.get("new_field_options").to_owned();
    let scope_item = form.get("new_field_scope") == "item" && item_id.is_some();
    let type_id = input.type_id;
    let (t, fields, error) = st
        .db
        .write(move |c| {
            let t = types::get(c, type_id)?.ok_or_else(|| anyhow::anyhow!("no such type"))?;
            let scope = match (scope_item, item_id) {
                (true, Some(id)) => FieldScope::Item(id),
                _ => FieldScope::Type(t.id),
            };
            let error = types::create_field(c, scope, &name, kind, &options)
                .err()
                .map(|e| e.to_string());
            let fields = types::fields_for(c, t.id, item_id)?;
            Ok((t, fields, error))
        })
        .await
        .map_err(|_| AppError::BadRequest("Unknown type.".into()))?;
    let mut errors = Invalid::default();
    if let Some(e) = error {
        errors.add("new_field", e);
    }
    Ok(html(av::type_fields(
        &t,
        &fields,
        &input,
        &errors,
        item_id.is_some(),
    )))
}

/// Reads a barcode from an uploaded photo; replies with the digits.
pub async fn barcode(Admin(_): Admin, form: Fields) -> Res {
    let Some(up) = form.files.into_iter().find(|f| f.name == "photo") else {
        return Ok((StatusCode::UNPROCESSABLE_ENTITY, "No photo received.").into_response());
    };
    let res = tokio::task::spawn_blocking(move || media::read_barcode(&up.bytes))
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(match res {
        Ok(code) => code.into_response(),
        Err(e) => (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response(),
    })
}

#[derive(Deserialize, Default)]
pub struct NextParam {
    #[serde(default)]
    next: String,
}

pub async fn new_type(
    State(st): State<AppState>,
    Admin(viewer): Admin,
    Query(p): Query<NextParam>,
) -> Res {
    let ctx = st.ctx(&viewer).await?;
    Ok(html(av::new_type_page(
        &ctx,
        &super::auth::safe_next(&p.next),
        "",
        None,
    )))
}

pub async fn create_type(State(st): State<AppState>, Admin(viewer): Admin, form: Fields) -> Res {
    let next = super::auth::safe_next(form.get("next"));
    let (name, section, creator, lookup) = (
        form.get("name").to_owned(),
        form.get("section").to_owned(),
        form.get("creator_label").to_owned(),
        Lookup::parse(form.get("lookup")),
    );
    let n2 = name.clone();
    let res = st
        .db
        .write(move |c| {
            types::create(
                c,
                &NewType {
                    name: &n2,
                    section: &section,
                    lookup,
                    creator_label: &creator,
                },
            )
        })
        .await;
    match res {
        Ok(t) => {
            // Return to the add form with the new type chosen.
            let dest = if next.starts_with("/admin/items/new") || next == "/" {
                format!("/admin/items/new?type={}", t.id)
            } else {
                next
            };
            Ok(see_other(&dest))
        }
        Err(e) => {
            let ctx = st.ctx(&viewer).await?;
            Ok(html_status(
                StatusCode::UNPROCESSABLE_ENTITY,
                av::new_type_page(&ctx, &next, &name, Some(&e.to_string())),
            ))
        }
    }
}

pub async fn delete_field(State(st): State<AppState>, Admin(_): Admin, Path(id): Path<i64>) -> Res {
    st.db.write(move |c| types::delete_field(c, id)).await?;
    Ok(see_other("/admin"))
}

#[derive(Deserialize, Default)]
pub struct DeskParams {
    #[serde(default)]
    msg: String,
}

pub async fn desk(
    State(st): State<AppState>,
    Admin(viewer): Admin,
    Query(p): Query<DeskParams>,
) -> Res {
    let ctx = st.ctx(&viewer).await?;
    let semantic = match (st.cfg.semantic.enabled, st.semantic.embedder().is_some()) {
        (false, _) => "switched off",
        (true, false) => "loading the model",
        (true, true) if st.semantic.is_indexing() => "indexing",
        (true, true) => "ready",
    };
    let (count, out, pending, places, types_fields, tagline) = st
        .db
        .read(|c| {
            let count = items::count(c)?;
            let out: i64 = c.query_row(
                "SELECT count(*) FROM loans WHERE returned_at IS NULL",
                [],
                |r| r.get(0),
            )?;
            let pending: i64 = c.query_row(
                "SELECT count(*) FROM items WHERE embed_hash IS NULL",
                [],
                |r| r.get(0),
            )?;
            let places = locations::counts(c)?;
            let mut tf = Vec::new();
            for t in types::list(c)? {
                let f = types::fields_for(c, t.id, None)?;
                tf.push((t, f));
            }
            let tagline = settings::get(c, settings::TAGLINE)?;
            Ok((count, out, pending, places, tf, tagline))
        })
        .await?;
    let flash = match p.msg.as_str() {
        "saved" => Some("Saved."),
        "renamed" => Some("Renamed."),
        "removed" => Some("Removed."),
        "" => None,
        other if other.starts_with("err:") => Some(&other[4..]),
        _ => None,
    };
    let title = ctx.branding.title.clone();
    let tagline = tagline.unwrap_or_else(|| ctx.branding.tagline.clone());
    Ok(html(av::desk(
        &ctx,
        &DeskView {
            item_count: count,
            on_loan: out,
            pending_embeddings: if st.cfg.semantic.enabled { pending } else { 0 },
            semantic,
            title: &title,
            tagline: &tagline,
            places: &places,
            types: &types_fields,
            flash,
        },
    )))
}

pub async fn save_settings(State(st): State<AppState>, Admin(_): Admin, form: Fields) -> Res {
    let title = crate::util::clean(form.get("title"), 80);
    let tagline = crate::util::clean(form.get("tagline"), 160);
    st.db
        .write(move |c| {
            settings::set(c, settings::TITLE, &title)?;
            settings::set(c, settings::TAGLINE, &tagline)
        })
        .await?;
    Ok(see_other("/admin?msg=saved"))
}

fn err_redirect(e: &anyhow::Error) -> String {
    format!(
        "/admin?msg={}",
        crate::views::urlencode(&format!("err:{e}"))
    )
}

pub async fn rename_place(
    State(st): State<AppState>,
    Admin(_): Admin,
    Path(id): Path<i64>,
    form: Fields,
) -> Res {
    let name = form.get("name").to_owned();
    match st.db.write(move |c| locations::rename(c, id, &name)).await {
        Ok(()) => Ok(see_other("/admin?msg=renamed")),
        Err(e) => Ok(see_other(&err_redirect(&e))),
    }
}

pub async fn delete_place(State(st): State<AppState>, Admin(_): Admin, Path(id): Path<i64>) -> Res {
    match st.db.write(move |c| locations::delete(c, id)).await {
        Ok(()) => Ok(see_other("/admin?msg=removed")),
        Err(e) => Ok(see_other(&err_redirect(&e))),
    }
}
