// SPDX-License-Identifier: AGPL-3.0-or-later
//! Real drives through libcdio + libcdio-paranoia (feature `cdio`).
//!
//! All `unsafe` code is in [`ffi`]; this module turns what libcdio reports
//! into the shared model types. The conversions are plain functions so they
//! are tested without a drive.
//!
//! Known limit: on enhanced CDs (audio session followed by a data session)
//! the last audio track's length runs to the data track's start, as
//! [`TrackEntry::length_sectors`] defines it, so it includes the 11,400
//! sector gap between sessions. That gap cannot be read as audio; rippers
//! normally stop 11,400 sectors early. The TOC model has no way to express
//! a gap yet.

mod ffi;

use std::ffi::c_int;

use super::{
    DiscDrive, DriveDescriptor, DriveError, DriveProvider, ParanoiaMode, Result, SectorRead,
};
use crate::model::{
    CdText, CdTextFields, CdTextTrack, DiscInfo, DriveIdentity, Isrc, Lba, Mcn, Sectors, Toc,
    TrackEntry, TrackNumber,
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
        let toc = toc_from_raw(&raw_tracks, lead_out)?;
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

/// Turns libcdio's track list into a checked [`Toc`].
///
/// Track 1's pre-gap is everything before it (hidden track one audio when
/// non-zero); later pre-gaps come from INDEX 00 when the driver reports it.
fn toc_from_raw(tracks: &[RawTrack], lead_out: i32) -> Result<Toc> {
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
    let mut entries = Vec::with_capacity(tracks.len());
    for (index, raw) in tracks.iter().enumerate() {
        let start = lba(raw.lsn)?;
        let end = match tracks.get(index + 1) {
            Some(next) => lba(next.lsn)?,
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
        });
    }
    Toc::new(entries, lead_out).map_err(|error| bad(error.to_string()))
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
        let toc = toc_from_raw(&tracks, 45_000).unwrap();
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
        let toc = toc_from_raw(&[raw(1, 3_000, true)], 20_000).unwrap();
        assert_eq!(toc.tracks()[0].pregap, Sectors(3_000));
        assert_eq!(toc.tracks()[0].start_lba, Lba(3_000));
    }

    #[test]
    fn keeps_data_tracks_marked() {
        let toc = toc_from_raw(&[raw(1, 0, true), raw(2, 30_000, false)], 50_000).unwrap();
        assert!(toc.tracks()[0].is_audio);
        assert!(!toc.tracks()[1].is_audio);
        assert_eq!(toc.audio_tracks().count(), 1);
    }

    #[test]
    fn rejects_nonsense() {
        assert!(toc_from_raw(&[raw(1, -5, true)], 100).is_err());
        assert!(toc_from_raw(&[raw(1, 0, true), raw(2, 50, true)], 40).is_err());
        assert!(toc_from_raw(&[raw(1, 100, true), raw(2, 50, true)], 400).is_err());
        assert!(toc_from_raw(&[raw(0, 0, true)], 100).is_err());
        assert!(toc_from_raw(&[], 100).is_err());
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
