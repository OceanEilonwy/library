// SPDX-License-Identifier: AGPL-3.0-or-later
//! The human-readable rip log.
//!
//! A stable plain-text format (LF line endings, UTF-8): a header with the
//! app version and source link, the album, the drive with its read offset
//! and read mode, the disc identifiers, the table of contents, one block per
//! track (file, peak level, copy and test CRCs, AccurateRip checksums and
//! result, read errors, ISRC, file checksum), the CUETools database result
//! and a one-paragraph summary from [`summary`]. Everything comes from the
//! [`RipReport`], so the same report always gives the same log.

use std::fmt::Write as _;

use super::{Result, accuraterip};
use crate::model::{
    AccurateRipStatus, AccurateRipVersion, CtdbStatus, OffsetSource, ReadMode, RipReport, Sectors,
    TrackNumber, TrackReport,
};

/// Width of the label column.
const LABEL: usize = 22;

/// Renders the log: drive, offset, mode, TOC, per-track CRCs and test CRCs,
/// AccurateRip and CTDB results, peak levels, unrecovered sectors, and a
/// plain-English summary at the end.
pub fn render(report: &RipReport) -> Result<String> {
    Ok(build(report))
}

fn build(report: &RipReport) -> String {
    let mut log = Log::default();
    log.raw(&format!("Library Rip {} - rip log", report.app_version));
    log.raw(&format!("Source code: {}", crate::SOURCE_URL));
    log.blank();
    log.field("Started", &timestamp(report.started_at));
    log.field("Finished", &timestamp(report.finished_at));

    let release = &report.release;
    log.heading("Album");
    log.field("Artist", or_unknown(&release.artist));
    log.field("Title", or_unknown(&release.title));
    if let Some(date) = release.date.as_deref() {
        log.field("Released", date);
    } else if let Some(year) = release.year {
        log.field("Released", &year.to_string());
    }
    if let Some(label) = release.label.as_deref() {
        log.field("Label", label);
    }
    if let Some(catalog) = release.catalog_number.as_deref() {
        log.field("Catalogue number", catalog);
    }
    if let Some(barcode) = release.barcode.as_deref() {
        log.field("Barcode", barcode);
    }
    if release.disc_total > 1 {
        log.field(
            "Disc",
            &format!("{} of {}", release.disc_number, release.disc_total),
        );
    }
    if let Some(mbid) = &release.release_mbid {
        log.field("MusicBrainz release", mbid.as_str());
    }

    log.heading("Drive");
    log.field("Drive", &report.drive.to_string());
    log.field(
        "Read offset",
        &format!(
            "{} samples ({})",
            report.read_offset,
            match report.offset_source {
                OffsetSource::AccurateRipDatabase => "from the AccurateRip drive database",
                OffsetSource::User => "set in Settings",
                OffsetSource::Unknown => "drive not recognised, no correction applied",
            }
        ),
    );
    log.field(
        "Read mode",
        match report.read_mode {
            ReadMode::Secure => "Secure (every sector read and verified by cdparanoia)",
            ReadMode::TestAndCopy => "Test and copy (every track read securely twice and compared)",
        },
    );

    let ids = &report.disc_ids;
    log.heading("Disc");
    log.field("MusicBrainz disc ID", &ids.musicbrainz);
    log.field("freedb disc ID", &format!("{:08X}", ids.cddb));
    log.field(
        "AccurateRip disc ID",
        &accuraterip::database_file_name(&ids.accuraterip),
    );
    log.field("CTDB TOC", &ids.ctdb_toc);
    log.field(
        "Media catalogue no.",
        report
            .mcn
            .as_ref()
            .filter(|m| !m.is_blank())
            .map_or("none", |m| m.as_str()),
    );

    log.heading("Table of contents");
    log.raw("  Track  Start     Length    Pre-gap   First sector  Last sector  Type");
    for track in report.toc.tracks() {
        let last_sector = track
            .start_lba
            .0
            .saturating_add(track.length_sectors.0)
            .saturating_sub(1);
        log.raw(&format!(
            "  {:>5}  {}  {}  {}  {:>12}  {:>11}  {}",
            // The model's Display impls ignore width, so pad strings.
            track.number.to_string(),
            Sectors(track.start_lba.0).to_msf(),
            track.length_sectors.to_msf(),
            track.pregap.to_msf(),
            track.start_lba.to_string(),
            last_sector,
            if track.is_audio { "audio" } else { "data" }
        ));
    }
    log.raw(&format!("  Lead-out at sector {}", report.toc.lead_out()));

    for track in &report.tracks {
        track_block(&mut log, track);
    }

    log.heading("CUETools database");
    log.field("Whole disc", &report.ctdb.summary());

    log.heading("Summary");
    log.raw(&format!("  {}", summary(report)));
    log.blank();
    log.raw("End of log");
    log.text
}

fn track_block(log: &mut Log, track: &TrackReport) {
    log.heading(&format!("Track {}", track.number));
    log.field("Title", or_unknown(&track.title));
    log.field("File", &track.file_name);
    if track.pregap.0 > 0 {
        log.field("Pre-gap", &track.pregap.to_msf().to_string());
    }
    log.field("Peak level", &format!("{:.1} %", track.peak.percent()));
    log.field("Copy CRC", &track.copy_crc.to_string());
    log.field(
        "Test CRC",
        &match track.test_crc {
            None => "not made (secure mode reads once)".to_owned(),
            Some(test) if test == track.copy_crc => format!("{test} (matches the copy)"),
            Some(test) => format!("{test} (DOES NOT MATCH the copy)"),
        },
    );
    log.field("AccurateRip v1", &track.accuraterip_crcs.v1.to_string());
    log.field("AccurateRip v2", &track.accuraterip_crcs.v2.to_string());
    log.field("AccurateRip", &accuraterip_text(&track.accuraterip));
    log.field("Read errors", &read_errors(track));
    match track.retried_sectors {
        0 => {}
        1 => log.field(
            "Re-read",
            "1 sector read wrong at first and came right on re-reading",
        ),
        n => log.field(
            "Re-read",
            &format!("{n} sectors read wrong at first and came right on re-reading"),
        ),
    }
    if let Some(isrc) = &track.isrc {
        log.field("ISRC", isrc.as_str());
    }
    if let Some(sha256) = &track.file_sha256 {
        log.field("File SHA-256", &sha256.to_hex());
    }
}

fn accuraterip_text(status: &AccurateRipStatus) -> String {
    match status {
        AccurateRipStatus::Accurate {
            version,
            confidence,
        } => format!(
            "Accurately ripped (confidence {confidence}, {} checksum)",
            match version {
                AccurateRipVersion::V1 => "v1",
                AccurateRipVersion::V2 => "v2",
            }
        ),
        AccurateRipStatus::Mismatch { best_confidence } => format!(
            "Mismatch (the database has other checksums, best confidence {best_confidence})"
        ),
        AccurateRipStatus::NotInDatabase | AccurateRipStatus::Unavailable { .. } => {
            status.summary()
        }
    }
}

fn read_errors(track: &TrackReport) -> String {
    let sectors = &track.unrecovered_sectors;
    if sectors.is_empty() {
        return "none".to_owned();
    }
    let list: Vec<String> = sectors.iter().map(ToString::to_string).collect();
    let noun = if sectors.len() == 1 {
        "sector"
    } else {
        "sectors"
    };
    format!(
        "{} {noun} could not be read exactly: {}",
        sectors.len(),
        list.join(", ")
    )
}

/// The overall verdict, as one short paragraph: read errors first, then
/// AccurateRip, then the CUETools database. Shown at the end of the log and
/// on the Done screen.
pub fn summary(report: &RipReport) -> String {
    let tracks = &report.tracks;
    let with_errors: Vec<TrackNumber> = tracks
        .iter()
        .filter(|t| {
            !t.unrecovered_sectors.is_empty() || t.test_crc.is_some_and(|test| test != t.copy_crc)
        })
        .map(|t| t.number)
        .collect();
    let mut text = if !with_errors.is_empty() {
        format!(
            "There were read errors in {}. {} may not be an exact copy; cleaning the disc and ripping again may help.",
            track_list(&with_errors),
            if with_errors.len() == 1 {
                "That track"
            } else {
                "Those tracks"
            }
        )
    } else {
        let numbers_where = |wanted: fn(&AccurateRipStatus) -> bool| -> Vec<TrackNumber> {
            tracks
                .iter()
                .filter(|t| wanted(&t.accuraterip))
                .map(|t| t.number)
                .collect()
        };
        let accurate = numbers_where(|s| matches!(s, AccurateRipStatus::Accurate { .. }));
        let mismatched = numbers_where(|s| matches!(s, AccurateRipStatus::Mismatch { .. }));
        let unavailable = numbers_where(|s| matches!(s, AccurateRipStatus::Unavailable { .. }));
        if !tracks.is_empty() && accurate.len() == tracks.len() {
            "All tracks accurately ripped.".to_owned()
        } else if !accurate.is_empty() {
            let mut text = format!(
                "{} of {} tracks accurately ripped.",
                accurate.len(),
                tracks.len()
            );
            if !mismatched.is_empty() {
                let _ = write!(
                    text,
                    " {} {} not match the AccurateRip database.",
                    capitalise(&track_list(&mismatched)),
                    if mismatched.len() == 1 { "does" } else { "do" }
                );
            }
            let unchecked = tracks.len() - accurate.len() - mismatched.len();
            if unchecked > 0 {
                let _ = write!(
                    text,
                    " {} could not be checked.",
                    if unchecked == 1 {
                        "One track"
                    } else {
                        "The other tracks"
                    }
                );
            }
            text
        } else if !mismatched.is_empty() {
            "All tracks read without errors, but they do not match the AccurateRip database (this may be a different pressing).".to_owned()
        } else if !unavailable.is_empty() {
            "All tracks read without errors. AccurateRip could not be checked.".to_owned()
        } else {
            "All tracks read without errors. The disc is not in the AccurateRip database."
                .to_owned()
        }
    };
    match &report.ctdb {
        CtdbStatus::Accurate { confidence, total } => {
            let _ = write!(
                text,
                " The CUETools database confirms the whole disc (confidence {confidence} of {total})."
            );
        }
        CtdbStatus::Mismatch { .. } => {
            text.push_str(" The CUETools database has this disc but does not match it.");
        }
        CtdbStatus::NotInDatabase | CtdbStatus::Unavailable { .. } => {}
    }
    text
}

/// "track 02", "tracks 02 and 05", "tracks 02, 03 and 05".
fn track_list(numbers: &[TrackNumber]) -> String {
    let names: Vec<String> = numbers.iter().map(ToString::to_string).collect();
    match names.as_slice() {
        [] => "no tracks".to_owned(),
        [one] => format!("track {one}"),
        [rest @ .., last] => format!("tracks {} and {last}", rest.join(", ")),
    }
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn or_unknown(text: &str) -> &str {
    if text.trim().is_empty() {
        "(unknown)"
    } else {
        text
    }
}

fn timestamp(at: jiff::Timestamp) -> String {
    at.strftime("%Y-%m-%d %H:%M:%S UTC").to_string()
}

/// A tiny line builder for the log's layout.
#[derive(Default)]
struct Log {
    text: String,
}

impl Log {
    fn raw(&mut self, line: &str) {
        self.text.push_str(line);
        self.text.push('\n');
    }

    fn blank(&mut self) {
        self.text.push('\n');
    }

    fn heading(&mut self, title: &str) {
        self.blank();
        self.raw(title);
    }

    fn field(&mut self, label: &str, value: &str) {
        let label = format!("{label}:");
        self.raw(&format!("  {label:<LABEL$}{value}"));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::core::discid;
    use crate::model::{
        AccurateRipCrc, AccurateRipCrcs, Crc32, DriveIdentity, Isrc, Lba, Mbid, Peak,
        ReleaseMetadata, SampleOffset, Sha256Digest, Toc, TrackEntry, TrackMetadata,
    };

    fn number(n: u8) -> TrackNumber {
        TrackNumber::new(n).unwrap()
    }

    /// A small, fully filled report used by the log and `rip.json` tests.
    pub(crate) fn sample_report() -> RipReport {
        let entries = vec![
            TrackEntry {
                number: number(1),
                start_lba: Lba(0),
                length_sectors: Sectors(15000),
                is_audio: true,
                pregap: Sectors(0),
                isrc: Some(Isrc::new("GBAYE0000351").unwrap()),
            },
            TrackEntry {
                number: number(2),
                start_lba: Lba(15000),
                length_sectors: Sectors(20000),
                is_audio: true,
                pregap: Sectors(150),
                isrc: None,
            },
        ];
        let toc = Toc::new(entries, Lba(35000)).unwrap();
        let disc_ids = discid::disc_ids(&toc).unwrap();
        let track = |n: u8, title: &str, start: u32, length: u32, pregap: u32| TrackReport {
            number: number(n),
            title: title.into(),
            start_lba: Lba(start),
            length_sectors: Sectors(length),
            pregap: Sectors(pregap),
            isrc: None,
            peak: Peak(32_112),
            copy_crc: Crc32(0x1234_ABCD),
            test_crc: Some(Crc32(0x1234_ABCD)),
            accuraterip_crcs: AccurateRipCrcs {
                v1: AccurateRipCrc(0x0BAD_F00D),
                v2: AccurateRipCrc(0xC0FF_EE00),
            },
            accuraterip: AccurateRipStatus::Accurate {
                version: AccurateRipVersion::V2,
                confidence: 12,
            },
            unrecovered_sectors: Vec::new(),
            retried_sectors: 0,
            file_name: format!("{n:02} - {title}.flac"),
            file_sha256: Some(Sha256Digest([n; 32])),
        };
        RipReport {
            app_version: "0.1.0".into(),
            started_at: "2026-11-19T10:00:00Z".parse().unwrap(),
            finished_at: "2026-11-19T10:21:30Z".parse().unwrap(),
            drive: DriveIdentity {
                vendor: "PLEXTOR".into(),
                model: "DVDR PX-760A".into(),
                revision: "1.07".into(),
            },
            read_offset: SampleOffset(30),
            offset_source: OffsetSource::AccurateRipDatabase,
            read_mode: ReadMode::TestAndCopy,
            toc,
            disc_ids,
            mcn: None,
            release: ReleaseMetadata {
                title: "Songs for Rainy Days".into(),
                artist: "The Quiet Readers".into(),
                release_mbid: Some(Mbid::new("b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d").unwrap()),
                year: Some(1999),
                date: Some("1999-04-01".into()),
                label: Some("Reading Room Records".into()),
                disc_number: 1,
                disc_total: 1,
                tracks: vec![TrackMetadata {
                    number: Some(number(1)),
                    title: "Opening".into(),
                    ..TrackMetadata::default()
                }],
                ..ReleaseMetadata::default()
            },
            tracks: vec![
                track(1, "Opening", 0, 15000, 0),
                track(2, "Middle", 15000, 20000, 150),
            ],
            ctdb: CtdbStatus::Accurate {
                confidence: 5,
                total: 7,
            },
        }
    }

    #[test]
    fn snapshot() {
        let report = sample_report();
        let ids = &report.disc_ids;
        let expected = format!(
            "\
Library Rip 0.1.0 - rip log
Source code: {source}

  Started:              2026-11-19 10:00:00 UTC
  Finished:             2026-11-19 10:21:30 UTC

Album
  Artist:               The Quiet Readers
  Title:                Songs for Rainy Days
  Released:             1999-04-01
  Label:                Reading Room Records
  MusicBrainz release:  b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d

Drive
  Drive:                PLEXTOR DVDR PX-760A (firmware 1.07)
  Read offset:          +30 samples (from the AccurateRip drive database)
  Read mode:            Test and copy (every track read securely twice and compared)

Disc
  MusicBrainz disc ID:  {mb}
  freedb disc ID:       {cddb:08X}
  AccurateRip disc ID:  {ar}
  CTDB TOC:             0:15000:35000
  Media catalogue no.:  none

Table of contents
  Track  Start     Length    Pre-gap   First sector  Last sector  Type
     01  00:00:00  03:20:00  00:00:00             0        14999  audio
     02  03:20:00  04:26:50  00:02:00         15000        34999  audio
  Lead-out at sector 35000

Track 01
  Title:                Opening
  File:                 01 - Opening.flac
  Peak level:           98.0 %
  Copy CRC:             1234ABCD
  Test CRC:             1234ABCD (matches the copy)
  AccurateRip v1:       0BADF00D
  AccurateRip v2:       C0FFEE00
  AccurateRip:          Accurately ripped (confidence 12, v2 checksum)
  Read errors:          none
  File SHA-256:         {sha1}

Track 02
  Title:                Middle
  File:                 02 - Middle.flac
  Pre-gap:              00:02:00
  Peak level:           98.0 %
  Copy CRC:             1234ABCD
  Test CRC:             1234ABCD (matches the copy)
  AccurateRip v1:       0BADF00D
  AccurateRip v2:       C0FFEE00
  AccurateRip:          Accurately ripped (confidence 12, v2 checksum)
  Read errors:          none
  File SHA-256:         {sha2}

CUETools database
  Whole disc:           Accurately ripped (confidence 5 of 7)

Summary
  All tracks accurately ripped. The CUETools database confirms the whole disc (confidence 5 of 7).

End of log
",
            source = crate::SOURCE_URL,
            mb = ids.musicbrainz,
            cddb = ids.cddb,
            ar = accuraterip::database_file_name(&ids.accuraterip),
            sha1 = "01".repeat(32),
            sha2 = "02".repeat(32),
        );
        let log = render(&report).unwrap();
        assert_eq!(log, expected);
        assert_eq!(render(&report).unwrap(), log, "deterministic");
    }

    #[test]
    fn corrected_sectors_are_recorded_but_are_not_errors() {
        let mut report = sample_report();
        report.tracks[0].retried_sectors = 1;
        report.tracks[1].retried_sectors = 3;
        let log = render(&report).unwrap();
        assert!(log.contains(
            "  Read errors:          none\n  Re-read:              1 sector read wrong at first and came right on re-reading\n"
        ));
        assert!(log.contains("  Re-read:              3 sectors read wrong at first"));
        assert!(log.contains("All tracks accurately ripped."));
        let json = crate::core::report_json::render(&report).unwrap();
        assert!(json.contains("\"retriedSectors\": 3"), "{json}");
        assert_eq!(crate::core::report_json::parse(&json).unwrap(), report);
    }

    #[test]
    fn errors_are_listed_and_summarised() {
        let mut report = sample_report();
        report.read_mode = ReadMode::Secure;
        report.tracks[0].test_crc = None;
        report.tracks[1].test_crc = None;
        report.tracks[1].unrecovered_sectors = vec![Lba(20_000), Lba(20_001)];
        let log = render(&report).unwrap();
        assert!(log.contains("  Test CRC:             not made (secure mode reads once)\n"));
        assert!(log.contains(
            "  Read errors:          2 sectors could not be read exactly: 20000, 20001\n"
        ));
        assert!(
            log.contains("There were read errors in track 02. That track may not be an exact copy")
        );

        report.tracks[1].unrecovered_sectors.clear();
        report.tracks[0].test_crc = Some(Crc32(1));
        let log = render(&report).unwrap();
        assert!(log.contains("00000001 (DOES NOT MATCH the copy)"));
        assert!(summary(&report).starts_with("There were read errors in track 01."));
    }

    fn with_statuses(statuses: [AccurateRipStatus; 2], ctdb: CtdbStatus) -> RipReport {
        let mut report = sample_report();
        for (track, status) in report.tracks.iter_mut().zip(statuses) {
            track.accuraterip = status;
        }
        report.ctdb = ctdb;
        report
    }

    #[test]
    fn summaries() {
        let accurate = AccurateRipStatus::Accurate {
            version: AccurateRipVersion::V1,
            confidence: 2,
        };
        let mismatch = AccurateRipStatus::Mismatch { best_confidence: 3 };
        let offline = AccurateRipStatus::Unavailable {
            reason: "no internet connection".into(),
        };
        let cases = [
            (
                [accurate.clone(), mismatch.clone()],
                CtdbStatus::NotInDatabase,
                "1 of 2 tracks accurately ripped. Track 02 does not match the AccurateRip database.",
            ),
            (
                [accurate.clone(), AccurateRipStatus::NotInDatabase],
                CtdbStatus::NotInDatabase,
                "1 of 2 tracks accurately ripped. One track could not be checked.",
            ),
            (
                [mismatch.clone(), mismatch.clone()],
                CtdbStatus::Mismatch { total: 4 },
                "All tracks read without errors, but they do not match the AccurateRip database (this may be a different pressing). The CUETools database has this disc but does not match it.",
            ),
            (
                [offline.clone(), offline.clone()],
                CtdbStatus::Unavailable {
                    reason: "no internet connection".into(),
                },
                "All tracks read without errors. AccurateRip could not be checked.",
            ),
            (
                [
                    AccurateRipStatus::NotInDatabase,
                    AccurateRipStatus::NotInDatabase,
                ],
                CtdbStatus::Accurate {
                    confidence: 1,
                    total: 1,
                },
                "All tracks read without errors. The disc is not in the AccurateRip database. The CUETools database confirms the whole disc (confidence 1 of 1).",
            ),
        ];
        for (statuses, ctdb, expected) in cases {
            assert_eq!(summary(&with_statuses(statuses, ctdb)), expected);
        }
    }

    #[test]
    fn track_lists_read_naturally() {
        assert_eq!(track_list(&[number(2)]), "track 02");
        assert_eq!(track_list(&[number(2), number(5)]), "tracks 02 and 05");
        assert_eq!(
            track_list(&[number(2), number(3), number(5)]),
            "tracks 02, 03 and 05"
        );
    }

    #[test]
    fn mismatch_and_unknown_drive_wording() {
        let mut report = with_statuses(
            [
                AccurateRipStatus::Mismatch { best_confidence: 7 },
                AccurateRipStatus::NotInDatabase,
            ],
            CtdbStatus::NotInDatabase,
        );
        report.offset_source = OffsetSource::Unknown;
        report.read_offset = SampleOffset(0);
        let log = render(&report).unwrap();
        assert!(log.contains("Mismatch (the database has other checksums, best confidence 7)"));
        assert!(log.contains("+0 samples (drive not recognised, no correction applied)"));
        assert!(log.contains("  AccurateRip:          Not in database\n"));
    }
}
