// SPDX-License-Identifier: AGPL-3.0-or-later
//! Cue sheet for the per-track FLAC files (pre-gaps, HTOA, ISRC, CD-Text).
//!
//! Layout: one `FILE` per track, with each track's pre-gap at the end of the
//! previous file (the way the tracks are cut: each runs up to the next
//! track's INDEX 01). So a pre-gap appears as `INDEX 00` inside the previous
//! `FILE`, and every track's `INDEX 01` is `00:00:00` of its own file. This
//! is the layout EAC calls "multiple files with gaps (noncompliant)"; it
//! recreates the disc exactly, gaps included.
//!
//! Hidden track one audio (track 1 starting after sector 0) is either its
//! own file (`INDEX 00` of track 1, when it was ripped) or a `PREGAP` of the
//! same length (when it was not, so the disc's timing is still recorded).
//!
//! Titles and performers come from the confirmed metadata, falling back to
//! the disc's CD-Text. `CATALOG` is the disc's MCN; `ISRC`s are those read
//! from the disc. Text is UTF-8 without a byte-order mark, lines end in CRLF,
//! and double quotes inside values become single quotes (cue sheets cannot
//! escape them).

use super::{CoreError, Result, discid};
use crate::model::{CdTextFields, DiscInfo, ReleaseMetadata, Sectors, TrackEntry, TrackNumber};

/// What the cue sheet describes.
#[derive(Clone, Debug)]
pub struct CueInput<'a> {
    /// Disc as read (TOC, MCN, CD-Text, ISRCs).
    pub disc: &'a DiscInfo,
    /// Confirmed metadata (titles, performers).
    pub release: &'a ReleaseMetadata,
    /// FLAC file names in track order (`FILE "…" WAVE` lines), one per
    /// audio track.
    pub file_names: &'a [String],
}

/// Renders the cue sheet text (CRLF line endings, UTF-8 with no BOM).
/// Hidden track one audio, if the disc has any, is recorded as a `PREGAP`.
pub fn render(input: &CueInput<'_>) -> Result<String> {
    render_with_htoa(input, None)
}

/// Like [`render`], with the file holding the ripped hidden track one audio
/// (`htoa_file`), which becomes track 1's `INDEX 00`. `None` writes a
/// `PREGAP` instead (or nothing, when the disc has no hidden audio).
pub fn render_with_htoa(input: &CueInput<'_>, htoa_file: Option<&str>) -> Result<String> {
    let disc = input.disc;
    let release = input.release;
    let toc = &disc.toc;
    let audio: Vec<&TrackEntry> = toc.audio_tracks().collect();
    if audio.len() != input.file_names.len() {
        return Err(CoreError::Invalid(format!(
            "the disc has {} audio tracks but {} file names were given",
            audio.len(),
            input.file_names.len()
        )));
    }
    let cd_text = disc.cd_text.as_ref();
    let album_text = cd_text.map(|t| &t.album);

    let mut lines: Vec<String> = Vec::new();
    if let Some(year) = release.year {
        lines.push(format!("REM DATE {year}"));
    }
    lines.push(format!("REM DISCID {:08X}", discid::cddb_disc_id(toc)?));
    lines.push(format!("REM COMMENT \"Library Rip {}\"", crate::VERSION));
    if let Some(mcn) = disc.mcn.as_ref().filter(|m| !m.is_blank()) {
        lines.push(format!("CATALOG {}", mcn.as_str()));
    }
    if let Some(performer) = pick(
        &release.artist,
        album_text.and_then(|t| t.performer.as_ref()),
    ) {
        lines.push(format!("PERFORMER \"{}\"", quote(performer)));
    }
    if let Some(title) = pick(&release.title, album_text.and_then(|t| t.title.as_ref())) {
        lines.push(format!("TITLE \"{}\"", quote(title)));
    }
    if let Some(songwriter) = album_text.and_then(|t| t.songwriter.as_deref()) {
        lines.push(format!("SONGWRITER \"{}\"", quote(songwriter)));
    }

    let first_on_disc = toc.first_track().map(|t| t.number);
    let mut previous: Option<&TrackEntry> = None;
    for (track, file_name) in audio.iter().zip(input.file_names) {
        let header = track_header(
            track,
            release,
            cd_text.and_then(|t| track_text(t, track.number)),
        );
        let file_line = format!("FILE \"{}\" WAVE", quote(file_name));
        match previous {
            None => {
                let hidden = Sectors(track.start_lba.0);
                let has_hidden_audio = Some(track.number) == first_on_disc && hidden.0 > 0;
                match (has_hidden_audio, htoa_file) {
                    (true, Some(htoa)) => {
                        lines.push(format!("FILE \"{}\" WAVE", quote(htoa)));
                        lines.extend(header);
                        lines.push("    INDEX 00 00:00:00".to_owned());
                        lines.push(file_line);
                    }
                    (true, None) => {
                        lines.push(file_line);
                        lines.extend(header);
                        lines.push(format!("    PREGAP {}", hidden.to_msf()));
                    }
                    (false, _) => {
                        lines.push(file_line);
                        lines.extend(header);
                    }
                }
            }
            Some(before) => {
                if track.pregap.0 > 0 {
                    let index00 = before
                        .length_sectors
                        .0
                        .checked_sub(track.pregap.0)
                        .ok_or_else(|| {
                            CoreError::Invalid(format!(
                                "track {}'s pre-gap is longer than track {}",
                                track.number, before.number
                            ))
                        })?;
                    lines.extend(header);
                    lines.push(format!("    INDEX 00 {}", Sectors(index00).to_msf()));
                    lines.push(file_line);
                } else {
                    lines.push(file_line);
                    lines.extend(header);
                }
            }
        }
        lines.push("    INDEX 01 00:00:00".to_owned());
        previous = Some(track);
    }

    let mut out = lines.join("\r\n");
    out.push_str("\r\n");
    Ok(out)
}

/// `TRACK nn AUDIO` and the track's TITLE, PERFORMER, SONGWRITER and ISRC.
fn track_header(
    track: &TrackEntry,
    release: &ReleaseMetadata,
    text: Option<&CdTextFields>,
) -> Vec<String> {
    let metadata = release
        .tracks
        .iter()
        .find(|t| t.number == Some(track.number));
    let mut lines = vec![format!("  TRACK {} AUDIO", track.number)];
    let title = metadata.map(|m| m.title.as_str()).unwrap_or_default();
    if let Some(title) = pick(title, text.and_then(|t| t.title.as_ref())) {
        lines.push(format!("    TITLE \"{}\"", quote(title)));
    }
    let artist = metadata.map(|m| m.artist.as_str()).unwrap_or_default();
    if let Some(performer) = pick(artist, text.and_then(|t| t.performer.as_ref())) {
        lines.push(format!("    PERFORMER \"{}\"", quote(performer)));
    }
    if let Some(songwriter) = text.and_then(|t| t.songwriter.as_deref()) {
        lines.push(format!("    SONGWRITER \"{}\"", quote(songwriter)));
    }
    if let Some(isrc) = &track.isrc {
        lines.push(format!("    ISRC {}", isrc.as_str()));
    }
    lines
}

fn track_text(text: &crate::model::CdText, number: TrackNumber) -> Option<&CdTextFields> {
    text.tracks
        .iter()
        .find(|t| t.number == number)
        .map(|t| &t.fields)
}

/// The confirmed value when it is not blank, else the CD-Text one.
fn pick<'a>(confirmed: &'a str, fallback: Option<&'a String>) -> Option<&'a str> {
    let confirmed = confirmed.trim();
    if !confirmed.is_empty() {
        return Some(confirmed);
    }
    fallback.map(|f| f.trim()).filter(|f| !f.is_empty())
}

/// A value safe inside cue-sheet double quotes.
fn quote(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '"' => '\'',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CdText, CdTextTrack, DriveIdentity, Isrc, Lba, Mcn, Toc, TrackMetadata};

    fn number(n: u8) -> TrackNumber {
        TrackNumber::new(n).unwrap()
    }

    fn entry(n: u8, start: u32, length: u32, pregap: u32) -> TrackEntry {
        TrackEntry {
            number: number(n),
            start_lba: Lba(start),
            length_sectors: Sectors(length),
            is_audio: true,
            pregap: Sectors(pregap),
            isrc: None,
        }
    }

    fn disc(tracks: Vec<TrackEntry>) -> DiscInfo {
        let lead_out = tracks.last().map(|t| t.end_lba().unwrap()).unwrap();
        DiscInfo {
            drive: DriveIdentity::default(),
            toc: Toc::new(tracks, lead_out).unwrap(),
            mcn: None,
            cd_text: None,
        }
    }

    fn release() -> ReleaseMetadata {
        ReleaseMetadata {
            title: "Songs for \"Rainy\" Days".into(),
            artist: "The Quiet Readers".into(),
            year: Some(1999),
            disc_number: 1,
            disc_total: 1,
            tracks: vec![
                TrackMetadata {
                    number: Some(number(1)),
                    title: "Opening".into(),
                    artist: "The Quiet Readers".into(),
                    ..TrackMetadata::default()
                },
                TrackMetadata {
                    number: Some(number(2)),
                    title: "Middle".into(),
                    artist: "The Quiet Readers feat. Ada".into(),
                    ..TrackMetadata::default()
                },
            ],
            ..ReleaseMetadata::default()
        }
    }

    fn names(count: usize) -> Vec<String> {
        (1..=count)
            .map(|n| format!("{n:02} - Track.flac"))
            .collect()
    }

    #[test]
    fn full_sheet_snapshot() {
        let mut tracks = vec![
            entry(1, 0, 15000, 0),
            entry(2, 15000, 20000, 150),
            entry(3, 35000, 10000, 0),
        ];
        tracks[0].isrc = Some(Isrc::new("GBAYE0000351").unwrap());
        let mut disc = disc(tracks);
        disc.mcn = Some(Mcn::new("0123456789012").unwrap());
        disc.cd_text = Some(CdText {
            album: CdTextFields {
                songwriter: Some("A. Writer".into()),
                ..CdTextFields::default()
            },
            tracks: vec![CdTextTrack {
                number: number(3),
                fields: CdTextFields {
                    title: Some("Closing (from CD-Text)".into()),
                    ..CdTextFields::default()
                },
            }],
        });
        let release = release();
        let file_names = names(3);
        let text = render(&CueInput {
            disc: &disc,
            release: &release,
            file_names: &file_names,
        })
        .unwrap();
        let expected = [
            "REM DATE 1999",
            &format!(
                "REM DISCID {:08X}",
                discid::cddb_disc_id(&disc.toc).unwrap()
            ),
            &format!("REM COMMENT \"Library Rip {}\"", crate::VERSION),
            "CATALOG 0123456789012",
            "PERFORMER \"The Quiet Readers\"",
            "TITLE \"Songs for 'Rainy' Days\"",
            "SONGWRITER \"A. Writer\"",
            "FILE \"01 - Track.flac\" WAVE",
            "  TRACK 01 AUDIO",
            "    TITLE \"Opening\"",
            "    PERFORMER \"The Quiet Readers\"",
            "    ISRC GBAYE0000351",
            "    INDEX 01 00:00:00",
            "  TRACK 02 AUDIO",
            "    TITLE \"Middle\"",
            "    PERFORMER \"The Quiet Readers feat. Ada\"",
            "    INDEX 00 03:18:00",
            "FILE \"02 - Track.flac\" WAVE",
            "    INDEX 01 00:00:00",
            "FILE \"03 - Track.flac\" WAVE",
            "  TRACK 03 AUDIO",
            "    TITLE \"Closing (from CD-Text)\"",
            "    INDEX 01 00:00:00",
            "",
        ]
        .join("\r\n");
        assert_eq!(text, expected);
        assert!(!text.starts_with('\u{feff}'));
    }

    #[test]
    fn hidden_track_one_audio() {
        let disc = disc(vec![entry(1, 3000, 15000, 3000), entry(2, 18000, 9000, 0)]);
        let release = ReleaseMetadata::default();
        let file_names = names(2);
        let input = CueInput {
            disc: &disc,
            release: &release,
            file_names: &file_names,
        };
        let with_file = render_with_htoa(&input, Some("00 - Hidden track.flac")).unwrap();
        assert!(with_file.contains(
            "FILE \"00 - Hidden track.flac\" WAVE\r\n  TRACK 01 AUDIO\r\n    INDEX 00 00:00:00\r\nFILE \"01 - Track.flac\" WAVE\r\n    INDEX 01 00:00:00\r\n"
        ), "{with_file}");
        assert!(!with_file.contains("PERFORMER"));

        let without = render(&input).unwrap();
        assert!(without.contains(
            "FILE \"01 - Track.flac\" WAVE\r\n  TRACK 01 AUDIO\r\n    PREGAP 00:40:00\r\n    INDEX 01 00:00:00\r\n"
        ), "{without}");
    }

    #[test]
    fn data_tracks_are_left_out() {
        let mut tracks = vec![entry(1, 0, 15000, 0), entry(2, 15000, 20000, 0)];
        tracks[1].is_audio = false;
        let disc = disc(tracks);
        let release = ReleaseMetadata::default();
        let file_names = names(1);
        let text = render(&CueInput {
            disc: &disc,
            release: &release,
            file_names: &file_names,
        })
        .unwrap();
        assert!(!text.contains("TRACK 02"));
        assert_eq!(text.matches("FILE").count(), 1);
    }

    #[test]
    fn bad_input_is_refused() {
        let disc = disc(vec![entry(1, 0, 100, 0), entry(2, 100, 100, 150)]);
        let release = ReleaseMetadata::default();
        let file_names = names(2);
        let input = CueInput {
            disc: &disc,
            release: &release,
            file_names: &file_names,
        };
        assert!(render(&input).is_err());
        let one = names(1);
        assert!(
            render(&CueInput {
                file_names: &one,
                ..input
            })
            .is_err()
        );
    }

    #[test]
    fn quotes_and_control_characters() {
        assert_eq!(quote("a \"b\"\nc"), "a 'b' c");
    }
}
