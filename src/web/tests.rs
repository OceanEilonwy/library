//! End-to-end tests of the HTTP surface, through the real router.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::auth::{Auth, Login, hash_password};
use super::*;
use crate::config::{Config, LookupConfig};
use crate::embed::{Semantic, fake::FakeEmbedder};
use crate::lookup::{CandidateCache, Lookups};

struct App {
    router: Router,
    state: AppState,
    admin_cookie: String,
    _dir: tempfile::TempDir,
}

const PASSWORD: &str = "correct horse battery";

fn app() -> App {
    app_with(|_| {})
}

fn app_with(tweak: impl FnOnce(&mut Config)) -> App {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config {
        data_dir: dir.path().to_owned(),
        default_title: "Tallow Lane Library".into(),
        lookup: LookupConfig {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };
    tweak(&mut cfg);
    let db = Db::open(&cfg.db_path()).unwrap();
    let auth = Auth::new([3u8; 32], Some(hash_password(PASSWORD).unwrap()));
    let Login::Ok(cookie) = auth.login(PASSWORD) else {
        panic!("login")
    };
    let state = AppState {
        art: Arc::new(ArtCache::new(cfg.art_dir())),
        lookups: Lookups::new(cfg.lookup.clone()).unwrap(),
        cfg: Arc::new(cfg),
        db,
        semantic: Semantic::with(Arc::new(FakeEmbedder)),
        candidates: Arc::new(CandidateCache::default()),
        auth: Arc::new(auth),
        throttles: Arc::new(accounts::Throttles::default()),
        rip: Arc::new(rip::RipState::default()),
    };
    App {
        router: router(state.clone()),
        state,
        admin_cookie: format!("{}={cookie}", auth::COOKIE),
        _dir: dir,
    }
}

struct Resp {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

impl App {
    async fn send(&self, req: Request<Body>) -> Resp {
        let res = self.router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        Resp {
            status,
            headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        }
    }

    async fn get(&self, uri: &str, admin: bool) -> Resp {
        let mut b = Request::get(uri);
        if admin {
            b = b.header(header::COOKIE, &self.admin_cookie);
        }
        self.send(b.body(Body::empty()).unwrap()).await
    }

    async fn htmx_get(&self, uri: &str, target: &str, admin: bool) -> Resp {
        let mut b = Request::get(uri)
            .header("hx-request", "true")
            .header("hx-target", target)
            .header("hx-current-url", "http://localhost/search?q=x");
        if admin {
            b = b.header(header::COOKIE, &self.admin_cookie);
        }
        self.send(b.body(Body::empty()).unwrap()).await
    }

    async fn post(&self, uri: &str, form: &str, admin: bool, htmx: bool) -> Resp {
        let mut b = Request::post(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::HOST, "localhost")
            .header(header::ORIGIN, "http://localhost");
        if admin {
            b = b.header(header::COOKIE, &self.admin_cookie);
        }
        if htmx {
            b = b
                .header("hx-request", "true")
                .header("hx-current-url", "http://localhost/search?q=x");
        }
        self.send(b.body(Body::from(form.to_owned())).unwrap())
            .await
    }

    async fn post_multipart(
        &self,
        uri: &str,
        fields: &[(&str, &str)],
        files: &[(&str, Vec<u8>)],
    ) -> Resp {
        let boundary = "XyZbOuNdArY";
        let mut body: Vec<u8> = Vec::new();
        for (k, v) in fields {
            body.extend(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n"
                )
                .bytes(),
            );
        }
        for (k, bytes) in files {
            body.extend(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"; filename=\"p.png\"\r\nContent-Type: image/png\r\n\r\n")
                    .bytes(),
            );
            body.extend(bytes);
            body.extend(b"\r\n");
        }
        body.extend(format!("--{boundary}--\r\n").bytes());
        let req = Request::post(uri)
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .header(header::HOST, "localhost")
            .header(header::ORIGIN, "http://localhost")
            .header(header::COOKIE, &self.admin_cookie)
            .body(Body::from(body))
            .unwrap();
        self.send(req).await
    }

    fn type_id(&self, slug: &str) -> i64 {
        self.state
            .db
            .read_blocking(|c| Ok(crate::db::types::by_slug(c, slug)?.unwrap().id))
            .unwrap()
    }

    async fn add(&self, title: &str, creator: &str) -> i64 {
        let tid = self.type_id("book").to_string();
        let r = self
            .post_multipart(
                "/admin/items",
                &[
                    ("type_id", &tid),
                    ("title", title),
                    ("creator", creator),
                    ("location", "Shed > Books Box 1"),
                    ("keep_duplicate", "1"),
                ],
                &[],
            )
            .await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
        let loc = r.headers.get(header::LOCATION).unwrap().to_str().unwrap();
        loc.trim_start_matches("/items/")
            .split('?')
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }
}

fn png() -> Vec<u8> {
    let img = image::RgbImage::from_fn(40, 60, |x, y| image::Rgb([x as u8 * 5, y as u8 * 4, 120]));
    let mut buf = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .unwrap();
    buf
}

#[tokio::test]
async fn landing_page_and_security_headers() {
    let a = app();
    let r = a.get("/", false).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("Tallow Lane Library"));
    assert!(r.body.contains("Search the catalogue"));
    assert!(r.body.contains("/search?type=book"), "sections nav");
    assert!(r.body.contains("Librarian"), "unlock button for visitors");
    assert!(!r.body.contains("New entry"), "no admin tools for visitors");
    let csp = r
        .headers
        .get(header::CONTENT_SECURITY_POLICY)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(csp.contains("script-src 'self'"));
    // The only inline style is allowed by its hash, so the page never flashes dark.
    use base64::Engine;
    let hash = base64::engine::general_purpose::STANDARD
        .encode(Sha256::digest(crate::views::CRITICAL_CSS.as_bytes()));
    assert!(csp.contains(&format!("'sha256-{hash}'")), "{csp}");
    assert!(r.body.contains(crate::views::CRITICAL_CSS));
    assert_eq!(r.headers.get(header::X_FRAME_OPTIONS).unwrap(), "DENY");
}

#[tokio::test]
async fn static_assets_and_photo_paths() {
    let a = app();
    let r = a.get("/static/js/htmx.min.js", false).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.headers
            .get(header::CACHE_CONTROL)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("immutable")
    );
    assert_eq!(
        a.get("/static/nope.js", false).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        a.get("/photos/..%2F..%2Fetc%2Fpasswd", false).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        a.get("/photos/missing.jpg", false).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(a.get("/healthz", false).await.body, "ok");
}

#[tokio::test]
async fn admin_pages_require_unlocking() {
    let a = app();
    let r = a.get("/admin/items/new", false).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(
        r.headers
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("/unlock?next=%2Fadmin")
    );
    let r = a
        .post("/admin/settings", "title=Hacked", false, false)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = a.post("/admin/settings", "title=Hacked", false, true).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(r.headers.get("hx-redirect").is_some());
    assert_eq!(a.get("/admin", true).await.status, StatusCode::OK);
}

#[tokio::test]
async fn unlocking_with_the_password() {
    let a = app();
    let r = a
        .post("/unlock", "password=wrong&next=%2Fadmin", false, false)
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(
        r.body.contains("isn&#39;t the admin password")
            || r.body.contains("isn't the admin password")
    );
    let r = a
        .post(
            "/unlock",
            "password=correct+horse+battery&next=%2Fadmin",
            false,
            false,
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.headers.get(header::LOCATION).unwrap(), "/admin");
    let cookie = r.headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Strict")
            && !cookie.contains("Max-Age")
    );
    // Off-site "next" is ignored.
    let r = a
        .post(
            "/unlock",
            "password=correct+horse+battery&next=https%3A%2F%2Fevil.example",
            false,
            false,
        )
        .await;
    assert_eq!(r.headers.get(header::LOCATION).unwrap(), "/");
}

#[tokio::test]
async fn cross_site_posts_are_refused() {
    let a = app();
    let id = a.add("Low", "David Bowie").await;
    let req = Request::post(format!("/items/{id}/loan"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::HOST, "localhost")
        .header(header::ORIGIN, "https://evil.example")
        .body(Body::from("borrower=Mallory"))
        .unwrap();
    assert_eq!(a.send(req).await.status, StatusCode::FORBIDDEN);
    let req = Request::post(format!("/items/{id}/loan"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header("sec-fetch-site", "cross-site")
        .body(Body::from("borrower=Mallory"))
        .unwrap();
    assert_eq!(a.send(req).await.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn add_item_with_photo_then_view_and_search() {
    let a = app();
    let tid = a.type_id("book").to_string();
    let r = a
        .post_multipart(
            "/admin/items",
            &[
                ("type_id", &tid),
                ("title", "The Hobbit"),
                ("creator", "J. R. R. Tolkien"),
                ("year", "1995"),
                ("location", "Lounge > Bookcase 1 > Shelf 4"),
                ("tags", "fantasy, dragons"),
            ],
            &[("photos", png())],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let loc = r
        .headers
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let page = a.get(&loc, false).await;
    assert!(
        page.body.contains("The Hobbit") && page.body.contains("Lounge › Bookcase 1 › Shelf 4")
    );
    assert!(page.body.contains("Saved."));
    assert!(page.body.contains("/photos/"), "photo shown");

    let r = a.get("/search?q=hobbit", false).await;
    assert!(r.body.contains("The Hobbit"));
    assert!(r.body.contains("1 entry"));
    assert!(
        r.body.contains("class=\"thumb\""),
        "result shows the photo thumbnail"
    );

    // Search by field, wildcard and location.
    assert!(
        a.get("/search?q=author%3Atolkien", false)
            .await
            .body
            .contains("The Hobbit")
    );
    assert!(
        a.get("/search?q=hob*", false)
            .await
            .body
            .contains("The Hobbit")
    );
    assert!(
        a.get("/search?q=in%3Alounge", false)
            .await
            .body
            .contains("The Hobbit")
    );
    assert!(
        a.get("/search?q=in%3Ashed", false)
            .await
            .body
            .contains("Nothing on the shelves")
    );
}

#[tokio::test]
async fn duplicates_warn_but_can_be_kept() {
    let a = app();
    a.add("The Hobbit", "Tolkien").await;
    let tid = a.type_id("book").to_string();
    let r = a
        .post_multipart(
            "/admin/items",
            &[
                ("type_id", &tid),
                ("title", "The Hobbit"),
                ("creator", "J.R.R. Tolkien"),
            ],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.body.contains("possible duplicate"));
    assert!(r.body.contains("keep_duplicate"));
    let r = a
        .post_multipart(
            "/admin/items",
            &[
                ("type_id", &tid),
                ("title", "The Hobbit"),
                ("creator", "J.R.R. Tolkien"),
                ("keep_duplicate", "1"),
            ],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    // The live check used while typing.
    let r = a
        .htmx_get(
            "/admin/duplicates?title=hobbit&creator=tolkien",
            "div#dupes",
            true,
        )
        .await;
    assert!(r.body.contains("These look similar"));
    let r = a
        .htmx_get("/admin/duplicates?title=moby", "div#dupes", true)
        .await;
    assert!(r.body.trim().is_empty());
}

#[tokio::test]
async fn validation_errors_keep_the_form() {
    let a = app();
    let tid = a.type_id("book").to_string();
    let r = a
        .post_multipart(
            "/admin/items",
            &[
                ("type_id", &tid),
                ("title", ""),
                ("year", "soon"),
                ("creator", "Kept"),
            ],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.body.contains("Every item needs a title"));
    assert!(r.body.contains("like 1971"));
    assert!(r.body.contains("value=\"Kept\""));
}

#[tokio::test]
async fn edit_and_delete() {
    let a = app();
    let id = a.add("Moby Dick", "Melville").await;
    let r = a.get(&format!("/admin/items/{id}/edit"), true).await;
    assert!(r.body.contains("value=\"Moby Dick\""));
    let tid = a.type_id("book").to_string();
    let r = a
        .post_multipart(
            &format!("/admin/items/{id}"),
            &[
                ("type_id", &tid),
                ("title", "Moby-Dick"),
                ("creator", "Herman Melville"),
            ],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(
        a.get(&format!("/items/{id}"), false)
            .await
            .body
            .contains("Herman Melville")
    );
    let r = a
        .post(&format!("/admin/items/{id}/delete"), "", true, false)
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        a.get(&format!("/items/{id}"), false).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn lending_inline_and_returning() {
    let a = app();
    let id = a.add("Ponyo", "Hayao Miyazaki").await;
    let r = a
        .htmx_get(
            &format!("/items/{id}/loan?n=3"),
            &format!("li#item-{id}"),
            false,
        )
        .await;
    assert!(r.body.contains("Borrower&#39;s name") || r.body.contains("Borrower's name"));
    assert!(!r.body.contains("<html"), "fragment only");
    assert!(r.body.contains(">03<"), "keeps the row number");

    let r = a
        .post(&format!("/items/{id}/loan?n=3"), "borrower=+", false, true)
        .await;
    assert!(r.body.contains("give the borrower"));

    let r = a
        .post(
            &format!("/items/{id}/loan?n=3"),
            "borrower=Kasia",
            false,
            true,
        )
        .await;
    assert!(r.body.contains("Out — with Kasia"), "{}", r.body);
    let r = a
        .post(&format!("/items/{id}/loan"), "borrower=Fraser", false, true)
        .await;
    assert!(r.body.contains("Out — with Kasia"), "already out");

    assert!(
        a.get("/search?q=loan%3A*", false)
            .await
            .body
            .contains("Ponyo")
    );
    assert!(
        a.get("/search?q=loan%3Akasia", false)
            .await
            .body
            .contains("Ponyo")
    );

    let r = a
        .post(&format!("/items/{id}/return?ctx=panel"), "", false, true)
        .await;
    assert!(r.body.contains("Confirm loan") && r.body.contains("History"));

    // Without JavaScript: plain POST redirects back to the item.
    let r = a
        .post(&format!("/items/{id}/loan"), "borrower=Ellie", false, false)
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = a.get(&format!("/items/{id}"), false).await;
    assert!(page.body.contains("Out — with Ellie"));
}

#[tokio::test]
async fn search_fragments_for_htmx() {
    let a = app();
    for i in 0..30 {
        a.add(&format!("Book number {i}"), "Sam Author").await;
    }
    let full = a.get("/search?q=book", false).await;
    assert!(
        full.body.contains("<html")
            && full.body.contains("Load more")
            && full.body.contains("30 entries")
    );
    let section = a
        .htmx_get("/search?q=book&sort=title", "section#results", false)
        .await;
    assert!(!section.body.contains("<html") && section.body.contains("id=\"results\""));
    let rows = a
        .htmx_get("/search?q=book&offset=25", "ol#result-list", false)
        .await;
    assert_eq!(rows.body.matches("class=\"result\"").count(), 5);
    assert!(rows.body.contains("<hx-partial"));
    assert!(rows.body.contains(">26<"), "numbering continues");
}

#[tokio::test]
async fn advanced_search_composes_a_query() {
    let a = app();
    let r = a
        .get(
            "/search?title=earth*&creator=le+guin&from=1960&to=1975&types=book&idea=",
            false,
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let to = r.headers.get(header::LOCATION).unwrap().to_str().unwrap();
    assert_eq!(
        to,
        "/search?q=title%3Aearth%2A+AND+creator%3A%22le+guin%22+AND+year%3A1960..1975+AND+type%3Abook"
    );
    let r = a
        .htmx_get("/advanced/preview?loan=*&q=", "textarea#adv-q", false)
        .await;
    assert!(r.body.contains("loan:*") && r.body.contains("<textarea"));
    assert_eq!(a.get("/advanced", false).await.status, StatusCode::OK);
}

#[tokio::test]
async fn custom_fields_and_new_types() {
    let a = app();
    let tid = a.type_id("camping");
    let r = a
        .post(
            "/admin/items/add-field",
            &format!("type_id={tid}&creator=MSR&new_field_name=Fuel&new_field_kind=choice&new_field_options=Meths%2C+Gas"),
            true,
            true,
        )
        .await;
    assert!(
        r.body.contains("Fuel") && r.body.contains("Meths") && r.body.contains("value=\"MSR\""),
        "{}",
        r.body
    );
    let r = a
        .post(
            "/admin/items/add-field",
            &format!("type_id={tid}&new_field_name=Fuel&new_field_kind=text"),
            true,
            true,
        )
        .await;
    assert!(r.body.contains("already a field"));
    let r = a
        .htmx_get(
            &format!("/admin/items/type-fields?type_id={tid}&creator=Trangia"),
            "div#type-fields",
            true,
        )
        .await;
    assert!(r.body.contains("Brand") && r.body.contains("Fuel"));

    let r = a.post("/admin/types", "name=Equipment+%C2%B7+Sports&section=&creator_label=Brand&lookup=product&next=%2Fadmin%2Fitems%2Fnew", true, false).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(
        r.headers
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("/admin/items/new?type=")
    );
    assert!(a.get("/", false).await.body.contains(">Sports<"));
    let r = a
        .post(
            "/admin/types",
            "name=Equipment+%C2%B7+Sports&lookup=none",
            true,
            false,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn settings_and_places() {
    let a = app();
    let r = a
        .post(
            "/admin/settings",
            "title=The+Shed+Library&tagline=Books+and+tents",
            true,
            false,
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let home = a.get("/", false).await;
    assert!(home.body.contains("The Shed Library") && home.body.contains("Books and tents"));
    let id = a.add("Kite", "").await;
    let place = a
        .state
        .db
        .read_blocking(move |c| Ok(crate::db::items::get(c, id)?.unwrap().location_id.unwrap()))
        .unwrap();
    let r = a
        .post(
            &format!("/admin/places/{place}/rename"),
            "name=Box+One",
            true,
            false,
        )
        .await;
    assert_eq!(
        r.headers.get(header::LOCATION).unwrap(),
        "/admin?msg=renamed"
    );
    assert!(
        a.get(&format!("/items/{id}"), false)
            .await
            .body
            .contains("Shed › Box One")
    );
    let r = a
        .post(&format!("/admin/places/{place}/delete"), "", true, false)
        .await;
    assert!(
        r.headers
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("err")
    );
}

#[tokio::test]
async fn lookup_page_without_network() {
    let a = app();
    let tid = a.type_id("book");
    let r = a
        .get(&format!("/admin/lookup?q=hobbit&type={tid}"), true)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("switched off"));
    let r = a
        .htmx_get(
            &format!("/admin/lookup?q=hobbit&type={tid}"),
            "div#lookup-results",
            true,
        )
        .await;
    assert!(!r.body.contains("<html"));
}

#[tokio::test]
async fn barcode_endpoint_reports_failures() {
    let a = app();
    let r = a
        .post_multipart("/admin/barcode", &[], &[("photo", png())])
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.body.contains("No barcode found"));
}

#[tokio::test]
async fn missing_things_are_404() {
    let a = app();
    assert_eq!(
        a.get("/items/999", false).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        a.get("/admin/items/999/edit", true).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(a.get("/nowhere", false).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn optional_site_password_guards_every_page_but_health() {
    use base64::Engine;
    let a = app_with(|c| c.site_password = Some("family only".into()));
    let r = a.get("/", false).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(
        r.headers
            .get(header::WWW_AUTHENTICATE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("Basic")
    );
    assert_eq!(a.get("/healthz", false).await.status, StatusCode::OK);
    let auth = |pw: &str| {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("anyone:{pw}"))
        )
    };
    let ok = a
        .send(
            Request::get("/")
                .header(header::AUTHORIZATION, auth("family only"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK);
    let bad = a
        .send(
            Request::get("/search")
                .header(header::AUTHORIZATION, auth("guess"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED);
    // Without a site password nothing is asked.
    assert_eq!(app().get("/", false).await.status, StatusCode::OK);
}

// ---------- the Player: accounts, API, streaming, uploads ----------

impl App {
    /// A JSON request, optionally with a Player account cookie.
    async fn api(
        &self,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
        cookie: Option<&str>,
    ) -> Resp {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "localhost")
            .header(header::ORIGIN, "http://localhost");
        if let Some(c) = cookie {
            b = b.header(header::COOKIE, c);
        }
        let body = match body {
            Some(v) => {
                b = b.header(header::CONTENT_TYPE, "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        self.send(b.body(body).unwrap()).await
    }

    /// Registers an account, which then waits for a librarian.
    async fn register_pending(&self, username: &str) -> Resp {
        let r = self
            .api(
                "POST",
                "/api/account/register",
                Some(serde_json::json!({
                    "username": username,
                    "email": format!("{username}@example.org"),
                    "password": "long enough password",
                })),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.body);
        assert!(r.headers.get(header::SET_COOKIE).is_none());
        r
    }

    /// Logs in with the password every test account has.
    async fn login(&self, login: &str) -> Resp {
        self.api(
            "POST",
            "/api/account/login",
            Some(serde_json::json!({"login": login, "password": "long enough password"})),
            None,
        )
        .await
    }

    /// The session cookie (`library_account=…`) a response set.
    fn cookie_of(r: &Resp) -> String {
        let set = r.headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
        set.split(';').next().unwrap().to_owned()
    }

    fn account_id(&self, username: &str) -> i64 {
        let name = username.to_owned();
        self.state
            .db
            .read_blocking(move |c| Ok(crate::db::accounts::by_username(c, &name)?.unwrap().id))
            .unwrap()
    }

    /// Registers an account, approves it (keeping the access it asked
    /// with) and logs in; returns its cookie (`library_account=…`).
    async fn register(&self, username: &str) -> String {
        self.register_pending(username).await;
        let name = username.to_owned();
        self.state
            .db
            .write_blocking(move |c| {
                let a = crate::db::accounts::by_username(c, &name)?.unwrap();
                assert!(crate::db::accounts::approve(c, a.id, a.access)?);
                Ok(())
            })
            .unwrap();
        let r = self.login(username).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        Self::cookie_of(&r)
    }

    /// Registers an account with adult access.
    async fn register_adult(&self, username: &str) -> String {
        let cookie = self.register(username).await;
        self.set_access(username, crate::db::accounts::Access::Adult);
        cookie
    }

    fn set_access(&self, username: &str, access: crate::db::accounts::Access) {
        let name = username.to_owned();
        self.state
            .db
            .write_blocking(move |c| {
                let a = crate::db::accounts::by_username(c, &name)?.unwrap();
                crate::db::accounts::set_access(c, a.id, access)?;
                Ok(())
            })
            .unwrap();
    }

    fn restrict(&self, item: i64) {
        self.state
            .db
            .write_blocking(move |c| {
                c.execute("UPDATE items SET restricted = 1 WHERE id = ?1", [item])?;
                Ok(())
            })
            .unwrap();
    }

    /// Stores a media file for an item directly (bypassing the studio).
    fn media(&self, item: i64, title: &str, kind: &str, bytes: &[u8]) -> i64 {
        use crate::db::media_files::{self, MediaKind, NewMediaFile};
        let (ext, mime) = match kind {
            "video" => ("mp4", "video/mp4"),
            "document" => ("pdf", "application/pdf"),
            "archive" => ("log", "text/plain"),
            _ => ("mp3", "audio/mpeg"),
        };
        let file = crate::digital::random_name(ext);
        let dir = self.state.media_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(&file), bytes).unwrap();
        let new = NewMediaFile {
            item_id: item,
            kind: MediaKind::parse(kind).unwrap(),
            title: title.into(),
            disc: 1,
            track: Some(1),
            file,
            original_name: format!("{title}.{ext}"),
            mime: mime.into(),
            size_bytes: bytes.len() as i64,
            duration_ms: Some(60_000),
            sha256: None,
        };
        self.state
            .db
            .write_blocking(move |c| media_files::add(c, &new))
            .unwrap()
    }

    /// Uploads one file to the studio as the librarian (or not).
    async fn upload(
        &self,
        item: i64,
        fields: &[(&str, &str)],
        file_name: &str,
        content_type: &str,
        bytes: &[u8],
        admin: bool,
    ) -> Resp {
        let boundary = "UpLoAdBoUnDaRy";
        let mut body: Vec<u8> = Vec::new();
        for (k, v) in fields {
            body.extend(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n"
                )
                .bytes(),
            );
        }
        body.extend(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\nContent-Type: {content_type}\r\n\r\n")
                .bytes(),
        );
        body.extend(bytes);
        body.extend(format!("\r\n--{boundary}--\r\n").bytes());
        let mut req = Request::post(format!("/admin/items/{item}/media/upload"))
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .header(header::HOST, "localhost")
            .header(header::ORIGIN, "http://localhost")
            .header(header::ACCEPT, "application/json");
        if admin {
            req = req.header(header::COOKIE, &self.admin_cookie);
        }
        self.send(req.body(Body::from(body)).unwrap()).await
    }

    fn media_dir_files(&self) -> Vec<String> {
        match std::fs::read_dir(self.state.media_dir()) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

fn json(r: &Resp) -> serde_json::Value {
    serde_json::from_str(&r.body).unwrap_or_else(|e| panic!("{e}: {}", r.body))
}

fn ids(v: &serde_json::Value) -> Vec<i64> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_i64().unwrap())
        .collect()
}

#[tokio::test]
async fn accounts_register_login_logout_and_me() {
    let a = app();
    let cookie = a.register("alice").await;
    let r = a.api("GET", "/api/account/me", None, Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::OK);
    let me = json(&r);
    assert_eq!(me["username"], "alice");
    assert_eq!(me["access"], "child", "new accounts start as child");
    assert!(me.get("passwordHash").is_none());

    // Duplicate and invalid details are 422 with per-field messages.
    let r = a
        .api(
            "POST",
            "/api/account/register",
            Some(serde_json::json!({"username": "ALICE", "email": "x", "password": "short"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let err = json(&r);
    assert!(err["fields"]["email"].is_string() && err["fields"]["password"].is_string());
    let r = a
        .api(
            "POST",
            "/api/account/register",
            Some(serde_json::json!({"username": "ALICE", "email": "new@example.org", "password": "long enough password"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        json(&r)["fields"]["username"],
        "That username is already taken."
    );

    // Log out, then the cookie no longer works.
    let r = a
        .api("POST", "/api/account/logout", None, Some(&cookie))
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert!(
        r.headers
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    let r = a.api("GET", "/api/account/me", None, Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(json(&r)["error"].is_string());

    // Log in by username or email.
    for login in ["alice", "Alice@Example.org"] {
        let r = a
            .api(
                "POST",
                "/api/account/login",
                Some(serde_json::json!({"login": login, "password": "long enough password"})),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        let set = r.headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
        assert!(set.contains("HttpOnly") && set.contains("SameSite=Lax"));
        let fresh = set.split(';').next().unwrap();
        assert_eq!(
            a.api("GET", "/api/account/me", None, Some(fresh))
                .await
                .status,
            StatusCode::OK
        );
    }
    let r = a
        .api(
            "POST",
            "/api/account/login",
            Some(serde_json::json!({"login": "alice", "password": "wrong password"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Cross-site JSON posts are refused like every other post.
    let req = Request::post("/api/account/login")
        .header(header::CONTENT_TYPE, "application/json")
        .header("sec-fetch-site", "cross-site")
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(a.send(req).await.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn registration_waits_for_a_librarian() {
    let a = app();
    let r = a.register_pending("newbie").await;
    let body = json(&r);
    assert_eq!(body["status"], "pending");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("librarian needs to approve")
    );

    // The right password says it is waiting; no session is made.
    let r = a.login("newbie").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(
        json(&r)["error"],
        "Your account is waiting for a librarian to approve it."
    );
    assert!(r.headers.get(header::SET_COOKIE).is_none());
    // A wrong password reveals nothing about the account.
    let r = a
        .api(
            "POST",
            "/api/account/login",
            Some(serde_json::json!({"login": "newbie", "password": "wrong password"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(json(&r)["error"], "That username or password isn't right.");

    // The desk shows a badge; the accounts page lists the request.
    let desk = a.get("/admin", true).await;
    assert!(desk.body.contains("count-badge") && desk.body.contains("1"));
    let page = a.get("/admin/accounts", true).await;
    assert!(page.body.contains("Waiting for approval"));
    assert!(page.body.contains("newbie@example.org"));
    assert!(page.body.contains("/approve") && page.body.contains("/reject"));

    // Only the librarian can approve.
    let id = a.account_id("newbie");
    let r = a
        .post(
            &format!("/admin/accounts/{id}/approve"),
            "access=adult",
            false,
            false,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(a.login("newbie").await.status, StatusCode::FORBIDDEN);
    let r = a
        .post(
            &format!("/admin/accounts/{id}/approve"),
            "access=teen",
            true,
            true,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // Approving (htmx) answers the lists, with the account moved across.
    let r = a
        .post(
            &format!("/admin/accounts/{id}/approve"),
            "access=adult",
            true,
            true,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!r.body.contains("<html") && r.body.contains("id=\"account-lists\""));
    assert!(r.body.contains("Approved newbie with adult access"));
    assert!(!r.body.contains("Waiting for approval"));
    assert!(r.body.contains(&format!("id=\"account-{id}\"")));
    // Approving twice finds nothing to approve.
    let r = a
        .post(
            &format!("/admin/accounts/{id}/approve"),
            "access=child",
            true,
            false,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let r = a.login("newbie").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let cookie = App::cookie_of(&r);
    let me = json(&a.api("GET", "/api/account/me", None, Some(&cookie)).await);
    assert_eq!(me["access"], "adult");
    assert!(!a.get("/admin", true).await.body.contains("count-badge"));
}

#[tokio::test]
async fn rejecting_a_request_deletes_it() {
    let a = app();
    a.register_pending("spammer").await;
    let id = a.account_id("spammer");
    let r = a
        .post(&format!("/admin/accounts/{id}/reject"), "", false, false)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = a
        .post(&format!("/admin/accounts/{id}/reject"), "", true, false)
        .await;
    assert_eq!(
        r.headers.get(header::LOCATION).unwrap(),
        "/admin/accounts?msg=rejected"
    );
    assert!(
        a.state
            .db
            .read_blocking(move |c| crate::db::accounts::get(c, id))
            .unwrap()
            .is_none()
    );
    assert_eq!(a.login("spammer").await.status, StatusCode::UNAUTHORIZED);
    // The name is free again.
    a.register_pending("spammer").await;
    let id = a.account_id("spammer");
    let r = a
        .post(&format!("/admin/accounts/{id}/reject"), "", true, true)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("Turned down spammer"));
    assert!(r.body.contains("No accounts yet."));

    // An approved account is deleted with Delete, not Reject.
    a.register("member").await;
    let id = a.account_id("member");
    let r = a
        .post(&format!("/admin/accounts/{id}/reject"), "", true, false)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(a.login("member").await.status, StatusCode::OK);
}

#[tokio::test]
async fn sessions_of_unapproved_accounts_are_refused() {
    let a = app();
    let cookie = a.register("drifter").await;
    let r = a.api("GET", "/api/account/me", None, Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::OK);
    // Should an account ever go back to waiting, its sessions stop working.
    let id = a.account_id("drifter");
    a.state
        .db
        .write_blocking(move |c| {
            c.execute("UPDATE accounts SET status = 'pending' WHERE id = ?1", [id])?;
            Ok(())
        })
        .unwrap();
    for uri in ["/api/account/me", "/api/items", "/api/playlists"] {
        let r = a.api("GET", uri, None, Some(&cookie)).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{uri}");
    }
}

#[tokio::test]
async fn admin_account_pages_require_unlocking() {
    let a = app();
    a.register_pending("waiting").await;
    let id = a.account_id("waiting");
    let r = a.get("/admin/accounts", false).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    for path in ["approve", "reject", "access", "delete"] {
        let r = a
            .post(
                &format!("/admin/accounts/{id}/{path}"),
                "access=adult",
                false,
                true,
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
    }
    assert_eq!(a.login("waiting").await.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn login_is_rate_limited() {
    let a = app();
    a.register("bob").await;
    for _ in 0..10 {
        let r = a
            .api(
                "POST",
                "/api/account/login",
                Some(serde_json::json!({"login": "bob", "password": "nope nope"})),
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let r = a
        .api(
            "POST",
            "/api/account/login",
            Some(serde_json::json!({"login": "BOB", "password": "long enough password"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn closed_registration_and_adult_default() {
    let a = app_with(|c| c.accounts.open_registration = false);
    let r = a
        .api(
            "POST",
            "/api/account/register",
            Some(serde_json::json!({"username": "carol", "email": "c@example.org", "password": "long enough password"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let a = app_with(|c| c.accounts.default_access = crate::db::accounts::Access::Adult);
    let cookie = a.register("dave").await;
    let me = json(&a.api("GET", "/api/account/me", None, Some(&cookie)).await);
    assert_eq!(me["access"], "adult");
}

#[tokio::test]
async fn api_needs_an_account() {
    let a = app();
    let item = a.add("Blue", "Joni Mitchell").await;
    let m = a.media(item, "Carey", "audio", b"abc");
    for uri in [
        "/api/account/me".to_owned(),
        "/api/items".to_owned(),
        format!("/api/items/{item}"),
        format!("/api/media/{m}"),
        format!("/api/media/{m}/stream"),
        "/api/subtitles/1".to_owned(),
        "/api/playlists".to_owned(),
        "/api/favourites".to_owned(),
    ] {
        let r = a.api("GET", &uri, None, None).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{uri}");
    }
    let r = a
        .api(
            "POST",
            "/api/media/check",
            Some(serde_json::json!({"ids": [m]})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = a
        .api(
            "POST",
            "/api/playlists",
            Some(serde_json::json!({"name": "x"})),
            Some("library_account=forged"),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = a.api("GET", "/api/nothing-here", None, None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(json(&r)["error"].is_string());
}

#[tokio::test]
async fn children_never_see_restricted_things() {
    let a = app();
    let open = a.add("Blue", "Joni Mitchell").await;
    let late = a.add("Late Film", "Someone").await;
    a.add("No copy", "").await;
    a.restrict(late);
    let song = a.media(open, "Carey", "audio", b"song bytes");
    let film = a.media(late, "Film", "video", b"film bytes");
    let sub = a
        .state
        .db
        .write_blocking(move |c| {
            crate::db::media_files::add_subtitle(c, film, "en", "English", "missing.vtt")
        })
        .unwrap();
    let child = a.register("kid").await;
    let adult = a.register_adult("parent").await;

    let r = json(&a.api("GET", "/api/items", None, Some(&child)).await);
    assert_eq!(r["total"], 1);
    assert_eq!(r["items"][0]["title"], "Blue");
    assert_eq!(r["items"][0]["restricted"], false);
    assert_eq!(r["items"][0]["kinds"], serde_json::json!(["audio"]));
    assert_eq!(r["items"][0]["typeSlug"], "book");
    let r = json(
        &a.api("GET", "/api/items?sort=title", None, Some(&adult))
            .await,
    );
    assert_eq!(r["total"], 2);
    assert_eq!(r["items"][1]["restricted"], true);
    let r = json(
        &a.api("GET", "/api/items?kind=video", None, Some(&adult))
            .await,
    );
    assert_eq!(r["total"], 1);
    let r = json(&a.api("GET", "/api/items?q=joni", None, Some(&adult)).await);
    assert_eq!(r["items"][0]["id"], open);

    for uri in [
        format!("/api/items/{late}"),
        format!("/api/media/{film}"),
        format!("/api/media/{film}/stream"),
        format!("/api/subtitles/{sub}"),
    ] {
        let r = a.api("GET", &uri, None, Some(&child)).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
    }
    let detail = json(
        &a.api("GET", &format!("/api/items/{late}"), None, Some(&adult))
            .await,
    );
    assert_eq!(detail["media"][0]["id"], film);
    assert_eq!(
        detail["media"][0]["subtitles"][0]["url"],
        format!("/api/subtitles/{sub}")
    );
    assert_eq!(
        detail["media"][0]["streamUrl"],
        format!("/api/media/{film}/stream")
    );
    assert_eq!(detail["media"][0]["itemTitle"], "Late Film");
    let m = json(
        &a.api("GET", &format!("/api/media/{song}"), None, Some(&child))
            .await,
    );
    assert_eq!(
        (m["kind"].as_str(), m["sizeBytes"].as_i64()),
        (Some("audio"), Some(10))
    );
    assert_eq!(m["durationMs"], 60_000);

    let check = |cookie: String| {
        let a = &a;
        async move {
            json(
                &a.api(
                    "POST",
                    "/api/media/check",
                    Some(serde_json::json!({"ids": [song, film, 9999, song]})),
                    Some(&cookie),
                )
                .await,
            )["allowed"]
                .clone()
        }
    };
    assert_eq!(check(child.clone()).await, serde_json::json!([song]));
    assert_eq!(check(adult.clone()).await, serde_json::json!([song, film]));
    let r = a
        .api(
            "GET",
            &format!("/api/media/{film}/stream"),
            None,
            Some(&adult),
        )
        .await;
    assert_eq!(r.body, "film bytes");
}

#[tokio::test]
async fn playlists_follow_the_visibility_rule() {
    let a = app();
    let open = a.add("Blue", "Joni Mitchell").await;
    let late = a.add("Late", "").await;
    a.restrict(late);
    let song = a.media(open, "Carey", "audio", b"1");
    let song2 = a.media(open, "River", "audio", b"2");
    let film = a.media(late, "Film", "video", b"3");
    let child = a.register("kid").await;
    let adult = a.register_adult("parent").await;

    let create = |cookie: String, name: &'static str| {
        let a = &a;
        async move {
            let r = a
                .api(
                    "POST",
                    "/api/playlists",
                    Some(serde_json::json!({"name": name})),
                    Some(&cookie),
                )
                .await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
            json(&r)["id"].as_i64().unwrap()
        }
    };
    let mixed = create(adult.clone(), "Mixed").await;
    let r = a
        .api(
            "POST",
            &format!("/api/playlists/{mixed}/entries"),
            Some(serde_json::json!({"mediaIds": [film, song]})),
            Some(&adult),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(json(&r)["entryCount"], 2);
    let late_only = create(adult.clone(), "Late only").await;
    a.api(
        "POST",
        &format!("/api/playlists/{late_only}/entries"),
        Some(serde_json::json!({"mediaIds": [film]})),
        Some(&adult),
    )
    .await;
    let empty = create(child.clone(), "Empty").await;

    // The child sees the mixed one without the film, and their own empty one.
    let list = json(&a.api("GET", "/api/playlists", None, Some(&child)).await);
    let mut names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, vec!["Empty", "Mixed"]);
    let p = json(
        &a.api(
            "GET",
            &format!("/api/playlists/{mixed}"),
            None,
            Some(&child),
        )
        .await,
    );
    assert_eq!(ids(&p["entries"]), vec![song]);
    assert_eq!(
        (p["mine"].as_bool(), p["owner"].as_str()),
        (Some(false), Some("parent"))
    );
    assert_eq!(p["entryCount"], 1);
    assert_eq!(
        a.api(
            "GET",
            &format!("/api/playlists/{late_only}"),
            None,
            Some(&child)
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // Others don't see an empty playlist; its owner does.
    assert_eq!(
        a.api(
            "GET",
            &format!("/api/playlists/{empty}"),
            None,
            Some(&adult)
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let mine = json(
        &a.api("GET", "/api/playlists?mine=1", None, Some(&child))
            .await,
    );
    assert_eq!(ids(&mine), vec![empty]);
    assert_eq!(mine[0]["mine"], true);

    // Only the owner may change a playlist; children can't add restricted files.
    let r = a
        .api(
            "PATCH",
            &format!("/api/playlists/{mixed}"),
            Some(serde_json::json!({"name": "Mine now"})),
            Some(&child),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = a
        .api(
            "POST",
            &format!("/api/playlists/{empty}/entries"),
            Some(serde_json::json!({"mediaIds": [film]})),
            Some(&child),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = a
        .api(
            "PATCH",
            &format!("/api/playlists/{empty}"),
            Some(serde_json::json!({"name": "  "})),
            Some(&child),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = a
        .api(
            "PATCH",
            &format!("/api/playlists/{empty}"),
            Some(serde_json::json!({"name": "Car songs"})),
            Some(&child),
        )
        .await;
    assert_eq!(json(&r)["name"], "Car songs");

    // Rewriting keeps entries hidden from the owner, after the rest.
    a.set_access("kid", crate::db::accounts::Access::Adult);
    a.api(
        "POST",
        &format!("/api/playlists/{empty}/entries"),
        Some(serde_json::json!({"mediaIds": [song, film, song2]})),
        Some(&child),
    )
    .await;
    a.set_access("kid", crate::db::accounts::Access::Child);
    let r = a
        .api(
            "PUT",
            &format!("/api/playlists/{empty}/entries"),
            Some(serde_json::json!({"mediaIds": [song2, song]})),
            Some(&child),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(ids(&json(&r)["entries"]), vec![song2, song]);
    let stored = a
        .state
        .db
        .read_blocking(move |c| crate::db::playlists::all_entry_ids(c, empty))
        .unwrap();
    assert_eq!(stored, vec![song2, song, film]);
    let r = a
        .api(
            "PUT",
            &format!("/api/playlists/{empty}/entries"),
            Some(serde_json::json!({"mediaIds": [film]})),
            Some(&child),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "can't place a hidden file");

    let r = a
        .api(
            "DELETE",
            &format!("/api/playlists/{mixed}"),
            None,
            Some(&child),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = a
        .api(
            "DELETE",
            &format!("/api/playlists/{mixed}"),
            None,
            Some(&adult),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        a.api(
            "GET",
            &format!("/api/playlists/{mixed}"),
            None,
            Some(&adult)
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn favourites_per_account() {
    let a = app();
    let open = a.add("Blue", "").await;
    let late = a.add("Late", "").await;
    a.restrict(late);
    let song = a.media(open, "Carey", "audio", b"1");
    let film = a.media(late, "Film", "video", b"2");
    let child = a.register("kid").await;
    let other = a.register("other").await;
    let r = a
        .api(
            "PUT",
            &format!("/api/favourites/{song}"),
            None,
            Some(&child),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = a
        .api(
            "PUT",
            &format!("/api/favourites/{film}"),
            None,
            Some(&child),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let favs = json(&a.api("GET", "/api/favourites", None, Some(&child)).await);
    assert_eq!(ids(&favs), vec![song]);
    assert!(
        json(&a.api("GET", "/api/favourites", None, Some(&other)).await)
            .as_array()
            .unwrap()
            .is_empty()
    );
    let r = a
        .api(
            "DELETE",
            &format!("/api/favourites/{song}"),
            None,
            Some(&child),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert!(
        json(&a.api("GET", "/api/favourites", None, Some(&child)).await)
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn streams_answer_range_requests() {
    let a = app();
    let item = a.add("Blue", "").await;
    let m = a.media(item, "Carey", "audio", b"0123456789");
    let cookie = a.register("kid").await;
    let get = |headers: Vec<(&'static str, String)>| {
        let a = &a;
        let cookie = cookie.clone();
        async move {
            let mut b = Request::get(format!("/api/media/{m}/stream"))
                .header(header::COOKIE, cookie)
                .header(header::ACCEPT_ENCODING, "gzip, br");
            for (k, v) in headers {
                b = b.header(k, v);
            }
            a.send(b.body(Body::empty()).unwrap()).await
        }
    };
    let full = get(vec![]).await;
    assert_eq!(full.status, StatusCode::OK);
    assert_eq!(full.body, "0123456789");
    let h = &full.headers;
    assert_eq!(h.get(header::ACCEPT_RANGES).unwrap(), "bytes");
    assert_eq!(h.get(header::CONTENT_TYPE).unwrap(), "audio/mpeg");
    assert_eq!(h.get(header::CONTENT_LENGTH).unwrap(), "10");
    assert!(
        h.get(header::CONTENT_ENCODING).is_none(),
        "never compressed"
    );
    assert_eq!(
        h.get(header::CACHE_CONTROL).unwrap(),
        "private, max-age=0, must-revalidate"
    );
    assert!(
        h.get(header::CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("inline; filename=\"Carey.mp3\"")
    );
    assert_eq!(h.get(header::X_FRAME_OPTIONS).unwrap(), "SAMEORIGIN");
    assert!(
        h.get(header::CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'self'")
    );
    let etag = h.get(header::ETAG).unwrap().to_str().unwrap().to_owned();
    let last_modified = h
        .get(header::LAST_MODIFIED)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();

    let r = get(vec![("range", "bytes=2-5".into())]).await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.body, "2345");
    assert_eq!(
        r.headers.get(header::CONTENT_RANGE).unwrap(),
        "bytes 2-5/10"
    );
    assert_eq!(r.headers.get(header::CONTENT_LENGTH).unwrap(), "4");
    let r = get(vec![("range", "bytes=-3".into())]).await;
    assert_eq!(
        (r.status, r.body.as_str()),
        (StatusCode::PARTIAL_CONTENT, "789")
    );
    let r = get(vec![("range", "bytes=7-".into())]).await;
    assert_eq!(r.body, "789");
    let r = get(vec![("range", "bytes=10-".into())]).await;
    assert_eq!(r.status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(r.headers.get(header::CONTENT_RANGE).unwrap(), "bytes */10");

    let r = get(vec![("if-none-match", etag.clone())]).await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);
    assert!(r.body.is_empty());
    let r = get(vec![
        ("range", "bytes=0-1".into()),
        ("if-range", etag.clone()),
    ])
    .await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    let r = get(vec![
        ("range", "bytes=0-1".into()),
        ("if-range", last_modified),
    ])
    .await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    let r = get(vec![
        ("range", "bytes=0-1".into()),
        ("if-range", "\"stale\"".into()),
    ])
    .await;
    assert_eq!((r.status, r.body.as_str()), (StatusCode::OK, "0123456789"));

    // HEAD answers with the headers only.
    let r = a
        .send(
            Request::head(format!("/api/media/{m}/stream"))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.is_empty());
}

#[tokio::test]
async fn upload_studio_streams_files_and_checks_them() {
    let a = app_with(|c| c.media.max_upload_mb = 1);
    let item = a.add("A Film", "Director").await;

    // Librarian only.
    let r = a
        .upload(item, &[], "a.mp3", "audio/mpeg", b"abc", false)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(
        a.get(&format!("/admin/items/{item}/media"), false)
            .await
            .status,
        StatusCode::SEE_OTHER
    );

    let r = a
        .upload(
            item,
            &[
                ("kind", "video"),
                ("title", "The Film"),
                ("disc", "1"),
                ("track", ""),
                ("duration_ms", "5400000.4"),
            ],
            "Film.mp4",
            "video/mp4",
            b"pretend film",
            true,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let created = json(&r);
    let film = created["id"].as_i64().unwrap();
    assert_eq!(created["kind"], "video");
    let stored = a
        .state
        .db
        .read_blocking(move |c| Ok(crate::db::media_files::get(c, film)?.unwrap()))
        .unwrap();
    assert_eq!(stored.title, "The Film");
    assert_eq!(stored.duration_ms, Some(5_400_000));
    assert_eq!(stored.track, None);
    assert_eq!(stored.size_bytes, 12);
    assert_eq!(stored.original_name, "Film.mp4");
    assert_eq!(
        std::fs::read(a.state.media_dir().join(&stored.file)).unwrap(),
        b"pretend film"
    );

    // Title from the file name when none is given.
    let r = a
        .upload(item, &[], "CD1/03 - Song.flac", "", b"flac", true)
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(json(&r)["title"], "03 - Song");

    // Wrong kinds and lying types are refused.
    let r = a
        .upload(
            item,
            &[],
            "setup.exe",
            "application/octet-stream",
            b"MZ",
            true,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        json(&r)["error"]
            .as_str()
            .unwrap()
            .contains("isn't a file the Player can use")
    );
    let r = a
        .upload(item, &[], "song.mp3", "text/html", b"<html>", true)
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = a
        .upload(item, &[("kind", "audio")], "film.mkv", "", b"x", true)
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Too big: refused, and nothing is left behind.
    let before = a.media_dir_files().len();
    let big = vec![b'x'; 1024 * 1024 + 10];
    let r = a
        .upload(item, &[], "big.mp3", "audio/mpeg", &big, true)
        .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(a.media_dir_files().len(), before);
    assert!(a.media_dir_files().iter().all(|f| !f.ends_with(".part")));

    // Subtitles: .srt becomes WebVTT and plays with the film.
    let srt = "1\r\n00:00:01,000 --> 00:00:02,000\r\nHello\r\n";
    let boundary = "SuBbOuNdArY";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"language\"\r\n\r\n\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"Film.fr.srt\"\r\nContent-Type: application/x-subrip\r\n\r\n{srt}\r\n--{boundary}--\r\n"
    );
    let req = Request::post(format!("/admin/items/{item}/media/{film}/subtitles"))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header(header::HOST, "localhost")
        .header(header::ORIGIN, "http://localhost")
        .header(header::ACCEPT, "application/json")
        .header(header::COOKIE, &a.admin_cookie)
        .body(Body::from(body))
        .unwrap();
    let r = a.send(req).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let sub = json(&r)["id"].as_i64().unwrap();
    let viewer = a.register("viewer").await;
    let r = a
        .api("GET", &format!("/api/subtitles/{sub}"), None, Some(&viewer))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.headers.get(header::CONTENT_TYPE).unwrap(),
        "text/vtt; charset=utf-8"
    );
    assert!(
        r.body
            .starts_with("WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.000\nHello")
    );
    let detail = json(
        &a.api("GET", &format!("/api/media/{film}"), None, Some(&viewer))
            .await,
    );
    assert_eq!(detail["subtitles"][0]["language"], "fr");
    assert_eq!(detail["subtitles"][0]["label"], "French");

    // The studio lists the files; edits and deletes work over htmx.
    let page = a.get(&format!("/admin/items/{item}/media"), true).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains("The Film")
            && page.body.contains("French")
            && page.body.contains("/static/js/media-upload.js")
    );
    let r = a
        .post(
            &format!("/admin/items/{item}/media/{film}"),
            "title=Renamed+Film&disc=2&track=",
            true,
            true,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("Renamed Film") && r.body.contains("id=\"media-list\""));
    let r = a
        .post(
            &format!("/admin/items/{item}/media/{film}/move"),
            "dir=up",
            true,
            true,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let files_before = a.media_dir_files().len();
    let r = a
        .post(
            &format!("/admin/items/{item}/media/{film}/delete"),
            "",
            true,
            true,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!r.body.contains("Renamed Film"));
    // The film and its subtitles are removed from disk (in the background).
    for _ in 0..50 {
        if a.media_dir_files().len() == files_before - 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(a.media_dir_files().len(), files_before - 2);
}

#[tokio::test]
async fn deleting_an_item_removes_its_digital_copies() {
    let a = app();
    let item = a.add("Gone", "").await;
    a.media(item, "Track", "audio", b"bytes");
    assert_eq!(a.media_dir_files().len(), 1);
    let r = a
        .post(&format!("/admin/items/{item}/delete"), "", true, false)
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    for _ in 0..50 {
        if a.media_dir_files().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(a.media_dir_files().is_empty());
}

#[tokio::test]
async fn player_is_served_with_fallback_and_headers() {
    use std::borrow::Cow;
    let files: std::collections::HashMap<&str, &[u8]> = [
        (
            "index.html",
            b"<!doctype html><title>Player</title>" as &[u8],
        ),
        ("assets/app-1a2b.js", b"console.log(1)"),
        ("sw.js", b"self.x=1"),
        ("manifest.webmanifest", b"{}"),
        ("icons/icon-192.png", b"png"),
    ]
    .into();
    let lookup = |p: &str| files.get(p).map(|b| Cow::Owned(b.to_vec()));
    let body_of = |r: Response| async {
        let bytes = r.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8_lossy(&bytes).into_owned()
    };
    for path in ["", "library", "playlists/3", "item/4/"] {
        let r = player::respond(path, lookup);
        assert_eq!(r.status(), StatusCode::OK, "{path}");
        assert_eq!(r.headers().get(header::CACHE_CONTROL).unwrap(), "no-cache");
        assert!(body_of(r).await.contains("<title>Player"));
    }
    let r = player::respond("assets/app-1a2b.js", lookup);
    assert_eq!(
        r.headers().get(header::CACHE_CONTROL).unwrap(),
        "public, max-age=31536000, immutable"
    );
    assert!(
        r.headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("javascript")
    );
    let r = player::respond("sw.js", lookup);
    assert_eq!(
        r.headers().get("service-worker-allowed").unwrap(),
        "/player/"
    );
    assert_eq!(r.headers().get(header::CACHE_CONTROL).unwrap(), "no-cache");
    let r = player::respond("manifest.webmanifest", lookup);
    assert_eq!(
        r.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/manifest+json"
    );
    assert_eq!(
        player::respond("assets/missing.js", lookup).status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        player::respond("../Cargo.toml", lookup).status(),
        StatusCode::NOT_FOUND
    );
    let r = player::respond("library", |_| None);
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    let page = body_of(r).await;
    assert!(page.contains("been built yet") && page.contains("npm --prefix player ci"));

    // Through the router: the redirect and the Player's own CSP.
    let a = app();
    let r = a.get("/player", false).await;
    assert_eq!(r.status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(r.headers.get(header::LOCATION).unwrap(), "/player/");
    let r = a.get("/player/", false).await;
    assert!(r.status == StatusCode::OK || r.status == StatusCode::SERVICE_UNAVAILABLE);
    let csp = r
        .headers
        .get(header::CONTENT_SECURITY_POLICY)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        csp.contains("worker-src 'self'")
            && csp.contains("manifest-src 'self'")
            && csp.contains("frame-src 'self'")
    );
    let site = a.get("/", false).await;
    let csp = site
        .headers
        .get(header::CONTENT_SECURITY_POLICY)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(!csp.contains("worker-src") && csp.contains("frame-ancestors 'none'"));
}

#[tokio::test]
async fn librarian_manages_accounts() {
    let a = app();
    a.register("kid").await;
    let desk = a.get("/admin", true).await;
    assert!(desk.body.contains("/admin/accounts"));
    assert_eq!(
        a.get("/admin/accounts", false).await.status,
        StatusCode::SEE_OTHER
    );
    let page = a.get("/admin/accounts", true).await;
    assert!(page.body.contains("kid") && page.body.contains("kid@example.org"));
    let id = a
        .state
        .db
        .read_blocking(|c| Ok(crate::db::accounts::by_username(c, "kid")?.unwrap().id))
        .unwrap();
    let r = a
        .post(
            &format!("/admin/accounts/{id}/access"),
            "access=adult",
            true,
            true,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("value=\"adult\" selected") && !r.body.contains("<html"));
    let r = a
        .post(
            &format!("/admin/accounts/{id}/access"),
            "access=teen",
            true,
            true,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = a
        .post(&format!("/admin/accounts/{id}/delete"), "", true, false)
        .await;
    assert_eq!(
        r.headers.get(header::LOCATION).unwrap(),
        "/admin/accounts?msg=removed"
    );
    assert!(
        !a.get("/admin/accounts", true)
            .await
            .body
            .contains("kid@example.org")
    );
}

#[tokio::test]
async fn restricted_checkbox_badge_and_player_button() {
    let a = app();
    let id = a.add("Late Film", "").await;
    let edit = a.get(&format!("/admin/items/{id}/edit"), true).await;
    assert!(edit.body.contains("Restricted (adults only)"));
    let tid = a.type_id("film").to_string();
    let r = a
        .post_multipart(
            &format!("/admin/items/{id}"),
            &[
                ("type_id", &tid),
                ("title", "Late Film"),
                ("restricted", "1"),
            ],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let page = a.get(&format!("/items/{id}"), true).await;
    assert!(page.body.contains("Adults only"));
    assert!(
        page.body.contains("Digital copy")
            && page.body.contains(&format!("/admin/items/{id}/media"))
    );
    let public = a.get(&format!("/items/{id}"), false).await;
    assert!(!public.body.contains("Adults only") && !public.body.contains("Add or manage files"));
    assert!(public.body.contains("href=\"/player/\""), "compact header");
    assert!(
        a.get("/", false).await.body.contains("href=\"/player/\""),
        "top bar"
    );
    // Unticking clears it.
    a.post_multipart(
        &format!("/admin/items/{id}"),
        &[("type_id", &tid), ("title", "Late Film")],
        &[],
    )
    .await;
    assert!(
        !a.get(&format!("/items/{id}"), true)
            .await
            .body
            .contains("Adults only")
    );
}

// ---------- the studio's "Fill from MusicBrainz" ----------

/// A stand-in MusicBrainz: a search that lists the wrong pressing first,
/// and the two releases' track lists. Counts the requests it gets.
async fn mock_musicbrainz() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use axum::extract::{Path as P, Query as Q, State as S};
    use axum::routing::get;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const RIGHT: &str = "11111111-1111-4111-8111-111111111111";
    const WRONG: &str = "22222222-2222-4222-8222-222222222222";
    fn tracks(lengths: &[i64]) -> serde_json::Value {
        let titles = ["Morning Glory", "Paper Boats", "Lanterns"];
        serde_json::Value::Array(
            lengths
                .iter()
                .enumerate()
                .map(|(i, l)| serde_json::json!({"position": i + 1, "number": (i + 1).to_string(), "title": titles[i], "length": l}))
                .collect(),
        )
    }
    let hits = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/mb/release/",
            get(
                |S(h): S<Arc<AtomicUsize>>, Q(q): Q<HashMap<String, String>>| async move {
                    h.fetch_add(1, Ordering::SeqCst);
                    let query = q.get("query").cloned().unwrap_or_default();
                    assert!(
                        query == "barcode:5012345678900" || query == "The Lanterns Night Songs",
                        "{query}"
                    );
                    axum::Json(serde_json::json!({"releases": [
                        {"id": WRONG, "title": "Night Songs (Remastered)", "date": "2011", "country": "US",
                         "artist-credit": [{"name": "The Lanterns"}], "media": [{"format": "CD", "track-count": 3}]},
                        {"id": RIGHT, "title": "Night Songs", "date": "1998-03-02", "country": "GB",
                         "artist-credit": [{"name": "The Lanterns"}], "media": [{"format": "CD", "track-count": 3}]},
                        {"id": "33333333-3333-4333-8333-333333333333", "title": "Night Songs (Deluxe)",
                         "media": [{"format": "CD", "track-count": 3}, {"format": "CD", "track-count": 9}]}
                    ]}))
                },
            ),
        )
        .route(
            "/mb/release/{id}",
            get(|S(h): S<Arc<AtomicUsize>>, P(id): P<String>| async move {
                h.fetch_add(1, Ordering::SeqCst);
                let (title, lengths): (&str, &[i64]) = match id.as_str() {
                    RIGHT => ("Night Songs", &[245_000, 181_000, 302_000]),
                    WRONG => ("Night Songs (Remastered)", &[263_000, 199_000, 321_000]),
                    _ => return Err(StatusCode::NOT_FOUND),
                };
                Ok(axum::Json(serde_json::json!({
                    "id": id, "title": title, "date": "1998-03-02", "country": "GB",
                    "artist-credit": [{"name": "The Lanterns"}],
                    "media": [{"position": 1, "format": "CD", "tracks": tracks(lengths)}]
                })))
            }),
        )
        .with_state(hits.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/mb"), hits)
}

fn ripped_files() -> serde_json::Value {
    serde_json::json!([
        {"key": "3", "name": "03 - Lanterns.flac", "track": 3, "title": "Lanterns", "durationMs": 302_400},
        {"key": "1", "name": "01 - Morning Glory.flac", "track": 1, "title": "Morning Glory", "durationMs": 245_300},
        {"key": "2", "name": "02 - Paper Boats.flac", "track": 2, "title": "Paper Boats", "durationMs": 180_600}
    ])
}

#[tokio::test]
async fn studio_fills_tracks_from_musicbrainz() {
    use std::sync::atomic::Ordering;
    let (url, hits) = mock_musicbrainz().await;
    let a = app_with(|c| {
        c.lookup.enabled = true;
        c.lookup.musicbrainz_url = url;
        c.lookup.musicbrainz_interval_ms = 0;
    });
    let item = a.add("Night Songs", "The Lanterns").await;
    a.state
        .db
        .write_blocking(move |c| {
            c.execute(
                "UPDATE items SET identifier = '5012345678900' WHERE id = ?1",
                [item],
            )?;
            Ok(())
        })
        .unwrap();
    let admin = Some(a.admin_cookie.as_str());

    // The panel is there, prefilled with the artist, title and barcode.
    let page = a.get(&format!("/admin/items/{item}/media"), true).await;
    assert!(page.body.contains(r#"id="mb-fill""#), "{}", page.body);
    assert!(page.body.contains(r#"value="The Lanterns Night Songs""#));
    assert!(page.body.contains(r#"data-barcode="5012345678900""#));

    // Librarian only.
    let search_uri = format!("/admin/items/{item}/media/musicbrainz/search");
    let body = serde_json::json!({"query": "The Lanterns Night Songs", "barcode": "5012345678900", "files": ripped_files()});
    let locked = a.api("POST", &search_uri, Some(body.clone()), None).await;
    assert_eq!(locked.status, StatusCode::FORBIDDEN);
    assert_eq!(hits.load(Ordering::SeqCst), 0);

    // Barcode and text searches, then the closest track lists, ranked by fit:
    // the right pressing comes first although MusicBrainz listed it second.
    let r = a.api("POST", &search_uri, Some(body.clone()), admin).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let v = json(&r);
    let cands = v["candidates"].as_array().unwrap();
    assert_eq!(cands.len(), 3, "deduplicated: {v}");
    assert_eq!(cands[0]["title"], "Night Songs");
    assert_eq!(cands[0]["country"], "GB");
    assert_eq!(cands[0]["year"], 1998);
    assert_eq!(cands[0]["discTracks"], serde_json::json!([3]));
    assert_eq!(cands[0]["totalMs"], 728_000);
    assert_eq!(cands[0]["fit"]["summary"], "Fits your 3 files");
    assert_eq!(cands[0]["fit"]["compared"], true);
    assert_eq!(cands[1]["title"], "Night Songs (Remastered)");
    assert!(cands[1]["fit"]["score"].as_u64() < cands[0]["fit"]["score"].as_u64());
    assert_eq!(cands[2]["format"], "2×CD");
    assert_eq!(
        cands[2]["fit"]["compared"], false,
        "its track list answered 404: compared by count only"
    );
    assert_eq!(cands[2]["fit"]["summary"], "12 tracks for your 3 files");
    assert!(
        cands[0]["url"]
            .as_str()
            .unwrap()
            .starts_with("https://musicbrainz.org/release/")
    );
    // 2 searches + 3 track lists (the unknown third answers 404 and is
    // compared by its count only).
    assert_eq!(hits.load(Ordering::SeqCst), 5);
    // Asked again: everything comes from the cache, except the 404.
    let again = a.api("POST", &search_uri, Some(body), admin).await;
    assert_eq!(json(&again)["candidates"][0]["title"], "Night Songs");
    assert_eq!(hits.load(Ordering::SeqCst), 6);

    // Matching the files to the chosen release.
    let right = cands[0]["id"].as_str().unwrap().to_owned();
    let m = a
        .api(
            "POST",
            &format!("/admin/items/{item}/media/musicbrainz/match"),
            Some(serde_json::json!({"release": right, "files": ripped_files()})),
            admin,
        )
        .await;
    assert_eq!(m.status, StatusCode::OK, "{}", m.body);
    let m = json(&m);
    let placed: Vec<(String, i64, String, String)> = m["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| {
            (
                x["key"].as_str().unwrap().to_owned(),
                x["track"].as_i64().unwrap(),
                x["title"].as_str().unwrap().to_owned(),
                x["confidence"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        placed,
        vec![
            ("3".into(), 3, "Lanterns".into(), "good".into()),
            ("1".into(), 1, "Morning Glory".into(), "good".into()),
            ("2".into(), 2, "Paper Boats".into(), "good".into()),
        ]
    );
    assert_eq!(m["missing"], serde_json::json!([]));
    assert_eq!(m["release"]["media"][0]["tracks"][1]["lengthMs"], 181_000);

    // The release on its own; ids that aren't, and ones MusicBrainz lacks.
    let rel = a
        .get(
            &format!("/admin/items/{item}/media/musicbrainz/release/{right}"),
            true,
        )
        .await;
    assert_eq!(json(&rel)["title"], "Night Songs");
    let bad = a
        .get(
            &format!("/admin/items/{item}/media/musicbrainz/release/nope"),
            true,
        )
        .await;
    assert_eq!(bad.status, StatusCode::NOT_FOUND);
    let gone = a
        .get(
            &format!(
                "/admin/items/{item}/media/musicbrainz/release/44444444-4444-4444-8444-444444444444"
            ),
            true,
        )
        .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
    assert!(
        gone.body.contains("MusicBrainz has no release"),
        "{}",
        gone.body
    );

    // A pasted MusicBrainz link searches for nothing else.
    let before = hits.load(Ordering::SeqCst);
    let linked = a
        .api(
            "POST",
            &search_uri,
            Some(serde_json::json!({"query": format!("https://musicbrainz.org/release/{right}"), "files": []})),
            admin,
        )
        .await;
    let linked = json(&linked);
    assert_eq!(linked["candidates"].as_array().unwrap().len(), 1);
    assert!(linked["candidates"][0]["fit"].is_null(), "no files, no fit");
    assert_eq!(hits.load(Ordering::SeqCst), before, "cached");

    // An empty search is refused; a missing item is 404.
    let empty = a
        .api(
            "POST",
            &search_uri,
            Some(serde_json::json!({"query": " "})),
            admin,
        )
        .await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);
    let missing = a
        .api(
            "POST",
            "/admin/items/9999/media/musicbrainz/search",
            Some(serde_json::json!({"query": "x"})),
            admin,
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);

    // The entry's artist and year are only filled where empty.
    let filled = a
        .api(
            "POST",
            &format!("/admin/items/{item}/media/musicbrainz/item"),
            Some(serde_json::json!({"creator": "Somebody Else", "year": 1998})),
            admin,
        )
        .await;
    assert_eq!(filled.status, StatusCode::OK, "{}", filled.body);
    assert_eq!(json(&filled)["creator"], "The Lanterns");
    assert_eq!(json(&filled)["year"], 1998);
}

#[tokio::test]
async fn studio_musicbrainz_panel_when_lookups_are_off() {
    let a = app();
    let item = a.add("Night Songs", "The Lanterns").await;
    let page = a.get(&format!("/admin/items/{item}/media"), true).await;
    assert!(page.body.contains(r#"data-enabled="0""#));
    assert!(page.body.contains("Online lookups are switched off"));
    assert!(
        !page.body.contains(r#"id="mb-query""#),
        "nothing to search with"
    );
    let r = a
        .api(
            "POST",
            &format!("/admin/items/{item}/media/musicbrainz/search"),
            Some(serde_json::json!({"query": "Night Songs"})),
            Some(&a.admin_cookie),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(json(&r)["error"].as_str().unwrap().contains("switched off"));
}

// ---------- Library Rip: connecting, the rip API, archive files ----------

/// The RFC 7636 example verifier and its S256 challenge.
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
const REDIRECT: &str = "http://127.0.0.1:53682/callback";

fn connect_query(redirect: &str, state: &str) -> String {
    format!(
        "client=library-rip&name={}&redirect_uri={}&state={state}&code_challenge={CHALLENGE}&code_challenge_method=S256",
        crate::views::urlencode("Henry's desktop"),
        crate::views::urlencode(redirect),
    )
}

/// The value of one query parameter in a URL.
fn query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| v.to_owned())
    })
}

impl App {
    /// A request to the rip API, as Library Rip makes it (no Origin, no
    /// cookies), with an optional API key.
    async fn rip(
        &self,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
        key: Option<&str>,
    ) -> Resp {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "library.example.org");
        if let Some(k) = key {
            b = b.header(header::AUTHORIZATION, format!("Bearer {k}"));
        }
        let body = match body {
            Some(v) => {
                b = b.header(header::CONTENT_TYPE, "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        self.send(b.body(body).unwrap()).await
    }

    /// The librarian answers the connect page; returns the redirect.
    async fn answer_connect(&self, redirect: &str, decision: &str) -> Resp {
        let form = format!("{}&decision={decision}", connect_query(redirect, "st8"));
        self.post("/connect", &form, true, false).await
    }

    /// The whole browser handshake up to the one-time code.
    async fn connect_code(&self) -> String {
        let r = self.answer_connect(REDIRECT, "yes").await;
        assert_eq!(r.status, StatusCode::FOUND, "{}", r.body);
        let to = r.headers[header::LOCATION].to_str().unwrap().to_owned();
        assert!(to.starts_with(&format!("{REDIRECT}?code=")), "{to}");
        assert_eq!(query_param(&to, "state").as_deref(), Some("st8"));
        query_param(&to, "code").unwrap()
    }

    async fn exchange(&self, code: &str, verifier: &str, redirect: &str) -> Resp {
        self.rip(
            "POST",
            "/api/rip/token",
            Some(serde_json::json!({
                "grantType": "authorization_code",
                "code": code,
                "codeVerifier": verifier,
                "redirectUri": redirect,
            })),
            None,
        )
        .await
    }

    /// Connects a ripper and returns its API key.
    async fn rip_key(&self) -> String {
        let code = self.connect_code().await;
        let r = self.exchange(&code, VERIFIER, REDIRECT).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        json(&r)["apiKey"].as_str().unwrap().to_owned()
    }

    async fn poll(&self, device_code: &str) -> Resp {
        self.rip(
            "POST",
            "/api/rip/token",
            Some(serde_json::json!({"grantType": "device_code", "deviceCode": device_code})),
            None,
        )
        .await
    }

    /// Uploads one file through the rip API.
    async fn rip_upload(
        &self,
        item: i64,
        fields: &[(&str, &str)],
        file_name: &str,
        bytes: &[u8],
        key: &str,
    ) -> Resp {
        let boundary = "RiPbOuNdArY";
        let mut body: Vec<u8> = Vec::new();
        for (k, v) in fields {
            body.extend(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n"
                )
                .bytes(),
            );
        }
        body.extend(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\nContent-Type: application/octet-stream\r\n\r\n")
                .bytes(),
        );
        body.extend(bytes);
        body.extend(format!("\r\n--{boundary}--\r\n").bytes());
        let req = Request::post(format!("/api/rip/items/{item}/media"))
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .header(header::HOST, "library.example.org")
            .header(header::AUTHORIZATION, format!("Bearer {key}"))
            .body(Body::from(body))
            .unwrap();
        self.send(req).await
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    rip::hex(&Sha256::digest(bytes))
}

#[tokio::test]
async fn rip_info_and_source_links() {
    let a = app();
    let r = a.rip("GET", "/api/rip/info", None, None).await;
    assert_eq!(r.status, StatusCode::OK);
    let info = json(&r);
    assert_eq!(info["name"], "Tallow Lane Library");
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(info["minRipperVersion"], rip::MIN_RIPPER_VERSION);
    assert_eq!(info["sourceUrl"], crate::config::DEFAULT_SOURCE_URL);

    // The Player can read it too, without an account.
    let r = a.api("GET", "/api/info", None, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(json(&r)["sourceUrl"], crate::config::DEFAULT_SOURCE_URL);

    // Every page's footer offers the source code (the AGPL's network clause).
    for (uri, admin) in [("/", false), ("/search?q=x", false), ("/admin", true)] {
        let page = a.get(uri, admin).await;
        assert!(
            page.body.contains(&format!(
                r#"href="{}" rel="noopener">Source code</a>"#,
                crate::config::DEFAULT_SOURCE_URL
            )),
            "{uri}"
        );
    }
    let custom = app_with(|c| c.source_url = "https://git.example.org/lib".into());
    assert!(
        custom
            .get("/", false)
            .await
            .body
            .contains("https://git.example.org/lib")
    );
}

#[tokio::test]
async fn connect_full_pkce_flow() {
    let a = app();
    let query = connect_query(REDIRECT, "st8");

    // Not unlocked yet: the unlock form first, coming back to the same link.
    let r = a.get(&format!("/connect?{query}"), false).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let to = r.headers[header::LOCATION].to_str().unwrap();
    assert!(
        to.starts_with("/unlock?next=%2Fconnect%3Fclient%3Dlibrary-rip"),
        "{to}"
    );

    let r = a.get(&format!("/connect?{query}"), true).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("Henry&#39;s desktop") || r.body.contains("Henry's desktop"));
    assert!(r.body.contains("Yes, connect") && r.body.contains(">No<"));
    assert!(r.body.contains("Librarian's desk"));
    let csp = r.headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    assert!(
        csp.contains("form-action 'self' http://127.0.0.1:* http://localhost:*"),
        "the answer may redirect to the ripper: {csp}"
    );
    assert!(
        a.get("/", false).await.headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("form-action 'self';"),
        "only the connect page relaxes form-action"
    );

    let code = a.connect_code().await;
    let r = a.exchange(&code, VERIFIER, REDIRECT).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.headers[header::CACHE_CONTROL], "no-store");
    let t = json(&r);
    let key = t["apiKey"].as_str().unwrap();
    assert!(key.starts_with("lrk_") && key.len() == 47);
    assert_eq!(t["keyName"], "Henry's desktop");
    assert_eq!(t["library"]["name"], "Tallow Lane Library");
    assert_eq!(t["library"]["url"], "http://library.example.org");

    let r = a.rip("GET", "/api/rip/whoami", None, Some(key)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let me = json(&r);
    assert_eq!(me["keyId"], t["keyId"]);
    assert_eq!(me["keyName"], "Henry's desktop");

    // Only the hash is stored, and using the key records when.
    let (hash_len, used): (i64, Option<i64>) = a
        .state
        .db
        .read_blocking(|c| {
            Ok(c.query_row(
                "SELECT length(key_hash), last_used_at FROM api_keys",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!(hash_len, 32);
    assert!(used.is_some());
}

#[tokio::test]
async fn connect_refusals_and_bad_codes() {
    let a = app();
    // No sends the ripper an access_denied answer with its state.
    let r = a.answer_connect(REDIRECT, "no").await;
    assert_eq!(r.status, StatusCode::FOUND);
    let to = r.headers[header::LOCATION].to_str().unwrap();
    assert_eq!(to, format!("{REDIRECT}?error=access_denied&state=st8"));

    // A wrong verifier fails, and burns the code.
    let code = a.connect_code().await;
    let r = a
        .exchange(&code, &VERIFIER.replace('d', "e"), REDIRECT)
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&r)["error"], "invalid_grant");
    assert!(
        json(&r)["message"]
            .as_str()
            .unwrap()
            .contains("didn't work")
    );
    let r = a.exchange(&code, VERIFIER, REDIRECT).await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "single use, even after a failure"
    );

    // A code works once.
    let code = a.connect_code().await;
    assert_eq!(
        a.exchange(&code, VERIFIER, REDIRECT).await.status,
        StatusCode::OK
    );
    let r = a.exchange(&code, VERIFIER, REDIRECT).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&r)["error"], "invalid_grant");

    // Bound to the redirect URI it was issued for.
    let code = a.connect_code().await;
    let r = a
        .exchange(&code, VERIFIER, "http://127.0.0.1:9999/callback")
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // Codes expire (after five minutes).
    let code = a.connect_code().await;
    a.state.rip.expire_all();
    let r = a.exchange(&code, VERIFIER, REDIRECT).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&r)["error"], "invalid_grant");

    // Nonsense bodies.
    let r = a
        .rip(
            "POST",
            "/api/rip/token",
            Some(serde_json::json!({"grantType": "password"})),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&r)["error"], "invalid_request");
}

#[tokio::test]
async fn connect_refuses_other_redirects() {
    let a = app();
    for bad in [
        "https://evil.example/callback",
        "http://127.0.0.1/callback",
        "http://example.org:5000/callback",
        "http://127.0.0.1:5000@evil.example/callback",
        "http://localhost.evil.example:5000/callback",
    ] {
        let r = a
            .get(&format!("/connect?{}", connect_query(bad, "s")), true)
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
        assert!(r.body.contains("somewhere other than Library Rip"), "{bad}");
        assert!(r.headers.get(header::LOCATION).is_none());
        let r = a.answer_connect(bad, "yes").await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
        assert!(r.headers.get(header::LOCATION).is_none(), "{bad}");
    }
    // Missing PKCE, a plain challenge or another client are refused too.
    let r = a
        .get(
            &format!(
                "/connect?client=library-rip&redirect_uri={}&state=s&code_challenge={CHALLENGE}&code_challenge_method=plain",
                crate::views::urlencode(REDIRECT)
            ),
            true,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = a
        .get(
            &format!(
                "/connect?{}",
                connect_query(REDIRECT, "s").replace("library-rip", "other")
            ),
            true,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // localhost works as well as 127.0.0.1.
    let r = a.answer_connect("http://localhost:4000/cb", "yes").await;
    assert_eq!(r.status, StatusCode::FOUND);
}

#[tokio::test]
async fn device_code_flow_in_every_state() {
    let a = app();
    let start = |name: &'static str| {
        let a = &a;
        async move {
            let r = a
                .rip(
                    "POST",
                    "/api/rip/device",
                    Some(serde_json::json!({"name": name})),
                    None,
                )
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.body);
            json(&r)
        }
    };
    let d = start("Henry's laptop").await;
    let user_code = d["userCode"].as_str().unwrap().to_owned();
    assert!(rip::normalise_user_code(&user_code).as_deref() == Some(user_code.as_str()));
    assert_eq!(user_code.len(), 9);
    assert_eq!(
        d["verificationUri"],
        "http://library.example.org/connect/device"
    );
    assert_eq!(d["interval"], 5);
    assert_eq!(d["expiresIn"], 600);
    let device_code = d["deviceCode"].as_str().unwrap().to_owned();

    // Waiting for the librarian, and polling too fast.
    let r = a.poll(&device_code).await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED);
    assert_eq!(json(&r)["error"], "authorization_pending");
    let r = a.poll(&device_code).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json(&r)["error"], "slow_down");

    // The librarian unlocks, types the code (any case, no dash) and says yes.
    let r = a.get("/connect/device", false).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "librarian only");
    let page = a.get("/connect/device", true).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("WXYZ-1234"));
    let r = a
        .post("/connect/device", "code=ZZZZ-0000", true, false)
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.body.contains("That code isn"));
    let typed = user_code.to_lowercase().replace('-', " ");
    let r = a
        .post(
            "/connect/device",
            &format!("code={}", crate::views::urlencode(&typed)),
            true,
            false,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("laptop") && r.body.contains("Yes, connect"));
    let r = a
        .post(
            "/connect/device",
            &format!("code={user_code}&decision=yes"),
            true,
            false,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("Connected"));
    let r = a.poll(&device_code).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let key = json(&r)["apiKey"].as_str().unwrap().to_owned();
    assert_eq!(json(&r)["keyName"], "Henry's laptop");
    assert_eq!(
        a.rip("GET", "/api/rip/whoami", None, Some(&key))
            .await
            .status,
        StatusCode::OK
    );
    let r = a.poll(&device_code).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "used up");

    // Refused.
    let d = start("Stranger").await;
    let r = a
        .post(
            "/connect/device",
            &format!("code={}&decision=no", d["userCode"].as_str().unwrap()),
            true,
            false,
        )
        .await;
    assert!(r.body.contains("Not connected"));
    let r = a.poll(d["deviceCode"].as_str().unwrap()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(json(&r)["error"], "access_denied");

    // Expired.
    let d = start("Slow").await;
    a.state.rip.expire_all();
    let r = a.poll(d["deviceCode"].as_str().unwrap()).await;
    assert_eq!(r.status, StatusCode::GONE);
    assert_eq!(json(&r)["error"], "expired_token");
    let r = a
        .post(
            "/connect/device",
            &format!("code={}", d["userCode"].as_str().unwrap()),
            true,
            false,
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "expired codes can't be approved"
    );

    // Never issued.
    let r = a.poll("made-up").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&r)["error"], "invalid_grant");
}

impl App {
    /// A rip API request arriving over a connection from `peer`, optionally
    /// with an `X-Forwarded-For` header.
    async fn rip_from(
        &self,
        peer: &str,
        forwarded_for: Option<&str>,
        uri: &str,
        body: serde_json::Value,
    ) -> Resp {
        let mut b = Request::post(uri)
            .header(header::HOST, "library.example.org")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(f) = forwarded_for {
            b = b.header("x-forwarded-for", f);
        }
        let mut req = b.body(Body::from(body.to_string())).unwrap();
        let peer: std::net::SocketAddr = peer.parse().unwrap();
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo(peer));
        self.send(req).await
    }

    async fn new_device_from(&self, peer: &str) -> Resp {
        self.rip_from(
            peer,
            None,
            "/api/rip/device",
            serde_json::json!({"name": "x"}),
        )
        .await
    }

    async fn exchange_from(&self, peer: &str, code: &str) -> Resp {
        self.rip_from(
            peer,
            None,
            "/api/rip/token",
            serde_json::json!({
                "grantType": "authorization_code",
                "code": code,
                "codeVerifier": VERIFIER,
                "redirectUri": REDIRECT,
            }),
        )
        .await
    }

    async fn poll_from(&self, peer: &str, device_code: &str) -> Resp {
        self.rip_from(
            peer,
            None,
            "/api/rip/token",
            serde_json::json!({"grantType": "device_code", "deviceCode": device_code}),
        )
        .await
    }

    /// Thirty codes that don't work, from `peer`: its next bad one is 429.
    async fn use_up_failures(&self, peer: &str) {
        for i in 0..30 {
            // Made-up codes of both kinds count.
            let r = if i % 2 == 0 {
                self.exchange_from(peer, "guess").await
            } else {
                self.poll_from(peer, "made-up").await
            };
            assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
            assert_eq!(json(&r)["error"], "invalid_grant");
        }
        let r = self.exchange_from(peer, "guess").await;
        assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(json(&r)["error"], "too_many_requests");
    }
}

#[tokio::test]
async fn device_codes_are_limited_per_address() {
    let a = app();
    for _ in 0..20 {
        assert_eq!(
            a.new_device_from("10.0.0.1:4000").await.status,
            StatusCode::OK
        );
    }
    let r = a.new_device_from("10.0.0.1:4001").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json(&r)["error"], "too_many_requests");
    // Someone else can still start connecting.
    assert_eq!(
        a.new_device_from("10.0.0.2:4000").await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn bad_codes_slow_down_only_their_sender() {
    let a = app();
    a.use_up_failures("10.0.0.1:4000").await;
    // Another address's mistakes are still just mistakes…
    let r = a.exchange_from("10.0.0.2:4000", "guess").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // …and its good code works.
    let code = a.connect_code().await;
    let r = a.exchange_from("10.0.0.2:4000", &code).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
}

#[tokio::test]
async fn a_good_code_works_even_from_a_slowed_down_address() {
    let a = app();
    a.use_up_failures("10.0.0.1:4000").await;
    let code = a.connect_code().await;
    let r = a.exchange_from("10.0.0.1:4000", &code).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert!(json(&r)["apiKey"].as_str().unwrap().starts_with("lrk_"));

    // An approved device code too, polled from the same address.
    let d = json(&a.new_device_from("10.0.0.1:4000").await);
    let device_code = d["deviceCode"].as_str().unwrap();
    let r = a
        .post(
            "/connect/device",
            &format!("code={}&decision=yes", d["userCode"].as_str().unwrap()),
            true,
            false,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = a.poll_from("10.0.0.1:4000", device_code).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
}

#[tokio::test]
async fn waiting_on_a_known_device_code_never_counts_as_a_failure() {
    let a = app();
    let d = json(&a.new_device_from("10.0.0.1:4000").await);
    let device_code = d["deviceCode"].as_str().unwrap();
    // Far more polls than the failure limit, pending and too fast.
    for _ in 0..40 {
        let r = a.poll_from("10.0.0.1:4000", device_code).await;
        assert!(
            [
                StatusCode::PRECONDITION_REQUIRED,
                StatusCode::TOO_MANY_REQUESTS
            ]
            .contains(&r.status),
            "{}",
            r.body
        );
        assert_ne!(json(&r)["error"], "too_many_requests");
    }
    // Not one of them was a failure: a bad code is still only a mistake.
    let r = a.exchange_from("10.0.0.1:4000", "guess").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&r)["error"], "invalid_grant");
}

#[tokio::test]
async fn forwarded_addresses_are_believed_only_from_the_proxy() {
    let a = app();
    // Behind the proxy on this computer, clients are told apart by the
    // address the proxy added (the last one).
    for _ in 0..20 {
        let r = a
            .rip_from(
                "127.0.0.1:5000",
                Some("6.6.6.6, 203.0.113.7"),
                "/api/rip/device",
                serde_json::json!({"name": "x"}),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK);
    }
    let blocked = a
        .rip_from(
            "127.0.0.1:5001",
            Some("203.0.113.7"),
            "/api/rip/device",
            serde_json::json!({"name": "x"}),
        )
        .await;
    assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
    let other = a
        .rip_from(
            "127.0.0.1:5000",
            Some("203.0.113.7, 198.51.100.1"),
            "/api/rip/device",
            serde_json::json!({"name": "x"}),
        )
        .await;
    assert_eq!(other.status, StatusCode::OK, "a different client");

    // A client connecting directly can't dodge its limit by making up
    // X-Forwarded-For.
    for i in 0..20 {
        let r = a
            .rip_from(
                "10.9.9.9:4000",
                Some(&format!("192.0.2.{i}")),
                "/api/rip/device",
                serde_json::json!({"name": "x"}),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK);
    }
    let r = a
        .rip_from(
            "10.9.9.9:4000",
            Some("192.0.2.200"),
            "/api/rip/device",
            serde_json::json!({"name": "x"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
}

#[test]
fn client_addresses_come_from_the_connection_or_the_local_proxy() {
    let headers = |xff: &str| {
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-forwarded-for", xff.parse().unwrap());
        h
    };
    let ip = |s: &str| Some(s.parse::<std::net::IpAddr>().unwrap());
    let remote = Some("198.51.100.4:1234".parse().unwrap());
    let local = Some("127.0.0.1:1234".parse().unwrap());
    let local6 = Some("[::1]:1234".parse().unwrap());
    assert_eq!(ClientIp::from_parts(None, &headers("1.2.3.4")).0, None);
    assert_eq!(
        ClientIp::from_parts(None, &headers("1.2.3.4")).key(),
        "unknown"
    );
    assert_eq!(
        ClientIp::from_parts(remote, &headers("1.2.3.4")).0,
        ip("198.51.100.4")
    );
    assert_eq!(
        ClientIp::from_parts(local, &headers("9.9.9.9, 1.2.3.4")).0,
        ip("1.2.3.4")
    );
    assert_eq!(
        ClientIp::from_parts(local6, &headers("[2001:db8::1]:443")).0,
        ip("2001:db8::1")
    );
    assert_eq!(
        ClientIp::from_parts(local, &headers("1.2.3.4:80")).0,
        ip("1.2.3.4")
    );
    // Garbage or no header: the proxy's own address.
    assert_eq!(
        ClientIp::from_parts(local, &headers("not an address")).0,
        ip("127.0.0.1")
    );
    assert_eq!(
        ClientIp::from_parts(local, &axum::http::HeaderMap::new()).0,
        ip("127.0.0.1")
    );
}

#[tokio::test]
async fn revoked_keys_get_401() {
    let a = app();
    let key = a.rip_key().await;
    // Missing or made-up keys.
    let r = a.rip("GET", "/api/rip/whoami", None, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.headers[header::WWW_AUTHENTICATE], "Bearer");
    assert!(
        json(&r)["error"]
            .as_str()
            .unwrap()
            .contains("isn't connected")
    );
    let r = a
        .rip(
            "GET",
            "/api/rip/whoami",
            None,
            Some("lrk_madeupmadeupmadeupmadeupmadeup"),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    // The ripper disconnecting itself.
    let r = a.rip("DELETE", "/api/rip/token", None, Some(&key)).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = a.rip("GET", "/api/rip/whoami", None, Some(&key)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    // The librarian disconnecting one from the desk.
    let key = a.rip_key().await;
    let desk = a.get("/admin", true).await;
    assert!(desk.body.contains("Connected rippers"));
    assert!(desk.body.contains("Henry"));
    assert!(desk.body.contains("Disconnect"));
    let id = json(&a.rip("GET", "/api/rip/whoami", None, Some(&key)).await)["keyId"]
        .as_i64()
        .unwrap();
    let r = a
        .post(&format!("/admin/rippers/{id}/disconnect"), "", false, false)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "librarian only");
    let r = a
        .post(&format!("/admin/rippers/{id}/disconnect"), "", true, false)
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let r = a.rip("GET", "/api/rip/whoami", None, Some(&key)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let desk = a.get("/admin?msg=disconnected", true).await;
    assert!(desk.body.contains("No computers are connected"));
    assert!(desk.body.contains("Disconnected."));
    let r = a
        .post(&format!("/admin/rippers/{id}/disconnect"), "", true, false)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "already disconnected");
}

#[tokio::test]
async fn api_keys_open_only_the_rip_api() {
    let a = app();
    let key = a.rip_key().await;
    let item = a.add("Blue", "Joni Mitchell").await;
    // Player endpoints want an account, not a key.
    for uri in [
        "/api/items",
        "/api/account/me",
        "/api/favourites",
        "/api/playlists",
    ] {
        let r = a.rip("GET", uri, None, Some(&key)).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{uri}");
    }
    // The desk and the studio want the librarian.
    let r = a.rip("GET", "/admin", None, Some(&key)).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let r = a
        .rip_upload(item, &[("kind", "audio")], "a.flac", b"x", &key)
        .await;
    assert_eq!(
        r.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "missing sha256, but the key works here"
    );
    let req = Request::post(format!("/admin/items/{item}/media/upload"))
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .header(header::CONTENT_TYPE, "multipart/form-data; boundary=x")
        .body(Body::from("--x--\r\n"))
        .unwrap();
    assert_eq!(a.send(req).await.status, StatusCode::FORBIDDEN);
    let r = a
        .rip("POST", "/admin/rippers/1/disconnect", None, Some(&key))
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // And the librarian's cookie doesn't open the rip API.
    let r = a
        .api("GET", "/api/rip/whoami", None, Some(&a.admin_cookie))
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn rip_api_types_items_and_covers() {
    let a = app();
    let key = a.rip_key().await;
    let r = a.rip("GET", "/api/rip/types", None, Some(&key)).await;
    assert_eq!(r.status, StatusCode::OK);
    let types = json(&r);
    let music = types
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["slug"] == "music")
        .unwrap();
    assert_eq!(music["lookup"], "music");

    let r = a
        .rip(
            "POST",
            "/api/rip/items",
            Some(serde_json::json!({
                "typeSlug": "music",
                "title": "Night Songs",
                "creator": "The Lanterns",
                "year": 1998,
                "identifier": "5012345678900",
                "description": "Ripped with Library Rip.",
                "tags": ["folk", "cd"],
                "musicbrainzReleaseId": "B84EE12A-09EF-421B-82DE-0441A926375B",
            })),
            Some(&key),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let created = json(&r);
    let id = created["id"].as_i64().unwrap();
    assert_eq!(
        created["url"],
        format!("http://library.example.org/items/{id}")
    );
    let it = a
        .state
        .db
        .read_blocking(move |c| Ok(crate::db::items::get(c, id)?.unwrap()))
        .unwrap();
    assert_eq!((it.type_slug.as_str(), it.year), ("music", Some(1998)));
    assert_eq!(it.tags, "folk, cd");
    assert!(
        it.notes
            .contains("https://musicbrainz.org/release/b84ee12a-09ef-421b-82de-0441a926375b")
    );

    // Problems are 422 with the field named.
    for (body, field) in [
        (
            serde_json::json!({"typeSlug": "music", "title": " "}),
            "title",
        ),
        (
            serde_json::json!({"typeSlug": "vinyl", "title": "X"}),
            "typeSlug",
        ),
        (
            serde_json::json!({"title": "X", "musicbrainzReleaseId": "nope"}),
            "musicbrainzReleaseId",
        ),
        (serde_json::json!({"title": "X", "year": "soon"}), "year"),
    ] {
        let r = a
            .rip("POST", "/api/rip/items", Some(body), Some(&key))
            .await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.body);
        assert!(json(&r)["fields"][field].is_string(), "{field}: {}", r.body);
    }

    // Finding an entry to attach to: words in any order, or the barcode.
    a.add("Night Train", "Someone Else").await;
    let r = a
        .rip(
            "GET",
            "/api/rip/items?q=lanterns+night&type=music",
            None,
            Some(&key),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let found = json(&r);
    assert_eq!(ids(&found), vec![id]);
    assert_eq!(found[0]["fileCount"], 0);
    assert_eq!(found[0]["identifier"], "5012345678900");
    assert_eq!(
        found[0]["url"],
        format!("http://library.example.org/items/{id}")
    );
    let r = a
        .rip("GET", "/api/rip/items?q=5012345678900", None, Some(&key))
        .await;
    assert_eq!(ids(&json(&r)), vec![id]);
    let r = a
        .rip("GET", "/api/rip/items?q=night&type=book", None, Some(&key))
        .await;
    assert_eq!(json(&r).as_array().unwrap().len(), 1, "type filter");

    // The front cover.
    let boundary = "CoVeR";
    let mut body: Vec<u8> = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"cover.png\"\r\nContent-Type: image/png\r\n\r\n"
    )
    .into_bytes();
    body.extend(png());
    body.extend(format!("\r\n--{boundary}--\r\n").bytes());
    let photo = |item: i64, body: Vec<u8>| {
        Request::post(format!("/api/rip/items/{item}/photos"))
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .header(header::AUTHORIZATION, format!("Bearer {key}"))
            .body(Body::from(body))
            .unwrap()
    };
    let r = a.send(photo(id, body.clone())).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let n = a
        .state
        .db
        .read_blocking(move |c| Ok(crate::db::photos::for_item(c, id)?.len()))
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(
        a.send(photo(id + 100, body)).await.status,
        StatusCode::NOT_FOUND
    );
    let not_png = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"c.png\"\r\n\r\nnot a picture\r\n--{boundary}--\r\n"
    );
    let r = a.send(photo(id, not_png.into_bytes())).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn rip_media_upload_checks_sha256() {
    let a = app();
    let key = a.rip_key().await;
    let item = a.add("Night Songs", "The Lanterns").await;
    let flac = b"fLaC pretend audio".to_vec();
    let good = sha256_hex(&flac);

    let r = a
        .rip_upload(
            item,
            &[
                ("kind", "audio"),
                ("title", "Lantern Light"),
                ("disc", "1"),
                ("track", "1"),
                ("duration_ms", "1000"),
                ("sha256", &good.to_uppercase()),
            ],
            "01 Lantern Light.flac",
            &flac,
            &key,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let audio = json(&r)["id"].as_i64().unwrap();

    // A damaged file is refused and nothing is kept.
    let before = a.media_dir_files().len();
    let r = a
        .rip_upload(
            item,
            &[("kind", "audio"), ("sha256", &"0".repeat(64))],
            "02.flac",
            &flac,
            &key,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        json(&r)["fields"]["sha256"]
            .as_str()
            .unwrap()
            .contains("damaged")
    );
    assert_eq!(
        a.media_dir_files().len(),
        before,
        "the damaged copy is removed"
    );
    // No checksum, or a malformed one, is refused before the file is stored.
    let r = a
        .rip_upload(item, &[("kind", "audio")], "02.flac", &flac, &key)
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = a
        .rip_upload(
            item,
            &[("kind", "audio"), ("sha256", "abc")],
            "02.flac",
            &flac,
            &key,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    // Unknown items and kinds.
    let r = a
        .rip_upload(
            item + 50,
            &[("kind", "audio"), ("sha256", &good)],
            "a.flac",
            &flac,
            &key,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = a
        .rip_upload(
            item,
            &[("kind", "hologram"), ("sha256", &good)],
            "a.flac",
            &flac,
            &key,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);

    // The log is kept as an archive file.
    let log = b"Library Rip log\nAccurately ripped (confidence 5)\n".to_vec();
    let r = a
        .rip_upload(
            item,
            &[
                ("kind", "archive"),
                ("title", "Rip log"),
                ("sha256", &sha256_hex(&log)),
            ],
            "rip.log",
            &log,
            &key,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let archived = json(&r)["id"].as_i64().unwrap();

    // Listing what's there lets an interrupted upload skip what arrived.
    let r = a
        .rip(
            "GET",
            &format!("/api/rip/items/{item}/media"),
            None,
            Some(&key),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let list = json(&r);
    assert_eq!(ids(&list), vec![audio, archived]);
    assert_eq!(list[0]["sha256"], good);
    assert_eq!(list[0]["kind"], "audio");
    assert_eq!(list[0]["track"], 1);
    assert_eq!(list[1]["kind"], "archive");
    let r = a
        .rip(
            "GET",
            &format!("/api/rip/items/{}/media", item + 50),
            None,
            Some(&key),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // The studio stores a SHA-256 for every upload too, and checks one if sent.
    let r = a
        .upload(
            item,
            &[("kind", "audio")],
            "03.flac",
            "audio/flac",
            b"studio bytes",
            true,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let sid = json(&r)["id"].as_i64().unwrap();
    let stored = a
        .state
        .db
        .read_blocking(move |c| Ok(crate::db::media_files::get(c, sid)?.unwrap().sha256))
        .unwrap();
    assert_eq!(stored, Some(sha256_hex(b"studio bytes")));
    let r = a
        .upload(
            item,
            &[("kind", "audio"), ("sha256", &good)],
            "04.flac",
            "audio/flac",
            b"other",
            true,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn archive_files_are_hidden_from_the_player() {
    let a = app();
    let cookie = a.register_adult("alice").await;
    let item = a.add("Night Songs", "The Lanterns").await;
    let track = a.media(item, "Lantern Light", "audio", b"audio");
    let log = a.media(item, "Rip log", "archive", b"the rip log");
    let only_archive = a.add("Log only", "").await;
    a.media(only_archive, "Lonely log", "archive", b"x");

    let r = a.api("GET", "/api/items", None, Some(&cookie)).await;
    let list = json(&r);
    assert_eq!(
        ids(&list["items"]),
        vec![item],
        "an item with only archive files isn't listed"
    );
    assert_eq!(list["items"][0]["fileCount"], 1);
    assert_eq!(list["items"][0]["kinds"], serde_json::json!(["audio"]));
    assert_eq!(list["total"], 1);

    let r = a
        .api("GET", &format!("/api/items/{item}"), None, Some(&cookie))
        .await;
    assert_eq!(ids(&json(&r)["media"]), vec![track]);
    for uri in [
        format!("/api/media/{log}"),
        format!("/api/media/{log}/stream"),
    ] {
        let r = a.api("GET", &uri, None, Some(&cookie)).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}");
    }
    let r = a
        .api(
            "GET",
            &format!("/api/items/{only_archive}"),
            None,
            Some(&cookie),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = a
        .api(
            "POST",
            "/api/media/check",
            Some(serde_json::json!({"ids": [track, log]})),
            Some(&cookie),
        )
        .await;
    assert_eq!(json(&r)["allowed"], serde_json::json!([track]));
    let r = a
        .api(
            "PUT",
            &format!("/api/favourites/{log}"),
            None,
            Some(&cookie),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = a
        .api(
            "POST",
            "/api/playlists",
            Some(serde_json::json!({"name": "Mine"})),
            Some(&cookie),
        )
        .await;
    let pid = json(&r)["id"].as_i64().unwrap();
    let r = a
        .api(
            "POST",
            &format!("/api/playlists/{pid}/entries"),
            Some(serde_json::json!({"mediaIds": [log]})),
            Some(&cookie),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = a
        .api("GET", "/api/items?kind=archive", None, Some(&cookie))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // The librarian sees them under "Archive files" and can download them.
    let page = a.get(&format!("/items/{item}"), true).await;
    assert!(page.body.contains("Archive files"));
    let link = format!("/admin/items/{item}/media/{log}/download");
    assert!(page.body.contains(&link));
    assert!(
        !a.get(&format!("/items/{item}"), false)
            .await
            .body
            .contains("Archive files")
    );
    let r = a.get(&link, true).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, "the rip log");
    assert!(
        r.headers[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );
    assert_eq!(a.get(&link, false).await.status, StatusCode::SEE_OTHER);
    let r = a
        .get(
            &format!("/admin/items/{only_archive}/media/{log}/download"),
            true,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "must belong to the item");
    let studio = a.get(&format!("/admin/items/{item}/media"), true).await;
    assert!(studio.body.contains(&link));
}

#[tokio::test]
async fn ripper_download_page() {
    let a = app();
    let page = a.get("/ripper", false).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Get Library Rip"));
    assert!(page.body.contains("No builds have been put here yet"));
    assert!(page.body.contains("com.makemkv.MakeMKV"));
    assert!(page.body.contains("restricted by law"));
    assert!(page.body.contains(crate::config::DEFAULT_SOURCE_URL));

    let dir = a.state.cfg.ripper_downloads_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("library-rip-0.1.0-x86_64.AppImage"), b"abc").unwrap();
    std::fs::write(dir.join("library-rip-0.1.0-windows-x64.zip"), b"windows").unwrap();
    std::fs::write(dir.join("library-rip-0.1.0-universal.dmg"), b"mac").unwrap();
    std::fs::write(dir.join("library-rip-0.1.0-x86_64.AppImage.sha256"), b"x").unwrap();
    let req = Request::get("/ripper")
        .header(
            header::USER_AGENT,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        )
        .body(Body::empty())
        .unwrap();
    let page = a.send(req).await;
    assert!(page.body.contains("For Windows"));
    assert!(page.body.contains("Other systems"));
    let windows = page.body.find("library-rip-0.1.0-windows-x64.zip").unwrap();
    let linux = page.body.find("library-rip-0.1.0-x86_64.AppImage").unwrap();
    assert!(windows < linux, "the visitor's system comes first");
    assert!(page.body.contains(&sha256_hex(b"abc")));
    assert!(page.body.contains("3 bytes"));
    assert!(
        !page.body.contains(".sha256<"),
        "checksum files aren't builds"
    );

    let r = a
        .get("/ripper/download/library-rip-0.1.0-x86_64.AppImage", false)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, "abc");
    assert!(
        r.headers[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );
    for bad in [
        "..%2Fsession.key",
        "missing.zip",
        ".hidden",
        "library-rip-0.1.0-x86_64.AppImage.sha256",
    ] {
        let r = a.get(&format!("/ripper/download/{bad}"), false).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{bad}");
    }

    // Linked from the desk and the studio.
    assert!(
        a.get("/admin", true)
            .await
            .body
            .contains(r#"href="/ripper""#)
    );
    let item = a.add("Night Songs", "").await;
    assert!(
        a.get(&format!("/admin/items/{item}/media"), true)
            .await
            .body
            .contains(r#"href="/ripper""#)
    );
}

#[tokio::test]
async fn site_password_leaves_the_rip_api_to_its_keys() {
    let a = app_with(|c| c.site_password = Some("family".into()));
    assert_eq!(a.get("/", false).await.status, StatusCode::UNAUTHORIZED);
    let r = a.rip("GET", "/api/rip/info", None, None).await;
    assert_eq!(r.status, StatusCode::OK);
    let r = a.rip("GET", "/api/rip/whoami", None, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.headers[header::WWW_AUTHENTICATE], "Bearer");
}

/// The dependency licences fit the AGPL (`deny.toml`). Skipped when
/// cargo-deny isn't installed.
#[test]
fn cargo_deny_licences_pass() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let installed = std::process::Command::new(&cargo)
        .args(["deny", "--version"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !installed {
        eprintln!("cargo-deny isn't installed; skipping the licence check");
        return;
    }
    let out = std::process::Command::new(&cargo)
        .args(["deny", "--offline", "check", "licenses"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cargo deny check licenses failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test]
async fn upload_text_fields_are_capped() {
    let a = app();
    let key = a.rip_key().await;
    let item = a.add("Night Songs", "The Lanterns").await;
    let long = "x".repeat(5000);
    let flac = b"fLaC pretend audio".to_vec();
    let good = sha256_hex(&flac);

    // The studio: a title far over 4 KB is refused, nothing is kept.
    let before = a.media_dir_files();
    let r = a
        .upload(
            item,
            &[("title", &long)],
            "song.mp3",
            "audio/mpeg",
            b"ID3 audio",
            true,
        )
        .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.body);
    assert!(json(&r)["error"].as_str().unwrap().contains("title"));
    assert_eq!(a.media_dir_files(), before);
    // Just under the cap is fine (and kept to 1000 characters).
    let r = a
        .upload(
            item,
            &[("title", &"y".repeat(4000))],
            "song.mp3",
            "audio/mpeg",
            b"ID3 audio",
            true,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);

    // The rip API, the same.
    let before = a.media_dir_files();
    let r = a
        .rip_upload(
            item,
            &[("kind", "audio"), ("sha256", &good), ("title", &long)],
            "01.flac",
            &flac,
            &key,
        )
        .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.body);
    assert_eq!(a.media_dir_files(), before);

    // Endless small fields are refused too.
    let many: Vec<(String, String)> = (0..40).map(|i| (format!("f{i}"), "v".into())).collect();
    let many: Vec<(&str, &str)> = many.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let r = a.rip_upload(item, &many, "01.flac", &flac, &key).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    assert_eq!(a.media_dir_files(), before);
}

#[tokio::test]
async fn a_dropped_upload_leaves_no_partial_file() {
    use tokio::io::AsyncWriteExt;
    let a = app();
    let item = a.add("Night Songs", "The Lanterns").await;
    let boundary = "DrOpPeDbOuNdArY";
    // The body arrives through a pipe the test writes to, so the request
    // can be dropped half way through the file.
    let (mut writer, reader) = tokio::io::duplex(64 * 1024);
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(reader));
    let req = Request::post(format!("/admin/items/{item}/media/upload"))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header(header::HOST, "localhost")
        .header(header::ORIGIN, "http://localhost")
        .header(header::ACCEPT, "application/json")
        .header(header::COOKIE, &a.admin_cookie)
        .body(body)
        .unwrap();
    let request = tokio::spawn(a.router.clone().oneshot(req));
    writer
        .write_all(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"song.mp3\"\r\nContent-Type: audio/mpeg\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    writer.write_all(&[7u8; 32 * 1024]).await.unwrap();
    writer.flush().await.unwrap();
    // Wait for the partial file to appear…
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !a.media_dir_files().iter().any(|f| f.ends_with(".part")) {
        assert!(
            std::time::Instant::now() < deadline,
            "no partial file appeared"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    // …then drop the request mid-write, as when the server shuts down.
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    drop(writer);
    assert!(
        a.media_dir_files().is_empty(),
        "left behind: {:?}",
        a.media_dir_files()
    );
}
