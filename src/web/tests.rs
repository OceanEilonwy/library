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
