// SPDX-License-Identifier: AGPL-3.0-or-later
//! Secure track reading on top of any [`DiscDrive`]: offset correction,
//! test-and-copy, CRCs, peak level and unrecovered-sector reporting.
//!
//! Every read goes through full paranoia ([`ParanoiaMode::Full`]).
//!
//! - **Secure**: one paranoia pass. Sectors paranoia could not verify are
//!   reported, never hidden.
//! - **Test and copy**: a copy pass (written to disk) and a test pass
//!   (checksums only). Where the two differ, those sectors are read again
//!   up to [`MAX_REREADS`] times; a sector is settled once the same data has
//!   come back twice, and the file is patched if the copy was the odd one
//!   out. A re-read that paranoia flags never counts towards settling and
//!   its sectors are reported. Sectors that never settle are reported as
//!   unrecovered and the
//!   test CRC is left as the test pass read it, so the track is marked as
//!   "with errors".
//!
//! Offset correction: a drive with read offset `+n` returns true sample
//! `N - n` at position `N`, so true sample `M` is read at drive position
//! `M + n`. Samples that fall before the start of the disc or past the
//! lead-out are filled with silence (drives cannot read there reliably).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{DiscDrive, DriveError, ParanoiaMode, Result};
use crate::model::{
    BYTES_PER_SAMPLE, BYTES_PER_SECTOR, Crc32, Lba, Pcm, Peak, ReadMode, SampleOffset, Sectors,
    TrackEntry, TrackReadResult,
};

/// Sectors asked of the drive in one call (one second of audio).
pub const CHUNK_SECTORS: u32 = 75;

/// In test-and-copy mode, how many extra times sectors whose copy and test
/// reads differ are read before giving up on them.
pub const MAX_REREADS: u32 = 5;

const SECTOR_BYTES: i64 = BYTES_PER_SECTOR as i64;
const SAMPLE_BYTES: i64 = BYTES_PER_SAMPLE as i64;

/// Progress of one track, reported after each chunk.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ReadProgress {
    /// Sectors done in the current pass.
    pub sectors_done: Sectors,
    /// Sectors in the pass.
    pub sectors_total: Sectors,
    /// 1 for the copy pass, 2 for the test pass, 3 and up for re-reads of
    /// sectors where the two disagreed.
    pub pass: u8,
    /// Sectors that have needed re-reads so far.
    pub retried_sectors: u32,
}

/// Options for [`read_track`].
#[derive(Clone, Debug)]
pub struct ReadOptions {
    /// Secure or test-and-copy.
    pub mode: ReadMode,
    /// Correction to apply.
    pub offset: SampleOffset,
    /// Set to stop reading; the read returns [`DriveError::Cancelled`].
    pub cancel: Arc<AtomicBool>,
}

/// Where a track lies, in drive byte positions (offset already applied).
#[derive(Copy, Clone, Debug)]
struct Geometry {
    /// Drive byte position of the track's first true byte.
    drive_start: i64,
    /// Bytes in the track.
    track_bytes: i64,
    /// Bytes on the disc up to the lead-out (readable range is `0..this`).
    disc_bytes: i64,
}

impl Geometry {
    /// Drive sectors holding output bytes `[from, to)` of the track, clipped
    /// to the readable part of the disc.
    fn drive_sectors(&self, from: i64, to: i64) -> impl Iterator<Item = Lba> {
        let first = (self.drive_start + from).max(0);
        let last = (self.drive_start + to).min(self.disc_bytes);
        let (first_sector, end_sector) = if first < last {
            (
                first / SECTOR_BYTES,
                (last + SECTOR_BYTES - 1) / SECTOR_BYTES,
            )
        } else {
            (0, 0)
        };
        (first_sector..end_sector).filter_map(|sector| u32::try_from(sector).ok().map(Lba))
    }
}

/// What a span read observed.
#[derive(Debug, Default)]
struct SpanReport {
    unrecovered: BTreeSet<Lba>,
    retried: u32,
}

/// Reads output bytes `[from, from + length)` of the track, handing them to
/// `sink` in order (pieces are always whole stereo samples).
fn read_span(
    drive: &mut dyn DiscDrive,
    geometry: Geometry,
    from: i64,
    length: i64,
    cancel: &AtomicBool,
    sink: &mut dyn FnMut(&[u8]) -> Result<()>,
    progress: &mut dyn FnMut(i64, u32),
) -> Result<SpanReport> {
    let zeros = [0u8; BYTES_PER_SECTOR];
    let mut report = SpanReport::default();
    let mut position = geometry.drive_start + from;
    let end = position + length;
    let emit_zeros = |count: i64, sink: &mut dyn FnMut(&[u8]) -> Result<()>| -> Result<()> {
        let mut left = count;
        while left > 0 {
            let piece = left.min(SECTOR_BYTES);
            let bytes = usize::try_from(piece)
                .ok()
                .and_then(|n| zeros.get(..n))
                .unwrap_or(&[]);
            sink(bytes)?;
            left -= piece;
        }
        Ok(())
    };
    while position < end {
        if cancel.load(Ordering::Relaxed) {
            return Err(DriveError::Cancelled);
        }
        if position < 0 {
            let count = end.min(0) - position;
            emit_zeros(count, sink)?;
            position += count;
            continue;
        }
        if position >= geometry.disc_bytes {
            emit_zeros(end - position, sink)?;
            break;
        }
        let sector = position / SECTOR_BYTES;
        let skip = position - sector * SECTOR_BYTES;
        let readable_end = end.min(geometry.disc_bytes);
        let end_sector = (readable_end + SECTOR_BYTES - 1) / SECTOR_BYTES;
        let count = (end_sector - sector).min(i64::from(CHUNK_SECTORS));
        let lba = u32::try_from(sector)
            .map(Lba)
            .map_err(|_| DriveError::Read {
                lba: Lba(u32::MAX),
                reason: "the position is beyond the end of any disc".into(),
            })?;
        let sectors = u32::try_from(count).unwrap_or(CHUNK_SECTORS);
        let read = drive.read_sectors(lba, Sectors(sectors), ParanoiaMode::Full)?;
        let expected = Sectors(sectors).bytes();
        if u64::try_from(read.data.len()).ok() != Some(expected) {
            return Err(short_read(lba, sectors, read.data.len()));
        }
        let take_end = (readable_end - sector * SECTOR_BYTES).min(count * SECTOR_BYTES);
        let piece = usize::try_from(skip)
            .ok()
            .zip(usize::try_from(take_end).ok())
            .and_then(|(a, b)| read.data.get(a..b))
            .ok_or_else(|| short_read(lba, sectors, read.data.len()))?;
        sink(piece)?;
        report.unrecovered.extend(read.unrecovered);
        report.retried = report.retried.saturating_add(read.retried_sectors);
        position = sector * SECTOR_BYTES + take_end;
        progress(position - geometry.drive_start - from, report.retried);
    }
    progress(length, report.retried);
    Ok(report)
}

/// The error for a drive that returned the wrong amount of data.
fn short_read(lba: Lba, sectors: u32, actual: usize) -> DriveError {
    DriveError::Read {
        lba,
        reason: format!("asked for {sectors} sectors but got {actual} bytes"),
    }
}

/// Accumulates one pass: whole-track CRC, peak, optional per-sector CRCs
/// and optionally the file.
struct PassSink {
    file: Option<(BufWriter<File>, std::path::PathBuf)>,
    crc: crc32fast::Hasher,
    peak: u16,
    sector_crcs: Option<Vec<u32>>,
    sector_crc: crc32fast::Hasher,
    sector_fill: usize,
}

impl PassSink {
    fn new(file: Option<(BufWriter<File>, std::path::PathBuf)>, per_sector: bool) -> Self {
        PassSink {
            file,
            crc: crc32fast::Hasher::new(),
            peak: 0,
            sector_crcs: per_sector.then(Vec::new),
            sector_crc: crc32fast::Hasher::new(),
            sector_fill: 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Result<()> {
        if let Some((file, path)) = &mut self.file {
            file.write_all(bytes).map_err(|source| DriveError::Io {
                path: path.clone(),
                source,
            })?;
        }
        self.crc.update(bytes);
        self.peak = self.peak.max(peak_of(bytes));
        if let Some(crcs) = &mut self.sector_crcs {
            let mut rest = bytes;
            while !rest.is_empty() {
                let room = BYTES_PER_SECTOR - self.sector_fill;
                let (now, later) = rest.split_at(room.min(rest.len()));
                self.sector_crc.update(now);
                self.sector_fill += now.len();
                if self.sector_fill == BYTES_PER_SECTOR {
                    let done = std::mem::replace(&mut self.sector_crc, crc32fast::Hasher::new());
                    crcs.push(done.finalize());
                    self.sector_fill = 0;
                }
                rest = later;
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<PassResult> {
        if let Some((file, path)) = self.file {
            file.into_inner()
                .map_err(|error| error.into_error())
                .and_then(|file| file.sync_all())
                .map_err(|source| DriveError::Io { path, source })?;
        }
        Ok(PassResult {
            crc: self.crc.finalize(),
            peak: self.peak,
            sector_crcs: self.sector_crcs.unwrap_or_default(),
        })
    }
}

struct PassResult {
    crc: u32,
    peak: u16,
    sector_crcs: Vec<u32>,
}

/// Largest absolute sample value in 16-bit LE PCM.
fn peak_of(bytes: &[u8]) -> u16 {
    let (samples, _) = bytes.as_chunks::<2>();
    samples
        .iter()
        .map(|pair| i16::from_le_bytes(*pair).unsigned_abs())
        .max()
        .unwrap_or(0)
}

/// Reads one track into `pcm_path` (offset-corrected, exactly
/// `length_sectors × 2352` bytes), verifying per the mode.
///
/// `lead_out_sectors` is the disc's lead-out position counted from sector 0
/// (`toc.lead_out().0`): the reader never asks the drive for sectors past it.
pub fn read_track(
    drive: &mut dyn DiscDrive,
    track: &TrackEntry,
    lead_out_sectors: Sectors,
    options: &ReadOptions,
    pcm_path: &Path,
    progress: &mut dyn FnMut(ReadProgress),
) -> Result<TrackReadResult> {
    if !track.is_audio {
        return Err(DriveError::NoAudio);
    }
    let result = read_track_inner(drive, track, lead_out_sectors, options, pcm_path, progress);
    if result.is_err() {
        // A half-written temporary file is useless; ignore a failure to
        // remove it (the caller's temporary folder is cleaned up anyway).
        let _ = std::fs::remove_file(pcm_path);
    }
    result
}

fn read_track_inner(
    drive: &mut dyn DiscDrive,
    track: &TrackEntry,
    lead_out_sectors: Sectors,
    options: &ReadOptions,
    pcm_path: &Path,
    progress: &mut dyn FnMut(ReadProgress),
) -> Result<TrackReadResult> {
    let geometry = Geometry {
        drive_start: i64::from(track.start_lba.0) * SECTOR_BYTES
            + i64::from(options.offset.0) * SAMPLE_BYTES,
        track_bytes: i64::from(track.length_sectors.0) * SECTOR_BYTES,
        disc_bytes: i64::from(lead_out_sectors.0) * SECTOR_BYTES,
    };
    let test_and_copy = options.mode == ReadMode::TestAndCopy;
    let total = track.length_sectors;
    let io_error = |source| DriveError::Io {
        path: pcm_path.to_owned(),
        source,
    };

    // Pass 1: the copy.
    let file = File::create(pcm_path).map_err(io_error)?;
    let mut copy = PassSink::new(
        Some((BufWriter::new(file), pcm_path.to_owned())),
        test_and_copy,
    );
    let mut retried_total = 0u32;
    let copy_report = run_pass(
        drive,
        geometry,
        options,
        1,
        total,
        &mut copy,
        &mut retried_total,
        progress,
    )?;
    let copy = copy.finish()?;
    let mut unrecovered = copy_report.unrecovered;

    let mut result = TrackReadResult {
        track: track.number,
        pcm: Pcm::File(pcm_path.to_owned()),
        samples: total.samples(),
        copy_crc: Crc32(copy.crc),
        test_crc: None,
        unrecovered_sectors: Vec::new(),
        retried_sectors: 0,
        peak: Peak(copy.peak),
    };
    if !test_and_copy {
        result.unrecovered_sectors = unrecovered.into_iter().collect();
        result.retried_sectors = retried_total;
        return Ok(result);
    }

    // Pass 2: the test.
    let mut test = PassSink::new(None, true);
    let test_report = run_pass(
        drive,
        geometry,
        options,
        2,
        total,
        &mut test,
        &mut retried_total,
        progress,
    )?;
    let test = test.finish()?;
    unrecovered.extend(test_report.unrecovered);

    let disagreeing: Vec<u32> = copy
        .sector_crcs
        .iter()
        .zip(&test.sector_crcs)
        .zip(0u32..)
        .filter(|((a, b), _)| a != b)
        .map(|(_, index)| index)
        .collect();
    if disagreeing.is_empty() && copy.crc == test.crc {
        result.test_crc = Some(Crc32(test.crc));
        result.unrecovered_sectors = unrecovered.into_iter().collect();
        result.retried_sectors = retried_total;
        return Ok(result);
    }

    let settled = settle(
        drive,
        geometry,
        options,
        &copy.sector_crcs,
        &test.sector_crcs,
        &disagreeing,
        &mut retried_total,
        progress,
    )?;
    if !settled.patches.is_empty() {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(pcm_path)
            .map_err(io_error)?;
        for (index, bytes) in &settled.patches {
            file.seek(SeekFrom::Start(u64::from(*index) * BYTES_PER_SECTOR as u64))
                .and_then(|_| file.write_all(bytes))
                .map_err(io_error)?;
        }
        file.sync_all().map_err(io_error)?;
        let (crc, peak) = crc_and_peak(pcm_path)?;
        result.copy_crc = Crc32(crc);
        result.peak = Peak(peak);
    }
    unrecovered.extend(settled.flagged);
    for index in &settled.unsettled {
        let from = i64::from(*index) * SECTOR_BYTES;
        unrecovered.extend(geometry.drive_sectors(from, from + SECTOR_BYTES));
    }
    result.test_crc = Some(if settled.unsettled.is_empty() {
        result.copy_crc
    } else {
        Crc32(test.crc)
    });
    result.unrecovered_sectors = unrecovered.into_iter().collect();
    result.retried_sectors = retried_total;
    Ok(result)
}

/// One full pass over the track into `sink`.
#[allow(clippy::too_many_arguments)]
fn run_pass(
    drive: &mut dyn DiscDrive,
    geometry: Geometry,
    options: &ReadOptions,
    pass: u8,
    total: Sectors,
    sink: &mut PassSink,
    retried_total: &mut u32,
    progress: &mut dyn FnMut(ReadProgress),
) -> Result<SpanReport> {
    let before = *retried_total;
    let report = read_span(
        drive,
        geometry,
        0,
        geometry.track_bytes,
        &options.cancel,
        &mut |bytes| sink.push(bytes),
        &mut |done, retried| {
            progress(ReadProgress {
                sectors_done: Sectors(u32::try_from(done / SECTOR_BYTES).unwrap_or(total.0)),
                sectors_total: total,
                pass,
                retried_sectors: before.saturating_add(retried),
            });
        },
    )?;
    *retried_total = before.saturating_add(report.retried);
    Ok(report)
}

/// The outcome of re-reading disagreeing sectors.
#[derive(Debug, Default)]
struct Settled {
    /// Track-relative sector index → the agreed data, where it differs from
    /// the copy pass.
    patches: BTreeMap<u32, Vec<u8>>,
    /// Sectors that never got two matching reads.
    unsettled: Vec<u32>,
    /// Drive sectors that paranoia could not verify on a re-read. They are
    /// reported as unrecovered like the copy and test passes' (a struggling
    /// disc is never hidden), and such a re-read never counts as a vote.
    flagged: BTreeSet<Lba>,
}

/// Re-reads the sectors where copy and test disagree until each has the
/// same data twice (counting the copy and test reads), up to
/// [`MAX_REREADS`] rounds.
#[allow(clippy::too_many_arguments)]
fn settle(
    drive: &mut dyn DiscDrive,
    geometry: Geometry,
    options: &ReadOptions,
    copy_crcs: &[u32],
    test_crcs: &[u32],
    disagreeing: &[u32],
    retried_total: &mut u32,
    progress: &mut dyn FnMut(ReadProgress),
) -> Result<Settled> {
    let mut votes: BTreeMap<u32, Vec<Vote>> = BTreeMap::new();
    for &index in disagreeing {
        let position = usize::try_from(index).unwrap_or(usize::MAX);
        let mut entry = Vec::with_capacity(4);
        for crc in [copy_crcs.get(position), test_crcs.get(position)]
            .into_iter()
            .flatten()
        {
            add_vote(&mut entry, *crc, None);
        }
        votes.insert(index, entry);
    }
    let mut settled = Settled::default();
    let mut open: Vec<u32> = disagreeing.to_vec();
    *retried_total = retried_total.saturating_add(u32::try_from(open.len()).unwrap_or(u32::MAX));
    let total = Sectors(u32::try_from(open.len()).unwrap_or(u32::MAX));

    for round in 0..MAX_REREADS {
        if open.is_empty() {
            break;
        }
        let pass = u8::try_from(round + 3).unwrap_or(u8::MAX);
        let mut done = 0u32;
        for (first, count) in runs(&open) {
            let mut data =
                Vec::with_capacity(usize::try_from(count).unwrap_or(0) * BYTES_PER_SECTOR);
            let report = read_span(
                drive,
                geometry,
                i64::from(first) * SECTOR_BYTES,
                i64::from(count) * SECTOR_BYTES,
                &options.cancel,
                &mut |bytes| {
                    data.extend_from_slice(bytes);
                    Ok(())
                },
                &mut |_, _| {},
            )?;
            *retried_total = retried_total.saturating_add(report.retried);
            for (sector, index) in data.as_chunks::<BYTES_PER_SECTOR>().0.iter().zip(first..) {
                // A read paranoia could not verify is only a guess: it must
                // not settle the sector (it could agree with a bad copy).
                let from = i64::from(index) * SECTOR_BYTES;
                let unverified = geometry
                    .drive_sectors(from, from + SECTOR_BYTES)
                    .any(|lba| report.unrecovered.contains(&lba));
                if unverified {
                    continue;
                }
                if let Some(entry) = votes.get_mut(&index) {
                    add_vote(entry, crc32fast::hash(sector), Some(sector.as_slice()));
                }
            }
            settled.flagged.extend(report.unrecovered);
            done = done.saturating_add(count);
            progress(ReadProgress {
                sectors_done: Sectors(done),
                sectors_total: total,
                pass,
                retried_sectors: *retried_total,
            });
        }
        open.retain(|index| {
            let Some(entry) = votes.get(index) else {
                return false;
            };
            let Some(Vote { crc, data, .. }) = entry.iter().find(|vote| vote.seen >= 2) else {
                return true;
            };
            let position = usize::try_from(*index).unwrap_or(usize::MAX);
            if copy_crcs.get(position) != Some(crc) {
                if let Some(data) = data {
                    settled.patches.insert(*index, data.clone());
                } else {
                    // Only the test pass and no re-read had this data:
                    // cannot happen (two votes need a re-read), so keep it
                    // open rather than guess.
                    return true;
                }
            }
            false
        });
    }
    settled.unsettled = open;
    Ok(settled)
}

/// One version of a sector's data seen while settling disagreements.
#[derive(Debug)]
struct Vote {
    /// CRC-32 of the sector's bytes.
    crc: u32,
    /// How many reads returned it.
    seen: u32,
    /// The bytes, once a re-read returned them (the copy and test passes
    /// keep only checksums).
    data: Option<Vec<u8>>,
}

/// Counts one read of a sector with checksum `crc`.
fn add_vote(entry: &mut Vec<Vote>, crc: u32, data: Option<&[u8]>) {
    if let Some(vote) = entry.iter_mut().find(|vote| vote.crc == crc) {
        vote.seen = vote.seen.saturating_add(1);
        if vote.data.is_none() {
            vote.data = data.map(<[u8]>::to_vec);
        }
    } else {
        entry.push(Vote {
            crc,
            seen: 1,
            data: data.map(<[u8]>::to_vec),
        });
    }
}

/// Groups ascending sector indexes into `(first, count)` runs of at most
/// [`CHUNK_SECTORS`].
fn runs(indexes: &[u32]) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &index in indexes {
        match out.last_mut() {
            Some((first, count))
                if first.saturating_add(*count) == index && *count < CHUNK_SECTORS =>
            {
                *count += 1;
            }
            _ => out.push((index, 1)),
        }
    }
    out
}

/// CRC-32 and peak of a PCM file.
fn crc_and_peak(path: &Path) -> Result<(u32, u16)> {
    let io_error = |source| DriveError::Io {
        path: path.to_owned(),
        source,
    };
    let mut file = File::open(path).map_err(io_error)?;
    let mut hasher = crc32fast::Hasher::new();
    let mut peak = 0u16;
    let mut buffer = vec![0u8; BYTES_PER_SECTOR * CHUNK_SECTORS as usize];
    loop {
        let read = file.read(&mut buffer).map_err(io_error)?;
        let Some(chunk) = buffer.get(..read).filter(|chunk| !chunk.is_empty()) else {
            break;
        };
        hasher.update(chunk);
        peak = peak.max(peak_of(chunk));
    }
    Ok((hasher.finalize(), peak))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive::fake::FakeDrive;
    use crate::drive::{Result as DriveResult, SectorRead};
    use crate::fakedisc::{InjectedError, InjectedErrorKind};
    use crate::model::{DiscInfo, TrackNumber};
    use crate::testsupport::{FakeDiscOptions, make_fake_disc};

    fn options(mode: ReadMode, offset: i32) -> ReadOptions {
        ReadOptions {
            mode,
            offset: SampleOffset(offset),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    struct Rig {
        dir: tempfile::TempDir,
        drive: FakeDrive,
    }

    fn rig(tracks: u8, offset: i32, errors: Vec<InjectedError>) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let disc_dir = dir.path().join("disc");
        make_fake_disc(
            &disc_dir,
            &FakeDiscOptions {
                tracks,
                seconds: 2,
                read_offset: SampleOffset(offset),
                errors,
            },
        )
        .unwrap();
        let drive = FakeDrive::open(&disc_dir).unwrap();
        Rig { dir, drive }
    }

    impl Rig {
        fn read(
            &mut self,
            number: u8,
            options: &ReadOptions,
        ) -> (TrackReadResult, Vec<u8>, Vec<ReadProgress>) {
            let info = self.drive.disc_info().unwrap();
            let track = info
                .toc
                .track(TrackNumber::new(number).unwrap())
                .unwrap()
                .clone();
            let path = self.dir.path().join(format!("t{number}.pcm"));
            let mut seen = Vec::new();
            let result = read_track(
                &mut self.drive,
                &track,
                Sectors(info.toc.lead_out().0),
                options,
                &path,
                &mut |p| seen.push(p),
            )
            .unwrap();
            (result, std::fs::read(&path).unwrap(), seen)
        }

        fn truth(&self, number: u8) -> Vec<u8> {
            std::fs::read(
                self.drive
                    .disc()
                    .track_path(TrackNumber::new(number).unwrap()),
            )
            .unwrap()
        }
    }

    #[test]
    fn secure_read_with_offset_recovers_the_files_exactly() {
        let mut rig = rig(3, 6, Vec::new());
        for number in 1..=3 {
            let (result, pcm, progress) = rig.read(number, &options(ReadMode::Secure, 6));
            let truth = rig.truth(number);
            if number == 3 {
                // The last 6 samples lie past the lead-out: silence.
                assert_eq!(pcm[..pcm.len() - 24], truth[..truth.len() - 24]);
                assert!(pcm[pcm.len() - 24..].iter().all(|b| *b == 0));
            } else {
                assert_eq!(pcm, truth, "track {number}");
            }
            assert_eq!(result.copy_crc, Crc32(crc32fast::hash(&pcm)));
            assert_eq!(result.test_crc, None);
            assert!(result.is_clean());
            assert_eq!(result.samples.bytes(), truth.len() as u64);
            assert_eq!(result.peak, Peak(peak_of(&truth)));
            let last = progress.last().unwrap();
            assert_eq!(last.sectors_done, last.sectors_total);
            assert_eq!(last.pass, 1);
        }
    }

    #[test]
    fn negative_offsets_pad_the_start_of_the_disc_with_silence() {
        let mut rig = rig(2, -30, Vec::new());
        let (_, pcm, _) = rig.read(1, &options(ReadMode::Secure, -30));
        let truth = rig.truth(1);
        assert!(pcm[..120].iter().all(|b| *b == 0));
        assert_eq!(pcm[120..], truth[120..]);
        let (_, pcm, _) = rig.read(2, &options(ReadMode::Secure, -30));
        assert_eq!(pcm, rig.truth(2));
    }

    #[test]
    fn wrong_offset_gives_different_audio() {
        let mut rig = rig(2, 6, Vec::new());
        let (_, pcm, _) = rig.read(1, &options(ReadMode::Secure, 0));
        assert_ne!(pcm, rig.truth(1));
    }

    #[test]
    fn secure_mode_reports_unreadable_sectors_and_corrects_jitter() {
        let errors = vec![
            InjectedError {
                lba: Lba(40),
                kind: InjectedErrorKind::Unreadable,
            },
            InjectedError {
                lba: Lba(41),
                kind: InjectedErrorKind::Jitter,
            },
            InjectedError {
                lba: Lba(42),
                kind: InjectedErrorKind::FlakyOnce,
            },
        ];
        let mut rig = rig(1, 0, errors);
        let (result, pcm, progress) = rig.read(1, &options(ReadMode::Secure, 0));
        assert_eq!(result.unrecovered_sectors, vec![Lba(40)]);
        assert!(!result.is_clean());
        let truth = rig.truth(1);
        // Only the unreadable sector differs.
        assert_ne!(pcm[40 * 2352..41 * 2352], truth[40 * 2352..41 * 2352]);
        assert_eq!(pcm[..40 * 2352], truth[..40 * 2352]);
        assert_eq!(pcm[41 * 2352..], truth[41 * 2352..]);
        assert_eq!(progress.last().unwrap().retried_sectors, 2);
    }

    #[test]
    fn test_and_copy_matches_on_a_clean_disc() {
        let mut rig = rig(2, 6, Vec::new());
        let (result, pcm, progress) = rig.read(1, &options(ReadMode::TestAndCopy, 6));
        assert_eq!(pcm, rig.truth(1));
        assert_eq!(result.test_crc, Some(result.copy_crc));
        assert!(result.is_clean());
        assert!(progress.iter().any(|p| p.pass == 2));
    }

    #[test]
    fn test_and_copy_reports_unreadable_sectors_with_mismatching_crcs() {
        let errors = vec![InjectedError {
            lba: Lba(100),
            kind: InjectedErrorKind::Unreadable,
        }];
        let mut rig = rig(1, 0, errors);
        let (result, _, progress) = rig.read(1, &options(ReadMode::TestAndCopy, 0));
        assert_eq!(result.unrecovered_sectors, vec![Lba(100)]);
        assert_ne!(result.test_crc, Some(result.copy_crc));
        assert!(!result.is_clean());
        // The disagreeing sector was read again in later passes.
        assert!(progress.iter().any(|p| p.pass == 3));
        assert!(progress.iter().all(|p| p.pass <= 2 + MAX_REREADS as u8));
    }

    /// A drive that silently returns wrong data for one sector, the way a
    /// drive's cache can fool paranoia. Only an independent read can catch
    /// it. Each lie is different, so lies never agree with each other.
    struct LyingDrive {
        inner: FakeDrive,
        bad: Lba,
        lies_left: u32,
        lies_told: u8,
    }

    impl DiscDrive for LyingDrive {
        fn disc_info(&mut self) -> DriveResult<DiscInfo> {
            self.inner.disc_info()
        }
        fn read_sectors(
            &mut self,
            start: Lba,
            count: Sectors,
            mode: ParanoiaMode,
        ) -> DriveResult<SectorRead> {
            let mut read = self.inner.read_sectors(start, count, mode)?;
            if self.lies_left > 0 && start <= self.bad && self.bad.0 < start.0 + count.0 {
                self.lies_left -= 1;
                self.lies_told += 1;
                let at = usize::try_from(self.bad.0 - start.0).unwrap() * 2352;
                read.data[at] ^= self.lies_told;
            }
            Ok(read)
        }
        fn eject(&mut self) -> DriveResult<()> {
            self.inner.eject()
        }
    }

    fn lying(lies: u32) -> (Rig, LyingDrive) {
        let rig = rig(2, 0, Vec::new());
        let inner = FakeDrive::open(&rig.drive.disc().dir).unwrap();
        let drive = LyingDrive {
            inner,
            bad: Lba(77),
            lies_left: lies,
            lies_told: 0,
        };
        (rig, drive)
    }

    fn read_with(
        drive: &mut dyn DiscDrive,
        dir: &Path,
        mode: ReadMode,
    ) -> (TrackReadResult, Vec<u8>) {
        let info = drive.disc_info().unwrap();
        let track = info.toc.tracks()[0].clone();
        let path = dir.join("lying.pcm");
        let lead_out = Sectors(info.toc.lead_out().0);
        let result = read_track(
            drive,
            &track,
            lead_out,
            &options(mode, 0),
            &path,
            &mut |_| {},
        )
        .unwrap();
        (result, std::fs::read(&path).unwrap())
    }

    #[test]
    fn secure_mode_cannot_see_a_silent_bad_read() {
        let (rig, mut drive) = lying(1);
        let (result, pcm) = read_with(&mut drive, rig.dir.path(), ReadMode::Secure);
        assert!(result.is_clean());
        assert_ne!(pcm, rig.truth(1));
    }

    #[test]
    fn test_and_copy_catches_and_repairs_a_silent_bad_read() {
        let (rig, mut drive) = lying(1);
        let (result, pcm) = read_with(&mut drive, rig.dir.path(), ReadMode::TestAndCopy);
        assert_eq!(
            pcm,
            rig.truth(1),
            "the copy was patched with the agreed data"
        );
        assert!(result.is_clean());
        assert_eq!(result.test_crc, Some(result.copy_crc));
        assert_eq!(result.copy_crc, Crc32(crc32fast::hash(&pcm)));
    }

    #[test]
    fn test_and_copy_settles_when_both_passes_were_wrong() {
        let (rig, mut drive) = lying(2);
        // Different lies on the copy and the test; two truthful re-reads
        // agree and the copy is patched.
        let (result, pcm) = read_with(&mut drive, rig.dir.path(), ReadMode::TestAndCopy);
        assert_eq!(pcm, rig.truth(1));
        assert!(result.is_clean());
    }

    #[test]
    fn test_and_copy_reports_sectors_that_never_agree() {
        let (rig, mut drive) = lying(u32::MAX);
        let (result, _) = read_with(&mut drive, rig.dir.path(), ReadMode::TestAndCopy);
        assert_eq!(result.unrecovered_sectors, vec![Lba(77)]);
        assert_ne!(result.test_crc, Some(result.copy_crc));
        assert!(!result.is_clean());
        // Copy, test and every re-read were each told a different lie.
        assert_eq!(u32::from(drive.lies_told), 2 + MAX_REREADS);
    }

    /// A drive whose copy read of one sector silently returns wrong data
    /// and whose test read is right; every later read returns that same
    /// wrong data again, but paranoia flags it as unverified.
    struct FlaggedRereadDrive {
        inner: FakeDrive,
        bad: Lba,
        reads_of_bad: u32,
    }

    impl DiscDrive for FlaggedRereadDrive {
        fn disc_info(&mut self) -> DriveResult<DiscInfo> {
            self.inner.disc_info()
        }
        fn read_sectors(
            &mut self,
            start: Lba,
            count: Sectors,
            mode: ParanoiaMode,
        ) -> DriveResult<SectorRead> {
            let mut read = self.inner.read_sectors(start, count, mode)?;
            if start <= self.bad && self.bad.0 < start.0 + count.0 {
                let at = usize::try_from(self.bad.0 - start.0).unwrap() * 2352;
                match self.reads_of_bad {
                    0 => read.data[at] ^= 0x55, // the copy: silently wrong
                    1 => {}                     // the test: right
                    _ => {
                        // Re-reads: the copy's wrong data, flagged.
                        read.data[at] ^= 0x55;
                        read.unrecovered.push(self.bad);
                    }
                }
                self.reads_of_bad += 1;
            }
            Ok(read)
        }
        fn eject(&mut self) -> DriveResult<()> {
            self.inner.eject()
        }
    }

    #[test]
    fn a_flagged_reread_never_settles_a_sector() {
        let rig = rig(2, 0, Vec::new());
        let mut drive = FlaggedRereadDrive {
            inner: FakeDrive::open(&rig.drive.disc().dir).unwrap(),
            bad: Lba(77),
            reads_of_bad: 0,
        };
        let (result, pcm) = read_with(&mut drive, rig.dir.path(), ReadMode::TestAndCopy);
        // The flagged re-reads agree with the bad copy, but they are only
        // guesses: the sector is reported, never passed as clean.
        assert_ne!(pcm, rig.truth(1));
        assert!(!result.is_clean());
        assert_eq!(result.unrecovered_sectors, vec![Lba(77)]);
        assert_ne!(result.test_crc, Some(result.copy_crc));
        // Every re-read round was tried.
        assert_eq!(drive.reads_of_bad, 2 + MAX_REREADS);
    }

    #[test]
    fn a_flagged_reread_is_reported_even_when_clean_reads_settle() {
        // Copy wrong, test right, first re-read flagged (wrong), later
        // re-reads right: the clean reads settle and patch the copy, and
        // the flagged look is still reported.
        struct Drive {
            inner: FakeDrive,
            reads_of_bad: u32,
        }
        impl DiscDrive for Drive {
            fn disc_info(&mut self) -> DriveResult<DiscInfo> {
                self.inner.disc_info()
            }
            fn read_sectors(
                &mut self,
                start: Lba,
                count: Sectors,
                mode: ParanoiaMode,
            ) -> DriveResult<SectorRead> {
                let mut read = self.inner.read_sectors(start, count, mode)?;
                if start.0 <= 77 && 77 < start.0 + count.0 {
                    let at = usize::try_from(77 - start.0).unwrap() * 2352;
                    if self.reads_of_bad == 0 || self.reads_of_bad == 2 {
                        read.data[at] ^= 0x55;
                    }
                    if self.reads_of_bad == 2 {
                        read.unrecovered.push(Lba(77));
                    }
                    self.reads_of_bad += 1;
                }
                Ok(read)
            }
            fn eject(&mut self) -> DriveResult<()> {
                self.inner.eject()
            }
        }
        let rig = rig(2, 0, Vec::new());
        let mut drive = Drive {
            inner: FakeDrive::open(&rig.drive.disc().dir).unwrap(),
            reads_of_bad: 0,
        };
        let (result, pcm) = read_with(&mut drive, rig.dir.path(), ReadMode::TestAndCopy);
        assert_eq!(pcm, rig.truth(1), "patched with the verified data");
        assert_eq!(result.unrecovered_sectors, vec![Lba(77)]);
        assert!(!result.is_clean());
    }

    #[test]
    fn cancelling_stops_the_read_and_removes_the_file() {
        let mut rig = rig(1, 0, Vec::new());
        let info = rig.drive.disc_info().unwrap();
        let track = info.toc.tracks()[0].clone();
        let path = rig.dir.path().join("cancelled.pcm");
        let options = options(ReadMode::Secure, 0);
        let cancel = options.cancel.clone();
        let outcome = read_track(
            &mut rig.drive,
            &track,
            Sectors(info.toc.lead_out().0),
            &options,
            &path,
            &mut |_| cancel.store(true, Ordering::Relaxed),
        );
        assert!(matches!(outcome, Err(DriveError::Cancelled)));
        assert!(!path.exists());
    }

    #[test]
    fn runs_group_consecutive_sectors() {
        assert_eq!(runs(&[1, 2, 3, 7, 9, 10]), vec![(1, 3), (7, 1), (9, 2)]);
        let long: Vec<u32> = (0..80).collect();
        assert_eq!(runs(&long), vec![(0, 75), (75, 5)]);
    }

    #[test]
    fn drive_sectors_follow_the_offset_and_stay_on_the_disc() {
        let geometry = Geometry {
            drive_start: 10 * SECTOR_BYTES + 24,
            track_bytes: 5 * SECTOR_BYTES,
            disc_bytes: 15 * SECTOR_BYTES,
        };
        let sectors: Vec<Lba> = geometry.drive_sectors(0, SECTOR_BYTES).collect();
        assert_eq!(sectors, vec![Lba(10), Lba(11)]);
        let sectors: Vec<Lba> = geometry
            .drive_sectors(4 * SECTOR_BYTES, 5 * SECTOR_BYTES)
            .collect();
        assert_eq!(sectors, vec![Lba(14)]);
    }
}
