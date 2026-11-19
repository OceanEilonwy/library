// SPDX-License-Identifier: AGPL-3.0-or-later
//! Drive read offsets: the AccurateRip drive database and applying an
//! offset sample-exactly.
//!
//! **Convention.** A [`SampleOffset`] of `+n` means the drive delivers each
//! sample `n` positions early, so the reference sample `q` is read at raw
//! position `q + n`. A track starting at sector `s` is therefore read from
//! raw sample `s × 588 + n` for its full length, which pulls in samples of
//! the neighbouring track when the window crosses a boundary: the result is
//! exact across track boundaries. Raw positions before the start of the
//! disc or after the lead-out cannot be read by ordinary drives, so they
//! are filled with silence (zero samples) — only the first or last `|n|`
//! samples of the disc are affected. AccurateRip and CTDB leave those
//! samples out of their checksums for this reason.
//!
//! **Drive database.** `DriveOffsets.bin` is a list of 69-byte records:
//! offset (`i16` little-endian), drive name (32 bytes, NUL-padded ASCII,
//! `"VENDOR - MODEL"` or `"- MODEL"` when there is no vendor), a zero byte,
//! a submission count (`u32` little-endian; AccurateRip appears to cap it)
//! and padding. Records with an offset beyond ±2940 samples or no model are
//! skipped.

use std::time::Duration;

use url::Url;

use super::{CoreError, Result};
use crate::model::{
    BYTES_PER_SECTOR, DriveIdentity, Lba, SAMPLES_PER_SECTOR, SampleOffset, Sectors,
};

/// Default URL of the AccurateRip drive offset database.
pub const DEFAULT_DRIVE_DATABASE: &str = "http://www.accuraterip.com/accuraterip/DriveOffsets.bin";

/// Size of one record in `DriveOffsets.bin`.
const RECORD_SIZE: usize = 69;

/// The largest offset the database is trusted with (five sectors).
const MAX_OFFSET: i16 = 5 * 588;

/// One drive in the offset database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveOffset {
    /// "VENDOR - MODEL" as listed by AccurateRip.
    pub name: String,
    /// Correction to apply.
    pub offset: SampleOffset,
    /// Number of submissions backing it.
    pub submissions: u32,
}

impl DriveOffset {
    /// The vendor and model parts of [`DriveOffset::name`] (vendor empty
    /// when the database lists none).
    pub fn vendor_and_model(&self) -> (&str, &str) {
        split_name(&self.name)
    }
}

fn split_name(name: &str) -> (&str, &str) {
    if let Some(model) = name.strip_prefix("- ") {
        return ("", model.trim());
    }
    if let Some(vendor) = name.trim_end().strip_suffix(" -") {
        return (vendor.trim(), "");
    }
    match name.split_once(" - ") {
        Some((vendor, model)) => (vendor.trim(), model.trim()),
        None => ("", name.trim()),
    }
}

/// Parses `DriveOffsets.bin`.
pub fn parse_drive_database(bytes: &[u8]) -> Result<Vec<DriveOffset>> {
    let (records, rest) = bytes.as_chunks::<RECORD_SIZE>();
    if !rest.is_empty() {
        return Err(CoreError::Invalid(
            "the AccurateRip drive list is cut short".into(),
        ));
    }
    let mut drives = Vec::with_capacity(records.len());
    for record in records {
        let Some((offset, after)) = record.split_first_chunk::<2>() else {
            continue;
        };
        let offset = i16::from_le_bytes(*offset);
        let Some((name, after)) = after.split_first_chunk::<32>() else {
            continue;
        };
        let submissions = after
            .get(1..5)
            .and_then(|b| b.first_chunk::<4>())
            .map_or(0, |b| u32::from_le_bytes(*b));
        let name_end = name.iter().position(|b| *b == 0).unwrap_or(name.len());
        let name = String::from_utf8_lossy(name.get(..name_end).unwrap_or_default())
            .trim()
            .to_owned();
        let (_, model) = split_name(&name);
        if model.is_empty() || !(-MAX_OFFSET..=MAX_OFFSET).contains(&offset) {
            continue;
        }
        drives.push(DriveOffset {
            name,
            offset: SampleOffset(i32::from(offset)),
            submissions,
        });
    }
    Ok(drives)
}

/// Downloads `DriveOffsets.bin`, giving up after `timeout`.
pub async fn fetch_drive_database(
    client: &reqwest::Client,
    url: &Url,
    timeout: Duration,
) -> Result<Vec<DriveOffset>> {
    let network = |source| CoreError::Network {
        service: "the AccurateRip drive list",
        source,
    };
    let bytes = client
        .get(url.clone())
        .timeout(timeout)
        .send()
        .await
        .map_err(network)?
        .error_for_status()
        .map_err(network)?
        .bytes()
        .await
        .map_err(network)?;
    parse_drive_database(&bytes)
}

/// Finds the drive by vendor and model (case- and space-insensitive).
/// When a drive is listed more than once, the entry with the most
/// submissions wins.
pub fn find_offset(database: &[DriveOffset], drive: &DriveIdentity) -> Option<SampleOffset> {
    let vendor = squash(&drive.vendor);
    let model = squash(&drive.model);
    if model.is_empty() {
        return None;
    }
    database
        .iter()
        .filter(|entry| {
            let (entry_vendor, entry_model) = entry.vendor_and_model();
            squash(entry_vendor) == vendor && squash(entry_model) == model
        })
        .max_by_key(|entry| entry.submissions)
        .map(|entry| entry.offset)
}

/// Upper case with all white space removed.
fn squash(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_uppercase)
        .collect()
}

/// Which sectors to read for a track so that, after shifting by `offset`
/// samples, exactly the track's samples remain; and how many bytes to skip
/// at the front. Positions beyond the disc are filled with silence by the
/// caller (see [`OffsetWindow::clamp_to_disc`]).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct OffsetWindow {
    /// First sector to read. May be negative (before the disc) for a
    /// negative offset on the first track.
    pub first_sector: i64,
    /// Sectors to read.
    pub sector_count: u32,
    /// Bytes to drop from the start of what was read.
    pub skip_bytes: usize,
}

/// The part of an [`OffsetWindow`] a drive can actually read, with the
/// silence to add before and after it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ReadPlan {
    /// Silent sectors to put before the data read (window before the disc).
    pub silence_before: Sectors,
    /// First sector to read from the drive.
    pub read_start: Lba,
    /// Sectors to read from the drive (may be zero).
    pub read_count: Sectors,
    /// Silent sectors to put after the data read (window past the lead-out).
    pub silence_after: Sectors,
}

impl OffsetWindow {
    /// Splits the window at the edges of the disc (sector 0 and the
    /// audio lead-out `lead_out`).
    pub fn clamp_to_disc(&self, lead_out: Lba) -> Result<ReadPlan> {
        let start = self.first_sector;
        let end = start + i64::from(self.sector_count);
        let disc_end = i64::from(lead_out.0);
        let read_from = start.clamp(0, disc_end);
        let read_to = end.clamp(read_from, disc_end);
        let to_sectors = |value: i64| -> Result<u32> {
            u32::try_from(value)
                .map_err(|_| CoreError::Invalid("the read window lies outside the disc".into()))
        };
        Ok(ReadPlan {
            silence_before: Sectors(to_sectors((read_from - start).min(end - start))?),
            read_start: Lba(to_sectors(read_from)?),
            read_count: Sectors(to_sectors(read_to - read_from)?),
            silence_after: Sectors(to_sectors(end - read_to.max(start))?),
        })
    }

    /// The track's corrected PCM out of the window's data: `window_pcm`
    /// holds exactly `sector_count` sectors (silence included); the result
    /// is `length × 2352` bytes.
    pub fn extract<'a>(&self, window_pcm: &'a [u8], length: Sectors) -> Result<&'a [u8]> {
        let expected = usize::try_from(self.sector_count)
            .ok()
            .and_then(|count| count.checked_mul(BYTES_PER_SECTOR));
        if expected != Some(window_pcm.len()) {
            return Err(CoreError::Invalid(format!(
                "expected {} sectors of audio but got {} bytes",
                self.sector_count,
                window_pcm.len()
            )));
        }
        let wanted = usize::try_from(length.bytes()).map_err(|_| too_long())?;
        let end = self.skip_bytes.checked_add(wanted).ok_or_else(too_long)?;
        window_pcm
            .get(self.skip_bytes..end)
            .ok_or_else(|| CoreError::Invalid("the read window is too short for the track".into()))
    }
}

/// Plans the read window for a track starting at `start` sectors and
/// `length` sectors long, with the given correction.
pub fn window(start: u32, length: u32, offset: SampleOffset) -> Result<OffsetWindow> {
    let per_sector = i64::from(SAMPLES_PER_SECTOR);
    let first_sample = i64::from(start) * per_sector + i64::from(offset.0);
    let end_sample = first_sample + i64::from(length) * per_sector;
    let first_sector = first_sample.div_euclid(per_sector);
    let end_sector = (end_sample + per_sector - 1).div_euclid(per_sector);
    let skip_samples = first_sample.rem_euclid(per_sector);
    Ok(OffsetWindow {
        first_sector,
        sector_count: u32::try_from(end_sector - first_sector).map_err(|_| too_long())?,
        skip_bytes: usize::try_from(skip_samples).map_err(|_| too_long())? * 4,
    })
}

fn too_long() -> CoreError {
    CoreError::Invalid("the track is longer than any CD".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{SERVICE_TIMEOUT, testserver};

    fn record(offset: i16, name: &str, submissions: u32) -> Vec<u8> {
        let mut out = offset.to_le_bytes().to_vec();
        let mut field = name.as_bytes().to_vec();
        field.resize(32, 0);
        out.extend(field);
        out.push(0);
        out.extend(submissions.to_le_bytes());
        out.resize(RECORD_SIZE, 0);
        out
    }

    /// Records as they appear in the live file.
    fn sample_database() -> Vec<u8> {
        [
            record(91, "- 16X12 DVD DUAL", 10),
            record(30, "PLEXTOR  - DVDR   PX-760A", 255),
            record(667, "PIONEER  - BD-RW   BDR-X13U", 191),
            record(6, "HL-DT-ST - DVDRAM GH24NSB0", 255),
            record(9999, "BROKEN   - DRIVE", 1),
            record(5, "NOMODEL  - ", 1),
        ]
        .concat()
    }

    #[test]
    fn parses_the_drive_list() {
        let drives = parse_drive_database(&sample_database()).unwrap();
        assert_eq!(drives.len(), 4);
        assert_eq!(
            drives[1],
            DriveOffset {
                name: "PLEXTOR  - DVDR   PX-760A".into(),
                offset: SampleOffset(30),
                submissions: 255,
            }
        );
        assert_eq!(drives[0].vendor_and_model(), ("", "16X12 DVD DUAL"));
        assert_eq!(
            drives[2].vendor_and_model(),
            ("PIONEER", "BD-RW   BDR-X13U")
        );
        assert!(parse_drive_database(&[0; 70]).is_err());
    }

    #[test]
    fn finds_drives_ignoring_case_and_spaces() {
        let drives = parse_drive_database(&sample_database()).unwrap();
        let identity = |vendor: &str, model: &str| DriveIdentity {
            vendor: vendor.into(),
            model: model.into(),
            revision: "1.00".into(),
        };
        assert_eq!(
            find_offset(&drives, &identity("PLEXTOR", "DVDR PX-760A")),
            Some(SampleOffset(30))
        );
        assert_eq!(
            find_offset(&drives, &identity("pioneer", "BD-RW BDR-X13U")),
            Some(SampleOffset(667))
        );
        assert_eq!(find_offset(&drives, &identity("HL-DT-ST", "Unknown")), None);
        assert_eq!(find_offset(&drives, &identity("", "")), None);
    }

    #[test]
    fn windows_for_positive_and_negative_offsets() {
        assert_eq!(
            window(10, 5, SampleOffset(0)).unwrap(),
            OffsetWindow {
                first_sector: 10,
                sector_count: 5,
                skip_bytes: 0
            }
        );
        assert_eq!(
            window(10, 5, SampleOffset(6)).unwrap(),
            OffsetWindow {
                first_sector: 10,
                sector_count: 6,
                skip_bytes: 24
            }
        );
        assert_eq!(
            window(10, 5, SampleOffset(-6)).unwrap(),
            OffsetWindow {
                first_sector: 9,
                sector_count: 6,
                skip_bytes: (588 - 6) * 4
            }
        );
        assert_eq!(
            window(0, 5, SampleOffset(-1200)).unwrap(),
            OffsetWindow {
                first_sector: -3,
                sector_count: 6,
                skip_bytes: (3 * 588 - 1200) * 4
            }
        );
        // A whole number of sectors needs no extra sector.
        assert_eq!(window(10, 5, SampleOffset(588)).unwrap().sector_count, 5);
    }

    #[test]
    fn clamping_at_the_disc_edges() {
        let plan = window(0, 5, SampleOffset(-6))
            .unwrap()
            .clamp_to_disc(Lba(100))
            .unwrap();
        assert_eq!(
            plan,
            ReadPlan {
                silence_before: Sectors(1),
                read_start: Lba(0),
                read_count: Sectors(5),
                silence_after: Sectors(0),
            }
        );
        let plan = window(95, 5, SampleOffset(6))
            .unwrap()
            .clamp_to_disc(Lba(100))
            .unwrap();
        assert_eq!(
            plan,
            ReadPlan {
                silence_before: Sectors(0),
                read_start: Lba(95),
                read_count: Sectors(5),
                silence_after: Sectors(1),
            }
        );
    }

    /// Simulates a drive with read offset `drive_offset` over a reference
    /// disc and checks that every track comes back sample-exact, with
    /// silence only where the drive would have to read off the disc.
    fn round_trip(drive_offset: i32) {
        let track_lengths = [7u32, 3, 9];
        let total_sectors: u32 = track_lengths.iter().sum();
        let total_samples = (total_sectors * 588) as i64;
        let reference: Vec<u32> = (0..total_samples as u32)
            .map(|i| i.wrapping_mul(2_654_435_761) | 1)
            .collect();
        // What the drive returns at raw sample position p.
        let raw = |p: i64| -> u32 {
            let q = p - drive_offset as i64;
            if (0..total_samples).contains(&q) {
                reference[q as usize]
            } else {
                0
            }
        };
        let read_sectors = |start: Lba, count: Sectors| -> Vec<u8> {
            let first = start.0 as i64 * 588;
            (first..first + count.0 as i64 * 588)
                .flat_map(|p| raw(p).to_le_bytes())
                .collect()
        };
        let mut start = 0u32;
        for length in track_lengths {
            let window = window(start, length, SampleOffset(drive_offset)).unwrap();
            let plan = window.clamp_to_disc(Lba(total_sectors)).unwrap();
            let mut data = vec![0u8; plan.silence_before.bytes() as usize];
            data.extend(read_sectors(plan.read_start, plan.read_count));
            data.extend(vec![0u8; plan.silence_after.bytes() as usize]);
            let track = window.extract(&data, Sectors(length)).unwrap();
            assert_eq!(track.len(), length as usize * 2352);
            for (i, sample) in track.as_chunks::<4>().0.iter().enumerate() {
                let q = start as i64 * 588 + i as i64;
                let readable = (0..total_samples).contains(&(q + drive_offset as i64));
                let expected = if readable { reference[q as usize] } else { 0 };
                assert_eq!(
                    u32::from_le_bytes(*sample),
                    expected,
                    "offset {drive_offset}, disc sample {q}"
                );
            }
            start += length;
        }
    }

    #[test]
    fn correction_is_exact_across_track_boundaries() {
        for offset in [0, 6, -6, 30, 667, -1200, 588, -588, 2940] {
            round_trip(offset);
        }
    }

    #[test]
    fn extract_checks_sizes() {
        let window = window(0, 1, SampleOffset(6)).unwrap();
        assert!(window.extract(&[0; 2352], Sectors(1)).is_err());
        assert!(window.extract(&[0; 4704], Sectors(1)).is_ok());
    }

    #[tokio::test]
    async fn fetch_from_a_mock_server() {
        use axum::{Router, routing::get};
        let app = Router::new().route("/DriveOffsets.bin", get(|| async { sample_database() }));
        let base = testserver::serve(app).await;
        let url = base.join("DriveOffsets.bin").unwrap();
        let drives = fetch_drive_database(&testserver::client(), &url, SERVICE_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(drives.len(), 4);
        let missing = base.join("nothing.bin").unwrap();
        assert!(
            fetch_drive_database(&testserver::client(), &missing, SERVICE_TIMEOUT)
                .await
                .is_err()
        );
    }
}
