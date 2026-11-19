// SPDX-License-Identifier: AGPL-3.0-or-later
//! The headless runner end to end: a simulated disc, a mock library (rip
//! API, device code and browser handshakes) and mock MusicBrainz,
//! AccurateRip and CTDB that know nothing, all on a local port.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use axum::extract::{Multipart, Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use library_rip::app::headless::{
    HeadlessError, HeadlessOptions, PrintOnlyBrowser, Reporter, run_with_browser,
};
use library_rip::app::pipeline::Endpoints;
use library_rip::fakedisc::{InjectedError, InjectedErrorKind};
use library_rip::library::connect::BrowserOpener;
use library_rip::model::{Lba, ReadMode, SampleOffset};
use library_rip::testsupport::{FakeDiscOptions, make_fake_disc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;

const KEY: &str = "lrk_test-key_0123456789abcdefghijklmnopqrstuvwxyz";

#[derive(Debug, Default)]
struct Recorded {
    token_requests: Vec<Value>,
    items: Vec<Value>,
    media: Vec<Media>,
    unauthorised: usize,
}

#[derive(Clone, Debug)]
struct Media {
    kind: String,
    title: String,
    track: Option<String>,
    sha256_matches: bool,
    size: usize,
}

#[derive(Clone)]
struct Mock {
    base: Url,
    recorded: Arc<Mutex<Recorded>>,
}

fn authorised(mock: &Mock, headers: &HeaderMap) -> bool {
    let ok = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        == Some(&format!("Bearer {KEY}"));
    if !ok {
        mock.recorded.lock().unwrap().unauthorised += 1;
    }
    ok
}

fn library() -> Value {
    json!({"name": "The Library", "url": "http://library.test/"})
}

async fn info() -> Json<Value> {
    Json(json!({"name": "The Library", "version": "0.2.0", "minRipperVersion": "0.1.0"}))
}

async fn device(State(mock): State<Mock>) -> Json<Value> {
    Json(json!({
        "deviceCode": "device-secret",
        "userCode": "WXYZ-1234",
        "verificationUri": format!("{}connect/device", mock.base),
        "interval": 1,
        "expiresIn": 600,
    }))
}

async fn token(State(mock): State<Mock>, Json(body): Json<Value>) -> Response {
    mock.recorded
        .lock()
        .unwrap()
        .token_requests
        .push(body.clone());
    let valid = match body["grantType"].as_str() {
        Some("device_code") => body["deviceCode"] == "device-secret",
        Some("authorization_code") => {
            body["code"] == "one-time-code" && body["codeVerifier"].as_str().is_some()
        }
        _ => false,
    };
    if !valid {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_grant"})),
        )
            .into_response();
    }
    Json(json!({"apiKey": KEY, "keyId": 7, "keyName": "Test desktop", "library": library()}))
        .into_response()
}

async fn whoami(State(mock): State<Mock>, headers: HeaderMap) -> Response {
    if !authorised(&mock, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({"keyId": 7, "keyName": "Test desktop", "library": library()})).into_response()
}

async fn create_item(
    State(mock): State<Mock>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorised(&mock, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut recorded = mock.recorded.lock().unwrap();
    recorded.items.push(body);
    let id = recorded.items.len();
    (
        StatusCode::CREATED,
        Json(json!({"id": id, "url": format!("{}items/{id}", mock.base)})),
    )
        .into_response()
}

async fn list_media(
    State(mock): State<Mock>,
    headers: HeaderMap,
    UrlPath(_id): UrlPath<i64>,
) -> Response {
    if !authorised(&mock, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!([])).into_response()
}

async fn upload_media(
    State(mock): State<Mock>,
    headers: HeaderMap,
    UrlPath(_id): UrlPath<i64>,
    mut multipart: Multipart,
) -> Response {
    if !authorised(&mock, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut fields = std::collections::BTreeMap::new();
    let mut file = Vec::new();
    while let Some(field) = multipart.next_field().await.unwrap() {
        let name = field.name().unwrap_or_default().to_owned();
        if name == "file" {
            file = field.bytes().await.unwrap().to_vec();
        } else {
            fields.insert(name, field.text().await.unwrap());
        }
    }
    let digest: String = Sha256::digest(&file)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut recorded = mock.recorded.lock().unwrap();
    recorded.media.push(Media {
        kind: fields.get("kind").cloned().unwrap_or_default(),
        title: fields.get("title").cloned().unwrap_or_default(),
        track: fields.get("track").cloned(),
        sha256_matches: fields.get("sha256") == Some(&digest),
        size: file.len(),
    });
    let id = recorded.media.len();
    (StatusCode::CREATED, Json(json!({"id": id}))).into_response()
}

async fn photos(State(mock): State<Mock>, headers: HeaderMap) -> Response {
    if !authorised(&mock, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    StatusCode::CREATED.into_response()
}

/// Starts the mock services and returns them with matching endpoints.
async fn start() -> (Mock, Endpoints) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let mock = Mock {
        base: base.clone(),
        recorded: Arc::default(),
    };
    let app = Router::new()
        .route("/api/rip/info", get(info))
        .route("/api/rip/device", post(device))
        .route("/api/rip/token", post(token))
        .route("/api/rip/whoami", get(whoami))
        .route("/api/rip/items", post(create_item))
        .route("/api/rip/items/{id}/photos", post(photos))
        .route(
            "/api/rip/items/{id}/media",
            get(list_media).post(upload_media),
        )
        // MusicBrainz, AccurateRip, the drive list and CTDB know nothing.
        .fallback(|| async { StatusCode::NOT_FOUND })
        .with_state(mock.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let endpoints = Endpoints {
        musicbrainz: base.join("ws/2/").unwrap(),
        cover_art: base.join("cover/").unwrap(),
        accuraterip: base.join("accuraterip/").unwrap(),
        drive_offsets: base.join("accuraterip/DriveOffsets.bin").unwrap(),
        ctdb: base.join("ctdb/").unwrap(),
    };
    (mock, endpoints)
}

fn options(endpoints: Endpoints, library: &Url, disc: &Path, work: &Path) -> HeadlessOptions {
    HeadlessOptions {
        library: library.clone(),
        fake_disc: disc.to_owned(),
        device_code: true,
        offset: Some(SampleOffset(6)),
        read_mode: ReadMode::Secure,
        keep: None,
        upload_with_errors: false,
        key_file: None,
        endpoints,
        work_root: work.join("work"),
    }
}

fn collect() -> (Reporter, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    let reporter: Reporter = Arc::new(move |line: &str| sink.lock().unwrap().push(line.to_owned()));
    (reporter, lines)
}

fn disc(dir: &Path, errors: Vec<InjectedError>) {
    make_fake_disc(
        dir,
        &FakeDiscOptions {
            tracks: 3,
            seconds: 2,
            read_offset: SampleOffset(6),
            errors,
            enhanced: false,
        },
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn device_code_rip_uploads_everything_with_checksums() {
    let (mock, endpoints) = start().await;
    let temp = tempfile::tempdir().unwrap();
    let disc_dir = temp.path().join("disc");
    disc(&disc_dir, Vec::new());
    let keep = temp.path().join("keep");
    let mut options = options(endpoints, &mock.base, &disc_dir, temp.path());
    options.keep = Some(keep.clone());
    let (reporter, lines) = collect();

    let outcome = run_with_browser(
        &options,
        Arc::new(PrintOnlyBrowser),
        reporter,
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .expect("the headless rip should succeed");

    let lines = lines.lock().unwrap().join("\n");
    assert!(lines.contains("WXYZ-1234"), "the code is printed:\n{lines}");
    assert!(lines.contains("Connected to The Library as Test desktop."));
    assert!(lines.contains("Track 1: read without errors."), "{lines}");
    assert!(lines.contains("Read offset: +6 samples (set in Settings)."));

    assert_eq!(outcome.uploaded, 6);
    assert_eq!(outcome.skipped, 0);
    assert!(outcome.item_url.ends_with("/items/1"));
    assert!(outcome.read_issues.is_empty());

    let recorded = mock.recorded.lock().unwrap();
    assert_eq!(recorded.unauthorised, 0);
    assert_eq!(recorded.items.len(), 1);
    let item = &recorded.items[0];
    assert_eq!(item["typeSlug"], "music");
    assert_eq!(item["title"], "Unknown album");
    assert_eq!(item["creator"], "Unknown artist");

    let kinds: Vec<(&str, Option<&str>)> = recorded
        .media
        .iter()
        .map(|m| (m.kind.as_str(), m.track.as_deref()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("audio", Some("1")),
            ("audio", Some("2")),
            ("audio", Some("3")),
            ("archive", None),
            ("archive", None),
            ("archive", None),
        ]
    );
    assert!(
        recorded
            .media
            .iter()
            .all(|m| m.sha256_matches && m.size > 0)
    );
    let titles: Vec<&str> = recorded.media.iter().map(|m| m.title.as_str()).collect();
    assert_eq!(&titles[..3], &["Track 1", "Track 2", "Track 3"]);
    assert!(titles.contains(&"Rip log"));
    assert!(titles.contains(&"Cue sheet"));

    // The local copy holds the same files.
    let kept = outcome.kept.expect("a local copy");
    assert!(kept.starts_with(&keep));
    let mut names: Vec<String> = std::fs::read_dir(&kept)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert!(names.contains(&"01 - Track 1.flac".to_owned()), "{names:?}");
    assert!(names.contains(&"rip.json".to_owned()));
    let report = std::fs::read_to_string(kept.join("rip.json")).unwrap();
    let report = library_rip::core::report_json::parse(&report).unwrap();
    assert!(report.read_without_errors());
    assert_eq!(report.read_offset, SampleOffset(6));

    // The work folder is cleaned up after the upload.
    let work = temp.path().join("work");
    let leftovers = std::fs::read_dir(&work).map(|d| d.count()).unwrap_or(0);
    assert_eq!(leftovers, 0);
}

/// Plays the librarian pressing "Yes, connect": sends the browser's
/// redirect straight to the ripper's loopback listener.
struct ApprovingBrowser;

impl BrowserOpener for ApprovingBrowser {
    fn open(&self, url: &Url) -> std::io::Result<()> {
        let query: std::collections::HashMap<String, String> =
            url.query_pairs().into_owned().collect();
        let redirect = Url::parse(&query["redirect_uri"]).unwrap();
        let state = query["state"].clone();
        std::thread::spawn(move || {
            let address = format!(
                "{}:{}",
                redirect.host_str().unwrap(),
                redirect.port().unwrap()
            );
            let mut stream = TcpStream::connect(address).unwrap();
            write!(
                stream,
                "GET {}?code=one-time-code&state={state} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                redirect.path()
            )
            .unwrap();
            let mut answer = String::new();
            let _ = stream.read_to_string(&mut answer);
        });
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_connect_is_saved_and_reused() {
    let (mock, endpoints) = start().await;
    let temp = tempfile::tempdir().unwrap();
    let disc_dir = temp.path().join("disc");
    disc(&disc_dir, Vec::new());
    let mut options = options(endpoints, &mock.base, &disc_dir, temp.path());
    options.device_code = false;
    options.key_file = Some(temp.path().join("key.json"));

    for _ in 0..2 {
        let (reporter, _) = collect();
        run_with_browser(
            &options,
            Arc::new(ApprovingBrowser),
            reporter,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("the headless rip should succeed");
    }

    let recorded = mock.recorded.lock().unwrap();
    // Approved once; the second run used the saved key.
    assert_eq!(recorded.token_requests.len(), 1);
    assert_eq!(
        recorded.token_requests[0]["grantType"],
        "authorization_code"
    );
    assert_eq!(recorded.items.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn read_errors_stop_the_upload() {
    let (mock, endpoints) = start().await;
    let temp = tempfile::tempdir().unwrap();
    let disc_dir = temp.path().join("disc");
    disc(
        &disc_dir,
        vec![InjectedError {
            lba: Lba(200),
            kind: InjectedErrorKind::Unreadable,
        }],
    );
    let options = options(endpoints, &mock.base, &disc_dir, temp.path());
    let (reporter, _) = collect();

    let error = run_with_browser(
        &options,
        Arc::new(PrintOnlyBrowser),
        reporter,
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .expect_err("a rip with read errors is not uploaded");

    assert_eq!(error.exit_code(), 5);
    let HeadlessError::ReadErrors { issues, folder } = &error else {
        panic!("expected read errors, got {error:?}");
    };
    // Two-second tracks are 150 sectors, so sector 200 is in track 2.
    assert_eq!(
        issues,
        &vec!["Track 2: 1 sector could not be read reliably.".to_owned()]
    );
    assert!(folder.exists(), "the rip is left for inspection");
    let recorded = mock.recorded.lock().unwrap();
    assert!(recorded.items.is_empty());
    assert!(recorded.media.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_library_is_a_connect_error() {
    let temp = tempfile::tempdir().unwrap();
    let disc_dir = temp.path().join("disc");
    disc(&disc_dir, Vec::new());
    // Nothing listens on port 9 of localhost.
    let library = Url::parse("http://127.0.0.1:9/").unwrap();
    let (_, endpoints) = start().await;
    let options = options(endpoints, &library, &disc_dir, temp.path());
    let (reporter, _) = collect();
    let error = run_with_browser(
        &options,
        Arc::new(PrintOnlyBrowser),
        reporter,
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .expect_err("nothing to connect to");
    assert_eq!(error.exit_code(), 3);
    assert!(error.problem().next_step.contains("address"));
}
