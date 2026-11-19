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

    /// Registers an account and returns its cookie (`library_account=…`).
    async fn register(&self, username: &str) -> String {
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
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
        let set = r.headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
        set.split(';').next().unwrap().to_owned()
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
