// SPDX-License-Identifier: AGPL-3.0-or-later
//! Real drives through libcdio + libcdio-paranoia (feature `cdio`).
//!
//! All `unsafe` code is in [`ffi`]; this module turns what libcdio reports
//! into the shared model types. The conversions are plain functions so they
//! are tested without a drive.
//!
//! **Sessions.** On an enhanced CD (an audio session followed by a data
//! session) the last audio track ends at the audio session's lead-out, not
//! at the data track: between them lie the lead-out, the next lead-in and
//! the data track's pre-gap, which cannot be read as audio. Where the
//! audio session ends is taken, in order of preference, from:
//!
//! 1. the drive's full TOC (MMC READ TOC format 2): each track's session and
//!    each session's lead-out (point A2);
//! 2. libcdio's `cdio_get_last_session` (where the last session starts),
//!    with the audio ending [`SESSION_GAP_SECTORS`] before it;
//! 3. neither: data tracks after the audio are taken to be a second
//!    session and the same 11,400-sector rule applies, as libdiscid and
//!    whipper do.
//!
//! A full TOC that disagrees with libcdio's track list is not trusted.

mod ffi;

use std::ffi::c_int;

use super::{
    DiscDrive, DriveDescriptor, DriveError, DriveProvider, ParanoiaMode, Result, SectorRead,
};
use crate::model::{
    CdText, CdTextFields, CdTextTrack, DiscInfo, DriveIdentity, Isrc, Lba, MSF_OFFSET_SECTORS, Mcn,
    SECTORS_PER_SECOND, SESSION_GAP_SECTORS, Sectors, SessionNumber, Toc, TrackEntry, TrackNumber,
};
use ffi::{RawCdTextFields, RawTrack};

/// How many times paranoia re-reads one bad sector before reporting it.
pub const MAX_RETRIES: u32 = 20;

/// Lists and opens the computer's optical drives.
#[derive(Clone, Copy, Debug, Default)]
pub struct CdioProvider;

impl DriveProvider for CdioProvider {
    fn list(&self) -> Result<Vec<DriveDescriptor>> {
        Ok(ffi::list_devices()
            .into_iter()
            .map(|device| {
                let hardware = ffi::Device::open(&device)
                    .ok()
                    .and_then(|opened| opened.hardware());
                let label = drive_label(&device, hardware.as_ref());
                DriveDescriptor { device, label }
            })
            .collect())
    }

    fn open(&self, device: &str) -> Result<Box<dyn DiscDrive>> {
        Ok(Box::new(CdioDrive::open(device)?))
    }
}

/// "VENDOR MODEL (/dev/sr0)", or just the path when the drive is silent.
fn drive_label(device: &str, hardware: Option<&(String, String, String)>) -> String {
    match hardware {
        Some((vendor, model, _)) if !(vendor.is_empty() && model.is_empty()) => {
            format!("{} ({device})", format!("{vendor} {model}").trim())
        }
        _ => format!("Optical drive ({device})"),
    }
}

/// One opened optical drive.
#[derive(Debug)]
pub struct CdioDrive {
    device: String,
    handle: ffi::Device,
}

impl CdioDrive {
    /// Opens `/dev/sr0`, `D:` and so on.
    pub fn open(device: &str) -> Result<Self> {
        let handle = ffi::Device::open(device).map_err(|reason| DriveError::Open {
            device: device.to_owned(),
            reason,
        })?;
        Ok(CdioDrive {
            device: device.to_owned(),
            handle,
        })
    }

    /// The device path this drive was opened from.
    pub fn device(&self) -> &str {
        &self.device
    }

    fn read_error(lba: Lba, reason: String) -> DriveError {
        DriveError::Read { lba, reason }
    }
}

impl DiscDrive for CdioDrive {
    fn disc_info(&mut self) -> Result<DiscInfo> {
        let (raw_tracks, lead_out) = self
            .handle
            .tracks()
            .map_err(|reason| Self::read_error(Lba(0), reason))?
            .ok_or(DriveError::NoDisc)?;
        let sessions = match self
            .handle
            .full_toc()
            .and_then(|bytes| parse_full_toc(&bytes))
        {
            Some(full) => Sessions::Full(full),
            None => match self.handle.last_session_start() {
                Some(start) => Sessions::LastSessionStart(start),
                None => Sessions::Unknown,
            },
        };
        let toc = toc_from_raw(&raw_tracks, lead_out, &sessions)?;
        if toc.audio_tracks().next().is_none() {
            return Err(DriveError::NoAudio);
        }
        let drive = drive_identity(self.handle.hardware());
        let mcn = self.handle.mcn().and_then(|text| mcn_from_raw(&text));
        let numbers: Vec<u8> = raw_tracks.iter().map(|track| track.number).collect();
        let cd_text = self
            .handle
            .cd_text(&numbers)
            .and_then(|(album, tracks)| cd_text_from_raw(album, tracks));
        Ok(DiscInfo {
            drive,
            toc,
            mcn,
            cd_text,
        })
    }

    fn read_sectors(
        &mut self,
        start: Lba,
        count: Sectors,
        mode: ParanoiaMode,
    ) -> Result<SectorRead> {
        let lsn = i32::try_from(start.0)
            .map_err(|_| Self::read_error(start, "the position is past any disc".into()))?;
        let full = match mode {
            ParanoiaMode::Full => true,
            ParanoiaMode::Off => false,
        };
        let retries = c_int::try_from(MAX_RETRIES).unwrap_or(c_int::MAX);
        let raw = self
            .handle
            .read_audio(lsn, count.0, full, retries)
            .map_err(|reason| Self::read_error(start, reason))?;
        Ok(SectorRead {
            data: raw.data,
            unrecovered: raw
                .skipped
                .into_iter()
                .filter_map(|index| start.0.checked_add(index).map(Lba))
                .collect(),
            retried_sectors: raw.corrected,
        })
    }

    fn eject(&mut self) -> Result<()> {
        self.handle.eject().map_err(|reason| DriveError::Open {
            device: self.device.clone(),
            reason,
        })
    }
}

/// Builds the drive identity, with placeholders when INQUIRY failed.
fn drive_identity(hardware: Option<(String, String, String)>) -> DriveIdentity {
    let (vendor, model, revision) = hardware.unwrap_or_default();
    DriveIdentity {
        vendor: if vendor.is_empty() {
            "Unknown".into()
        } else {
            vendor
        },
        model: if model.is_empty() {
            "drive".into()
        } else {
            model
        },
        revision,
    }
}

/// What the drive says about sessions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Sessions {
    /// The full TOC.
    Full(FullToc),
    /// Only where the last session starts (its first track's LSN).
    LastSessionStart(i32),
    /// Nothing.
    #[default]
    Unknown,
}

/// The parts of a full TOC (READ TOC format 2) that matter here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FullToc {
    /// (track number, session, start LSN) for every track.
    tracks: Vec<(u8, u8, i32)>,
    /// (session, lead-out LSN) for every session.
    lead_outs: Vec<(u8, i32)>,
}

/// Parses the answer to READ TOC format 2: a 4-byte header (data length,
/// first and last session) and 11-byte descriptors (session, ADR/control,
/// TNO, POINT, MIN, SEC, FRAME, zero, PMIN, PSEC, PFRAME). Points 1–99 give
/// a track's start, point A2 a session's lead-out, both as PMIN:PSEC:PFRAME
/// (binary, including the 150-sector lead-in). `None` when it makes no
/// sense.
fn parse_full_toc(bytes: &[u8]) -> Option<FullToc> {
    let (header, rest) = bytes.split_first_chunk::<4>()?;
    let [length_high, length_low, _, _] = *header;
    // The length counts the bytes after itself.
    let length = usize::from(u16::from_be_bytes([length_high, length_low])).checked_sub(2)?;
    let body = rest.get(..length)?;
    let (descriptors, _) = body.as_chunks::<11>();
    let mut full = FullToc::default();
    for descriptor in descriptors {
        let [
            session,
            adr_control,
            _,
            point,
            _,
            _,
            _,
            _,
            minutes,
            seconds,
            frames,
        ] = *descriptor;
        if adr_control >> 4 != 1 || seconds > 59 || frames > 74 {
            continue;
        }
        let msf =
            (u32::from(minutes) * 60 + u32::from(seconds)) * SECTORS_PER_SECOND + u32::from(frames);
        let lsn = i32::try_from(msf).ok()? - i32::try_from(MSF_OFFSET_SECTORS).ok()?;
        match point {
            1..=99 => full.tracks.push((point, session, lsn)),
            0xA2 => full.lead_outs.push((session, lsn)),
            _ => {}
        }
    }
    (!full.tracks.is_empty()).then_some(full)
}

/// Each track's session number, from what the drive said, or `None` when
/// that is unusable (and every track is then taken to be in session 1).
fn track_sessions(tracks: &[RawTrack], sessions: &Sessions) -> Option<Vec<u8>> {
    match sessions {
        Sessions::Full(full) => tracks
            .iter()
            .map(|raw| {
                full.tracks
                    .iter()
                    .find(|(number, _, lsn)| *number == raw.number && *lsn == raw.lsn)
                    .map(|(_, session, _)| *session)
            })
            .collect(),
        Sessions::LastSessionStart(start) => {
            let later = tracks.iter().filter(|t| t.lsn >= *start).count();
            (later > 0 && later < tracks.len()).then(|| {
                tracks
                    .iter()
                    .map(|t| if t.lsn >= *start { 2 } else { 1 })
                    .collect()
            })
        }
        Sessions::Unknown => None,
    }
}

/// Turns libcdio's track list into a checked [`Toc`], with sessions.
///
/// Track 1's pre-gap is everything before it (hidden track one audio when
/// non-zero); later pre-gaps come from INDEX 00 when the driver reports it.
/// The last track of a session ends at that session's lead-out: from the
/// full TOC when the drive gave one, else [`SESSION_GAP_SECTORS`] before
/// the next session's first track.
fn toc_from_raw(tracks: &[RawTrack], lead_out: i32, sessions: &Sessions) -> Result<Toc> {
    let bad = |reason: String| DriveError::Read {
        lba: Lba(0),
        reason,
    };
    let lba = |lsn: i32| {
        u32::try_from(lsn).map(Lba).map_err(|_| {
            bad(format!(
                "the disc lists a position before its start ({lsn})"
            ))
        })
    };
    let lead_out = lba(lead_out)?;
    let numbers = track_sessions(tracks, sessions);
    let session_of = |index: usize| -> Result<SessionNumber> {
        let number = numbers
            .as_ref()
            .and_then(|n| n.get(index).copied())
            .unwrap_or(1);
        SessionNumber::new(number).map_err(|error| bad(error.to_string()))
    };
    let mut entries = Vec::with_capacity(tracks.len());
    for (index, raw) in tracks.iter().enumerate() {
        let start = lba(raw.lsn)?;
        let session = session_of(index)?;
        let end = match tracks.get(index + 1) {
            Some(next) if session_of(index + 1)? == session => lba(next.lsn)?,
            Some(next) => {
                let next_start = lba(next.lsn)?;
                let reported = match sessions {
                    Sessions::Full(full) => full
                        .lead_outs
                        .iter()
                        .find(|(number, _)| *number == session.get())
                        .and_then(|(_, lsn)| u32::try_from(*lsn).ok())
                        .map(Lba)
                        .filter(|end| *end > start && *end < next_start),
                    Sessions::LastSessionStart(_) | Sessions::Unknown => None,
                };
                match reported {
                    Some(end) => end,
                    None => {
                        Lba(next_start
                            .0
                            .checked_sub(SESSION_GAP_SECTORS)
                            .ok_or_else(|| {
                                bad(format!(
                                    "session {} starts too early",
                                    session_of(index + 1).map_or(0, SessionNumber::get)
                                ))
                            })?)
                    }
                }
            }
            None => lead_out,
        };
        let length = end.sectors_since(start).ok_or_else(|| {
            bad(format!(
                "track {} starts after the next one (sector {} > {})",
                raw.number, start, end
            ))
        })?;
        let pregap = if index == 0 {
            Sectors(start.0)
        } else {
            raw.pregap_lsn
                .and_then(|pregap| u32::try_from(pregap).ok())
                .and_then(|pregap| start.sectors_since(Lba(pregap)))
                .unwrap_or_default()
        };
        let number = TrackNumber::new(raw.number).map_err(|error| bad(error.to_string()))?;
        entries.push(TrackEntry {
            number,
            start_lba: start,
            length_sectors: length,
            is_audio: raw.is_audio,
            pregap,
            isrc: raw.isrc.as_deref().and_then(isrc_from_raw),
            session,
        });
    }
    let toc = if numbers.is_some() {
        Toc::new(entries, lead_out)
    } else {
        Toc::new_assuming_enhanced_gap(entries, lead_out)
    };
    toc.map_err(|error| bad(error.to_string()))
}

/// A checked ISRC, ignoring the all-zero "none" value some discs carry.
fn isrc_from_raw(text: &str) -> Option<Isrc> {
    Isrc::new(text)
        .ok()
        .filter(|isrc| isrc.as_str().chars().any(|c| c != '0'))
}

/// A checked, non-blank MCN.
fn mcn_from_raw(text: &str) -> Option<Mcn> {
    Mcn::new(text).ok().filter(|mcn| !mcn.is_blank())
}

fn fields_from_raw(raw: RawCdTextFields) -> CdTextFields {
    CdTextFields {
        title: raw.title,
        performer: raw.performer,
        songwriter: raw.songwriter,
        composer: raw.composer,
        arranger: raw.arranger,
        message: raw.message,
    }
}

/// CD-Text in model form, or `None` when the disc has no strings at all.
fn cd_text_from_raw(album: RawCdTextFields, tracks: Vec<(u8, RawCdTextFields)>) -> Option<CdText> {
    let empty = CdTextFields::default();
    let album = fields_from_raw(album);
    let tracks: Vec<CdTextTrack> = tracks
        .into_iter()
        .filter_map(|(number, raw)| {
            let fields = fields_from_raw(raw);
            let number = TrackNumber::new(number).ok()?;
            (fields != empty).then_some(CdTextTrack { number, fields })
        })
        .collect();
    (album != empty || !tracks.is_empty()).then_some(CdText { album, tracks })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(number: u8, lsn: i32, is_audio: bool) -> RawTrack {
        RawTrack {
            number,
            lsn,
            pregap_lsn: None,
            is_audio,
            isrc: None,
        }
    }

    #[test]
    fn converts_a_plain_audio_toc() {
        let mut tracks = vec![raw(1, 0, true), raw(2, 15_000, true), raw(3, 30_500, true)];
        tracks[1].pregap_lsn = Some(14_850);
        tracks[2].isrc = Some("GBAYE0000351".into());
        let toc = toc_from_raw(&tracks, 45_000, &Sessions::Unknown).unwrap();
        let entries = toc.tracks();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].length_sectors, Sectors(15_000));
        assert_eq!(entries[0].pregap, Sectors(0));
        assert_eq!(entries[1].pregap, Sectors(150));
        assert_eq!(entries[2].length_sectors, Sectors(14_500));
        assert_eq!(entries[2].isrc.as_ref().unwrap().as_str(), "GBAYE0000351");
        assert_eq!(toc.lead_out(), Lba(45_000));
    }

    #[test]
    fn hidden_track_one_audio_becomes_a_pregap() {
        let toc = toc_from_raw(&[raw(1, 3_000, true)], 20_000, &Sessions::Unknown).unwrap();
        assert_eq!(toc.tracks()[0].pregap, Sectors(3_000));
        assert_eq!(toc.tracks()[0].start_lba, Lba(3_000));
    }

    #[test]
    fn keeps_data_tracks_marked() {
        let toc = toc_from_raw(
            &[raw(1, 0, true), raw(2, 30_000, false)],
            50_000,
            &Sessions::Unknown,
        )
        .unwrap();
        assert!(toc.tracks()[0].is_audio);
        assert!(!toc.tracks()[1].is_audio);
        assert_eq!(toc.audio_tracks().count(), 1);
        // With nothing known about sessions, a trailing data track is an
        // enhanced CD's second session: the audio ends 11,400 sectors early.
        assert_eq!(toc.audio_end(), Some(Lba(30_000 - 11_400)));
        assert_eq!(toc.tracks()[1].session.get(), 2);
    }

    /// A full TOC answer for an enhanced CD: two audio tracks in session 1
    /// (lead-out at LSN 18_500), a data track in session 2 from 30_000,
    /// disc lead-out 50_000.
    fn full_toc_answer() -> Vec<u8> {
        let msf = |lsn: u32| {
            let total = lsn + 150;
            [
                (total / 75 / 60) as u8,
                (total / 75 % 60) as u8,
                (total % 75) as u8,
            ]
        };
        let descriptor = |session: u8, point: u8, lsn: u32| {
            let [m, s, f] = msf(lsn);
            [session, 0x10, 0, point, 0, 0, 0, 0, m, s, f]
        };
        let mut body = Vec::new();
        // A0/A1 (first and last track) carry track numbers, not addresses.
        body.extend([1, 0x10, 0, 0xA0, 0, 0, 0, 0, 1, 0, 0]);
        body.extend([1, 0x10, 0, 0xA1, 0, 0, 0, 0, 2, 0, 0]);
        body.extend(descriptor(1, 0xA2, 18_500));
        body.extend(descriptor(1, 1, 0));
        body.extend(descriptor(1, 2, 9_000));
        body.extend(descriptor(2, 0xA2, 50_000));
        body.extend(descriptor(2, 3, 30_000));
        // Mode-5 (ADR 5) pointers to the next session are skipped.
        body.extend([1, 0x50, 0, 0xB0, 0, 0, 0, 0, 0, 0, 0]);
        let mut answer = ((body.len() + 2) as u16).to_be_bytes().to_vec();
        answer.extend([1, 2]);
        answer.extend(body);
        answer.resize(1544, 0);
        answer
    }

    fn enhanced_tracks() -> Vec<RawTrack> {
        vec![raw(1, 0, true), raw(2, 9_000, true), raw(3, 30_000, false)]
    }

    #[test]
    fn the_full_toc_gives_sessions_and_the_audio_session_lead_out() {
        let full = parse_full_toc(&full_toc_answer()).unwrap();
        assert_eq!(full.tracks, vec![(1, 1, 0), (2, 1, 9_000), (3, 2, 30_000)]);
        assert_eq!(full.lead_outs, vec![(1, 18_500), (2, 50_000)]);
        let toc = toc_from_raw(&enhanced_tracks(), 50_000, &Sessions::Full(full)).unwrap();
        // The session's own lead-out (18_500), not the 11,400 rule (18_600).
        assert_eq!(toc.audio_end(), Some(Lba(18_500)));
        assert_eq!(toc.tracks()[1].length_sectors, Sectors(9_500));
        assert_eq!(toc.tracks()[2].session.get(), 2);
        assert_eq!(toc.tracks()[2].length_sectors, Sectors(20_000));
        assert!(parse_full_toc(&[0, 2, 1, 1]).is_none());
        assert!(parse_full_toc(&[0, 200, 1, 1, 0]).is_none());
    }

    #[test]
    fn a_full_toc_that_disagrees_is_not_trusted() {
        let mut full = parse_full_toc(&full_toc_answer()).unwrap();
        full.tracks[2].2 = 31_000;
        let toc = toc_from_raw(&enhanced_tracks(), 50_000, &Sessions::Full(full)).unwrap();
        // Falls back to the 11,400 rule.
        assert_eq!(toc.audio_end(), Some(Lba(18_600)));
        assert_eq!(toc.tracks()[2].session.get(), 2);
    }

    #[test]
    fn the_last_session_start_splits_the_sessions() {
        let toc = toc_from_raw(
            &enhanced_tracks(),
            50_000,
            &Sessions::LastSessionStart(30_000),
        )
        .unwrap();
        assert_eq!(toc.audio_end(), Some(Lba(18_600)));
        assert_eq!(toc.tracks()[2].session.get(), 2);
        // A single-session disc reports its first track: no split.
        let plain = toc_from_raw(
            &[raw(1, 3_000, true), raw(2, 9_000, true)],
            20_000,
            &Sessions::LastSessionStart(3_000),
        )
        .unwrap();
        assert_eq!(plain.session_count(), 1);
        assert_eq!(plain.audio_end(), Some(Lba(20_000)));
    }

    #[test]
    fn rejects_nonsense() {
        assert!(toc_from_raw(&[raw(1, -5, true)], 100, &Sessions::Unknown).is_err());
        assert!(
            toc_from_raw(&[raw(1, 0, true), raw(2, 50, true)], 40, &Sessions::Unknown).is_err()
        );
        assert!(
            toc_from_raw(
                &[raw(1, 100, true), raw(2, 50, true)],
                400,
                &Sessions::Unknown
            )
            .is_err()
        );
        assert!(toc_from_raw(&[raw(0, 0, true)], 100, &Sessions::Unknown).is_err());
        assert!(toc_from_raw(&[], 100, &Sessions::Unknown).is_err());
    }

    #[test]
    fn ignores_blank_identifiers() {
        assert!(isrc_from_raw("000000000000").is_none());
        assert!(isrc_from_raw("garbage").is_none());
        assert!(isrc_from_raw("USRC17607839").is_some());
        assert!(mcn_from_raw("0000000000000").is_none());
        assert!(mcn_from_raw("0724384960650").is_some());
    }

    #[test]
    fn cd_text_keeps_only_what_is_there() {
        assert!(
            cd_text_from_raw(RawCdTextFields::default(), vec![(1, Default::default())]).is_none()
        );
        let album = RawCdTextFields {
            title: Some("Kind of Blue".into()),
            performer: Some("Miles Davis".into()),
            ..Default::default()
        };
        let track = RawCdTextFields {
            title: Some("So What".into()),
            ..Default::default()
        };
        let text = cd_text_from_raw(album, vec![(1, track), (2, Default::default())]).unwrap();
        assert_eq!(text.album.title.as_deref(), Some("Kind of Blue"));
        assert_eq!(text.tracks.len(), 1);
        assert_eq!(text.tracks[0].fields.title.as_deref(), Some("So What"));
    }

    #[test]
    fn labels_and_identities() {
        let hardware = (
            "PLEXTOR".to_owned(),
            "DVDR PX-760A".to_owned(),
            "1.07".to_owned(),
        );
        assert_eq!(
            drive_label("/dev/sr0", Some(&hardware)),
            "PLEXTOR DVDR PX-760A (/dev/sr0)"
        );
        assert_eq!(drive_label("D:", None), "Optical drive (D:)");
        let identity = drive_identity(None);
        assert_eq!(identity.vendor, "Unknown");
        assert_eq!(drive_identity(Some(hardware)).revision, "1.07");
    }

    #[test]
    fn opening_a_missing_drive_explains_why() {
        let error = CdioDrive::open("/nonexistent/sr9").unwrap_err();
        assert!(matches!(error, DriveError::Open { .. }));
    }
}
