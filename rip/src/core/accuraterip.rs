// SPDX-License-Identifier: AGPL-3.0-or-later
//! AccurateRip v1/v2 checksums and the `dBAR-…bin` database.
//!
//! **Checksums.** Each stereo sample is read as one little-endian `u32`
//! (left channel in the low half) and multiplied by its 1-based position in
//! the track. v1 is the sum of the low 32 bits of the products; v2 adds the
//! high 32 bits as well, so it covers every bit of the audio. Both wrap at
//! 2³². The first audio track skips its first 2939 samples (five sectors
//! less one sample: counting starts at position 2940) and the last audio
//! track skips its final 2940 samples (five sectors), because drives cannot
//! read reliably right at the edges of the disc.
//!
//! **Database file.** A `dBAR` file is a list of entries, one per pressing
//! and checksum version submitted: a 13-byte header (track count `u8`, disc
//! id 1, disc id 2 and the freedb id, each `u32` little-endian), then nine
//! bytes per track (confidence `u8`, checksum `u32`, checksum of sector 450
//! `u32`). The file does not say whether a checksum is v1 or v2, so ours are
//! compared with both.
//!
//! **Pressings.** Different pressings of the same album are often shifted by
//! some samples. [`verify_disc`] first compares at our own position; tracks
//! that do not match are searched over a small window of shifts using v1
//! (which can be updated in constant time per shift). A track that matches
//! at another shift is accurate: its audio is identical, only positioned
//! differently on that pressing.

use std::time::Duration;

use url::Url;

use super::{CoreError, Result, with_trailing_slash};
use crate::model::{
    AccurateRipCrc, AccurateRipCrcs, AccurateRipDiscId, AccurateRipStatus, AccurateRipVersion,
    BYTES_PER_SAMPLE, SAMPLES_PER_SECTOR, TrackNumber,
};

/// Default database base URL.
pub const DEFAULT_BASE: &str = "http://www.accuraterip.com/accuraterip/";

/// Samples skipped at the start of the first track (counting starts at
/// multiplier `SKIP_SAMPLES`, so `SKIP_SAMPLES - 1` samples are skipped) and
/// at the end of the last track.
const SKIP_SAMPLES: u64 = 5 * SAMPLES_PER_SECTOR as u64;

/// A sensible window for [`verify_disc`]'s pressing search: ±5 sectors.
pub const DEFAULT_PRESSING_SEARCH: u32 = 5 * SAMPLES_PER_SECTOR;

/// Where a track sits on the disc; the first track skips its first five
/// sectors less one sample and the last track its final five sectors when
/// checksumming.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TrackPosition {
    /// First audio track of several.
    First,
    /// Neither first nor last.
    Middle,
    /// Last audio track of several.
    Last,
    /// The disc's only audio track (both rules apply).
    Only,
}

impl TrackPosition {
    /// The position of the audio track at `index` (0-based) among `count`
    /// audio tracks, or `None` when `index` is out of range.
    pub fn for_track(index: usize, count: usize) -> Option<TrackPosition> {
        if index >= count {
            return None;
        }
        let first = index == 0;
        let last = index + 1 == count;
        Some(match (first, last) {
            (true, true) => TrackPosition::Only,
            (true, false) => TrackPosition::First,
            (false, true) => TrackPosition::Last,
            (false, false) => TrackPosition::Middle,
        })
    }

    /// The inclusive range of multipliers (1-based sample positions) that
    /// count towards the checksum of a track of `total` samples. Empty
    /// (`first > last`) for tracks shorter than the skipped edges.
    fn counted(self, total: u64) -> (u64, u64) {
        let first = match self {
            TrackPosition::First | TrackPosition::Only => SKIP_SAMPLES,
            TrackPosition::Middle | TrackPosition::Last => 1,
        };
        let last = match self {
            TrackPosition::Last | TrackPosition::Only => total.saturating_sub(SKIP_SAMPLES),
            TrackPosition::First | TrackPosition::Middle => total,
        };
        (first, last)
    }
}

/// Computes the v1 and v2 checksums of one track's offset-corrected PCM
/// (16-bit LE stereo, a whole number of sectors).
pub fn track_crcs(pcm: &[u8], position: TrackPosition) -> Result<AccurateRipCrcs> {
    let total = u64::try_from(pcm.len() / BYTES_PER_SAMPLE).map_err(|_| too_long())?;
    let mut state = TrackCrcState::new(position, total);
    state.update(pcm)?;
    state.finish()
}

/// Incremental form of [`track_crcs`] for streaming from a file.
#[derive(Debug)]
pub struct TrackCrcState {
    position: TrackPosition,
    total_samples: u64,
    samples_seen: u64,
    v1: u32,
    v2: u32,
}

impl TrackCrcState {
    /// Starts a checksum for a track of `total_samples` stereo samples.
    pub fn new(position: TrackPosition, total_samples: u64) -> Self {
        TrackCrcState {
            position,
            total_samples,
            samples_seen: 0,
            v1: 0,
            v2: 0,
        }
    }

    /// Feeds the next PCM bytes (any multiple of 4 bytes).
    pub fn update(&mut self, pcm: &[u8]) -> Result<()> {
        let (samples, rest) = pcm.as_chunks::<4>();
        if !rest.is_empty() {
            return Err(CoreError::Invalid(
                "audio data must be whole stereo samples (4 bytes each)".into(),
            ));
        }
        let count = u64::try_from(samples.len()).map_err(|_| too_long())?;
        let end = self.samples_seen.checked_add(count).ok_or_else(too_long)?;
        if end > self.total_samples {
            return Err(CoreError::Invalid(format!(
                "the track has {} samples but more were given",
                self.total_samples
            )));
        }
        let (first, last) = self.position.counted(self.total_samples);
        for (index, sample) in samples.iter().enumerate() {
            let offset = u64::try_from(index).map_err(|_| too_long())?;
            let multiplier = self.samples_seen + offset + 1;
            if multiplier >= first && multiplier <= last {
                let value = u32::from_le_bytes(*sample);
                let multiplier = u32::try_from(multiplier).map_err(|_| too_long())?;
                let (low, high) = product(multiplier, value);
                self.v1 = self.v1.wrapping_add(low);
                self.v2 = self.v2.wrapping_add(low).wrapping_add(high);
            }
        }
        self.samples_seen = end;
        Ok(())
    }

    /// Finishes and returns both checksums.
    pub fn finish(self) -> Result<AccurateRipCrcs> {
        if self.samples_seen != self.total_samples {
            return Err(CoreError::Invalid(format!(
                "the track should have {} samples but only {} were given",
                self.total_samples, self.samples_seen
            )));
        }
        Ok(AccurateRipCrcs {
            v1: AccurateRipCrc(self.v1),
            v2: AccurateRipCrc(self.v2),
        })
    }
}

/// The low and high 32 bits of `a × b`.
fn product(a: u32, b: u32) -> (u32, u32) {
    let wide = u64::from(a) * u64::from(b);
    let [b0, b1, b2, b3, b4, b5, b6, b7] = wide.to_le_bytes();
    (
        u32::from_le_bytes([b0, b1, b2, b3]),
        u32::from_le_bytes([b4, b5, b6, b7]),
    )
}

/// One submission set in the database for one pressing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseEntry {
    /// The disc identity the entry is for (must match ours).
    pub disc: AccurateRipDiscId,
    /// Per track: (confidence, checksum, checksum of frame 450).
    pub tracks: Vec<DatabaseTrack>,
}

/// One track of a [`DatabaseEntry`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DatabaseTrack {
    /// Number of submissions.
    pub confidence: u8,
    /// The track checksum (v1 or v2; the file does not say which).
    pub crc: u32,
    /// Checksum of sector 450, used for offset detection.
    pub frame450_crc: u32,
}

/// The `dBAR-…bin` file name for a disc, e.g.
/// `dBAR-004-0002189a-00087f33-1f02e004.bin`.
pub fn database_file_name(disc: &AccurateRipDiscId) -> String {
    format!(
        "dBAR-{:03}-{:08x}-{:08x}-{:08x}.bin",
        disc.track_count, disc.id1, disc.id2, disc.cddb
    )
}

/// The database URL for a disc:
/// `{base}{id1&0xF:x}/{id1>>4&0xF:x}/{id1>>8&0xF:x}/dBAR-{nnn}-{id1:08x}-{id2:08x}-{cddb:08x}.bin`.
pub fn database_url(base: &Url, disc: &AccurateRipDiscId) -> Result<Url> {
    let path = format!(
        "{:x}/{:x}/{:x}/{}",
        disc.id1 & 0xF,
        (disc.id1 >> 4) & 0xF,
        (disc.id1 >> 8) & 0xF,
        database_file_name(disc)
    );
    with_trailing_slash(base)
        .join(&path)
        .map_err(|e| CoreError::Invalid(format!("bad AccurateRip address: {e}")))
}

/// Parses a `dBAR` file (several concatenated entries).
pub fn parse_database(bytes: &[u8]) -> Result<Vec<DatabaseEntry>> {
    let mut rest = bytes;
    let mut entries = Vec::new();
    while !rest.is_empty() {
        let (header, after) = rest.split_first_chunk::<13>().ok_or_else(cut_short)?;
        let [count, a0, a1, a2, a3, b0, b1, b2, b3, c0, c1, c2, c3] = *header;
        if count == 0 {
            return Err(CoreError::Invalid(
                "the AccurateRip database file lists a disc with no tracks".into(),
            ));
        }
        let disc = AccurateRipDiscId {
            track_count: count,
            id1: u32::from_le_bytes([a0, a1, a2, a3]),
            id2: u32::from_le_bytes([b0, b1, b2, b3]),
            cddb: u32::from_le_bytes([c0, c1, c2, c3]),
        };
        let body_length = usize::from(count) * 9;
        let body = after.get(..body_length).ok_or_else(cut_short)?;
        let (records, _) = body.as_chunks::<9>();
        let tracks = records
            .iter()
            .map(|record| {
                let [confidence, c0, c1, c2, c3, f0, f1, f2, f3] = *record;
                DatabaseTrack {
                    confidence,
                    crc: u32::from_le_bytes([c0, c1, c2, c3]),
                    frame450_crc: u32::from_le_bytes([f0, f1, f2, f3]),
                }
            })
            .collect();
        entries.push(DatabaseEntry { disc, tracks });
        rest = after.get(body_length..).ok_or_else(cut_short)?;
    }
    Ok(entries)
}

/// Fetches and parses the database for a disc. A 404 means "not in
/// database" and returns an empty list. Entries for a different disc
/// identity (never expected) are dropped.
pub async fn fetch_database(
    client: &reqwest::Client,
    base: &Url,
    disc: &AccurateRipDiscId,
    timeout: Duration,
) -> Result<Vec<DatabaseEntry>> {
    let url = database_url(base, disc)?;
    let network = |source| CoreError::Network {
        service: "the AccurateRip database",
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
    let bytes = response
        .error_for_status()
        .map_err(network)?
        .bytes()
        .await
        .map_err(network)?;
    let mut entries = parse_database(&bytes)?;
    entries.retain(|entry| entry.disc == *disc);
    Ok(entries)
}

/// Compares our checksums with the database, one status per track in order.
///
/// `ours` lists every audio track in disc order. Only entries with the same
/// number of tracks are used. Per track, a v2 match wins over a v1 match;
/// the confidence is the sum over every entry with that checksum (different
/// pressings can share a checksum). Zero checksums in the file mean "no
/// data" and are ignored.
pub fn verify(
    ours: &[(TrackNumber, AccurateRipCrcs)],
    entries: &[DatabaseEntry],
) -> Vec<(TrackNumber, AccurateRipStatus)> {
    let relevant: Vec<&DatabaseEntry> = entries
        .iter()
        .filter(|entry| entry.tracks.len() == ours.len())
        .collect();
    ours.iter()
        .enumerate()
        .map(|(index, (number, crcs))| {
            let candidates = relevant
                .iter()
                .filter_map(|entry| entry.tracks.get(index))
                .filter(|track| track.crc != 0);
            let mut v1 = 0u32;
            let mut v2 = 0u32;
            let mut best = None::<u32>;
            for track in candidates {
                let confidence = u32::from(track.confidence);
                if track.crc == crcs.v2.0 {
                    v2 += confidence;
                }
                if track.crc == crcs.v1.0 {
                    v1 += confidence;
                }
                best = Some(best.map_or(confidence, |b| b.max(confidence)));
            }
            let status = if v2 > 0 {
                AccurateRipStatus::Accurate {
                    version: AccurateRipVersion::V2,
                    confidence: v2,
                }
            } else if v1 > 0 {
                AccurateRipStatus::Accurate {
                    version: AccurateRipVersion::V1,
                    confidence: v1,
                }
            } else {
                match best {
                    Some(best_confidence) => AccurateRipStatus::Mismatch { best_confidence },
                    None => AccurateRipStatus::NotInDatabase,
                }
            };
            (*number, status)
        })
        .collect()
}

/// Result of [`verify_disc`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscVerification {
    /// Per audio track in disc order: our checksums (of the files as ripped)
    /// and the database outcome.
    pub tracks: Vec<(TrackNumber, AccurateRipCrcs, AccurateRipStatus)>,
    /// When some tracks only matched another pressing: that pressing's
    /// audio starts this many samples later in our rip (negative: earlier).
    pub pressing_shift: Option<i32>,
}

/// A shift at which tracks match the database (see [`find_pressing_shifts`]).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PressingMatch {
    /// Samples by which the database's audio is shifted relative to ours:
    /// track sample `i` of that pressing is our disc sample `start + i + shift`.
    pub shift: i32,
    /// Tracks whose v1 checksum matches at this shift.
    pub matched_tracks: usize,
    /// Sum of the matching entries' confidence over those tracks.
    pub confidence: u32,
}

/// Checksums a whole rip and compares it with the database, searching up to
/// `max_shift` samples either way for another pressing when tracks do not
/// match at our own position. `tracks` holds every audio track's
/// offset-corrected PCM in disc order; samples beyond the disc's edges count
/// as silence.
pub fn verify_disc(
    tracks: &[(TrackNumber, &[u8])],
    entries: &[DatabaseEntry],
    max_shift: u32,
) -> Result<DiscVerification> {
    let count = tracks.len();
    let mut ours = Vec::with_capacity(count);
    for (index, (number, pcm)) in tracks.iter().enumerate() {
        let position = TrackPosition::for_track(index, count).ok_or_else(too_long)?;
        ours.push((*number, track_crcs(pcm, position)?));
    }
    let mut statuses = verify(&ours, entries);
    let accurate_here = statuses.iter().filter(|(_, s)| is_accurate(s)).count();
    let mut pressing_shift = None;
    if max_shift > 0 && accurate_here < count && !entries.is_empty() {
        let pcm: Vec<&[u8]> = tracks.iter().map(|(_, pcm)| *pcm).collect();
        let best = find_pressing_shifts(&pcm, entries, max_shift)?
            .into_iter()
            .find(|m| m.shift != 0);
        if let Some(best) = best {
            let disc = DiscSamples::new(&pcm)?;
            let mut shifted = Vec::with_capacity(count);
            for (index, (number, _)) in tracks.iter().enumerate() {
                let position = TrackPosition::for_track(index, count).ok_or_else(too_long)?;
                let audio = disc.shifted_track(index, best.shift)?;
                shifted.push((*number, track_crcs(&audio, position)?));
            }
            let there = verify(&shifted, entries);
            let mut improved = false;
            for ((_, status), (_, other)) in statuses.iter_mut().zip(there) {
                if !is_accurate(status) && is_accurate(&other) {
                    *status = other;
                    improved = true;
                }
            }
            if improved {
                pressing_shift = Some(best.shift);
            }
        }
    }
    Ok(DiscVerification {
        tracks: ours
            .into_iter()
            .zip(statuses)
            .map(|((number, crcs), (_, status))| (number, crcs, status))
            .collect(),
        pressing_shift,
    })
}

/// Every shift in `-max_shift..=max_shift` at which at least one track's v1
/// checksum matches an entry, best first (most tracks, then most
/// confidence, then the smallest shift).
pub fn find_pressing_shifts(
    tracks: &[&[u8]],
    entries: &[DatabaseEntry],
    max_shift: u32,
) -> Result<Vec<PressingMatch>> {
    let disc = DiscSamples::new(tracks)?;
    let count = tracks.len();
    let relevant: Vec<&DatabaseEntry> = entries
        .iter()
        .filter(|entry| entry.tracks.len() == count)
        .collect();
    let max = i64::from(max_shift);
    // Per track: the counted multipliers (a..=b), the disc index of its
    // first sample, the running v1 sum S and the plain sum P at the current
    // shift.
    let mut windows = Vec::with_capacity(count);
    for index in 0..count {
        let position = TrackPosition::for_track(index, count).ok_or_else(too_long)?;
        let length = disc.track_length(index)?;
        let (a, b) = position.counted(u64::try_from(length).map_err(|_| too_long())?);
        let a = i64::try_from(a).map_err(|_| too_long())?;
        let b = i64::try_from(b).map_err(|_| too_long())?;
        let base = disc.track_start(index)?;
        let mut weighted = 0u32;
        let mut plain = 0u32;
        let mut m = a;
        while m <= b {
            let value = disc.at(base + m - 1 - max);
            let factor = u32::try_from(m).map_err(|_| too_long())?;
            weighted = weighted.wrapping_add(factor.wrapping_mul(value));
            plain = plain.wrapping_add(value);
            m += 1;
        }
        windows.push(ShiftWindow {
            a,
            b,
            base,
            weighted,
            plain,
        });
    }
    let mut matches = Vec::new();
    let mut shift = -max;
    while shift <= max {
        let mut matched_tracks = 0usize;
        let mut confidence = 0u32;
        for (index, window) in windows.iter().enumerate() {
            if window.a > window.b {
                continue;
            }
            let found: u32 = relevant
                .iter()
                .filter_map(|entry| entry.tracks.get(index))
                .filter(|track| track.crc != 0 && track.crc == window.weighted)
                .map(|track| u32::from(track.confidence))
                .sum();
            if found > 0 {
                matched_tracks += 1;
                confidence += found;
            }
        }
        if matched_tracks > 0 {
            matches.push(PressingMatch {
                shift: i32::try_from(shift).map_err(|_| too_long())?,
                matched_tracks,
                confidence,
            });
        }
        for window in &mut windows {
            window.step(&disc, shift)?;
        }
        shift += 1;
    }
    matches.sort_by(|x, y| {
        y.matched_tracks
            .cmp(&x.matched_tracks)
            .then(y.confidence.cmp(&x.confidence))
            .then(x.shift.unsigned_abs().cmp(&y.shift.unsigned_abs()))
            .then(x.shift.cmp(&y.shift))
    });
    Ok(matches)
}

/// The v1 sum of one track at one shift, updated in constant time per step.
struct ShiftWindow {
    a: i64,
    b: i64,
    base: i64,
    weighted: u32,
    plain: u32,
}

impl ShiftWindow {
    /// Moves from `shift` to `shift + 1`. With `y(m)` the sample under
    /// multiplier `m` at the current shift:
    /// `S' = S − a·y(a) + (b+1)·y(b+1) − (P − y(a) + y(b+1))` and
    /// `P' = P − y(a) + y(b+1)`.
    fn step(&mut self, disc: &DiscSamples<'_>, shift: i64) -> Result<()> {
        if self.a > self.b {
            return Ok(());
        }
        let leaving = disc.at(self.base + self.a - 1 + shift);
        let entering = disc.at(self.base + self.b + shift);
        let a = u32::try_from(self.a).map_err(|_| too_long())?;
        let after_b = u32::try_from(self.b + 1).map_err(|_| too_long())?;
        let plain = self.plain.wrapping_sub(leaving).wrapping_add(entering);
        self.weighted = self
            .weighted
            .wrapping_sub(a.wrapping_mul(leaving))
            .wrapping_add(after_b.wrapping_mul(entering))
            .wrapping_sub(plain);
        self.plain = plain;
        Ok(())
    }
}

/// All audio tracks seen as one run of samples, silent beyond both edges.
struct DiscSamples<'a> {
    tracks: &'a [&'a [u8]],
    /// Disc sample index of each track's first sample, plus the total.
    starts: Vec<i64>,
}

impl<'a> DiscSamples<'a> {
    fn new(tracks: &'a [&'a [u8]]) -> Result<Self> {
        let mut starts = Vec::with_capacity(tracks.len() + 1);
        let mut total = 0i64;
        starts.push(0);
        for pcm in tracks {
            if pcm.len() % BYTES_PER_SAMPLE != 0 {
                return Err(CoreError::Invalid(
                    "audio data must be whole stereo samples (4 bytes each)".into(),
                ));
            }
            let samples = i64::try_from(pcm.len() / BYTES_PER_SAMPLE).map_err(|_| too_long())?;
            total = total.checked_add(samples).ok_or_else(too_long)?;
            starts.push(total);
        }
        Ok(DiscSamples { tracks, starts })
    }

    fn track_start(&self, index: usize) -> Result<i64> {
        self.starts.get(index).copied().ok_or_else(too_long)
    }

    fn track_length(&self, index: usize) -> Result<i64> {
        let end = self.starts.get(index + 1).copied().ok_or_else(too_long)?;
        Ok(end - self.track_start(index)?)
    }

    /// The sample at disc index `index`, or silence outside the disc.
    fn at(&self, index: i64) -> u32 {
        if index < 0 {
            return 0;
        }
        let track = self.starts.partition_point(|start| *start <= index);
        let Some(track) = track.checked_sub(1) else {
            return 0;
        };
        let (Some(pcm), Some(start)) = (self.tracks.get(track), self.starts.get(track)) else {
            return 0;
        };
        let Ok(within) = usize::try_from(index - start) else {
            return 0;
        };
        let offset = within * BYTES_PER_SAMPLE;
        pcm.get(offset..offset + BYTES_PER_SAMPLE)
            .and_then(|bytes| bytes.first_chunk::<4>())
            .map_or(0, |bytes| u32::from_le_bytes(*bytes))
    }

    /// Track `index` as it would be cut with its start moved by `shift`.
    fn shifted_track(&self, index: usize, shift: i32) -> Result<Vec<u8>> {
        let start = self.track_start(index)? + i64::from(shift);
        let length = self.track_length(index)?;
        let mut out = Vec::with_capacity(usize::try_from(length).map_err(|_| too_long())? * 4);
        for i in 0..length {
            out.extend_from_slice(&self.at(start + i).to_le_bytes());
        }
        Ok(out)
    }
}

fn is_accurate(status: &AccurateRipStatus) -> bool {
    matches!(status, AccurateRipStatus::Accurate { .. })
}

fn cut_short() -> CoreError {
    CoreError::Invalid("the AccurateRip database file is cut short".into())
}

fn too_long() -> CoreError {
    CoreError::Invalid("the audio is longer than any CD".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{SERVICE_TIMEOUT, testserver};

    /// A reply from the live database for
    /// `dBAR-004-0002189a-00087f33-1f02e004.bin` (two entries).
    const LIVE_DBAR: [u8; 98] = [
        0x04, 0x9a, 0x18, 0x02, 0x00, 0x33, 0x7f, 0x08, 0x00, 0x04, 0xe0, 0x02, 0x1f, 0x05, 0xa7,
        0x60, 0x2b, 0xf3, 0xe8, 0x35, 0xef, 0x52, 0x05, 0x05, 0xca, 0x93, 0xa9, 0x3f, 0xe8, 0xee,
        0xea, 0x05, 0x9a, 0xdb, 0x15, 0x4f, 0x77, 0x3a, 0x25, 0xfe, 0x05, 0x3a, 0x14, 0x0a, 0x83,
        0x83, 0x4f, 0x1f, 0x29, 0x04, 0x9a, 0x18, 0x02, 0x00, 0x33, 0x7f, 0x08, 0x00, 0x04, 0xe0,
        0x02, 0x1f, 0x03, 0x9a, 0x95, 0xd1, 0x91, 0x00, 0x00, 0x00, 0x00, 0x03, 0xc4, 0x6f, 0xef,
        0xb8, 0x00, 0x00, 0x00, 0x00, 0x03, 0xf3, 0xcf, 0x73, 0xe9, 0x00, 0x00, 0x00, 0x00, 0x03,
        0x45, 0x16, 0x6b, 0x8b, 0x00, 0x00, 0x00, 0x00,
    ];

    fn disc_id() -> AccurateRipDiscId {
        AccurateRipDiscId {
            track_count: 4,
            id1: 0x0002_189a,
            id2: 0x0008_7f33,
            cddb: 0x1f02_e004,
        }
    }

    fn number(n: u8) -> TrackNumber {
        TrackNumber::new(n).unwrap()
    }

    /// Straightforward reference: multiplier = index + 1, both sums.
    fn reference(pcm: &[u8], position: TrackPosition) -> (u32, u32) {
        let total = (pcm.len() / 4) as u64;
        let (first, last) = position.counted(total);
        let mut v1 = 0u32;
        let mut v2 = 0u32;
        for (i, chunk) in pcm.as_chunks::<4>().0.iter().enumerate() {
            let m = i as u64 + 1;
            if m >= first && m <= last {
                let value = u32::from_le_bytes(*chunk) as u64;
                let p = value * m;
                v1 = v1.wrapping_add(p as u32);
                v2 = v2.wrapping_add(p as u32).wrapping_add((p >> 32) as u32);
            }
        }
        (v1, v2)
    }

    /// Deterministic pseudo-random PCM (xorshift), `sectors` long.
    fn noise(seed: u32, sectors: usize) -> Vec<u8> {
        let mut x = seed.max(1);
        let mut out = Vec::with_capacity(sectors * 2352);
        for _ in 0..sectors * 588 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            out.extend_from_slice(&x.to_le_bytes());
        }
        out
    }

    #[test]
    fn small_known_answer() {
        // Three samples of a middle track: v1 = 1·a + 2·b + 3·c.
        let a = 0x0001_0002u32;
        let b = 0xFFFF_FFFFu32;
        let c = 0x8000_0000u32;
        let pcm: Vec<u8> = [a, b, c].iter().flat_map(|v| v.to_le_bytes()).collect();
        let crcs = track_crcs(&pcm, TrackPosition::Middle).unwrap();
        let v1 = a
            .wrapping_add(b.wrapping_mul(2))
            .wrapping_add(c.wrapping_mul(3));
        assert_eq!(crcs.v1.0, v1);
        // High halves: a·1 → 0, b·2 → 1, c·3 → 1.
        assert_eq!(crcs.v2.0, v1.wrapping_add(2));
    }

    #[test]
    fn edges_are_skipped_on_the_first_and_last_track() {
        // A track of 10 sectors with a single non-zero sample at a time.
        let total = 10 * 588;
        let poke = |index: usize| {
            let mut pcm = vec![0u8; total * 4];
            pcm[index * 4] = 1;
            pcm
        };
        // Sample index 2938 (multiplier 2939) is skipped on the first
        // track; index 2939 (multiplier 2940) counts.
        assert_eq!(
            track_crcs(&poke(2938), TrackPosition::First).unwrap().v1.0,
            0
        );
        assert_eq!(
            track_crcs(&poke(2939), TrackPosition::First).unwrap().v1.0,
            2940
        );
        assert_eq!(
            track_crcs(&poke(2938), TrackPosition::Middle).unwrap().v1.0,
            2939
        );
        // The final 2940 samples are skipped on the last track.
        let last_counted = total - 2940 - 1;
        assert_eq!(
            track_crcs(&poke(last_counted), TrackPosition::Last)
                .unwrap()
                .v1
                .0,
            (last_counted + 1) as u32
        );
        assert_eq!(
            track_crcs(&poke(last_counted + 1), TrackPosition::Last)
                .unwrap()
                .v1
                .0,
            0
        );
        assert_eq!(
            track_crcs(&poke(last_counted + 1), TrackPosition::Only)
                .unwrap()
                .v1
                .0,
            0
        );
        assert_eq!(
            track_crcs(&poke(2938), TrackPosition::Only).unwrap().v1.0,
            0
        );
    }

    #[test]
    fn matches_the_reference_for_every_position() {
        let pcm = noise(7, 12);
        for position in [
            TrackPosition::First,
            TrackPosition::Middle,
            TrackPosition::Last,
            TrackPosition::Only,
        ] {
            let crcs = track_crcs(&pcm, position).unwrap();
            assert_eq!(
                (crcs.v1.0, crcs.v2.0),
                reference(&pcm, position),
                "{position:?}"
            );
        }
    }

    #[test]
    fn streaming_equals_one_shot() {
        let pcm = noise(3, 9);
        let whole = track_crcs(&pcm, TrackPosition::First).unwrap();
        let mut state = TrackCrcState::new(TrackPosition::First, (pcm.len() / 4) as u64);
        for chunk in pcm.chunks(4 * 1001) {
            state.update(chunk).unwrap();
        }
        assert_eq!(state.finish().unwrap(), whole);
    }

    #[test]
    fn streaming_rejects_wrong_lengths() {
        let mut state = TrackCrcState::new(TrackPosition::Middle, 2);
        assert!(state.update(&[0; 3]).is_err());
        assert!(state.update(&[0; 12]).is_err());
        state.update(&[0; 4]).unwrap();
        assert!(state.finish().is_err());
    }

    #[test]
    fn positions() {
        assert_eq!(TrackPosition::for_track(0, 1), Some(TrackPosition::Only));
        assert_eq!(TrackPosition::for_track(0, 3), Some(TrackPosition::First));
        assert_eq!(TrackPosition::for_track(1, 3), Some(TrackPosition::Middle));
        assert_eq!(TrackPosition::for_track(2, 3), Some(TrackPosition::Last));
        assert_eq!(TrackPosition::for_track(3, 3), None);
    }

    #[test]
    fn url_follows_the_published_layout() {
        let base = Url::parse(DEFAULT_BASE).unwrap();
        assert_eq!(
            database_url(&base, &disc_id()).unwrap().as_str(),
            "http://www.accuraterip.com/accuraterip/a/9/8/dBAR-004-0002189a-00087f33-1f02e004.bin"
        );
        // A base without the trailing slash works too.
        let bare = Url::parse("http://127.0.0.1:9/ar").unwrap();
        assert_eq!(
            database_url(&bare, &disc_id()).unwrap().path(),
            "/ar/a/9/8/dBAR-004-0002189a-00087f33-1f02e004.bin"
        );
    }

    #[test]
    fn parses_a_live_reply() {
        let entries = parse_database(&LIVE_DBAR).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.disc == disc_id()));
        assert_eq!(
            entries[0].tracks[0],
            DatabaseTrack {
                confidence: 5,
                crc: 0xf32b_60a7,
                frame450_crc: 0x52ef_35e8,
            }
        );
        assert_eq!(entries[1].tracks[0].confidence, 3);
        assert_eq!(entries[1].tracks[0].crc, 0x91d1_959a);
        assert_eq!(entries[1].tracks[3].crc, 0x8b6b_1645);
    }

    #[test]
    fn rejects_truncated_files() {
        assert!(parse_database(&LIVE_DBAR[..20]).is_err());
        assert!(parse_database(&LIVE_DBAR[..97]).is_err());
        assert!(parse_database(&[0; 13]).is_err());
        assert!(parse_database(&[]).unwrap().is_empty());
    }

    fn crcs(v1: u32, v2: u32) -> AccurateRipCrcs {
        AccurateRipCrcs {
            v1: AccurateRipCrc(v1),
            v2: AccurateRipCrc(v2),
        }
    }

    #[test]
    fn verify_against_the_live_reply() {
        let entries = parse_database(&LIVE_DBAR).unwrap();
        let ours = [
            (number(1), crcs(0xf32b_60a7, 0x1111_1111)), // v1 matches entry 0
            (number(2), crcs(0x2222_2222, 0xb8ef_6fc4)), // v2 matches entry 1
            (number(3), crcs(0x3333_3333, 0x4444_4444)), // nothing matches
            (number(4), crcs(0x8b6b_1645, 0x8b6b_1645)), // both versions agree
        ];
        let result = verify(&ours, &entries);
        assert_eq!(
            result[0].1,
            AccurateRipStatus::Accurate {
                version: AccurateRipVersion::V1,
                confidence: 5
            }
        );
        assert_eq!(
            result[1].1,
            AccurateRipStatus::Accurate {
                version: AccurateRipVersion::V2,
                confidence: 3
            }
        );
        assert_eq!(
            result[2].1,
            AccurateRipStatus::Mismatch { best_confidence: 5 }
        );
        assert_eq!(
            result[3].1,
            AccurateRipStatus::Accurate {
                version: AccurateRipVersion::V2,
                confidence: 3
            }
        );
        assert_eq!(result[3].0, number(4));
    }

    #[test]
    fn verify_without_entries_or_with_other_layouts() {
        let ours = [(number(1), crcs(1, 2))];
        assert_eq!(verify(&ours, &[])[0].1, AccurateRipStatus::NotInDatabase);
        let entries = parse_database(&LIVE_DBAR).unwrap();
        assert_eq!(
            verify(&ours, &entries)[0].1,
            AccurateRipStatus::NotInDatabase
        );
    }

    /// Builds a database entry from checksums.
    fn entry(crcs: &[u32], confidence: u8) -> DatabaseEntry {
        DatabaseEntry {
            disc: disc_id(),
            tracks: crcs
                .iter()
                .map(|crc| DatabaseTrack {
                    confidence,
                    crc: *crc,
                    frame450_crc: 0,
                })
                .collect(),
        }
    }

    #[test]
    fn shift_search_matches_direct_computation() {
        let disc = noise(11, 30);
        let tracks: Vec<&[u8]> = vec![
            &disc[..10 * 2352],
            &disc[10 * 2352..18 * 2352],
            &disc[18 * 2352..],
        ];
        let samples = DiscSamples::new(&tracks).unwrap();
        for shift in [-7i32, -1, 0, 3, 12] {
            let mut expected = Vec::new();
            for index in 0..3 {
                let audio = samples.shifted_track(index, shift).unwrap();
                let position = TrackPosition::for_track(index, 3).unwrap();
                expected.push(track_crcs(&audio, position).unwrap().v1.0);
            }
            let found = find_pressing_shifts(&tracks, &[entry(&expected, 2)], 15).unwrap();
            let best = found[0];
            assert_eq!(best.shift, shift);
            assert_eq!(best.matched_tracks, 3);
            assert_eq!(best.confidence, 6);
        }
    }

    #[test]
    fn verify_disc_finds_another_pressing() {
        // The database's pressing starts 9 samples later than our rip.
        let disc = noise(5, 24);
        let tracks: Vec<&[u8]> = vec![
            &disc[..8 * 2352],
            &disc[8 * 2352..16 * 2352],
            &disc[16 * 2352..],
        ];
        let samples = DiscSamples::new(&tracks).unwrap();
        let theirs: Vec<AccurateRipCrcs> = (0..3)
            .map(|i| {
                let audio = samples.shifted_track(i, 9).unwrap();
                track_crcs(&audio, TrackPosition::for_track(i, 3).unwrap()).unwrap()
            })
            .collect();
        let entries = [
            entry(&theirs.iter().map(|c| c.v1.0).collect::<Vec<_>>(), 4),
            entry(&theirs.iter().map(|c| c.v2.0).collect::<Vec<_>>(), 6),
        ];
        let numbered: Vec<(TrackNumber, &[u8])> = tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (number(i as u8 + 1), *t))
            .collect();

        let without_search = verify_disc(&numbered, &entries, 0).unwrap();
        assert!(
            without_search
                .tracks
                .iter()
                .all(|(_, _, s)| matches!(s, AccurateRipStatus::Mismatch { .. }))
        );
        assert_eq!(without_search.pressing_shift, None);

        let result = verify_disc(&numbered, &entries, 20).unwrap();
        assert_eq!(result.pressing_shift, Some(9));
        for (index, (_, ours, status)) in result.tracks.iter().enumerate() {
            assert_eq!(
                *status,
                AccurateRipStatus::Accurate {
                    version: AccurateRipVersion::V2,
                    confidence: 6
                }
            );
            // Our checksums still describe our own files.
            let position = TrackPosition::for_track(index, 3).unwrap();
            assert_eq!(*ours, track_crcs(tracks[index], position).unwrap());
        }
    }

    #[test]
    fn verify_disc_prefers_our_own_position() {
        let disc = noise(9, 16);
        let tracks: Vec<&[u8]> = vec![&disc[..8 * 2352], &disc[8 * 2352..]];
        let ours: Vec<AccurateRipCrcs> = (0..2)
            .map(|i| track_crcs(tracks[i], TrackPosition::for_track(i, 2).unwrap()).unwrap())
            .collect();
        let entries = [entry(&[ours[0].v2.0, ours[1].v2.0], 9)];
        let numbered = [(number(1), tracks[0]), (number(2), tracks[1])];
        let result = verify_disc(&numbered, &entries, 50).unwrap();
        assert_eq!(result.pressing_shift, None);
        assert!(result.tracks.iter().all(|(_, _, s)| *s
            == AccurateRipStatus::Accurate {
                version: AccurateRipVersion::V2,
                confidence: 9
            }));
    }

    #[tokio::test]
    async fn fetch_from_a_mock_server() {
        use axum::{Router, http::StatusCode, routing::get};
        let app = Router::new()
            .route(
                "/ar/a/9/8/dBAR-004-0002189a-00087f33-1f02e004.bin",
                get(|| async { LIVE_DBAR.to_vec() }),
            )
            .route(
                "/ar/0/0/0/dBAR-004-00000000-00000000-00000000.bin",
                get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
            );
        let base = testserver::serve(app).await.join("ar/").unwrap();
        let client = testserver::client();
        let entries = fetch_database(&client, &base, &disc_id(), SERVICE_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(entries.len(), 2);

        let missing = AccurateRipDiscId {
            id1: 0x123,
            ..disc_id()
        };
        assert!(
            fetch_database(&client, &base, &missing, SERVICE_TIMEOUT)
                .await
                .unwrap()
                .is_empty()
        );

        let broken = AccurateRipDiscId {
            track_count: 4,
            id1: 0,
            id2: 0,
            cddb: 0,
        };
        let error = fetch_database(&client, &base, &broken, SERVICE_TIMEOUT)
            .await
            .unwrap_err();
        assert!(matches!(error, CoreError::Network { .. }), "{error}");
    }
}
