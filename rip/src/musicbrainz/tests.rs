// SPDX-License-Identifier: AGPL-3.0-or-later
//! MusicBrainz client tests against a local mock web service.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use serde_json::{Value, json};

use super::*;
use crate::model::{Lba, Sectors, TrackEntry, TrackNumber};

const RELEASE: &str = "11111111-2222-3333-4444-555555555555";
const OTHER_RELEASE: &str = "99999999-2222-3333-4444-555555555555";
const DISC_ONE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaa-";
const DISC_TWO: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbb-";
const UNKNOWN_DISC: &str = "ccccccccccccccccccccccccccc-";

fn toc(tracks: u8) -> Toc {
    let entries = (1..=tracks)
        .map(|number| TrackEntry {
            number: TrackNumber::new(number).unwrap(),
            start_lba: Lba(u32::from(number - 1) * 1000),
            length_sectors: Sectors(1000),
            is_audio: true,
            pregap: Sectors(0),
            isrc: None,
            session: crate::model::SessionNumber::FIRST,
        })
        .collect();
    Toc::new(entries, Lba(u32::from(tracks) * 1000)).unwrap()
}

fn credit(name: &str, id: &str, sort: &str, join: &str) -> Value {
    json!({"name": name, "joinphrase": join,
        "artist": {"id": id, "name": name, "sort-name": sort}})
}

fn track(position: u32, title: &str, id_digit: char) -> Value {
    let track_id = format!(
        "{}-0000-0000-0000-000000000000",
        id_digit.to_string().repeat(8)
    );
    let recording_id = format!(
        "{}-1111-0000-0000-000000000000",
        id_digit.to_string().repeat(8)
    );
    json!({"id": track_id, "position": position, "number": position.to_string(),
        "title": title, "length": 180000,
        "artist-credit": [credit("Guest", "cccccccc-0000-0000-0000-000000000000", "Guest", "")],
        "recording": {"id": recording_id, "title": title, "isrcs": ["GBAYE0000351"]}})
}

/// A two-disc release: disc 1 has two tracks, disc 2 has three.
fn two_disc_release() -> Value {
    json!({
        "id": RELEASE,
        "title": "Collected Tones",
        "date": "1999-04-01",
        "country": "GB",
        "barcode": "0123456789012",
        "artist-credit": [
            credit("Ann Sine", "aaaaaaaa-0000-0000-0000-000000000000", "Sine, Ann", " & "),
            credit("Bob Square", "bbbbbbbb-0000-0000-0000-000000000000", "Square, Bob", "")
        ],
        "cover-art-archive": {"front": true, "count": 1},
        "label-info": [{"catalog-number": "TONE 1", "label": {"id": "x", "name": "Test Tones"}}],
        "release-group": {"id": "dddddddd-0000-0000-0000-000000000000"},
        "media": [
            {"position": 1, "format": "CD", "track-count": 2,
             "discs": [{"id": DISC_ONE}],
             "tracks": [track(1, "One", '1'), track(2, "Two", '2')]},
            {"position": 2, "format": "CD", "track-count": 3,
             "discs": [{"id": DISC_TWO}],
             "tracks": [track(1, "Three", '3'), track(2, "Four", '4'), track(3, "Five", '5')]}
        ]
    })
}

#[derive(Clone, Default)]
struct Mock {
    hits: Arc<AtomicUsize>,
}

fn check_agent(headers: &HeaderMap) {
    let agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap();
    assert_eq!(agent, user_agent());
    assert!(agent.starts_with("LibraryRip/"));
    assert!(agent.contains(crate::SOURCE_URL));
}

async fn discid(
    State(mock): State<Mock>,
    Path(id): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    mock.hits.fetch_add(1, Ordering::SeqCst);
    check_agent(&headers);
    let query = query.unwrap_or_default();
    assert!(
        query.contains("inc=artist-credits+recordings+isrcs+labels"),
        "{query}"
    );
    assert!(query.contains("fmt=json"));
    assert!(query.contains("toc="));
    match id.as_str() {
        DISC_ONE | DISC_TWO => {
            Json(json!({"id": id, "releases": [two_disc_release()]})).into_response()
        }
        UNKNOWN_DISC if query.contains("toc=1+3+") => {
            // Fuzzy TOC match: no disc ids on the media.
            let mut release = two_disc_release();
            release["id"] = json!(OTHER_RELEASE);
            for medium in release["media"].as_array_mut().unwrap() {
                medium["discs"] = json!([]);
            }
            Json(json!({"release-count": 1, "release-offset": 0, "releases": [release]}))
                .into_response()
        }
        _ => (StatusCode::NOT_FOUND, Json(json!({"error": "Not Found"}))).into_response(),
    }
}

use axum::Json;

async fn release(State(mock): State<Mock>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    mock.hits.fetch_add(1, Ordering::SeqCst);
    check_agent(&headers);
    if id == RELEASE {
        Json(two_disc_release()).into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

async fn search(State(mock): State<Mock>, RawQuery(query): RawQuery) -> Response {
    mock.hits.fetch_add(1, Ordering::SeqCst);
    let query = query.unwrap_or_default();
    assert!(query.contains("query=collected+tones"), "{query}");
    let mut release = two_disc_release();
    // Search results carry media without positions, discs or tracks.
    release["media"] =
        json!([{"format": "CD", "track-count": 2}, {"format": "CD", "track-count": 3}]);
    Json(json!({"count": 1, "releases": [release]})).into_response()
}

async fn busy(State(mock): State<Mock>) -> Response {
    let hit = mock.hits.fetch_add(1, Ordering::SeqCst);
    if hit == 0 {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    } else {
        Json(json!({"releases": []})).into_response()
    }
}

async fn cover(Path((id, size)): Path<(String, String)>, headers: HeaderMap) -> Response {
    check_agent(&headers);
    match (id.as_str(), size.as_str()) {
        (RELEASE, "front-500") => Redirect::temporary("/images/front.jpg").into_response(),
        (OTHER_RELEASE, "front-500") => Redirect::temporary("/images/huge.jpg").into_response(),
        (RELEASE, "front-250") => ([(header::CONTENT_TYPE, "text/html")], "<html>").into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn image(Path(name): Path<String>) -> Response {
    match name.as_str() {
        "front.jpg" => (
            [(header::CONTENT_TYPE, "image/jpeg")],
            vec![0xFF, 0xD8, 0xFF, 0xD9],
        )
            .into_response(),
        _ => (
            [(header::CONTENT_TYPE, "image/jpeg")],
            vec![0u8; MAX_COVER_BYTES + 1],
        )
            .into_response(),
    }
}

async fn mock_server() -> (MusicBrainzClient, Mock) {
    let mock = Mock::default();
    let router = Router::new()
        .route("/ws/2/discid/{id}", get(discid))
        .route("/ws/2/release/{id}", get(release))
        .route("/ws/2/release", get(search))
        .route("/busy/ws/2/release", get(busy))
        .route("/caa/release/{id}/{size}", get(cover))
        .route("/images/{name}", get(image))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = MusicBrainzClient::new(
        reqwest::Client::new(),
        Url::parse(&format!("http://{address}/ws/2")).unwrap(),
        Url::parse(&format!("http://{address}/caa/")).unwrap(),
    )
    .with_min_interval(Duration::from_millis(1));
    (client, mock)
}

#[tokio::test]
async fn disc_lookup_picks_the_matching_medium() {
    let (client, _) = mock_server().await;
    let candidates = client.lookup_disc(DISC_TWO, &toc(3)).await.unwrap();
    assert_eq!(candidates.len(), 1);
    let candidate = &candidates[0];
    assert_eq!(candidate.release_mbid.as_str(), RELEASE);
    assert_eq!(candidate.title, "Collected Tones");
    assert_eq!(candidate.artist, "Ann Sine & Bob Square");
    assert_eq!(candidate.medium, "CD 2 of 2");
    assert_eq!(candidate.medium_position, 2);
    assert_eq!(candidate.track_count, 3);
    assert_eq!(candidate.date.as_deref(), Some("1999-04-01"));
    assert_eq!(candidate.country.as_deref(), Some("GB"));
    assert!(candidate.has_front_cover);

    let first = client.lookup_disc(DISC_ONE, &toc(2)).await.unwrap();
    assert_eq!(first[0].medium_position, 1);
}

#[tokio::test]
async fn fuzzy_toc_matches_pick_by_track_count() {
    let (client, _) = mock_server().await;
    let candidates = client.lookup_disc(UNKNOWN_DISC, &toc(3)).await.unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].release_mbid.as_str(), OTHER_RELEASE);
    assert_eq!(candidates[0].medium_position, 2);
}

#[tokio::test]
async fn unknown_disc_gives_no_candidates() {
    let (client, _) = mock_server().await;
    let candidates = client.lookup_disc(UNKNOWN_DISC, &toc(5)).await.unwrap();
    assert!(candidates.is_empty());
    assert!(matches!(
        client.lookup_disc("../release", &toc(5)).await,
        Err(MusicBrainzError::Unexpected(_))
    ));
}

#[tokio::test]
async fn release_maps_one_medium() {
    let (client, _) = mock_server().await;
    let mbid = Mbid::new(RELEASE).unwrap();
    let metadata = client.release(&mbid, 2).await.unwrap();
    assert_eq!(metadata.title, "Collected Tones");
    assert_eq!(metadata.artist, "Ann Sine & Bob Square");
    assert_eq!(metadata.artist_sort, None);
    assert_eq!(metadata.artist_mbids.len(), 2);
    assert_eq!(metadata.release_mbid, Some(mbid.clone()));
    assert_eq!(
        metadata.release_group_mbid.as_ref().map(Mbid::as_str),
        Some("dddddddd-0000-0000-0000-000000000000")
    );
    assert_eq!(metadata.year, Some(1999));
    assert_eq!(metadata.label.as_deref(), Some("Test Tones"));
    assert_eq!(metadata.catalog_number.as_deref(), Some("TONE 1"));
    assert_eq!(metadata.barcode.as_deref(), Some("0123456789012"));
    assert_eq!((metadata.disc_number, metadata.disc_total), (2, 2));
    assert_eq!(metadata.disc_id, None);
    let titles: Vec<&str> = metadata.tracks.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, ["Three", "Four", "Five"]);
    let third = &metadata.tracks[2];
    assert_eq!(third.number.map(TrackNumber::get), Some(3));
    assert_eq!(third.artist, "Guest");
    assert_eq!(third.isrc.as_ref().map(Isrc::as_str), Some("GBAYE0000351"));
    assert_eq!(
        third.recording_mbid.as_ref().map(Mbid::as_str),
        Some("55555555-1111-0000-0000-000000000000")
    );
    assert_eq!(
        third.track_mbid.as_ref().map(Mbid::as_str),
        Some("55555555-0000-0000-0000-000000000000")
    );

    assert!(matches!(
        client.release(&mbid, 3).await,
        Err(MusicBrainzError::NotFound(_))
    ));
    assert!(matches!(
        client.release(&Mbid::new(OTHER_RELEASE).unwrap(), 1).await,
        Err(MusicBrainzError::NotFound(_))
    ));
}

use crate::model::Isrc;

#[tokio::test]
async fn answers_are_cached() {
    let (client, mock) = mock_server().await;
    let mbid = Mbid::new(RELEASE).unwrap();
    client.release(&mbid, 1).await.unwrap();
    client.clone().release(&mbid, 2).await.unwrap();
    assert_eq!(mock.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn search_lists_each_disc() {
    let (client, _) = mock_server().await;
    let candidates = client.search("collected tones").await.unwrap();
    let media: Vec<(&str, u8, u8)> = candidates
        .iter()
        .map(|c| (c.medium.as_str(), c.medium_position, c.track_count))
        .collect();
    assert_eq!(media, [("CD 1 of 2", 1, 2), ("CD 2 of 2", 2, 3)]);
}

#[tokio::test]
async fn busy_answers_are_retried() {
    let (client, mock) = mock_server().await;
    let busy_base = client.base.join("../../busy/ws/2/").unwrap();
    let busy = MusicBrainzClient {
        base: busy_base,
        ..client
    };
    assert!(busy.search("collected tones").await.unwrap().is_empty());
    assert_eq!(mock.hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn front_cover_follows_the_redirect() {
    let (client, _) = mock_server().await;
    let cover = client
        .front_cover(&Mbid::new(RELEASE).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cover.mime, "image/jpeg");
    assert_eq!(&cover.bytes[..], &[0xFF, 0xD8, 0xFF, 0xD9]);

    let none = Mbid::new("00000000-0000-0000-0000-000000000000").unwrap();
    assert_eq!(client.front_cover(&none).await.unwrap(), None);

    assert!(matches!(
        client.front_cover(&Mbid::new(OTHER_RELEASE).unwrap()).await,
        Err(MusicBrainzError::CoverTooBig)
    ));
    assert!(matches!(
        client.front_thumbnail(&Mbid::new(RELEASE).unwrap()).await,
        Err(MusicBrainzError::Unexpected(_))
    ));
}

#[tokio::test]
async fn the_process_limiter_spaces_requests_a_second_apart() {
    let (client, mock) = mock_server().await;
    let shared = MusicBrainzClient::new(
        reqwest::Client::new(),
        client.base.clone(),
        client.cover_base.clone(),
    );
    let other = MusicBrainzClient::new(
        reqwest::Client::new(),
        client.base.clone(),
        client.cover_base.clone(),
    );
    let started = Instant::now();
    shared.search("collected tones").await.unwrap();
    // A different client (own cache) still shares the limiter.
    other.search("collected tones").await.unwrap();
    assert_eq!(mock.hits.load(Ordering::SeqCst), 2);
    assert!(started.elapsed() >= Duration::from_millis(990));
}

#[test]
fn toc_parameter_includes_the_lead_in() {
    assert_eq!(toc_parameter(&toc(3)), "1 3 3150 150 1150 2150");
}

#[test]
fn toc_parameter_leaves_out_trailing_data_tracks() {
    let mut entries: Vec<TrackEntry> = toc(2).tracks().to_vec();
    entries.push(TrackEntry {
        number: TrackNumber::new(3).unwrap(),
        start_lba: Lba(13_400),
        length_sectors: Sectors(1000),
        is_audio: false,
        pregap: Sectors(0),
        isrc: None,
        session: crate::model::SessionNumber::FIRST,
    });
    entries[1].length_sectors = Sectors(12_400);
    let enhanced = Toc::new(entries, Lba(14_400)).unwrap();
    assert_eq!(toc_parameter(&enhanced), "1 2 2150 150 1150");
}
