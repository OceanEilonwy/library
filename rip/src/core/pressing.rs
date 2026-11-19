// SPDX-License-Identifier: AGPL-3.0-or-later
//! Matching other pressings of the same album.
//!
//! Different pressings (manufacturing runs) of an album often hold exactly
//! the same audio, positioned a little differently on the disc. AccurateRip
//! and the CUETools database keep the checksums each pressing's rippers
//! sent, so a perfect rip of our pressing does not match another pressing's
//! entries at our own position, only at a shift. Following CUETools, every
//! track's AccurateRip v1 and v2 checksums are worked out at every shift in
//! `-max..=max` samples ([`DEFAULT_MAX_SHIFT`] = 5 × 588 − 1 = 2,939, the
//! largest shift for which the first and last tracks' skipped edges keep
//! every counted sample inside our own audio), and the CTDB disc CRC at
//! every shift as well. The neighbouring samples a shift pulls into a
//! track come from the adjacent tracks' saved PCM files; [`DiscAudio`]
//! streams them from disk, never holding more than a chunk in memory.
//!
//! **Sign.** A pressing offset of `+k` means that pressing's track (or
//! disc) sample `i` is our disc sample `i + k`: its audio sits `k` samples
//! later on its disc than on ours.
//!
//! # AccurateRip v1 at every shift, in constant time per shift
//!
//! For a track starting at disc sample `s`, with counted multipliers
//! `a..=b` (see [`TrackPosition`]) and `y(j)` the disc sample `j` read as a
//! little-endian `u32`, the checksum sums at shift `k` are
//!
//! ```text
//! P(k) = Σ_{m=a..b} m · y(s + m − 1 + k)        (exact, as an integer)
//! S(k) = Σ_{m=a..b}     y(s + m − 1 + k)
//! v1(k) = P(k) mod 2³²
//! ```
//!
//! Moving from `k` to `k + 1` renumbers every multiplier down by one, drops
//! the sample leaving at the front and takes in the one entering at the back.
//! With `y_out = y(s + a − 1 + k)` and `y_in = y(s + b + k)`:
//!
//! ```text
//! S(k+1) = S(k) − y_out + y_in
//! P(k+1) = P(k) − a · y_out + (b + 1) · y_in − S(k+1)
//! ```
//!
//! so after one pass over the track for `P(−max)` and `S(−max)`, each
//! further shift costs a few operations ([`ShiftScan`]).
//!
//! # AccurateRip v2: no constant-time form, so an exact filter
//!
//! v2 adds the high halves of the 64-bit products: with `m·y = lo + 2³²·hi`,
//! `L = Σ lo`, `H = Σ hi`, we have `P = L + 2³²·H` and
//! `v2 = (L + H) mod 2³² = (v1 + H) mod 2³²`. `H` is a sum of
//! floors, which does not follow the window as it slides (each sample's
//! `hi` changes with its multiplier in a way that depends on its value), so
//! v2 has no constant-time update. But `H` is pinned down tightly: `L ≡ P`
//! (mod 2³²) and `0 ≤ L ≤ Z·(2³² − 1)`, where `Z` is the number of non-zero
//! samples in the window (also kept up to date per shift), so
//!
//! ```text
//! H = ⌊P / 2³²⌋ − j   for some 0 ≤ j ≤ J = ⌊(Z·(2³² − 1) − v1) / 2³²⌋
//! ```
//!
//! For a database checksum `T`, `v2(k) = T` needs `j ≡ v1 + ⌊P/2³²⌋ − T`
//! (mod 2³²) with `j ≤ J < 2³²`: one comparison per shift and checksum.
//! The test never misses a true match; it lets through a false one with a
//! chance of about `Z / 2³²` (under 0.3 % for a five-minute track), and
//! each shift it lets through is then checksummed exactly in a single
//! streaming pass shared by all such shifts ([`crcs_at_shifts`]). Only exact
//! matches count. Measured (release build, a five-minute track of noise, six
//! database checksums): the scan of all 5,879 shifts takes about 20 ms and
//! lets through about 100 shifts, whose exact check takes about 0.5 s; the
//! CTDB rolling search over the whole disc about 15 ms.
//!
//! # CTDB at every shift: a rolling CRC-32
//!
//! The CTDB disc CRC is a CRC-32 of a fixed-length window of the disc's
//! bytes. CRC-32 is linear over GF(2): with `R(I, M)` the register after
//! message `M` from register `I`, `R(I, M) = Z_L(I) ⊕ R(0, M)` where `Z_L`
//! runs a register through `L = |M|` zero bytes. Sliding the window one byte,
//! `R(0, b₁…b_L) = step(R(0, b₀…b_{L−1}), b_L) ⊕ Z_L(table[b₀])`, so with
//! `Z_L` worked out once (32 × 32 bit matrices, squared up to `L`) every
//! byte of shift costs one table step and one look-up.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::accuraterip::{DatabaseEntry, TrackPosition};
use super::ctdb::{CtdbEntry, EDGE_SAMPLES};
use super::{CoreError, Result};
use crate::model::{
    AccurateRipCrc, AccurateRipCrcs, AccurateRipStatus, AccurateRipVersion, BYTES_PER_SAMPLE,
    CtdbStatus, Pcm, SAMPLES_PER_SECTOR, SampleOffset, Samples, TrackNumber,
};

/// The default search window: five sectors less one sample either way, as
/// CUETools searches (`_arOffsetRange = 5 * 588 - 1`).
pub const DEFAULT_MAX_SHIFT: u32 = 5 * SAMPLES_PER_SECTOR - 1;

/// Samples read from disk at a time.
const CHUNK_SAMPLES: usize = 1 << 16;

/// Bytes per stereo sample as `i64`, for signed position maths.
const SAMPLE_BYTES: i64 = BYTES_PER_SAMPLE as i64;

// ---------------------------------------------------------------------------
// The disc's audio, streamed from the track files
// ---------------------------------------------------------------------------

/// Where one track's samples are.
#[derive(Debug)]
enum Source<'a> {
    Memory(&'a [u8]),
    File { path: &'a Path, file: Option<File> },
}

/// The audio tracks of a rip in disc order, seen as one run of stereo
/// samples that is silent before the first and after the last. Reads go
/// straight to the per-track PCM (files are opened once and kept open).
#[derive(Debug)]
pub struct DiscAudio<'a> {
    tracks: Vec<Source<'a>>,
    /// Disc sample index of each track's first sample, then the total.
    starts: Vec<u64>,
}

impl<'a> DiscAudio<'a> {
    /// The audio of `tracks` (each the PCM and its length in samples).
    pub fn new(tracks: &[(&'a Pcm, Samples)]) -> Result<Self> {
        let mut sources = Vec::with_capacity(tracks.len());
        let mut starts = Vec::with_capacity(tracks.len() + 1);
        let mut total = 0u64;
        starts.push(0);
        for (pcm, samples) in tracks {
            match pcm {
                Pcm::Memory(bytes) => {
                    if u64::try_from(bytes.len()).ok() != Some(samples.bytes()) {
                        return Err(CoreError::Invalid(format!(
                            "a track should hold {} bytes of audio but holds {}",
                            samples.bytes(),
                            bytes.len()
                        )));
                    }
                    sources.push(Source::Memory(bytes));
                }
                Pcm::File(path) => sources.push(Source::File { path, file: None }),
            }
            total = total.checked_add(samples.0).ok_or_else(too_long)?;
            starts.push(total);
        }
        Ok(DiscAudio {
            tracks: sources,
            starts,
        })
    }

    /// Number of audio tracks.
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Samples in all tracks together.
    pub fn total_samples(&self) -> u64 {
        self.starts.last().copied().unwrap_or(0)
    }

    /// Disc sample index of track `index`'s first sample.
    fn track_start(&self, index: usize) -> Result<u64> {
        self.starts.get(index).copied().ok_or_else(no_track)
    }

    /// Samples in track `index`.
    fn track_samples(&self, index: usize) -> Result<u64> {
        let end = self.starts.get(index + 1).copied().ok_or_else(no_track)?;
        Ok(end - self.track_start(index)?)
    }

    /// Fills `out` (a whole number of samples) with the disc's bytes from
    /// sample `from` on, silence outside the audio.
    pub fn read_bytes(&mut self, from: i64, out: &mut [u8]) -> Result<()> {
        if !out.len().is_multiple_of(BYTES_PER_SAMPLE) {
            return Err(CoreError::Invalid(
                "audio is read in whole stereo samples".into(),
            ));
        }
        out.fill(0);
        let count = i64::try_from(out.len() / BYTES_PER_SAMPLE).map_err(|_| too_long())?;
        let end = from.checked_add(count).ok_or_else(too_long)?;
        for (index, source) in self.tracks.iter_mut().enumerate() {
            let (Some(start), Some(stop)) = (self.starts.get(index), self.starts.get(index + 1))
            else {
                continue;
            };
            let start = i64::try_from(*start).map_err(|_| too_long())?;
            let stop = i64::try_from(*stop).map_err(|_| too_long())?;
            let first = from.max(start);
            let last = end.min(stop);
            if first >= last {
                continue;
            }
            let window = byte_range(first - from, last - from)
                .and_then(|(a, b)| out.get_mut(a..b))
                .ok_or_else(too_long)?;
            let in_track = u64::try_from(first - start).map_err(|_| too_long())?;
            match source {
                Source::Memory(bytes) => {
                    let offset = usize::try_from(in_track)
                        .ok()
                        .and_then(|i| i.checked_mul(BYTES_PER_SAMPLE))
                        .ok_or_else(too_long)?;
                    let part = bytes
                        .get(offset..offset + window.len())
                        .ok_or_else(too_long)?;
                    window.copy_from_slice(part);
                }
                Source::File { path, file } => {
                    let io = |source| CoreError::Io {
                        path: path.to_path_buf(),
                        source,
                    };
                    let file = match file {
                        Some(file) => file,
                        slot @ None => slot.insert(File::open(*path).map_err(io)?),
                    };
                    file.seek(SeekFrom::Start(in_track * BYTES_PER_SAMPLE as u64))
                        .map_err(io)?;
                    file.read_exact(window).map_err(io)?;
                }
            }
        }
        Ok(())
    }

    /// Fills `out` with samples (as little-endian `u32`s) from sample `from`.
    pub fn read_samples(&mut self, from: i64, out: &mut [u32]) -> Result<()> {
        let mut bytes = vec![0u8; out.len() * BYTES_PER_SAMPLE];
        self.read_bytes(from, &mut bytes)?;
        let (chunks, _) = bytes.as_chunks::<4>();
        for (slot, chunk) in out.iter_mut().zip(chunks) {
            *slot = u32::from_le_bytes(*chunk);
        }
        Ok(())
    }

    /// Calls `visit(first_index, samples)` for disc samples `from..to` in
    /// order, a chunk at a time.
    fn for_each_chunk(
        &mut self,
        from: i64,
        to: i64,
        mut visit: impl FnMut(i64, &[u32]) -> Result<()>,
    ) -> Result<()> {
        let mut buffer = vec![0u32; CHUNK_SAMPLES];
        let mut position = from;
        while position < to {
            let count = usize::try_from(to - position)
                .map_err(|_| too_long())?
                .min(CHUNK_SAMPLES);
            let chunk = buffer.get_mut(..count).ok_or_else(too_long)?;
            self.read_samples(position, chunk)?;
            visit(position, chunk)?;
            position += i64::try_from(count).map_err(|_| too_long())?;
        }
        Ok(())
    }

    /// Calls `visit` with the disc's bytes for samples `from..to`.
    fn for_each_byte_chunk(
        &mut self,
        from: i64,
        to: i64,
        mut visit: impl FnMut(&[u8]),
    ) -> Result<()> {
        let mut buffer = vec![0u8; CHUNK_SAMPLES * BYTES_PER_SAMPLE];
        let mut position = from;
        while position < to {
            let count = usize::try_from(to - position)
                .map_err(|_| too_long())?
                .min(CHUNK_SAMPLES);
            let chunk = buffer
                .get_mut(..count * BYTES_PER_SAMPLE)
                .ok_or_else(too_long)?;
            self.read_bytes(position, chunk)?;
            visit(chunk);
            position += i64::try_from(count).map_err(|_| too_long())?;
        }
        Ok(())
    }
}

/// `[a, b)` sample positions as a byte range.
fn byte_range(a: i64, b: i64) -> Option<(usize, usize)> {
    let a = usize::try_from(a).ok()?.checked_mul(BYTES_PER_SAMPLE)?;
    let b = usize::try_from(b).ok()?.checked_mul(BYTES_PER_SAMPLE)?;
    Some((a, b))
}

// ---------------------------------------------------------------------------
// AccurateRip
// ---------------------------------------------------------------------------

/// Where a track's counted samples are: multipliers `a..=b`, the track's
/// first disc sample `start`.
#[derive(Copy, Clone, Debug)]
struct Counted {
    a: i64,
    b: i64,
    start: i64,
}

fn counted(audio: &DiscAudio<'_>, index: usize) -> Result<Counted> {
    let position = TrackPosition::for_track(index, audio.track_count()).ok_or_else(no_track)?;
    let (a, b) = position.counted(audio.track_samples(index)?);
    Ok(Counted {
        a: i64::try_from(a).map_err(|_| too_long())?,
        b: i64::try_from(b).map_err(|_| too_long())?,
        start: i64::try_from(audio.track_start(index)?).map_err(|_| too_long())?,
    })
}

/// A track's checksum sums at one shift, as [`ShiftScan`] keeps them.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ShiftSums {
    /// The shift.
    pub shift: i64,
    /// `P = Σ m·y`, exact.
    pub weighted: u128,
    /// Non-zero samples in the window.
    pub nonzero: u64,
}

impl ShiftSums {
    /// The v1 checksum at this shift (exact).
    pub fn v1(&self) -> u32 {
        low_u32(self.weighted)
    }

    /// False when the v2 checksum at this shift certainly is not `target`;
    /// true when it may be (see the module documentation).
    pub fn may_have_v2(&self, target: u32) -> bool {
        let v1 = self.v1();
        let high = self.weighted >> 32;
        let j0 = v1.wrapping_add(low_u32(high)).wrapping_sub(target);
        // J = ⌊(Z·(2³² − 1) − v1) / 2³²⌋.
        let ceiling = u128::from(self.nonzero) * u128::from(u32::MAX);
        let limit = ceiling.saturating_sub(u128::from(v1)) >> 32;
        u128::from(j0) <= limit
    }
}

/// The low 32 bits of a wide integer.
fn low_u32(value: u128) -> u32 {
    let bytes = value.to_le_bytes();
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Visits a track's [`ShiftSums`] at every shift in `-max..=max`, in
/// order, with one pass over the track and constant work per shift.
pub fn scan_shifts(
    audio: &mut DiscAudio<'_>,
    index: usize,
    max_shift: u32,
    mut visit: impl FnMut(&ShiftSums),
) -> Result<()> {
    let Counted { a, b, start } = counted(audio, index)?;
    if a > b {
        return Ok(());
    }
    let max = i64::from(max_shift);
    // P, S and Z at shift −max: samples s + a − 1 − max ..= s + b − 1 − max.
    let mut weighted = 0u128;
    let mut plain = 0u128;
    let mut nonzero = 0u64;
    let first = start + a - 1 - max;
    audio.for_each_chunk(first, first + (b - a + 1), |at, chunk| {
        let from = u128::try_from(at - first + a).map_err(|_| too_long())?;
        for (multiplier, value) in (from..).zip(chunk) {
            weighted += multiplier * u128::from(*value);
            plain += u128::from(*value);
            nonzero += u64::from(*value != 0);
        }
        Ok(())
    })?;
    // The samples leaving (y(s + a − 1 + k)) and entering (y(s + b + k))
    // for k = −max .. max − 1.
    let steps = usize::try_from(2 * max).map_err(|_| too_long())?;
    let mut leaving = vec![0u32; steps];
    let mut entering = vec![0u32; steps];
    audio.read_samples(start + a - 1 - max, &mut leaving)?;
    audio.read_samples(start + b - max, &mut entering)?;
    let a_wide = u128::try_from(a).map_err(|_| too_long())?;
    let after_b = u128::try_from(b + 1).map_err(|_| too_long())?;
    let mut sums = ShiftSums {
        shift: -max,
        weighted,
        nonzero,
    };
    visit(&sums);
    for (out, into) in leaving.iter().zip(&entering) {
        let (out, into) = (u128::from(*out), u128::from(*into));
        plain = plain + into - out;
        // P − a·y_out ≥ 0 (that term is part of P), and the result is a sum
        // of non-negative terms again, so the order below never underflows.
        sums.weighted = sums.weighted - a_wide * out + after_b * into - plain;
        sums.nonzero = sums.nonzero - u64::from(out != 0) + u64::from(into != 0);
        sums.shift += 1;
        visit(&sums);
    }
    Ok(())
}

/// Track `index`'s v1 and v2 checksums at each of `shifts`, computed
/// directly in one streaming pass over the samples all of them need.
pub fn crcs_at_shifts(
    audio: &mut DiscAudio<'_>,
    index: usize,
    shifts: &[i64],
) -> Result<Vec<AccurateRipCrcs>> {
    let Counted { a, b, start } = counted(audio, index)?;
    let mut sums = vec![(0u32, 0u32); shifts.len()];
    if a <= b
        && let (Some(lowest), Some(highest)) = (shifts.iter().min(), shifts.iter().max())
    {
        let from = start + a - 1 + lowest;
        let to = start + b + highest;
        audio.for_each_chunk(from, to, |at, chunk| {
            let length = i64::try_from(chunk.len()).map_err(|_| too_long())?;
            for (shift, (v1, v2)) in shifts.iter().zip(sums.iter_mut()) {
                // Sample `at + i` has multiplier `at + i − start + 1 − shift`.
                let base = at - start + 1 - shift;
                let first = (a - base).clamp(0, length);
                let last = (b - base + 1).clamp(0, length);
                let (Ok(first), Ok(last)) = (usize::try_from(first), usize::try_from(last)) else {
                    continue;
                };
                let from = u64::try_from(base + i64::try_from(first).map_err(|_| too_long())?)
                    .map_err(|_| too_long())?;
                for (multiplier, value) in (from..).zip(chunk.get(first..last).unwrap_or_default())
                {
                    let product = multiplier * u64::from(*value);
                    let low = low_u32(u128::from(product));
                    let high = low_u32(u128::from(product >> 32));
                    *v1 = v1.wrapping_add(low);
                    *v2 = v2.wrapping_add(low).wrapping_add(high);
                }
            }
            Ok(())
        })?;
    }
    Ok(sums
        .into_iter()
        .map(|(v1, v2)| AccurateRipCrcs {
            v1: AccurateRipCrc(v1),
            v2: AccurateRipCrc(v2),
        })
        .collect())
}

/// A track's best match with another pressing.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PressingMatch {
    /// The pressing offset (non-zero).
    pub shift: i32,
    /// v2 when any v2 checksum matched at this shift.
    pub version: AccurateRipVersion,
    /// Sum of the confidence of the matching entries.
    pub confidence: u32,
}

/// Searches track `index` for matches with `entries` at every non-zero
/// shift up to `max_shift` either way, and returns the best: the highest
/// confidence, then the smallest shift (positive first on a tie).
pub fn search_track(
    audio: &mut DiscAudio<'_>,
    index: usize,
    entries: &[DatabaseEntry],
    max_shift: u32,
) -> Result<Option<PressingMatch>> {
    // Checksum → total confidence, for this track of every entry with our
    // track count. Zero checksums mean "no data".
    let count = audio.track_count();
    let mut targets: Vec<(u32, u32)> = Vec::new();
    for entry in entries.iter().filter(|e| e.tracks.len() == count) {
        if let Some(track) = entry.tracks.get(index).filter(|t| t.crc != 0) {
            match targets.iter_mut().find(|(crc, _)| *crc == track.crc) {
                Some((_, confidence)) => *confidence += u32::from(track.confidence),
                None => targets.push((track.crc, u32::from(track.confidence))),
            }
        }
    }
    if targets.is_empty() {
        return Ok(None);
    }
    let mut v1_hits: Vec<(i64, u32)> = Vec::new();
    let mut maybe_v2: Vec<i64> = Vec::new();
    scan_shifts(audio, index, max_shift, |sums| {
        if sums.shift == 0 {
            return;
        }
        let v1 = sums.v1();
        for (crc, confidence) in &targets {
            if *crc == v1 {
                v1_hits.push((sums.shift, *confidence));
            }
        }
        if targets.iter().any(|(crc, _)| sums.may_have_v2(*crc)) {
            maybe_v2.push(sums.shift);
        }
    })?;
    let exact = crcs_at_shifts(audio, index, &maybe_v2)?;
    let mut v2_hits: Vec<(i64, u32)> = Vec::new();
    for (shift, crcs) in maybe_v2.iter().zip(&exact) {
        for (crc, confidence) in &targets {
            if *crc == crcs.v2.0 {
                v2_hits.push((*shift, *confidence));
            }
        }
    }
    let mut best: Option<PressingMatch> = None;
    let mut shifts: Vec<i64> = v1_hits.iter().chain(&v2_hits).map(|(s, _)| *s).collect();
    shifts.sort_unstable();
    shifts.dedup();
    for shift in shifts {
        let total = |hits: &[(i64, u32)]| -> u32 {
            hits.iter()
                .filter(|(s, _)| *s == shift)
                .map(|(_, c)| *c)
                .sum()
        };
        let (version, confidence) = match total(&v2_hits) {
            0 => (AccurateRipVersion::V1, total(&v1_hits)),
            v2 => (AccurateRipVersion::V2, v2),
        };
        let candidate = PressingMatch {
            shift: i32::try_from(shift).map_err(|_| too_long())?,
            version,
            confidence,
        };
        let better = match best {
            None => true,
            Some(current) => {
                (
                    candidate.confidence,
                    std::cmp::Reverse(candidate.shift.unsigned_abs()),
                ) > (
                    current.confidence,
                    std::cmp::Reverse(current.shift.unsigned_abs()),
                ) || (candidate.confidence == current.confidence
                    && candidate.shift.unsigned_abs() == current.shift.unsigned_abs()
                    && candidate.shift > current.shift)
            }
        };
        if better {
            best = Some(candidate);
        }
    }
    Ok(best)
}

/// Turns the statuses at our own position into the final ones: every track
/// that is not accurate at `+0` is searched for another pressing, and a
/// match there makes it accurate with that pressing offset. A match with
/// this pressing always wins over one with another, whatever the
/// confidences: it is the more direct proof.
pub fn verify_with_pressings(
    audio: &mut DiscAudio<'_>,
    statuses: Vec<(TrackNumber, AccurateRipStatus)>,
    entries: &[DatabaseEntry],
    max_shift: u32,
) -> Result<Vec<(TrackNumber, AccurateRipStatus)>> {
    if max_shift == 0 || entries.is_empty() {
        return Ok(statuses);
    }
    let mut out = Vec::with_capacity(statuses.len());
    for (index, (number, status)) in statuses.into_iter().enumerate() {
        let status = match status {
            AccurateRipStatus::Mismatch { .. } => {
                match search_track(audio, index, entries, max_shift)? {
                    Some(found) => AccurateRipStatus::Accurate {
                        version: found.version,
                        confidence: found.confidence,
                        pressing_offset: SampleOffset(found.shift),
                    },
                    None => status,
                }
            }
            AccurateRipStatus::Accurate { .. }
            | AccurateRipStatus::NotInDatabase
            | AccurateRipStatus::Unavailable { .. } => status,
        };
        out.push((number, status));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// CTDB
// ---------------------------------------------------------------------------

/// The reflected CRC-32 polynomial (IEEE, as in zip and `crc32fast`).
const POLYNOMIAL: u32 = 0xEDB8_8320;

/// The byte-at-a-time table of the reflected CRC-32.
fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    for (byte, slot) in (0u32..).zip(table.iter_mut()) {
        let mut value = byte;
        for _ in 0..8 {
            value = if value & 1 == 1 {
                (value >> 1) ^ POLYNOMIAL
            } else {
                value >> 1
            };
        }
        *slot = value;
    }
    table
}

/// One table step of the register with `byte`.
fn crc_step(table: &[u32; 256], register: u32, byte: u8) -> u32 {
    let index = usize::from(low_byte(register) ^ byte);
    (register >> 8) ^ table.get(index).copied().unwrap_or(0)
}

fn low_byte(value: u32) -> u8 {
    value.to_le_bytes()[0]
}

/// A linear map on 32-bit registers over GF(2): column `i` is the image of
/// bit `i`.
#[derive(Copy, Clone)]
struct Gf2Matrix([u32; 32]);

impl Gf2Matrix {
    fn apply(&self, vector: u32) -> u32 {
        let mut out = 0;
        for (bit, column) in self.0.iter().enumerate() {
            if (vector >> bit) & 1 == 1 {
                out ^= column;
            }
        }
        out
    }

    /// `self ∘ other` (first `other`, then `self`).
    fn compose(&self, other: &Gf2Matrix) -> Gf2Matrix {
        let mut out = [0u32; 32];
        for (slot, column) in out.iter_mut().zip(other.0) {
            *slot = self.apply(column);
        }
        Gf2Matrix(out)
    }

    /// The register after `count` zero bytes, as a matrix.
    fn zero_bytes(table: &[u32; 256], count: u64) -> Gf2Matrix {
        let mut one = [0u32; 32];
        for (bit, slot) in one.iter_mut().enumerate() {
            *slot = crc_step(table, 1u32 << bit, 0);
        }
        let mut power = Gf2Matrix(one);
        let mut result = Gf2Matrix(std::array::from_fn(|bit| 1u32 << bit));
        let mut left = count;
        while left > 0 {
            if left & 1 == 1 {
                result = power.compose(&result);
            }
            power = power.compose(&power);
            left >>= 1;
        }
        result
    }
}

/// The CTDB disc CRC (see [`super::ctdb`]) of the disc shifted by `shift`
/// samples, at every shift in `-max..=max`, in order: one pass for the
/// first window, then a rolling update.
pub fn ctdb_crcs_at_shifts(
    audio: &mut DiscAudio<'_>,
    max_shift: u32,
    mut visit: impl FnMut(i64, u32),
) -> Result<()> {
    let total = i64::try_from(audio.total_samples()).map_err(|_| too_long())?;
    let edge = i64::try_from(EDGE_SAMPLES).map_err(|_| too_long())?;
    let window = total - 2 * edge;
    if window <= 0 {
        return Ok(());
    }
    let max = i64::from(max_shift);
    let window_bytes = u64::try_from(window * SAMPLE_BYTES).map_err(|_| too_long())?;
    let table = crc_table();
    let zero = Gf2Matrix::zero_bytes(&table, window_bytes);
    // crc32(M) = R(I, M) ⊕ !0 = R(0, M) ⊕ Z_L(I) ⊕ !0 with I = !0.
    let constant = zero.apply(u32::MAX) ^ u32::MAX;
    let mut hasher = crc32fast::Hasher::new();
    audio.for_each_byte_chunk(edge - max, edge - max + window, |bytes| {
        hasher.update(bytes)
    })?;
    let mut register = hasher.finalize() ^ constant;
    let mut dropped = [0u32; 256];
    for (slot, value) in dropped.iter_mut().zip(table) {
        *slot = zero.apply(value);
    }
    let steps = usize::try_from(2 * max).map_err(|_| too_long())?;
    let mut leaving = vec![0u8; steps * BYTES_PER_SAMPLE];
    let mut entering = vec![0u8; steps * BYTES_PER_SAMPLE];
    audio.read_bytes(edge - max, &mut leaving)?;
    audio.read_bytes(edge - max + window, &mut entering)?;
    let mut shift = -max;
    visit(shift, register ^ constant);
    let (out_samples, _) = leaving.as_chunks::<4>();
    let (in_samples, _) = entering.as_chunks::<4>();
    for (out, into) in out_samples.iter().zip(in_samples) {
        for (old, new) in out.iter().zip(into) {
            register = crc_step(&table, register, *new)
                ^ dropped.get(usize::from(*old)).copied().unwrap_or(0);
        }
        shift += 1;
        visit(shift, register ^ constant);
    }
    Ok(())
}

/// The CTDB result at our own position, or, when that is a mismatch, the
/// best match with another pressing within `max_shift` samples (most
/// confidence, then the smallest shift). Shifts beyond the CTDB edge (10
/// sectors) are not searched: they would need audio we do not have.
pub fn ctdb_with_pressings(
    audio: &mut DiscAudio<'_>,
    status: CtdbStatus,
    entries: &[CtdbEntry],
    max_shift: u32,
) -> Result<CtdbStatus> {
    let CtdbStatus::Mismatch { total } = status else {
        return Ok(status);
    };
    let max = max_shift.min(u32::try_from(EDGE_SAMPLES).unwrap_or(u32::MAX));
    if max == 0 {
        return Ok(status);
    }
    let mut best: Option<(u32, i64)> = None;
    ctdb_crcs_at_shifts(audio, max, |shift, crc| {
        if shift == 0 {
            return;
        }
        for entry in entries.iter().filter(|e| e.crc32 == crc) {
            let better = match best {
                None => true,
                Some((confidence, at)) => {
                    entry.confidence > confidence
                        || (entry.confidence == confidence
                            && (shift.unsigned_abs(), std::cmp::Reverse(shift))
                                < (at.unsigned_abs(), std::cmp::Reverse(at)))
                }
            };
            if better {
                best = Some((entry.confidence, shift));
            }
        }
    })?;
    Ok(match best {
        Some((confidence, shift)) => CtdbStatus::Accurate {
            confidence,
            total,
            pressing_offset: SampleOffset(i32::try_from(shift).map_err(|_| too_long())?),
        },
        None => status,
    })
}

fn too_long() -> CoreError {
    CoreError::Invalid("the audio is longer than any CD".into())
}

fn no_track() -> CoreError {
    CoreError::Invalid("there is no such track".into())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::core::accuraterip::{DatabaseTrack, track_crcs, verify};
    use crate::model::AccurateRipDiscId;

    /// Deterministic pseudo-random PCM (xorshift), `samples` long.
    fn noise(seed: u32, samples: usize) -> Vec<u8> {
        let mut x = seed.max(1);
        let mut out = Vec::with_capacity(samples * 4);
        for _ in 0..samples {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            out.extend_from_slice(&x.to_le_bytes());
        }
        out
    }

    /// Splits `disc` into tracks of the given sample counts.
    fn split(disc: &[u8], lengths: &[usize]) -> Vec<(Pcm, Samples)> {
        let mut at = 0;
        lengths
            .iter()
            .map(|n| {
                let part: Arc<[u8]> = disc[at * 4..(at + n) * 4].into();
                at += n;
                (Pcm::Memory(part), Samples(*n as u64))
            })
            .collect()
    }

    fn refs(tracks: &[(Pcm, Samples)]) -> Vec<(&Pcm, Samples)> {
        tracks.iter().map(|(p, s)| (p, *s)).collect()
    }

    /// Track `index` of `disc` as another pressing at `shift` would hold it:
    /// its sample `i` is our disc sample `start + i + shift`, silence
    /// beyond our audio.
    fn shifted_track(disc: &[u8], lengths: &[usize], index: usize, shift: i64) -> Vec<u8> {
        let start: usize = lengths[..index].iter().sum();
        let total = (disc.len() / 4) as i64;
        let mut out = Vec::with_capacity(lengths[index] * 4);
        for i in 0..lengths[index] as i64 {
            let j = start as i64 + i + shift;
            if (0..total).contains(&j) {
                out.extend_from_slice(&disc[j as usize * 4..j as usize * 4 + 4]);
            } else {
                out.extend_from_slice(&[0; 4]);
            }
        }
        out
    }

    fn database(per_track: &[u32], confidence: u8, count: u8) -> DatabaseEntry {
        DatabaseEntry {
            disc: AccurateRipDiscId {
                track_count: count,
                id1: 1,
                id2: 2,
                cddb: 3,
            },
            tracks: per_track
                .iter()
                .map(|crc| DatabaseTrack {
                    confidence,
                    crc: *crc,
                    frame450_crc: 0,
                })
                .collect(),
        }
    }

    const LENGTHS: [usize; 3] = [9 * 588 + 17, 7 * 588, 10 * 588 + 101];

    #[test]
    fn incremental_sums_equal_brute_force_at_every_shift() {
        let disc = noise(42, LENGTHS.iter().sum());
        let tracks = split(&disc, &LENGTHS);
        let mut audio = DiscAudio::new(&refs(&tracks)).unwrap();
        let max = 300u32;
        for (index, length) in LENGTHS.iter().enumerate() {
            let position = TrackPosition::for_track(index, 3).unwrap();
            let mut seen = Vec::new();
            scan_shifts(&mut audio, index, max, |sums| seen.push(*sums)).unwrap();
            assert_eq!(seen.len(), 601);
            let shifts: Vec<i64> = seen.iter().map(|s| s.shift).collect();
            let direct = crcs_at_shifts(&mut audio, index, &shifts).unwrap();
            for (sums, direct) in seen.iter().zip(&direct) {
                let pcm = shifted_track(&disc, &LENGTHS, index, sums.shift);
                let brute = track_crcs(&pcm, position).unwrap();
                assert_eq!(*direct, brute, "track {index} shift {}", sums.shift);
                assert_eq!(sums.v1(), brute.v1.0, "track {index} shift {}", sums.shift);
                // The exact sum, too.
                let (a, b) = position.counted(*length as u64);
                let exact: u128 = pcm
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| (a..=b).contains(&(*i as u64 + 1)))
                    .map(|(i, s)| (i as u128 + 1) * u128::from(u32::from_le_bytes(*s)))
                    .sum();
                assert_eq!(sums.weighted, exact);
                // The v2 filter never rejects the true v2.
                assert!(sums.may_have_v2(brute.v2.0), "shift {}", sums.shift);
            }
            // And it rejects almost everything else.
            let rejected = seen
                .iter()
                .zip(&direct)
                .filter(|(s, d)| !s.may_have_v2(d.v2.0 ^ 0x5555_5555))
                .count();
            assert!(rejected > 590, "{rejected}");
        }
    }

    #[test]
    fn files_and_memory_read_the_same() {
        let disc = noise(7, LENGTHS.iter().sum());
        let dir = tempfile::tempdir().unwrap();
        let mut at = 0;
        let mut files = Vec::new();
        for (i, n) in LENGTHS.iter().enumerate() {
            let path = dir.path().join(format!("{i}.pcm"));
            std::fs::write(&path, &disc[at * 4..(at + n) * 4]).unwrap();
            at += n;
            files.push((Pcm::File(path), Samples(*n as u64)));
        }
        let memory = split(&disc, &LENGTHS);
        let mut from_files = DiscAudio::new(&refs(&files)).unwrap();
        let mut from_memory = DiscAudio::new(&refs(&memory)).unwrap();
        for start in [-50i64, 0, 5000, 9 * 588 + 10, 26 * 588] {
            let mut a = vec![0u8; 4 * 700];
            let mut b = vec![0u8; 4 * 700];
            from_files.read_bytes(start, &mut a).unwrap();
            from_memory.read_bytes(start, &mut b).unwrap();
            assert_eq!(a, b, "{start}");
        }
        let mut before = vec![1u8; 8];
        from_files.read_bytes(-2, &mut before).unwrap();
        assert_eq!(before, [0; 8]);
    }

    /// Database checksums for a pressing at `shift`.
    fn pressing(disc: &[u8], shift: i64) -> Vec<AccurateRipCrcs> {
        (0..3)
            .map(|i| {
                let pcm = shifted_track(disc, &LENGTHS, i, shift);
                track_crcs(&pcm, TrackPosition::for_track(i, 3).unwrap()).unwrap()
            })
            .collect()
    }

    fn ours(disc: &[u8]) -> Vec<(TrackNumber, AccurateRipCrcs)> {
        pressing(disc, 0)
            .into_iter()
            .enumerate()
            .map(|(i, c)| (TrackNumber::new(i as u8 + 1).unwrap(), c))
            .collect()
    }

    fn statuses(
        disc: &[u8],
        entries: &[DatabaseEntry],
        max: u32,
    ) -> Vec<(TrackNumber, AccurateRipStatus)> {
        let tracks = split(disc, &LENGTHS);
        let mut audio = DiscAudio::new(&refs(&tracks)).unwrap();
        let here = verify(&ours(disc), entries);
        verify_with_pressings(&mut audio, here, entries, max).unwrap()
    }

    fn v1s(crcs: &[AccurateRipCrcs]) -> Vec<u32> {
        crcs.iter().map(|c| c.v1.0).collect()
    }

    fn v2s(crcs: &[AccurateRipCrcs]) -> Vec<u32> {
        crcs.iter().map(|c| c.v2.0).collect()
    }

    #[test]
    fn finds_a_pressing_at_plus_102_and_minus_48() {
        let disc = noise(3, LENGTHS.iter().sum());
        for shift in [102i64, -48] {
            let theirs = pressing(&disc, shift);
            // A v2 entry only: found through the v2 filter and exact check.
            let found = statuses(&disc, &[database(&v2s(&theirs), 8, 3)], DEFAULT_MAX_SHIFT);
            for (_, status) in &found {
                assert_eq!(
                    *status,
                    AccurateRipStatus::Accurate {
                        version: AccurateRipVersion::V2,
                        confidence: 8,
                        pressing_offset: SampleOffset(shift as i32),
                    }
                );
                assert!(
                    status
                        .summary()
                        .ends_with(&format!("pressing offset {shift:+})"))
                );
            }
            // A v1 entry only.
            let found = statuses(&disc, &[database(&v1s(&theirs), 5, 3)], DEFAULT_MAX_SHIFT);
            for (_, status) in &found {
                assert_eq!(
                    *status,
                    AccurateRipStatus::Accurate {
                        version: AccurateRipVersion::V1,
                        confidence: 5,
                        pressing_offset: SampleOffset(shift as i32),
                    }
                );
            }
        }
    }

    #[test]
    fn no_match_stays_a_mismatch() {
        let disc = noise(5, LENGTHS.iter().sum());
        let other = noise(6, LENGTHS.iter().sum());
        let theirs = pressing(&other, 40);
        let found = statuses(&disc, &[database(&v2s(&theirs), 9, 3)], DEFAULT_MAX_SHIFT);
        for (_, status) in &found {
            assert_eq!(*status, AccurateRipStatus::Mismatch { best_confidence: 9 });
        }
        // Outside the window: not found.
        let far = pressing(&disc, 400);
        let found = statuses(&disc, &[database(&v2s(&far), 9, 3)], 300);
        assert!(found.iter().all(|(_, s)| !s.is_accurate()));
        // Searching is off with a window of 0.
        let near = pressing(&disc, 4);
        let found = statuses(&disc, &[database(&v2s(&near), 9, 3)], 0);
        assert!(found.iter().all(|(_, s)| !s.is_accurate()));
    }

    #[test]
    fn several_pressings_the_most_confident_wins_but_ours_comes_first() {
        let disc = noise(8, LENGTHS.iter().sum());
        let plus = pressing(&disc, 102);
        let minus = pressing(&disc, -48);
        let entries = [
            database(&v2s(&plus), 3, 3),
            database(&v1s(&minus), 11, 3),
            database(&v2s(&minus), 6, 3),
        ];
        for (_, status) in statuses(&disc, &entries, DEFAULT_MAX_SHIFT) {
            assert_eq!(
                status,
                AccurateRipStatus::Accurate {
                    version: AccurateRipVersion::V2,
                    confidence: 6,
                    pressing_offset: SampleOffset(-48),
                }
            );
        }
        // With our own pressing in the database too, it wins.
        let mut with_ours = entries.to_vec();
        with_ours.push(database(&v2s(&pressing(&disc, 0)), 1, 3));
        for (_, status) in statuses(&disc, &with_ours, DEFAULT_MAX_SHIFT) {
            assert_eq!(
                status,
                AccurateRipStatus::Accurate {
                    version: AccurateRipVersion::V2,
                    confidence: 1,
                    pressing_offset: SampleOffset(0),
                }
            );
        }
    }

    #[test]
    fn rolling_crc_equals_direct_crc_at_every_shift() {
        let lengths = [12 * 588 + 3, 9 * 588, 11 * 588];
        let disc = noise(11, lengths.iter().sum());
        let tracks = split(&disc, &lengths);
        let mut audio = DiscAudio::new(&refs(&tracks)).unwrap();
        let total = disc.len() as i64 / 4;
        let edge = EDGE_SAMPLES as i64;
        let mut seen = Vec::new();
        ctdb_crcs_at_shifts(&mut audio, 200, |shift, crc| seen.push((shift, crc))).unwrap();
        assert_eq!(seen.len(), 401);
        for (shift, crc) in seen {
            let from = ((edge + shift) * 4) as usize;
            let to = ((total - edge + shift) * 4) as usize;
            assert_eq!(crc, crc32fast::hash(&disc[from..to]), "shift {shift}");
        }
    }

    #[test]
    fn ctdb_finds_a_shifted_pressing() {
        let lengths = [12 * 588, 9 * 588, 11 * 588];
        let disc = noise(12, lengths.iter().sum());
        let tracks = split(&disc, &lengths);
        let mut audio = DiscAudio::new(&refs(&tracks)).unwrap();
        let total = disc.len() / 4;
        let edge = EDGE_SAMPLES as usize;
        let theirs = crc32fast::hash(&disc[(edge + 102) * 4..(total - edge + 102) * 4]);
        let entries = [
            CtdbEntry {
                crc32: 0x1234_5678,
                confidence: 9,
            },
            CtdbEntry {
                crc32: theirs,
                confidence: 4,
            },
        ];
        let status = ctdb_with_pressings(
            &mut audio,
            CtdbStatus::Mismatch { total: 13 },
            &entries,
            DEFAULT_MAX_SHIFT,
        )
        .unwrap();
        assert_eq!(
            status,
            CtdbStatus::Accurate {
                confidence: 4,
                total: 13,
                pressing_offset: SampleOffset(102),
            }
        );
        assert_eq!(
            status.summary(),
            "Accurately ripped (confidence 4 of 13, pressing offset +102)"
        );
    }
}
