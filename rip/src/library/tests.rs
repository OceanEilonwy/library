// SPDX-License-Identifier: AGPL-3.0-or-later
//! The library client and both connect flows, end to end against a local
//! mock library that implements the rip API and checks PKCE.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::task::JoinHandle;
use url::Url;

use super::api::ApiKey;
use super::client::{LibraryClient, UploadProgress};
use super::connect::{BrowserOpener, device, loopback};
use super::keystore::{FileStore, KeyStore, StoredConnection};
use super::{Disconnected, LibraryError, disconnect, http_client};
use crate::model::{CoverArt, MediaKind, NewItem, Sha256Digest, TrackNumber, UploadFile};

const KEY: &str = "lrk_testkey";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ConnectMode {
    #[default]
    Approve,
    Deny,
    WrongState,
}

/// One received media upload.
#[derive(Clone, Debug)]
struct Upload {
    fields: Vec<String>,
    values: HashMap<String, String>,
    bytes: usize,
}

#[derive(Default)]
struct Library {
    codes: Mutex<HashMap<String, (String, String)>>,
    connect_mode: Mutex<ConnectMode>,
    connect_query: Mutex<HashMap<String, String>>,
    device_polls: AtomicUsize,
    revoked: AtomicBool,
    media: Mutex<Vec<Value>>,
    uploads: Mutex<Vec<Upload>>,
    photos: Mutex<Vec<(String, usize)>>,
    created: Mutex<Vec<Value>>,
    min_version: Mutex<String>,
}

type Shared = Arc<Library>;

fn authorised(library: &Library, headers: &HeaderMap) -> bool {
    !library.revoked.load(Ordering::SeqCst)
        && headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            == Some(&format!("Bearer {KEY}"))
}

fn unauthorised() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "This key is not valid."})),
    )
        .into_response()
}

fn token_json() -> Value {
    json!({"apiKey": KEY, "keyId": 7, "keyName": "Henry's desktop",
        "library": {"name": "The Library", "url": "http://library.local/"}})
}

async fn info(State(library): State<Shared>, headers: HeaderMap) -> Json<Value> {
    let agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert_eq!(agent, format!("LibraryRip/{}", crate::VERSION));
    let min = library.min_version.lock().unwrap().clone();
    Json(json!({"name": "The Library", "version": "0.2.0",
        "minRipperVersion": if min.is_empty() { "0.1.0".to_owned() } else { min }}))
}

async fn connect_page(
    State(library): State<Shared>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    *library.connect_query.lock().unwrap() = query.clone();
    assert_eq!(query["client"], "library-rip");
    assert_eq!(query["code_challenge_method"], "S256");
    let redirect = &query["redirect_uri"];
    assert!(redirect.starts_with("http://127.0.0.1:"));
    let mut target = Url::parse(redirect).unwrap();
    match *library.connect_mode.lock().unwrap() {
        ConnectMode::Approve => {
            let code = format!("code-{}", library.codes.lock().unwrap().len());
            library.codes.lock().unwrap().insert(
                code.clone(),
                (query["code_challenge"].clone(), redirect.clone()),
            );
            target
                .query_pairs_mut()
                .append_pair("code", &code)
                .append_pair("state", &query["state"]);
        }
        ConnectMode::Deny => {
            target
                .query_pairs_mut()
                .append_pair("error", "access_denied")
                .append_pair("state", &query["state"]);
        }
        ConnectMode::WrongState => {
            target
                .query_pairs_mut()
                .append_pair("code", "x")
                .append_pair("state", "not-the-state");
        }
    }
    Redirect::to(target.as_str()).into_response()
}

async fn token(State(library): State<Shared>, Json(body): Json<Value>) -> Response {
    let bad =
        |message: &str| (StatusCode::BAD_REQUEST, Json(json!({"error": message}))).into_response();
    match body["grantType"].as_str() {
        Some("authorization_code") => {
            let code = body["code"].as_str().unwrap_or_default();
            // Single use: taken out of the map.
            let Some((challenge, redirect)) = library.codes.lock().unwrap().remove(code) else {
                return bad("That code is not valid.");
            };
            let verifier = body["codeVerifier"].as_str().unwrap_or_default();
            if !(43..=128).contains(&verifier.len()) {
                return bad("The verifier is the wrong length.");
            }
            if URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) != challenge {
                return bad("The verifier does not match.");
            }
            if body["redirectUri"].as_str() != Some(redirect.as_str()) {
                return bad("The redirect address does not match.");
            }
            Json(token_json()).into_response()
        }
        Some("device_code") => {
            assert_eq!(body["deviceCode"], "device-secret");
            if library.device_polls.fetch_add(1, Ordering::SeqCst) < 1 {
                (
                    StatusCode::PRECONDITION_REQUIRED,
                    Json(json!({"error": "authorization_pending"})),
                )
                    .into_response()
            } else {
                Json(token_json()).into_response()
            }
        }
        _ => bad("Unknown grant type."),
    }
}

async fn revoke(State(library): State<Shared>, headers: HeaderMap) -> Response {
    if !authorised(&library, &headers) {
        return unauthorised();
    }
    library.revoked.store(true, Ordering::SeqCst);
    StatusCode::NO_CONTENT.into_response()
}

async fn device_code(Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(body["name"], "Henry's desktop");
    Json(
        json!({"deviceCode": "device-secret", "userCode": "WXYZ-1234",
        "verificationUri": "http://library.local/connect/device",
        "interval": 1, "expiresIn": 600}),
    )
}

async fn whoami(State(library): State<Shared>, headers: HeaderMap) -> Response {
    if !authorised(&library, &headers) {
        return unauthorised();
    }
    Json(json!({"keyId": 7, "keyName": "Henry's desktop",
        "library": {"name": "The Library", "url": "http://library.local/"}}))
    .into_response()
}

async fn types(State(library): State<Shared>, headers: HeaderMap) -> Response {
    if !authorised(&library, &headers) {
        return unauthorised();
    }
    Json(
        json!([{"id": 1, "name": "Music", "slug": "music", "lookup": "musicbrainz"},
        {"id": 2, "name": "Book", "slug": "book", "lookup": null}]),
    )
    .into_response()
}

async fn search_items(
    State(library): State<Shared>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if !authorised(&library, &headers) {
        return unauthorised();
    }
    assert_eq!(query["q"], "tones & more");
    assert_eq!(query["type"], "music");
    Json(
        json!([{"id": 5, "title": "Collected Tones", "creator": "Ann Sine",
        "year": 1999, "identifier": null, "fileCount": 2, "url": "http://library.local/items/5"}]),
    )
    .into_response()
}

async fn create_item(
    State(library): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorised(&library, &headers) {
        return unauthorised();
    }
    if body["title"] == "" {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": "Please check the form.", "fields": {"title": "A title is needed"}})),
        )
            .into_response();
    }
    library.created.lock().unwrap().push(body);
    (
        StatusCode::CREATED,
        Json(json!({"id": 5, "url": "http://library.local/items/5"})),
    )
        .into_response()
}

async fn photos(
    State(library): State<Shared>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    mut form: Multipart,
) -> Response {
    if !authorised(&library, &headers) {
        return unauthorised();
    }
    assert_eq!(id, 5);
    while let Some(field) = form.next_field().await.unwrap() {
        assert_eq!(field.name(), Some("file"));
        let mime = field.content_type().unwrap_or_default().to_owned();
        let bytes = field.bytes().await.unwrap();
        library.photos.lock().unwrap().push((mime, bytes.len()));
    }
    StatusCode::CREATED.into_response()
}

async fn list_media(
    State(library): State<Shared>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Response {
    if !authorised(&library, &headers) {
        return unauthorised();
    }
    assert_eq!(id, 5);
    Json(Value::Array(library.media.lock().unwrap().clone())).into_response()
}

async fn upload_media(
    State(library): State<Shared>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    mut form: Multipart,
) -> Response {
    if !authorised(&library, &headers) {
        return unauthorised();
    }
    assert_eq!(id, 5);
    let mut fields = Vec::new();
    let mut values = HashMap::new();
    let mut digest = None;
    let mut size = 0;
    loop {
        let field = match form.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            // The client abandoned the upload part-way.
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        };
        let name = field.name().unwrap_or_default().to_owned();
        fields.push(name.clone());
        if name == "file" {
            let Ok(bytes) = field.bytes().await else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            size = bytes.len();
            digest = Some(Sha256Digest(Sha256::digest(&bytes).into()));
        } else {
            values.insert(name, field.text().await.unwrap());
        }
    }
    assert_eq!(fields.last().map(String::as_str), Some("file"));
    if values["title"] == "too big" {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({"error": "That file is over the 4 GB limit."})),
        )
            .into_response();
    }
    let declared = Sha256Digest::from_hex(&values["sha256"]).unwrap();
    if digest != Some(declared) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": "The file does not match its checksum."})),
        )
            .into_response();
    }
    let new_id = 100 + library.uploads.lock().unwrap().len();
    library.media.lock().unwrap().push(json!({
        "id": new_id, "kind": values["kind"], "title": values["title"],
        "disc": values.get("disc").map(|d| d.parse::<i64>().unwrap()),
        "track": values.get("track").map(|t| t.parse::<i64>().unwrap()),
        "sha256": values["sha256"]}));
    library.uploads.lock().unwrap().push(Upload {
        fields,
        values,
        bytes: size,
    });
    (StatusCode::CREATED, Json(json!({"id": new_id}))).into_response()
}

async fn not_a_library() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html")],
        "<html><body>Welcome to my blog</body></html>",
    )
        .into_response()
}

/// Starts a mock library; returns its base URL and state.
async fn mock_library() -> (Url, Shared) {
    let library = Shared::default();
    let router = Router::new()
        .route("/connect", get(connect_page))
        .route("/api/rip/info", get(info))
        .route("/api/rip/token", post(token).delete(revoke))
        .route("/api/rip/device", post(device_code))
        .route("/api/rip/whoami", get(whoami))
        .route("/api/rip/types", get(types))
        .route("/api/rip/items", get(search_items).post(create_item))
        .route("/api/rip/items/{id}/photos", post(photos))
        .route(
            "/api/rip/items/{id}/media",
            get(list_media).post(upload_media),
        )
        .route("/blog/api/rip/info", get(not_a_library))
        .with_state(Arc::clone(&library));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (Url::parse(&format!("http://{address}")).unwrap(), library)
}

fn client(base: &Url) -> LibraryClient {
    LibraryClient::new(
        http_client().unwrap(),
        base.clone(),
        Some(ApiKey::new(KEY).unwrap()),
    )
}

/// A browser that follows the redirects itself and keeps the final page.
#[derive(Default)]
struct FakeBrowser {
    page: Mutex<Option<JoinHandle<String>>>,
    opened: Mutex<Option<Url>>,
}

impl FakeBrowser {
    async fn page(&self) -> String {
        let handle = self.page.lock().unwrap().take().unwrap();
        handle.await.unwrap()
    }
}

impl BrowserOpener for FakeBrowser {
    fn open(&self, url: &Url) -> std::io::Result<()> {
        *self.opened.lock().unwrap() = Some(url.clone());
        let url = url.clone();
        *self.page.lock().unwrap() = Some(tokio::spawn(async move {
            reqwest::get(url).await.unwrap().text().await.unwrap()
        }));
        Ok(())
    }
}

/// A browser that cannot be opened.
struct NoBrowser;

impl BrowserOpener for NoBrowser {
    fn open(&self, _: &Url) -> std::io::Result<()> {
        Err(std::io::Error::other("no browser installed"))
    }
}

/// A browser the user never looks at.
struct IgnoredBrowser;

impl BrowserOpener for IgnoredBrowser {
    fn open(&self, _: &Url) -> std::io::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Connect flows
// ---------------------------------------------------------------------------

#[tokio::test]
async fn loopback_connect_end_to_end() {
    let (base, library) = mock_library().await;
    let browser = FakeBrowser::default();
    let http = http_client().unwrap();
    let token = loopback::connect(
        &http,
        &base,
        "Henry's desktop",
        &browser,
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(token.api_key.expose(), KEY);
    assert_eq!(token.key_name, "Henry's desktop");
    assert_eq!(token.library.name, "The Library");

    let page = browser.page().await;
    assert!(page.contains("You can close this tab"), "{page}");
    let query = library.connect_query.lock().unwrap().clone();
    assert_eq!(query["name"], "Henry's desktop");
    assert_eq!(query["state"].len(), 22);
    assert_eq!(query["code_challenge"].len(), 43);
    // The code was used up.
    assert!(library.codes.lock().unwrap().is_empty());

    // The key works, and can be kept and revoked.
    let connected = client(&base).with_key(token.api_key.clone());
    assert_eq!(connected.whoami().await.unwrap().key_id, 7);
    let dir = tempfile::tempdir().unwrap();
    let store = FileStore::new(dir.path().join("connection-key.json"));
    store
        .save(&StoredConnection::from_token(base.clone(), token))
        .unwrap();
    assert_eq!(store.load().unwrap().map(|saved| saved.key_id), Some(7));
    assert_eq!(
        disconnect(&connected, &store).await.unwrap(),
        Disconnected::Revoked
    );
    assert_eq!(store.load().unwrap(), None);
    assert!(matches!(
        connected.whoami().await,
        Err(LibraryError::Unauthorised)
    ));
    assert_eq!(
        disconnect(&connected, &store).await.unwrap(),
        Disconnected::AlreadyRevoked
    );
}

#[tokio::test]
async fn loopback_session_exposes_the_connect_address() {
    let (base, _) = mock_library().await;
    let base = base.join("/").unwrap();
    let session = loopback::LoopbackSession::start(&base, "Desk")
        .await
        .unwrap();
    let url = session.connect_url().clone();
    assert_eq!(url.path(), "/connect");
    let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!(query["redirect_uri"], session.redirect_uri());
    assert_eq!(query["client"], "library-rip");
    // Opened by hand, it completes the same way.
    let browser = FakeBrowser::default();
    browser.open(&url).unwrap();
    let token = session
        .finish(&http_client().unwrap(), Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(token.key_id, 7);
}

#[tokio::test]
async fn loopback_refused_by_the_librarian() {
    let (base, library) = mock_library().await;
    *library.connect_mode.lock().unwrap() = ConnectMode::Deny;
    let browser = FakeBrowser::default();
    let result = loopback::connect(
        &http_client().unwrap(),
        &base,
        "Desk",
        &browser,
        Duration::from_secs(10),
    )
    .await;
    assert!(matches!(result, Err(LibraryError::Denied)), "{result:?}");
    assert!(browser.page().await.contains("not allowed"));
}

#[tokio::test]
async fn loopback_rejects_a_wrong_state() {
    let (base, library) = mock_library().await;
    *library.connect_mode.lock().unwrap() = ConnectMode::WrongState;
    let browser = FakeBrowser::default();
    let result = loopback::connect(
        &http_client().unwrap(),
        &base,
        "Desk",
        &browser,
        Duration::from_secs(10),
    )
    .await;
    assert!(
        matches!(result, Err(LibraryError::StateMismatch)),
        "{result:?}"
    );
}

#[tokio::test]
async fn loopback_times_out_and_reports_a_missing_browser() {
    let (base, _) = mock_library().await;
    let http = http_client().unwrap();
    let result = loopback::connect(
        &http,
        &base,
        "Desk",
        &IgnoredBrowser,
        Duration::from_millis(100),
    )
    .await;
    assert!(matches!(result, Err(LibraryError::TimedOut)), "{result:?}");

    let result = loopback::connect(&http, &base, "Desk", &NoBrowser, Duration::from_secs(1)).await;
    match result {
        Err(LibraryError::Browser { url, .. }) => assert_eq!(url.path(), "/connect"),
        other => panic!("expected a browser error, got {other:?}"),
    }
}

#[tokio::test]
async fn device_code_connect_end_to_end() {
    let (base, library) = mock_library().await;
    let http = http_client().unwrap();
    let code = device::request_code(&http, &base, "Henry's desktop")
        .await
        .unwrap();
    assert_eq!(code.user_code, "WXYZ-1234");
    let token = device::poll_for_key(&http, &base, &code).await.unwrap();
    assert_eq!(token.api_key.expose(), KEY);
    assert_eq!(library.device_polls.load(Ordering::SeqCst), 2);
}

// ---------------------------------------------------------------------------
// Rip API
// ---------------------------------------------------------------------------

#[tokio::test]
async fn info_checks_the_library_and_version() {
    let (base, library) = mock_library().await;
    let anonymous = LibraryClient::new(http_client().unwrap(), base.clone(), None);
    assert_eq!(anonymous.info().await.unwrap().name, "The Library");

    *library.min_version.lock().unwrap() = "99.0.0".into();
    assert!(matches!(
        anonymous.info().await,
        Err(LibraryError::TooOld { .. })
    ));

    let blog = LibraryClient::new(http_client().unwrap(), base.join("/blog").unwrap(), None);
    assert!(matches!(
        blog.info().await,
        Err(LibraryError::NotALibrary(_))
    ));
    let missing = LibraryClient::new(
        http_client().unwrap(),
        base.join("/nothing/").unwrap(),
        None,
    );
    assert!(matches!(
        missing.info().await,
        Err(LibraryError::NotALibrary(_))
    ));
    assert!(matches!(
        anonymous.whoami().await,
        Err(LibraryError::NotConnected)
    ));
}

#[tokio::test]
async fn catalogue_calls() {
    let (base, library) = mock_library().await;
    let client = client(&base);
    let types = client.types().await.unwrap();
    assert_eq!(types[0].slug, "music");
    assert_eq!(types[1].lookup, None);

    let items = client.search_items("tones & more", "music").await.unwrap();
    assert_eq!(items[0].file_count, 2);

    let item = NewItem {
        type_slug: "music".into(),
        title: "Collected Tones".into(),
        creator: "Ann Sine".into(),
        year: Some(1999),
        ..NewItem::default()
    };
    let created = client.create_item(&item).await.unwrap();
    assert_eq!(created.id, 5);
    let sent = library.created.lock().unwrap()[0].clone();
    assert_eq!(sent["typeSlug"], "music");
    assert_eq!(sent["year"], 1999);

    let refused = client
        .create_item(&NewItem {
            title: String::new(),
            ..item
        })
        .await;
    match refused {
        Err(LibraryError::Rejected(message)) => {
            assert_eq!(message, "Please check the form. (title: A title is needed)");
        }
        other => panic!("expected a rejection, got {other:?}"),
    }

    let cover = CoverArt {
        bytes: vec![0xFF, 0xD8, 0xFF, 0xD9].into(),
        mime: "image/jpeg".into(),
    };
    client.upload_photo(5, &cover).await.unwrap();
    assert_eq!(
        library.photos.lock().unwrap().as_slice(),
        [("image/jpeg".to_owned(), 4)]
    );

    let wrong = LibraryClient::new(
        http_client().unwrap(),
        base,
        Some(ApiKey::new("lrk_wrong").unwrap()),
    );
    assert!(matches!(
        wrong.types().await,
        Err(LibraryError::Unauthorised)
    ));
}

fn write_file(dir: &std::path::Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn upload(path: PathBuf, kind: MediaKind, title: &str, track: Option<u8>) -> UploadFile {
    let bytes = std::fs::read(&path).unwrap();
    UploadFile {
        kind,
        title: title.into(),
        disc: 1,
        track: track.map(|number| TrackNumber::new(number).unwrap()),
        duration_ms: track.map(|_| 1000),
        sha256: Sha256Digest(Sha256::digest(&bytes).into()),
        path,
    }
}

#[tokio::test]
async fn uploads_resume_by_checksum() {
    let (base, library) = mock_library().await;
    let client = client(&base);
    let dir = tempfile::tempdir().unwrap();
    let big: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
    let files = vec![
        upload(
            write_file(dir.path(), "01.flac", b"first track"),
            MediaKind::Audio,
            "One",
            Some(1),
        ),
        upload(
            write_file(dir.path(), "02.flac", &big),
            MediaKind::Audio,
            "Two",
            Some(2),
        ),
        upload(
            write_file(dir.path(), "rip.log", b"log"),
            MediaKind::Archive,
            "Rip log",
            None,
        ),
    ];
    // Track 1 is already there from an interrupted upload.
    library
        .media
        .lock()
        .unwrap()
        .push(json!({"id": 1, "kind": "audio", "title": "One",
        "disc": 1, "track": 1, "sha256": files[0].sha256.to_hex()}));
    // A file without a checksum does not confuse it.
    library
        .media
        .lock()
        .unwrap()
        .push(json!({"id": 2, "kind": "document", "title": "Booklet",
        "disc": null, "track": null, "sha256": null}));

    let seen: Mutex<Vec<(String, UploadProgress)>> = Mutex::default();
    let summary = client
        .upload_files(5, &files, &|file, progress| {
            seen.lock().unwrap().push((file.title.clone(), progress));
        })
        .await
        .unwrap();
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.uploaded.len(), 2);

    let uploads = library.uploads.lock().unwrap().clone();
    assert_eq!(uploads.len(), 2);
    assert_eq!(
        uploads[0].fields,
        [
            "kind",
            "title",
            "disc",
            "track",
            "duration_ms",
            "sha256",
            "file"
        ]
    );
    assert_eq!(uploads[0].values["kind"], "audio");
    assert_eq!(uploads[0].values["track"], "2");
    assert_eq!(uploads[0].bytes, big.len());
    assert_eq!(
        uploads[1].fields,
        ["kind", "title", "disc", "sha256", "file"]
    );
    assert_eq!(uploads[1].values["kind"], "archive");

    let seen = seen.into_inner().unwrap();
    let two: Vec<UploadProgress> = seen
        .iter()
        .filter(|(title, _)| title == "Two")
        .map(|(_, progress)| *progress)
        .collect();
    assert_eq!(two.first().map(|p| p.sent), Some(0));
    assert_eq!(two.last().map(|p| p.sent), Some(big.len() as u64));
    assert!(two.len() > 2, "progress should arrive in steps: {two:?}");
    assert!(two.windows(2).all(|pair| pair[0].sent <= pair[1].sent));
    assert!(
        seen.iter()
            .any(|(title, p)| title == "One" && p.sent == p.total)
    );

    // Running it again uploads nothing.
    let again = client.upload_files(5, &files, &|_, _| {}).await.unwrap();
    assert_eq!((again.uploaded.len(), again.skipped), (0, 3));
}

#[tokio::test]
async fn upload_errors_are_explained() {
    let (base, _) = mock_library().await;
    let client = client(&base);
    let dir = tempfile::tempdir().unwrap();

    let mut changed = upload(
        write_file(dir.path(), "01.flac", b"original"),
        MediaKind::Audio,
        "One",
        Some(1),
    );
    std::fs::write(&changed.path, b"edited!!").unwrap();
    let result = client.upload_media(5, &changed, &|_| {}).await;
    assert!(
        matches!(result, Err(LibraryError::FileChanged(_))),
        "{result:?}"
    );

    changed.title = "too big".into();
    changed.sha256 = Sha256Digest(Sha256::digest(b"edited!!").into());
    match client.upload_media(5, &changed, &|_| {}).await {
        Err(LibraryError::TooLarge(message)) => {
            assert_eq!(message, "That file is over the 4 GB limit.");
        }
        other => panic!("expected too large, got {other:?}"),
    }

    let missing = upload(
        write_file(dir.path(), "gone.flac", b"x"),
        MediaKind::Audio,
        "Gone",
        Some(2),
    );
    std::fs::remove_file(&missing.path).unwrap();
    assert!(matches!(
        client.upload_media(5, &missing, &|_| {}).await,
        Err(LibraryError::Io { .. })
    ));
}

#[tokio::test]
async fn disconnect_forgets_the_key_when_the_library_is_unreachable() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let base = Url::parse(&format!("http://{address}/")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = FileStore::new(dir.path().join("connection-key.json"));
    store
        .save(&StoredConnection {
            library_url: base.clone(),
            library_name: "The Library".into(),
            key_id: 7,
            key_name: "Desk".into(),
            api_key: ApiKey::new(KEY).unwrap(),
        })
        .unwrap();
    assert_eq!(
        disconnect(&client(&base), &store).await.unwrap(),
        Disconnected::ForgottenOnly
    );
    assert_eq!(store.load().unwrap(), None);
}
