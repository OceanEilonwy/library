// SPDX-License-Identifier: AGPL-3.0-or-later
//! CUETools database (CTDB) lookup: an independent confidence count.
//!
//! **Lookup.** `GET {base}lookup2.php?version=3&ctdb=1&fuzzy=0&toc=…` with
//! the TOC string from [`super::discid::ctdb_toc_id`] answers with XML:
//! `<ctdb><entry confidence="8" crc32="e0a03be5" trackcrcs="…" …/>…</ctdb>`.
//! `fuzzy=0` asks only for entries with exactly our TOC.
//!
//! **Disc CRC.** CTDB's checksum is a plain CRC-32 (IEEE, as in zip) of the
//! whole disc's audio from track 1's INDEX 01 to the end of the last audio
//! track, leaving out 10 × 588 samples at the very start and at the very end
//! (which a drive's offset can push off the disc). Hidden track one audio
//! before INDEX 01 is not part of it. Assumptions, following the CUETools
//! wiki: the PCM is offset-corrected, 16-bit little-endian stereo, and the
//! tracks are given exactly as ripped (each running to the next track's
//! INDEX 01). On an enhanced CD the audio ends with the last audio track,
//! at the end of the audio session (CUETools `AudioLength`); the data
//! session is not part of it. CTDB stores entries at their own pressing
//! offset; [`verify`] compares at our position and
//! [`super::pressing::ctdb_with_pressings`] searches other pressings with
//! a rolling CRC (CUETools does it with the parity data instead).

use std::time::Duration;

use url::Url;

use super::{CoreError, Result, discid, with_trailing_slash};
use crate::model::{BYTES_PER_SAMPLE, CtdbStatus, SAMPLES_PER_SECTOR, SampleOffset, Toc};

/// Default lookup base URL.
pub const DEFAULT_BASE: &str = "http://db.cuetools.net/";

/// Samples left out of the disc CRC at each end of the disc.
pub const EDGE_SAMPLES: u64 = 10 * SAMPLES_PER_SECTOR as u64;

/// One CTDB entry for this TOC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CtdbEntry {
    /// Whole-disc CRC-32 as computed by [`disc_crc`].
    pub crc32: u32,
    /// Submissions with this CRC.
    pub confidence: u32,
}

/// The lookup URL (`lookup2.php?version=3&ctdb=1&fuzzy=0&toc=…`).
pub fn lookup_url(base: &Url, toc: &Toc) -> Result<Url> {
    let mut url = with_trailing_slash(base)
        .join("lookup2.php")
        .map_err(|e| CoreError::Invalid(format!("bad CUETools database address: {e}")))?;
    // The TOC string is digits, `-` and `:` only, all fine in a query.
    url.set_query(Some(&format!(
        "version=3&ctdb=1&fuzzy=0&toc={}",
        discid::ctdb_toc_id(toc)?
    )));
    Ok(url)
}

/// Parses the XML lookup response.
pub fn parse_lookup(xml: &str) -> Result<Vec<CtdbEntry>> {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_str(xml);
    let mut entries = Vec::new();
    loop {
        match reader.read_event().map_err(unreadable)? {
            Event::Start(element) | Event::Empty(element)
                if element.local_name().as_ref() == b"entry" =>
            {
                let mut crc32 = None;
                let mut confidence = None;
                for attribute in element.attributes() {
                    let attribute = attribute.map_err(unreadable)?;
                    let value = std::str::from_utf8(&attribute.value).map_err(unreadable)?;
                    match attribute.key.local_name().as_ref() {
                        b"crc32" => {
                            crc32 = Some(u32::from_str_radix(value, 16).map_err(unreadable)?);
                        }
                        b"confidence" => {
                            confidence = Some(value.parse::<u32>().map_err(unreadable)?);
                        }
                        _ => {}
                    }
                }
                match (crc32, confidence) {
                    (Some(crc32), Some(confidence)) => {
                        entries.push(CtdbEntry { crc32, confidence });
                    }
                    _ => {
                        return Err(unreadable("an entry without crc32 or confidence"));
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(entries)
}

/// Fetches entries for a TOC, giving up after `timeout`. A 404 means "not
/// in database".
pub async fn lookup(
    client: &reqwest::Client,
    base: &Url,
    toc: &Toc,
    timeout: Duration,
) -> Result<Vec<CtdbEntry>> {
    let url = lookup_url(base, toc)?;
    let network = |source| CoreError::Network {
        service: "the CUETools database",
        source,
    };
    let response = client
        .get(url)
        .timeout(timeout)
        .send()
        .await
        .map_err(network)?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    let text = response
        .error_for_status()
        .map_err(network)?
        .text()
        .await
        .map_err(network)?;
    parse_lookup(&text)
}

/// Computes the CTDB whole-disc CRC from all audio tracks' PCM in order.
/// `tracks_pcm` must hold one buffer per audio track of `toc`.
pub fn disc_crc(tracks_pcm: &[&[u8]], toc: &Toc) -> Result<u32> {
    let audio_tracks = toc.audio_tracks().count();
    if tracks_pcm.len() != audio_tracks {
        return Err(CoreError::Invalid(format!(
            "the disc has {audio_tracks} audio tracks but {} were given",
            tracks_pcm.len()
        )));
    }
    let mut total = 0u64;
    for pcm in tracks_pcm {
        total = total
            .checked_add(u64::try_from(pcm.len()).map_err(|_| too_long())?)
            .ok_or_else(too_long)?;
    }
    let mut state = DiscCrcState::new(total / BYTES_PER_SAMPLE as u64);
    for pcm in tracks_pcm {
        state.update(pcm)?;
    }
    state.finish()
}

/// Incremental form of [`disc_crc`] for streaming the tracks from files.
#[derive(Debug)]
pub struct DiscCrcState {
    hasher: crc32fast::Hasher,
    /// Byte range of the disc that is checksummed.
    first_byte: u64,
    end_byte: u64,
    total_bytes: u64,
    seen: u64,
}

impl DiscCrcState {
    /// Starts a CRC for a disc whose audio tracks hold `total_samples`.
    pub fn new(total_samples: u64) -> Self {
        let bytes_per_sample = BYTES_PER_SAMPLE as u64;
        let total_bytes = total_samples.saturating_mul(bytes_per_sample);
        let edge = EDGE_SAMPLES * bytes_per_sample;
        let first_byte = edge.min(total_bytes);
        let end_byte = total_bytes.saturating_sub(edge).max(first_byte);
        DiscCrcState {
            hasher: crc32fast::Hasher::new(),
            first_byte,
            end_byte,
            total_bytes,
            seen: 0,
        }
    }

    /// Feeds the next PCM bytes, in disc order across track boundaries.
    pub fn update(&mut self, pcm: &[u8]) -> Result<()> {
        let length = u64::try_from(pcm.len()).map_err(|_| too_long())?;
        let start = self.seen;
        let end = start.checked_add(length).ok_or_else(too_long)?;
        if end > self.total_bytes {
            return Err(CoreError::Invalid(
                "more audio was given than the disc holds".into(),
            ));
        }
        let from = self.first_byte.clamp(start, end) - start;
        let to = self.end_byte.clamp(start, end) - start;
        let from = usize::try_from(from).map_err(|_| too_long())?;
        let to = usize::try_from(to).map_err(|_| too_long())?;
        if let Some(part) = pcm.get(from..to) {
            self.hasher.update(part);
        }
        self.seen = end;
        Ok(())
    }

    /// Finishes and returns the CRC.
    pub fn finish(self) -> Result<u32> {
        if self.seen != self.total_bytes {
            return Err(CoreError::Invalid(format!(
                "the disc should have {} bytes of audio but only {} were given",
                self.total_bytes, self.seen
            )));
        }
        Ok(self.hasher.finalize())
    }
}

/// Compares our disc CRC with the entries.
pub fn verify(our_crc: u32, entries: &[CtdbEntry]) -> CtdbStatus {
    let total = entries.iter().map(|e| e.confidence).sum();
    match entries.iter().find(|e| e.crc32 == our_crc) {
        Some(entry) => CtdbStatus::Accurate {
            confidence: entry.confidence,
            total,
            pressing_offset: SampleOffset(0),
        },
        None if entries.is_empty() => CtdbStatus::NotInDatabase,
        None => CtdbStatus::Mismatch { total },
    }
}

fn unreadable(error: impl std::fmt::Display) -> CoreError {
    CoreError::Invalid(format!(
        "the CUETools database sent a reply that could not be read ({error})"
    ))
}

fn too_long() -> CoreError {
    CoreError::Invalid("the audio is longer than any CD".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::discid::tests::{SIMPLE, toc_from_cdtoc};
    use crate::core::{SERVICE_TIMEOUT, testserver};

    /// A reply from the live database for the TOC
    /// `0:11413:25024:45713:55220`.
    const LIVE_REPLY: &str = r#"<ctdb xmlns="http://db.cuetools.net/ns/mmd-1.0#" xmlns:ext="http://db.cuetools.net/ns/ext-1.0#">
 <entry confidence="8" crc32="e0a03be5" hasparity="http://p.cuetools.net/94933" id="94933" npar="8" stride="5880" syndrome="U5VQSKpJeu1+GUqT6KNUrw==" toc="0:11413:25024:45713:55220" trackcrcs="c6858467 bd788446 d479ac2e 193890d4" />
 <entry confidence="1" crc32="9b38421f" id="4700756" npar="16" stride="5880" syndrome="U5VQSKpJeu1+GUqT6KNUr89AVe1jHL/mwiMSSBI9X2w=" toc="0:11413:25024:45713:55220" trackcrcs="c6858467 bd788446 dc379f45 193890d4" />
 <metadata />
</ctdb>"#;

    #[test]
    fn url_uses_the_toc_string() {
        let url = lookup_url(&Url::parse(DEFAULT_BASE).unwrap(), &toc_from_cdtoc(SIMPLE)).unwrap();
        assert_eq!(
            url.as_str(),
            "http://db.cuetools.net/lookup2.php?version=3&ctdb=1&fuzzy=0&toc=0:11413:25024:45713:55220"
        );
    }

    #[test]
    fn parses_a_live_reply() {
        assert_eq!(
            parse_lookup(LIVE_REPLY).unwrap(),
            vec![
                CtdbEntry {
                    crc32: 0xe0a0_3be5,
                    confidence: 8
                },
                CtdbEntry {
                    crc32: 0x9b38_421f,
                    confidence: 1
                },
            ]
        );
        assert!(parse_lookup("<ctdb></ctdb>").unwrap().is_empty());
        assert!(parse_lookup(r#"<ctdb><entry crc32="zz" confidence="1"/></ctdb>"#).is_err());
        assert!(parse_lookup(r#"<ctdb><entry confidence="1"/></ctdb>"#).is_err());
        assert!(parse_lookup("<ctdb><entry").is_err());
    }

    #[test]
    fn verify_counts_confidence() {
        let entries = parse_lookup(LIVE_REPLY).unwrap();
        assert_eq!(
            verify(0x9b38_421f, &entries),
            CtdbStatus::Accurate {
                confidence: 1,
                total: 9,
                pressing_offset: SampleOffset(0),
            }
        );
        assert_eq!(verify(1, &entries), CtdbStatus::Mismatch { total: 9 });
        assert_eq!(verify(1, &[]), CtdbStatus::NotInDatabase);
    }

    fn two_track_toc(first: u32, second: u32) -> Toc {
        use crate::model::{Lba, Sectors, TrackEntry, TrackNumber};
        let track = |n: u8, start: u32, length: u32| TrackEntry {
            number: TrackNumber::new(n).unwrap(),
            start_lba: Lba(start),
            length_sectors: Sectors(length),
            is_audio: true,
            pregap: Sectors(0),
            isrc: None,
            session: crate::model::SessionNumber::FIRST,
        };
        Toc::new(
            vec![track(1, 0, first), track(2, first, second)],
            Lba(first + second),
        )
        .unwrap()
    }

    #[test]
    fn disc_crc_leaves_out_ten_sectors_at_each_end() {
        let disc: Vec<u8> = (0..40 * 2352).map(|i| (i * 7 % 251) as u8).collect();
        let (one, two) = disc.split_at(15 * 2352);
        let toc = two_track_toc(15, 25);
        let expected = crc32fast::hash(&disc[10 * 2352..30 * 2352]);
        assert_eq!(disc_crc(&[one, two], &toc).unwrap(), expected);

        // Changing a sample inside the edges does not change the CRC.
        let mut edited = disc.clone();
        edited[100] ^= 0xFF;
        edited[39 * 2352] ^= 0xFF;
        let (one, two) = edited.split_at(15 * 2352);
        assert_eq!(disc_crc(&[one, two], &toc).unwrap(), expected);

        // Streaming in odd pieces gives the same answer.
        let mut state = DiscCrcState::new((disc.len() / 4) as u64);
        for piece in disc.chunks(999) {
            state.update(piece).unwrap();
        }
        assert_eq!(state.finish().unwrap(), expected);
    }

    #[test]
    fn disc_crc_checks_its_input() {
        let toc = two_track_toc(15, 25);
        assert!(disc_crc(&[&[0u8; 4]], &toc).is_err());
        let mut state = DiscCrcState::new(2);
        assert!(state.update(&[0; 12]).is_err());
        state.update(&[0; 4]).unwrap();
        assert!(state.finish().is_err());
        // A disc shorter than both edges has nothing left to checksum.
        let tiny = DiscCrcState::new(0);
        assert_eq!(tiny.finish().unwrap(), crc32fast::hash(&[]));
    }

    #[tokio::test]
    async fn lookup_from_a_mock_server() {
        use axum::extract::Query;
        use axum::http::StatusCode;
        use axum::{Router, routing::get};
        use std::collections::HashMap;

        let app = Router::new().route(
            "/lookup2.php",
            get(|Query(query): Query<HashMap<String, String>>| async move {
                match query.get("toc").map(String::as_str) {
                    Some("0:11413:25024:45713:55220") => (StatusCode::OK, LIVE_REPLY.to_owned()),
                    _ => (StatusCode::NOT_FOUND, String::new()),
                }
            }),
        );
        let base = testserver::serve(app).await;
        let client = testserver::client();
        let entries = lookup(&client, &base, &toc_from_cdtoc(SIMPLE), SERVICE_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(entries.len(), 2);
        let other = two_track_toc(15, 25);
        assert!(
            lookup(&client, &base, &other, SERVICE_TIMEOUT)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
