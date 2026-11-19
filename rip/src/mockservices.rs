// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stand-ins for MusicBrainz, the Cover Art Archive, AccurateRip (its
//! database and drive offset list) and the CUETools database that know one
//! simulated disc, so a rip of it can be identified and verified with no
//! network.
//!
//! The answers are worked out from the disc's own track files (which hold
//! exactly what a perfect rip produces), so a correct rip comes out as
//! "Accurately ripped" and a wrong one (bad offset, read errors) as a
//! mismatch, just as with the real services.
//!
//! Used by the end-to-end tests and by `library-rip mock-services DIR`
//! (testsupport feature), which prints the `LIBRARY_RIP_*_URL` variables
//! that point the ripper here.

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path as UrlPath, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value, json};
use url::Url;

use crate::app::pipeline::Endpoints;
use crate::core::accuraterip::{TrackPosition, database_file_name, track_crcs};
use crate::core::{CoreError, ctdb, discid};
use crate::fakedisc::{FakeDisc, FakeDiscError};
use crate::model::{AccurateRipDiscId, Toc};

/// The album the mock MusicBrainz reports for the disc.
pub const ALBUM_TITLE: &str = "Sine Tones";
/// Its artist.
pub const ALBUM_ARTIST: &str = "The Test Card";
/// Its release date (the year is what the catalogue shows).
pub const ALBUM_DATE: &str = "2024-05-17";
/// Its MusicBrainz release id.
pub const RELEASE_ID: &str = "5a1e0000-0000-4000-8000-000000000001";
/// Its release group id.
const RELEASE_GROUP_ID: &str = "5a1e0000-0000-4000-8000-000000000002";
/// The artist's id.
const ARTIST_ID: &str = "5a1e0000-0000-4000-8000-000000000003";
/// AccurateRip confidence given to every track.
pub const ACCURATERIP_CONFIDENCE: u8 = 12;
/// CUETools database confidence for the disc.
pub const CTDB_CONFIDENCE: u32 = 7;

/// The front cover served by the mock Cover Art Archive (a PNG).
pub const COVER_PNG: &[u8] = include_bytes!("../ui/icons/library-rip-256.png");

/// Problems starting the mock services.
#[derive(Debug, thiserror::Error)]
pub enum MockError {
    /// The simulated disc could not be loaded.
    #[error(transparent)]
    Disc(#[from] FakeDiscError),
    /// A checksum or id could not be worked out.
    #[error(transparent)]
    Core(#[from] CoreError),
    /// A track file could not be read.
    #[error("could not read {path}: {source}")]
    Io {
        /// The file.
        path: std::path::PathBuf,
        /// Why.
        source: std::io::Error,
    },
    /// The listener's address is unusable.
    #[error("the mock services' address is unusable: {0}")]
    Address(String),
}

/// Everything the mock answers with, worked out once.
#[derive(Debug)]
struct Answers {
    disc_id: String,
    accuraterip_id: AccurateRipDiscId,
    accuraterip_file: String,
    dbar: Vec<u8>,
    drive_offsets: Vec<u8>,
    ctdb_toc: String,
    ctdb_xml: String,
    release: Value,
}

/// Names a track after its tone (track 1 is A4, rising a semitone).
pub fn track_title(number: u8) -> String {
    const NOTES: [&str; 12] = [
        "A", "B flat", "B", "C", "C sharp", "D", "E flat", "E", "F", "F sharp", "G", "A flat",
    ];
    let index = usize::from(number.saturating_sub(1));
    let note = NOTES.get(index % NOTES.len()).copied().unwrap_or("A");
    // A4 is index 0; C5 starts the next octave number.
    let octave = 4 + (index + 9) / 12;
    format!("{note} {octave}")
}

/// A running mock: its endpoints and the task serving it.
#[derive(Debug)]
pub struct MockServices {
    /// Where the ripper should look for each service.
    pub endpoints: Endpoints,
    /// The base address everything is served under.
    pub base: Url,
    /// The MusicBrainz disc id of the disc.
    pub disc_id: String,
    task: tokio::task::JoinHandle<()>,
}

impl MockServices {
    /// The environment variables that point `library-rip` here.
    pub fn environment(&self) -> Vec<(&'static str, String)> {
        vec![
            (
                "LIBRARY_RIP_MUSICBRAINZ_URL",
                self.endpoints.musicbrainz.to_string(),
            ),
            (
                "LIBRARY_RIP_COVER_ART_URL",
                self.endpoints.cover_art.to_string(),
            ),
            (
                "LIBRARY_RIP_ACCURATERIP_URL",
                self.endpoints.accuraterip.to_string(),
            ),
            (
                "LIBRARY_RIP_DRIVE_OFFSETS_URL",
                self.endpoints.drive_offsets.to_string(),
            ),
            ("LIBRARY_RIP_CTDB_URL", self.endpoints.ctdb.to_string()),
        ]
    }

    /// Waits until the server stops (it runs until the process ends).
    pub async fn wait(mut self) {
        let _ = (&mut self.task).await;
    }
}

impl Drop for MockServices {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Starts the mock services for the simulated disc in `disc_dir` on
/// `listener`.
pub async fn start(
    disc_dir: &Path,
    listener: tokio::net::TcpListener,
) -> Result<MockServices, MockError> {
    let answers = Arc::new(answers(disc_dir)?);
    let address = listener
        .local_addr()
        .map_err(|e| MockError::Address(e.to_string()))?;
    let base =
        Url::parse(&format!("http://{address}/")).map_err(|e| MockError::Address(e.to_string()))?;
    let join = |path: &str| {
        base.join(path)
            .map_err(|e| MockError::Address(e.to_string()))
    };
    let endpoints = Endpoints {
        musicbrainz: join("ws/2/")?,
        cover_art: join("cover/")?,
        accuraterip: join("accuraterip/")?,
        drive_offsets: join("accuraterip/DriveOffsets.bin")?,
        ctdb: join("ctdb/")?,
    };
    let disc_id = answers.disc_id.clone();
    let app = Router::new()
        .route("/ws/2/discid/{id}", get(discid_lookup))
        .route("/ws/2/release/{id}", get(release_lookup))
        .route("/cover/release/{id}/{size}", get(cover))
        .route("/accuraterip/DriveOffsets.bin", get(drive_offsets))
        .route("/accuraterip/{a}/{b}/{c}/{file}", get(dbar))
        .route("/ctdb/lookup2.php", get(ctdb_lookup))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .with_state(answers);
    let task = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            tracing::warn!(%error, "the mock services stopped");
        }
    });
    Ok(MockServices {
        endpoints,
        base,
        disc_id,
        task,
    })
}

fn answers(disc_dir: &Path) -> Result<Answers, MockError> {
    let disc = FakeDisc::load(disc_dir)?;
    let toc = &disc.spec.toc;
    let mut pcm = Vec::new();
    for track in toc.audio_tracks() {
        let path = disc.track_path(track.number);
        let bytes = std::fs::read(&path).map_err(|source| MockError::Io { path, source })?;
        pcm.push(bytes);
    }
    let count = pcm.len();

    // AccurateRip: two entries, as the real database often has: one with
    // the v2 checksums and one with the v1 checksums.
    let accuraterip_id = discid::accuraterip_disc_id(toc)?;
    let mut header = vec![accuraterip_id.track_count];
    header.extend_from_slice(&accuraterip_id.id1.to_le_bytes());
    header.extend_from_slice(&accuraterip_id.id2.to_le_bytes());
    header.extend_from_slice(&accuraterip_id.cddb.to_le_bytes());
    let mut crcs = Vec::with_capacity(count);
    for (index, track) in pcm.iter().enumerate() {
        let position = TrackPosition::for_track(index, count)
            .ok_or_else(|| CoreError::Invalid("track out of range".into()))?;
        crcs.push(track_crcs(track, position)?);
    }
    let mut dbar = Vec::new();
    for (confidence, version_two) in [(ACCURATERIP_CONFIDENCE, true), (3, false)] {
        dbar.extend_from_slice(&header);
        for crc in &crcs {
            dbar.push(confidence);
            let value = if version_two { crc.v2.0 } else { crc.v1.0 };
            dbar.extend_from_slice(&value.to_le_bytes());
            dbar.extend_from_slice(&0u32.to_le_bytes());
        }
    }

    // The drive offset list: the simulated drive at its true offset.
    let drive = &disc.spec.drive;
    let mut record = Vec::with_capacity(69);
    let offset = i16::try_from(disc.spec.read_offset.0).unwrap_or(0);
    record.extend_from_slice(&offset.to_le_bytes());
    let mut name = format!("{} - {}", drive.vendor.trim(), drive.model.trim()).into_bytes();
    name.resize(32, 0);
    record.extend_from_slice(&name);
    record.push(0);
    record.extend_from_slice(&42u32.to_le_bytes());
    record.resize(69, 0);

    // CTDB: the whole-disc CRC.
    let slices: Vec<&[u8]> = pcm.iter().map(Vec::as_slice).collect();
    let crc = ctdb::disc_crc(&slices, toc)?;
    let ctdb_xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <ctdb xmlns=\"http://db.cuetools.net/ns/mmd-1.0#\">\n  \
         <entry confidence=\"{CTDB_CONFIDENCE}\" crc32=\"{crc:08x}\" id=\"1\" npar=\"8\" stride=\"5880\" \
         toc=\"{}\" trackcrcs=\"\" />\n</ctdb>\n",
        discid::ctdb_toc_id(toc)?
    );

    let disc_id = discid::musicbrainz_disc_id(toc)?;
    let release = release_json(&disc_id, toc);
    Ok(Answers {
        accuraterip_file: database_file_name(&accuraterip_id),
        accuraterip_id,
        dbar,
        drive_offsets: record,
        ctdb_toc: discid::ctdb_toc_id(toc)?,
        ctdb_xml,
        disc_id,
        release,
    })
}

fn release_json(disc_id: &str, toc: &Toc) -> Value {
    let credit = json!([{
        "name": ALBUM_ARTIST,
        "joinphrase": "",
        "artist": {"id": ARTIST_ID, "name": ALBUM_ARTIST, "sort-name": "Test Card, The"},
    }]);
    let tracks: Vec<Value> = toc
        .audio_tracks()
        .enumerate()
        .map(|(index, track)| {
            let number = track.number.get();
            let recording_id = format!("5a1e0000-0000-4000-9000-{number:012}");
            let track_id = format!("5a1e0000-0000-4000-a000-{number:012}");
            json!({
                "id": track_id,
                "position": index + 1,
                "number": (index + 1).to_string(),
                "title": track_title(number),
                "length": track.length_sectors.duration_ms(),
                "artist-credit": credit,
                "recording": {"id": recording_id, "title": track_title(number), "artist-credit": credit, "isrcs": []},
            })
        })
        .collect();
    json!({
        "id": RELEASE_ID,
        "title": ALBUM_TITLE,
        "status": "Official",
        "date": ALBUM_DATE,
        "country": "XW",
        "barcode": "",
        "artist-credit": credit,
        "cover-art-archive": {"front": true, "artwork": true, "count": 1},
        "label-info": [{"catalog-number": "TONE-1", "label": {"name": "Test Pressings"}}],
        "release-group": {"id": RELEASE_GROUP_ID, "title": ALBUM_TITLE},
        "media": [{
            "position": 1,
            "format": "CD",
            "track-count": tracks.len(),
            "discs": [{"id": disc_id}],
            "tracks": tracks,
        }],
    })
}

fn json_response(value: &Value) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        value.to_string(),
    )
        .into_response()
}

async fn discid_lookup(
    State(answers): State<Arc<Answers>>,
    UrlPath(id): UrlPath<String>,
) -> Response {
    if id != answers.disc_id {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    }
    json_response(&json!({
        "id": answers.disc_id,
        "releases": [answers.release],
    }))
}

async fn release_lookup(
    State(answers): State<Arc<Answers>>,
    UrlPath(id): UrlPath<String>,
) -> Response {
    if id != RELEASE_ID {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    }
    json_response(&answers.release)
}

async fn cover(UrlPath((id, _size)): UrlPath<(String, String)>) -> Response {
    if id != RELEASE_ID {
        return StatusCode::NOT_FOUND.into_response();
    }
    ([(header::CONTENT_TYPE, "image/png")], COVER_PNG).into_response()
}

async fn drive_offsets(State(answers): State<Arc<Answers>>) -> Response {
    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        answers.drive_offsets.clone(),
    )
        .into_response()
}

async fn dbar(
    State(answers): State<Arc<Answers>>,
    UrlPath((_a, _b, _c, file)): UrlPath<(String, String, String, String)>,
) -> Response {
    if file != answers.accuraterip_file {
        tracing::debug!(asked = %file, have = %answers.accuraterip_file, id = ?answers.accuraterip_id, "unknown dBAR");
        return StatusCode::NOT_FOUND.into_response();
    }
    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        answers.dbar.clone(),
    )
        .into_response()
}

async fn ctdb_lookup(
    State(answers): State<Arc<Answers>>,
    query: axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if query.get("toc") != Some(&answers.ctdb_toc) {
        return StatusCode::NOT_FOUND.into_response();
    }
    (
        [(header::CONTENT_TYPE, "text/xml; charset=utf-8")],
        answers.ctdb_xml.clone(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::SERVICE_TIMEOUT;
    use crate::core::accuraterip::{fetch_database, verify};
    use crate::core::offset::{fetch_drive_database, find_offset};
    use crate::model::{AccurateRipStatus, CtdbStatus, SampleOffset, TrackNumber};
    use crate::testsupport::{FakeDiscOptions, make_fake_disc};

    #[test]
    fn tracks_are_named_after_their_tones() {
        assert_eq!(track_title(1), "A 4");
        assert_eq!(track_title(2), "B flat 4");
        assert_eq!(track_title(4), "C 5");
        assert_eq!(track_title(13), "A 5");
    }

    #[tokio::test]
    async fn the_mock_confirms_a_perfect_rip() {
        let temp = tempfile::tempdir().unwrap();
        let disc = make_fake_disc(
            temp.path(),
            &FakeDiscOptions {
                tracks: 3,
                seconds: 2,
                read_offset: SampleOffset(30),
                errors: Vec::new(),
            },
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mock = start(temp.path(), listener).await.unwrap();
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let toc = &disc.spec.toc;

        // The drive list knows the simulated drive's offset.
        let drives = fetch_drive_database(&http, &mock.endpoints.drive_offsets, SERVICE_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(
            find_offset(&drives, &disc.spec.drive),
            Some(SampleOffset(30))
        );

        // AccurateRip confirms the track files themselves.
        let id = discid::accuraterip_disc_id(toc).unwrap();
        let entries = fetch_database(&http, &mock.endpoints.accuraterip, &id, SERVICE_TIMEOUT)
            .await
            .unwrap();
        let mut ours = Vec::new();
        for (index, track) in toc.audio_tracks().enumerate() {
            let pcm = std::fs::read(disc.track_path(track.number)).unwrap();
            let position = TrackPosition::for_track(index, 3).unwrap();
            ours.push((track.number, track_crcs(&pcm, position).unwrap()));
        }
        let results = verify(&ours, &entries);
        assert_eq!(results.len(), 3);
        for (_, status) in &results {
            assert!(
                matches!(status, AccurateRipStatus::Accurate { confidence, .. } if *confidence == u32::from(ACCURATERIP_CONFIDENCE)),
                "{status:?}"
            );
        }

        // CTDB knows the disc.
        let found = ctdb::lookup(&http, &mock.endpoints.ctdb, toc, SERVICE_TIMEOUT)
            .await
            .unwrap();
        let pcm: Vec<Vec<u8>> = toc
            .audio_tracks()
            .map(|t| std::fs::read(disc.track_path(t.number)).unwrap())
            .collect();
        let slices: Vec<&[u8]> = pcm.iter().map(Vec::as_slice).collect();
        let crc = ctdb::disc_crc(&slices, toc).unwrap();
        assert!(matches!(
            ctdb::verify(crc, &found),
            CtdbStatus::Accurate { confidence: 7, .. }
        ));

        // MusicBrainz knows the release, and the cover is a PNG.
        let musicbrainz = crate::musicbrainz::MusicBrainzClient::new(
            http.clone(),
            mock.endpoints.musicbrainz.clone(),
            mock.endpoints.cover_art.clone(),
        )
        .with_min_interval(std::time::Duration::ZERO);
        let candidates = musicbrainz.lookup_disc(&mock.disc_id, toc).await.unwrap();
        assert_eq!(candidates.len(), 1);
        let release = musicbrainz
            .release(&crate::model::Mbid::new(RELEASE_ID).unwrap(), 1)
            .await
            .unwrap();
        assert_eq!(release.title, ALBUM_TITLE);
        assert_eq!(release.artist, ALBUM_ARTIST);
        assert_eq!(release.tracks.len(), 3);
        assert_eq!(release.tracks[1].number, Some(TrackNumber::new(2).unwrap()));
        assert_eq!(release.tracks[1].title, "B flat 4");
        let cover = musicbrainz
            .front_cover(&crate::model::Mbid::new(RELEASE_ID).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&cover.bytes[..], COVER_PNG);
    }
}
