// SPDX-License-Identifier: AGPL-3.0-or-later
//! A simulated drive backed by a [`FakeDisc`] directory.
//!
//! The drive behaves like a real one closely enough that the secure-read
//! logic is genuinely exercised:
//!
//! - **Read offset.** Data comes back shifted by `-readOffset` samples, so
//!   the reader's `+readOffset` correction recovers the track files exactly.
//!   Samples before the start or past the end of the disc read as silence.
//! - **Raw reads** (one look at a sector) are what [`ParanoiaMode::Off`]
//!   returns, injected errors and all:
//!   - `unreadable`: different garbage on every read;
//!   - `jitter`: reads land a few samples early or late in a repeating
//!     cycle (`+4, 0, -3, 0` samples), so only some reads are right;
//!   - `flaky_once`: the very first read of the sector is garbage, every
//!     later one is right.
//! - **Simulated paranoia** ([`ParanoiaMode::Full`]) reads each sector until
//!   the same data has come back twice, up to [`PARANOIA_ATTEMPTS`] reads.
//!   Jitter and flaky sectors are recovered (and counted as retried);
//!   unreadable sectors are reported as unrecovered with the last read as
//!   the best guess, as libcdio-paranoia does.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::{
    DiscDrive, DriveDescriptor, DriveError, DriveProvider, ParanoiaMode, Result, SectorRead,
};
use crate::fakedisc::{FakeDisc, InjectedErrorKind};
use crate::model::{BYTES_PER_SAMPLE, BYTES_PER_SECTOR, DiscInfo, Lba, Sectors};

/// Most reads simulated paranoia makes of one sector before giving up.
pub const PARANOIA_ATTEMPTS: u32 = 20;

/// The repeating cycle of sample shifts a `jitter` sector reads with.
const JITTER_CYCLE: [i64; 4] = [4, 0, -3, 0];

/// Bytes per sector and per sample as `i64`, for signed position maths.
const SECTOR_BYTES: i64 = BYTES_PER_SECTOR as i64;
const SAMPLE_BYTES: i64 = BYTES_PER_SAMPLE as i64;

/// One audio track's file, opened lazily.
#[derive(Debug)]
struct TrackFile {
    /// First byte of the track on the disc (zero read offset).
    start_byte: i64,
    /// One past the last byte.
    end_byte: i64,
    path: PathBuf,
    file: Option<File>,
}

/// A drive that plays back a simulated disc, including injected errors
/// (`unreadable`, `jitter`, `flaky_once`) and its simulated read offset.
#[derive(Debug)]
pub struct FakeDrive {
    disc: FakeDisc,
    ejected: bool,
    tracks: Vec<TrackFile>,
    errors: HashMap<Lba, InjectedErrorKind>,
    /// Raw reads made of each sector that has an injected error.
    reads: HashMap<Lba, u32>,
    /// Raw reads made in total (every sector), for tests.
    raw_reads: u64,
}

impl FakeDrive {
    /// Opens a simulated disc directory.
    pub fn open(dir: &Path) -> Result<Self> {
        let disc = FakeDisc::load(dir)?;
        let tracks = disc
            .spec
            .toc
            .audio_tracks()
            .map(|track| {
                let start_byte = sector_byte(track.start_lba);
                let length = i64::try_from(track.length_sectors.bytes()).unwrap_or(i64::MAX);
                TrackFile {
                    start_byte,
                    end_byte: start_byte.saturating_add(length),
                    path: disc.track_path(track.number),
                    file: None,
                }
            })
            .collect();
        let errors = disc
            .spec
            .errors
            .iter()
            .map(|error| (error.lba, error.kind))
            .collect();
        Ok(FakeDrive {
            disc,
            ejected: false,
            tracks,
            errors,
            reads: HashMap::new(),
            raw_reads: 0,
        })
    }

    /// The simulated disc.
    pub fn disc(&self) -> &FakeDisc {
        &self.disc
    }

    /// True after [`DiscDrive::eject`].
    pub fn is_ejected(&self) -> bool {
        self.ejected
    }

    /// How many single-sector looks at the disc have been made so far
    /// (simulated paranoia makes at least two per sector).
    pub fn raw_reads(&self) -> u64 {
        self.raw_reads
    }

    /// Bytes `[start, start + length)` of the disc as a zero-offset drive
    /// would read them perfectly: silence outside the audio tracks.
    fn disc_bytes(&mut self, start: i64, length: usize) -> Result<Vec<u8>> {
        let mut out = vec![0u8; length];
        let end = start.saturating_add(i64::try_from(length).unwrap_or(i64::MAX));
        for track in &mut self.tracks {
            let from = start.max(track.start_byte);
            let to = end.min(track.end_byte);
            if from >= to {
                continue;
            }
            let path = &track.path;
            let io_error = |source| DriveError::Io {
                path: path.clone(),
                source,
            };
            let file = match &mut track.file {
                Some(file) => file,
                slot @ None => slot.insert(File::open(path).map_err(io_error)?),
            };
            let in_file = u64::try_from(from - track.start_byte).unwrap_or(0);
            file.seek(SeekFrom::Start(in_file)).map_err(io_error)?;
            let window = usize::try_from(from - start)
                .ok()
                .zip(usize::try_from(to - start).ok())
                .and_then(|(a, b)| out.get_mut(a..b))
                .ok_or_else(|| DriveError::Read {
                    lba: Lba(0),
                    reason: "the simulated disc position is out of range".into(),
                })?;
            file.read_exact(window).map_err(io_error)?;
        }
        Ok(out)
    }

    /// One raw look at sector `lba`, with the read offset and any injected
    /// error applied.
    fn raw_sector(&mut self, lba: Lba) -> Result<Vec<u8>> {
        self.raw_reads = self.raw_reads.saturating_add(1);
        let offset_bytes = i64::from(self.disc.spec.read_offset.0) * SAMPLE_BYTES;
        // The drive returns true sample N - offset at position N.
        let base = sector_byte(lba) - offset_bytes;
        let Some(kind) = self.errors.get(&lba).copied() else {
            return self.disc_bytes(base, BYTES_PER_SECTOR);
        };
        let count = self.reads.entry(lba).or_insert(0);
        let attempt = *count;
        *count = count.saturating_add(1);
        match kind {
            InjectedErrorKind::Unreadable => Ok(garbage(lba, attempt)),
            InjectedErrorKind::FlakyOnce if attempt == 0 => Ok(garbage(lba, attempt)),
            InjectedErrorKind::FlakyOnce => self.disc_bytes(base, BYTES_PER_SECTOR),
            InjectedErrorKind::Jitter => {
                let cycle = usize::try_from(attempt).unwrap_or(0) % JITTER_CYCLE.len();
                let shift = JITTER_CYCLE.get(cycle).copied().unwrap_or(0);
                self.disc_bytes(base + shift * SAMPLE_BYTES, BYTES_PER_SECTOR)
            }
        }
    }

    /// Simulated full paranoia: re-read until two reads agree.
    /// Returns the data, whether it verified and how many reads it took.
    fn paranoid_sector(&mut self, lba: Lba) -> Result<(Vec<u8>, bool, u32)> {
        let mut seen: Vec<Vec<u8>> = Vec::with_capacity(2);
        for attempt in 1..=PARANOIA_ATTEMPTS {
            let data = self.raw_sector(lba)?;
            if seen.contains(&data) {
                return Ok((data, true, attempt));
            }
            seen.push(data);
        }
        let best_guess = seen.pop().unwrap_or_else(|| vec![0; BYTES_PER_SECTOR]);
        Ok((best_guess, false, PARANOIA_ATTEMPTS))
    }
}

/// Byte position of the start of a sector.
fn sector_byte(lba: Lba) -> i64 {
    i64::from(lba.0) * SECTOR_BYTES
}

/// Deterministic noise for a bad read: different for every sector and read.
fn garbage(lba: Lba, attempt: u32) -> Vec<u8> {
    let mut state = ((u64::from(lba.0) << 32) | u64::from(attempt)) ^ 0x9E37_79B9_7F4A_7C15;
    let mut out = Vec::with_capacity(BYTES_PER_SECTOR + 8);
    while out.len() < BYTES_PER_SECTOR {
        // xorshift64*
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        out.extend_from_slice(&state.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes());
    }
    out.truncate(BYTES_PER_SECTOR);
    out
}

impl DiscDrive for FakeDrive {
    fn disc_info(&mut self) -> Result<DiscInfo> {
        if self.ejected {
            return Err(DriveError::NoDisc);
        }
        let spec = &self.disc.spec;
        Ok(DiscInfo {
            drive: spec.drive.clone(),
            toc: spec.toc.clone(),
            mcn: spec.mcn.clone(),
            cd_text: spec.cd_text.clone(),
        })
    }

    fn read_sectors(
        &mut self,
        start: Lba,
        count: Sectors,
        mode: ParanoiaMode,
    ) -> Result<SectorRead> {
        if self.ejected {
            return Err(DriveError::NoDisc);
        }
        let lead_out = self.disc.spec.toc.lead_out();
        let Some(end) = start.checked_add(count).filter(|end| *end <= lead_out) else {
            return Err(DriveError::Read {
                lba: start,
                reason: format!(
                    "{} sectors from there run past the end of the disc (sector {lead_out})",
                    count.0
                ),
            });
        };
        let capacity = usize::try_from(count.bytes()).unwrap_or(0);
        let mut read = SectorRead {
            data: Vec::with_capacity(capacity),
            ..SectorRead::default()
        };
        for lba in (start.0..end.0).map(Lba) {
            match mode {
                ParanoiaMode::Off => {
                    let data = self.raw_sector(lba)?;
                    read.data.extend_from_slice(&data);
                }
                ParanoiaMode::Full => {
                    let (data, verified, attempts) = self.paranoid_sector(lba)?;
                    read.data.extend_from_slice(&data);
                    if !verified {
                        read.unrecovered.push(lba);
                    } else if attempts > 2 {
                        read.retried_sectors = read.retried_sectors.saturating_add(1);
                    }
                }
            }
        }
        Ok(read)
    }

    fn eject(&mut self) -> Result<()> {
        self.ejected = true;
        Ok(())
    }
}

/// Offers one simulated disc as the only drive.
#[derive(Clone, Debug)]
pub struct FakeDriveProvider {
    dir: PathBuf,
}

impl FakeDriveProvider {
    /// A provider whose single drive holds the disc in `dir`.
    pub fn new(dir: PathBuf) -> Self {
        FakeDriveProvider { dir }
    }
}

impl DriveProvider for FakeDriveProvider {
    fn list(&self) -> Result<Vec<DriveDescriptor>> {
        Ok(vec![DriveDescriptor {
            device: self.dir.display().to_string(),
            label: format!("Simulated drive ({})", self.dir.display()),
        }])
    }

    fn open(&self, device: &str) -> Result<Box<dyn DiscDrive>> {
        Ok(Box::new(FakeDrive::open(Path::new(device))?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakedisc::InjectedError;
    use crate::model::{SampleOffset, TrackNumber};
    use crate::testsupport::{FakeDiscOptions, make_fake_disc};

    fn disc(errors: Vec<InjectedError>, offset: i32) -> (tempfile::TempDir, FakeDrive) {
        let dir = tempfile::tempdir().unwrap();
        let options = FakeDiscOptions {
            tracks: 2,
            seconds: 1,
            read_offset: SampleOffset(offset),
            errors,
        };
        make_fake_disc(dir.path(), &options).unwrap();
        let drive = FakeDrive::open(dir.path()).unwrap();
        (dir, drive)
    }

    fn track_bytes(drive: &FakeDrive, number: u8) -> Vec<u8> {
        std::fs::read(drive.disc().track_path(TrackNumber::new(number).unwrap())).unwrap()
    }

    fn error(lba: u32, kind: InjectedErrorKind) -> InjectedError {
        InjectedError {
            lba: Lba(lba),
            kind,
        }
    }

    #[test]
    fn reports_disc_info_and_ejects() {
        let (_dir, mut drive) = disc(Vec::new(), 0);
        assert_eq!(drive.disc_info().unwrap().toc.tracks().len(), 2);
        drive.eject().unwrap();
        assert!(drive.is_ejected());
        assert!(matches!(drive.disc_info(), Err(DriveError::NoDisc)));
        assert!(matches!(
            drive.read_sectors(Lba(0), Sectors(1), ParanoiaMode::Full),
            Err(DriveError::NoDisc)
        ));
    }

    #[test]
    fn zero_offset_reads_the_track_files() {
        let (_dir, mut drive) = disc(Vec::new(), 0);
        let read = drive
            .read_sectors(Lba(0), Sectors(150), ParanoiaMode::Full)
            .unwrap();
        let mut expected = track_bytes(&drive, 1);
        expected.extend(track_bytes(&drive, 2));
        assert_eq!(read.data, expected);
        assert!(read.unrecovered.is_empty());
        assert_eq!(read.retried_sectors, 0);
    }

    #[test]
    fn read_offset_shifts_the_data() {
        let (_dir, mut drive) = disc(Vec::new(), 6);
        let read = drive
            .read_sectors(Lba(0), Sectors(2), ParanoiaMode::Off)
            .unwrap();
        let track = track_bytes(&drive, 1);
        // The first 6 samples are from before the disc: silence.
        assert!(read.data[..24].iter().all(|b| *b == 0));
        assert_eq!(&read.data[24..], &track[..2 * 2352 - 24]);
    }

    #[test]
    fn reading_past_the_lead_out_fails() {
        let (_dir, mut drive) = disc(Vec::new(), 0);
        assert!(matches!(
            drive.read_sectors(Lba(149), Sectors(2), ParanoiaMode::Full),
            Err(DriveError::Read { .. })
        ));
    }

    #[test]
    fn unreadable_sectors_are_reported_after_retries() {
        let (_dir, mut drive) = disc(vec![error(10, InjectedErrorKind::Unreadable)], 0);
        let before = drive.raw_reads();
        let read = drive
            .read_sectors(Lba(9), Sectors(3), ParanoiaMode::Full)
            .unwrap();
        assert_eq!(read.unrecovered, vec![Lba(10)]);
        assert_eq!(read.data.len(), 3 * 2352);
        // Two reads for each good sector, every attempt for the bad one.
        assert_eq!(drive.raw_reads() - before, 4 + u64::from(PARANOIA_ATTEMPTS));
        // Without paranoia nothing is noticed.
        let off = drive
            .read_sectors(Lba(10), Sectors(1), ParanoiaMode::Off)
            .unwrap();
        assert!(off.unrecovered.is_empty());
        assert_ne!(&off.data[..], &track_bytes(&drive, 1)[10 * 2352..11 * 2352]);
    }

    #[test]
    fn paranoia_corrects_jitter_but_a_plain_read_does_not() {
        let (_dir, mut drive) = disc(vec![error(20, InjectedErrorKind::Jitter)], 0);
        let truth = track_bytes(&drive, 1)[20 * 2352..21 * 2352].to_vec();
        let plain = drive
            .read_sectors(Lba(20), Sectors(1), ParanoiaMode::Off)
            .unwrap();
        assert_ne!(
            plain.data, truth,
            "the first jittered read is 4 samples off"
        );
        let secure = drive
            .read_sectors(Lba(20), Sectors(1), ParanoiaMode::Full)
            .unwrap();
        assert_eq!(secure.data, truth);
        assert!(secure.unrecovered.is_empty());
        assert_eq!(secure.retried_sectors, 1);
    }

    #[test]
    fn flaky_sectors_fail_only_the_first_read() {
        let (_dir, mut drive) = disc(vec![error(30, InjectedErrorKind::FlakyOnce)], 0);
        let truth = track_bytes(&drive, 1)[30 * 2352..31 * 2352].to_vec();
        let first = drive
            .read_sectors(Lba(30), Sectors(1), ParanoiaMode::Off)
            .unwrap();
        assert_ne!(first.data, truth);
        let second = drive
            .read_sectors(Lba(30), Sectors(1), ParanoiaMode::Off)
            .unwrap();
        assert_eq!(second.data, truth);

        let (_dir, mut drive) = disc(vec![error(30, InjectedErrorKind::FlakyOnce)], 0);
        let secure = drive
            .read_sectors(Lba(30), Sectors(1), ParanoiaMode::Full)
            .unwrap();
        assert_eq!(secure.data, truth);
        assert_eq!(secure.retried_sectors, 1);
    }

    #[test]
    fn provider_lists_and_opens_the_simulated_drive() {
        let (dir, _drive) = disc(Vec::new(), 0);
        let provider = FakeDriveProvider::new(dir.path().to_owned());
        let drives = provider.list().unwrap();
        assert_eq!(drives.len(), 1);
        let mut opened = provider.open(&drives[0].device).unwrap();
        assert_eq!(opened.disc_info().unwrap().toc.lead_out(), Lba(150));
    }
}
