// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared types: the integration contract between the lanes.
//!
//! Everything here is plain data with validating constructors. The drive
//! produces [`DiscInfo`] and [`TrackReadResult`]s, `core` turns them into
//! checksums, a [`VerificationResult`] and a [`RipReport`], `musicbrainz`
//! fills [`ReleaseMetadata`], `encode` writes FLACs and `library` executes an
//! [`UploadPlan`]. Change a type here only with every lane's agreement.
//!
//! Units on an audio CD (Red Book):
//! - a **sector** (frame) holds 2352 bytes = 588 stereo samples, 75 per second;
//! - a **sample** here is one stereo pair: two little-endian `i16`s, left first;
//! - an [`Lba`] counts sectors from the start of the program area (track 1
//!   usually starts at LBA 0; MSF time adds the 150-sector lead-in).

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Bytes in one audio sector (588 samples × 2 channels × 2 bytes).
pub const BYTES_PER_SECTOR: usize = 2352;
/// Stereo samples in one audio sector.
pub const SAMPLES_PER_SECTOR: u32 = 588;
/// Sectors per second of audio.
pub const SECTORS_PER_SECOND: u32 = 75;
/// Stereo samples per second (44.1 kHz).
pub const SAMPLE_RATE: u32 = 44_100;
/// Bytes in one stereo sample (two 16-bit channels).
pub const BYTES_PER_SAMPLE: usize = 4;
/// Sectors between MSF 00:00:00 and LBA 0 (the two-second lead-in offset).
pub const MSF_OFFSET_SECTORS: u32 = 150;
/// Sectors between the end of the audio session and the start of the data
/// track on an enhanced CD (Blue Book / CD-Extra): the first session's
/// lead-out (6,750 sectors, 90 s), the second session's lead-in (4,500,
/// 60 s) and the data track's pre-gap (150, 2 s). None of it can be read as
/// audio. libdiscid, whipper and CUETools all use this figure when the drive
/// does not report the session's own lead-out.
pub const SESSION_GAP_SECTORS: u32 = 11_400;

// ---------------------------------------------------------------------------
// Positions and quantities
// ---------------------------------------------------------------------------

/// A logical block address: a sector position counted from the start of the
/// program area.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Lba(pub u32);

impl Lba {
    /// Position `count` sectors after this one, or `None` on overflow.
    pub fn checked_add(self, count: Sectors) -> Option<Lba> {
        self.0.checked_add(count.0).map(Lba)
    }

    /// Sectors from `earlier` to `self`, or `None` if `earlier` is later.
    pub fn sectors_since(self, earlier: Lba) -> Option<Sectors> {
        self.0.checked_sub(earlier.0).map(Sectors)
    }

    /// MSF address (minutes, seconds, frames), including the 150-sector
    /// lead-in offset, as used in TOCs and disc-ID calculations.
    pub fn to_msf(self) -> Msf {
        Msf::from_sectors(Sectors(self.0.saturating_add(MSF_OFFSET_SECTORS)))
    }
}

impl fmt::Display for Lba {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A number of sectors (a length, not a position).
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Sectors(pub u32);

impl Sectors {
    /// The number of stereo samples in this many sectors.
    pub fn samples(self) -> Samples {
        Samples(u64::from(self.0) * u64::from(SAMPLES_PER_SECTOR))
    }

    /// The number of PCM bytes in this many sectors.
    pub fn bytes(self) -> u64 {
        u64::from(self.0) * BYTES_PER_SECTOR as u64
    }

    /// Playing time in whole milliseconds (rounded down).
    pub fn duration_ms(self) -> u64 {
        u64::from(self.0) * 1000 / u64::from(SECTORS_PER_SECOND)
    }

    /// Cue-sheet style time `MM:SS:FF` (no lead-in offset).
    pub fn to_msf(self) -> Msf {
        Msf::from_sectors(self)
    }
}

/// A number of stereo samples.
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Samples(pub u64);

impl Samples {
    /// The number of PCM bytes in this many samples.
    pub fn bytes(self) -> u64 {
        self.0 * BYTES_PER_SAMPLE as u64
    }

    /// Playing time in whole milliseconds (rounded down).
    pub fn duration_ms(self) -> u64 {
        self.0 * 1000 / u64::from(SAMPLE_RATE)
    }
}

/// Minutes, seconds and frames (75 frames per second).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Msf {
    /// Minutes (may exceed 59 for long discs).
    pub minutes: u32,
    /// Seconds, 0–59.
    pub seconds: u8,
    /// Frames, 0–74.
    pub frames: u8,
}

impl Msf {
    /// Splits a sector count into minutes, seconds and frames.
    pub fn from_sectors(sectors: Sectors) -> Msf {
        let total = sectors.0;
        // Both remainders are below 75 and 60, so they fit in a u8.
        let frames = u8::try_from(total % SECTORS_PER_SECOND).unwrap_or(0);
        let total_seconds = total / SECTORS_PER_SECOND;
        let seconds = u8::try_from(total_seconds % 60).unwrap_or(0);
        Msf {
            minutes: total_seconds / 60,
            seconds,
            frames,
        }
    }
}

impl fmt::Display for Msf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02}:{:02}:{:02}",
            self.minutes, self.seconds, self.frames
        )
    }
}

/// A drive's read offset correction in stereo samples, as published in the
/// AccurateRip drive database: the number of samples to *add* to the read
/// position so that data lines up with the reference. Positive for most
/// drives (for example `+6`). Applied sample-exact by the reader.
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct SampleOffset(pub i32);

impl fmt::Display for SampleOffset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:+}", self.0)
    }
}

/// Where the read offset in use came from.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OffsetSource {
    /// Matched in the AccurateRip drive database by vendor and model.
    AccurateRipDatabase,
    /// Typed in by the user in Settings.
    User,
    /// Unknown drive: no correction applied (offset 0). The log says so.
    Unknown,
}

/// A track number, 1–99.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct TrackNumber(u8);

impl TrackNumber {
    /// Validates a track number (1–99).
    pub fn new(number: u8) -> Result<Self, ModelError> {
        if (1..=99).contains(&number) {
            Ok(TrackNumber(number))
        } else {
            Err(ModelError::TrackNumber(number))
        }
    }

    /// The number as a plain integer.
    pub fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for TrackNumber {
    type Error = ModelError;
    fn try_from(number: u8) -> Result<Self, ModelError> {
        TrackNumber::new(number)
    }
}

impl From<TrackNumber> for u8 {
    fn from(number: TrackNumber) -> u8 {
        number.0
    }
}

impl fmt::Display for TrackNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}", self.0)
    }
}

/// A session number, 1–99. Audio CDs have one session; an enhanced CD
/// (CD-Extra) has its audio in session 1 and a data track in session 2.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct SessionNumber(u8);

impl SessionNumber {
    /// Session 1, where every audio CD's music is.
    pub const FIRST: SessionNumber = SessionNumber(1);

    /// Validates a session number (1–99).
    pub fn new(number: u8) -> Result<Self, ModelError> {
        if (1..=99).contains(&number) {
            Ok(SessionNumber(number))
        } else {
            Err(ModelError::SessionNumber(number))
        }
    }

    /// The number as a plain integer.
    pub fn get(self) -> u8 {
        self.0
    }

    /// The session after this one, if there can be one.
    pub fn next(self) -> Option<SessionNumber> {
        SessionNumber::new(self.0.checked_add(1)?).ok()
    }
}

impl Default for SessionNumber {
    fn default() -> Self {
        SessionNumber::FIRST
    }
}

impl TryFrom<u8> for SessionNumber {
    type Error = ModelError;
    fn try_from(number: u8) -> Result<Self, ModelError> {
        SessionNumber::new(number)
    }
}

impl From<SessionNumber> for u8 {
    fn from(number: SessionNumber) -> u8 {
        number.0
    }
}

impl fmt::Display for SessionNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A CRC-32 (IEEE, as computed by `crc32fast`) of a track's PCM bytes, as
/// reported in rip logs ("Copy CRC" / "Test CRC").
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Crc32(pub u32);

impl fmt::Display for Crc32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08X}", self.0)
    }
}

/// An AccurateRip track checksum (v1 or v2).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccurateRipCrc(pub u32);

impl fmt::Display for AccurateRipCrc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08X}", self.0)
    }
}

/// The loudest absolute sample value in a track (0–32768), either channel.
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Peak(pub u16);

impl Peak {
    /// Full scale: the magnitude of `i16::MIN`.
    pub const FULL_SCALE: u16 = 32768;

    /// Peak as a percentage of full scale with one decimal, e.g. `98.7`.
    pub fn percent(self) -> f64 {
        (f64::from(self.0) * 1000.0 / f64::from(Self::FULL_SCALE)).round() / 10.0
    }
}

/// A SHA-256 digest; serialised as 64 lowercase hex digits.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct Sha256Digest(pub [u8; 32]);

impl Sha256Digest {
    /// Lowercase hex, as sent in the `sha256` upload field.
    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }

    /// Parses 64 hex digits (either case).
    pub fn from_hex(text: &str) -> Result<Self, ModelError> {
        let bytes = text.as_bytes();
        if bytes.len() != 64 {
            return Err(ModelError::Sha256(text.to_owned()));
        }
        let mut out = [0u8; 32];
        let (pairs, _) = bytes.as_chunks::<2>();
        for (slot, pair) in out.iter_mut().zip(pairs) {
            let pair =
                std::str::from_utf8(pair).map_err(|_| ModelError::Sha256(text.to_owned()))?;
            *slot =
                u8::from_str_radix(pair, 16).map_err(|_| ModelError::Sha256(text.to_owned()))?;
        }
        Ok(Sha256Digest(out))
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha256Digest({})", self.to_hex())
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Sha256Digest::from_hex(&text).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// An International Standard Recording Code: 12 ASCII letters/digits
/// (for example `GBAYE0000351`), stored upper-case.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Isrc(String);

impl Isrc {
    /// Validates and normalises an ISRC (dashes are removed).
    pub fn new(text: &str) -> Result<Self, ModelError> {
        let cleaned: String = text
            .chars()
            .filter(|c| *c != '-')
            .map(|c| c.to_ascii_uppercase())
            .collect();
        if cleaned.len() == 12 && cleaned.chars().all(|c| c.is_ascii_alphanumeric()) {
            Ok(Isrc(cleaned))
        } else {
            Err(ModelError::Isrc(text.to_owned()))
        }
    }

    /// The 12-character code.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Isrc {
    type Error = ModelError;
    fn try_from(text: String) -> Result<Self, ModelError> {
        Isrc::new(&text)
    }
}

impl From<Isrc> for String {
    fn from(isrc: Isrc) -> String {
        isrc.0
    }
}

/// A Media Catalogue Number (the disc's UPC/EAN barcode): 13 digits.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Mcn(String);

impl Mcn {
    /// Validates an MCN (exactly 13 ASCII digits).
    pub fn new(text: &str) -> Result<Self, ModelError> {
        if text.len() == 13 && text.bytes().all(|b| b.is_ascii_digit()) {
            Ok(Mcn(text.to_owned()))
        } else {
            Err(ModelError::Mcn(text.to_owned()))
        }
    }

    /// The 13 digits.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Drives report all zeros when a disc has no MCN.
    pub fn is_blank(&self) -> bool {
        self.0.bytes().all(|b| b == b'0')
    }
}

impl TryFrom<String> for Mcn {
    type Error = ModelError;
    fn try_from(text: String) -> Result<Self, ModelError> {
        Mcn::new(&text)
    }
}

impl From<Mcn> for String {
    fn from(mcn: Mcn) -> String {
        mcn.0
    }
}

/// A MusicBrainz identifier (a UUID such as
/// `b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d`), stored lower-case.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Mbid(String);

impl Mbid {
    /// Validates the 8-4-4-4-12 hex UUID form.
    pub fn new(text: &str) -> Result<Self, ModelError> {
        let lower = text.to_ascii_lowercase();
        let groups: Vec<&str> = lower.split('-').collect();
        let lengths_ok = groups.iter().map(|g| g.len()).eq([8, 4, 4, 4, 12]);
        let hex_ok = groups
            .iter()
            .all(|g| g.bytes().all(|b| b.is_ascii_hexdigit()));
        if lengths_ok && hex_ok {
            Ok(Mbid(lower))
        } else {
            Err(ModelError::Mbid(text.to_owned()))
        }
    }

    /// The UUID text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Mbid {
    type Error = ModelError;
    fn try_from(text: String) -> Result<Self, ModelError> {
        Mbid::new(&text)
    }
}

impl From<Mbid> for String {
    fn from(mbid: Mbid) -> String {
        mbid.0
    }
}

impl fmt::Display for Mbid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The disc identifiers computed from a TOC (by `core::discid`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscIds {
    /// MusicBrainz disc ID (28 characters, e.g. `49HHV7Eb8UKF3aQiNmu1GR8vKTY-`).
    pub musicbrainz: String,
    /// freedb/CDDB disc ID (also used by AccurateRip).
    pub cddb: u32,
    /// AccurateRip disc ID 1 and 2 plus the audio track count.
    pub accuraterip: AccurateRipDiscId,
    /// CUETools database TOC id (as sent to `db.cuetools.net`).
    pub ctdb_toc: String,
}

/// The AccurateRip disc identity; names the `dBAR-…bin` file.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccurateRipDiscId {
    /// Number of audio tracks.
    pub track_count: u8,
    /// Sum of audio track offsets (AccurateRip "disc ID 1").
    pub id1: u32,
    /// Weighted sum of audio track offsets (AccurateRip "disc ID 2").
    pub id2: u32,
    /// The freedb/CDDB id.
    pub cddb: u32,
}

// ---------------------------------------------------------------------------
// Disc and TOC
// ---------------------------------------------------------------------------

/// One entry of the table of contents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackEntry {
    /// Track number.
    pub number: TrackNumber,
    /// First sector of the track proper (INDEX 01).
    pub start_lba: Lba,
    /// Length up to the next track's INDEX 01 (or the lead-out), so it
    /// includes the next track's pre-gap, matching how rippers split tracks.
    /// The last track of a session ends at that session's lead-out: on an
    /// enhanced CD the last audio track stops where the audio session does,
    /// not at the data track (the gap between sessions is no track's).
    pub length_sectors: Sectors,
    /// Audio track (`false` for data tracks of enhanced/mixed-mode discs,
    /// which are never ripped as audio).
    pub is_audio: bool,
    /// Length of this track's pre-gap (INDEX 00 → INDEX 01). For track 1 a
    /// non-zero pre-gap beyond the standard is hidden track one audio (HTOA).
    #[serde(default)]
    pub pregap: Sectors,
    /// The track's ISRC, when the drive reports one.
    #[serde(default)]
    pub isrc: Option<Isrc>,
    /// The session the track is in (1 unless the disc has several; absent
    /// in files written before sessions were recorded, read as 1).
    #[serde(default)]
    pub session: SessionNumber,
}

impl TrackEntry {
    /// First sector after the track (next track's INDEX 01 or the lead-out).
    pub fn end_lba(&self) -> Option<Lba> {
        self.start_lba.checked_add(self.length_sectors)
    }
}

/// How audio and data tracks are arranged on a disc.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiscLayout {
    /// Audio tracks only (a Red Book audio CD).
    Audio,
    /// Audio first, then data tracks (CD-Extra / Blue Book "enhanced CD",
    /// normally in a second session). The audio rips normally; the data is
    /// left alone.
    Enhanced,
    /// A data track before (or between) audio tracks ("mixed mode", e.g.
    /// game and CD-ROM discs with music after the program). Not ripped.
    MixedMode,
    /// No audio tracks at all.
    NoAudio,
}

/// A validated table of contents: tracks in order, numbered consecutively,
/// non-overlapping, ending at the lead-out. Within a session each track
/// starts where the previous one ends; between sessions there is a gap
/// (the earlier session's lead-out and the next one's lead-in), which
/// belongs to no track.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "TocParts", into = "TocParts")]
pub struct Toc {
    tracks: Vec<TrackEntry>,
    lead_out: Lba,
}

/// The unvalidated shape of a [`Toc`] (its serde form).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TocParts {
    /// Tracks in disc order.
    pub tracks: Vec<TrackEntry>,
    /// The lead-out position (first sector after the last track).
    pub lead_out: Lba,
}

impl Toc {
    /// Validates the track list: at least one track, consecutive numbers,
    /// sessions that start at 1 and go up one at a time, each track
    /// starting where the previous one ends (or, at the start of a new
    /// session, after it) and the last one ending at `lead_out`.
    pub fn new(tracks: Vec<TrackEntry>, lead_out: Lba) -> Result<Self, ModelError> {
        let Some(first) = tracks.first() else {
            return Err(ModelError::Toc("the disc has no tracks".into()));
        };
        if first.session != SessionNumber::FIRST {
            return Err(ModelError::Toc(format!(
                "track {} is in session {} but the disc starts with session 1",
                first.number, first.session
            )));
        }
        let mut expected_number = first.number.get();
        let mut expected_start = first.start_lba;
        let mut session = first.session;
        for track in &tracks {
            if track.number.get() != expected_number {
                return Err(ModelError::Toc(format!(
                    "track {} follows track {}",
                    track.number.get(),
                    expected_number.saturating_sub(1)
                )));
            }
            if track.session == session {
                if track.start_lba != expected_start {
                    return Err(ModelError::Toc(format!(
                        "track {} starts at sector {} but the previous track ends at {}",
                        track.number, track.start_lba, expected_start
                    )));
                }
            } else if Some(track.session) == session.next() {
                if track.start_lba <= expected_start {
                    return Err(ModelError::Toc(format!(
                        "track {} starts session {} at sector {}, but session {} runs to sector {}",
                        track.number, track.session, track.start_lba, session, expected_start
                    )));
                }
                session = track.session;
            } else {
                return Err(ModelError::Toc(format!(
                    "track {} is in session {} after a track in session {}",
                    track.number, track.session, session
                )));
            }
            if track.length_sectors.0 == 0 {
                return Err(ModelError::Toc(format!("track {} is empty", track.number)));
            }
            expected_start = track
                .end_lba()
                .ok_or_else(|| ModelError::Toc("sector position overflow".into()))?;
            expected_number = expected_number.saturating_add(1);
        }
        if expected_start != lead_out {
            return Err(ModelError::Toc(format!(
                "the last track ends at sector {expected_start} but the lead-out is at {lead_out}"
            )));
        }
        Ok(Toc { tracks, lead_out })
    }

    /// Builds a TOC from a drive's flat listing, where every track runs to
    /// the next one's start and the session of each track is unknown (all
    /// are given as session 1). Data tracks after the last audio track are
    /// taken to be the second session of an enhanced CD, as libdiscid and
    /// whipper assume: they move to session 2 and the last audio track ends
    /// [`SESSION_GAP_SECTORS`] before the first of them. Any other listing
    /// is validated as it is.
    pub fn new_assuming_enhanced_gap(
        mut tracks: Vec<TrackEntry>,
        lead_out: Lba,
    ) -> Result<Self, ModelError> {
        let last_audio = tracks.iter().rposition(|t| t.is_audio);
        if let Some(last_audio) = last_audio
            && last_audio + 1 < tracks.len()
            && tracks.iter().all(|t| t.session == SessionNumber::FIRST)
        {
            let second = SessionNumber::FIRST
                .next()
                .ok_or_else(|| ModelError::Toc("no second session".into()))?;
            let data_start = tracks
                .get(last_audio + 1)
                .map(|t| t.start_lba)
                .ok_or_else(|| ModelError::Toc("the data track is missing".into()))?;
            for track in tracks.iter_mut().skip(last_audio + 1) {
                track.session = second;
            }
            let audio = tracks
                .get_mut(last_audio)
                .ok_or_else(|| ModelError::Toc("the last audio track is missing".into()))?;
            let audio_end = data_start.0.checked_sub(SESSION_GAP_SECTORS).ok_or_else(|| {
                ModelError::Toc(format!(
                    "the data track at sector {data_start} is too close to the start of the disc for an enhanced CD"
                ))
            })?;
            let length = Lba(audio_end).sectors_since(audio.start_lba).ok_or_else(|| {
                ModelError::Toc(format!(
                    "track {} would end before it starts once the {SESSION_GAP_SECTORS}-sector gap before the data track is left out",
                    audio.number
                ))
            })?;
            audio.length_sectors = length;
        }
        Toc::new(tracks, lead_out)
    }

    /// Tracks in disc order (never empty).
    pub fn tracks(&self) -> &[TrackEntry] {
        &self.tracks
    }

    /// Audio tracks only, in disc order.
    pub fn audio_tracks(&self) -> impl Iterator<Item = &TrackEntry> {
        self.tracks.iter().filter(|t| t.is_audio)
    }

    /// Data tracks only, in disc order (never ripped).
    pub fn data_tracks(&self) -> impl Iterator<Item = &TrackEntry> {
        self.tracks.iter().filter(|t| !t.is_audio)
    }

    /// The track with this number.
    pub fn track(&self, number: TrackNumber) -> Option<&TrackEntry> {
        self.tracks.iter().find(|t| t.number == number)
    }

    /// The first track (a valid TOC always has one).
    pub fn first_track(&self) -> Option<&TrackEntry> {
        self.tracks.first()
    }

    /// The last track.
    pub fn last_track(&self) -> Option<&TrackEntry> {
        self.tracks.last()
    }

    /// The lead-out position (of the last session).
    pub fn lead_out(&self) -> Lba {
        self.lead_out
    }

    /// The first sector after the last audio track: the end of the audio
    /// that can be read. On an enhanced CD this is the audio session's
    /// lead-out, well before the data track; `None` without audio tracks.
    pub fn audio_end(&self) -> Option<Lba> {
        self.audio_tracks().last().and_then(TrackEntry::end_lba)
    }

    /// How many sessions the disc has.
    pub fn session_count(&self) -> u8 {
        self.tracks.last().map_or(1, |t| t.session.get())
    }

    /// How audio and data are arranged.
    pub fn layout(&self) -> DiscLayout {
        let Some(first_audio) = self.tracks.iter().position(|t| t.is_audio) else {
            return DiscLayout::NoAudio;
        };
        let last_audio = self
            .tracks
            .iter()
            .rposition(|t| t.is_audio)
            .unwrap_or(first_audio);
        let data_among_audio = self
            .tracks
            .get(..=last_audio)
            .is_some_and(|before| before.iter().any(|t| !t.is_audio));
        if data_among_audio {
            DiscLayout::MixedMode
        } else if last_audio + 1 < self.tracks.len() {
            DiscLayout::Enhanced
        } else {
            DiscLayout::Audio
        }
    }

    /// Total playing time of the audio tracks in milliseconds.
    pub fn audio_duration_ms(&self) -> u64 {
        self.audio_tracks()
            .map(|t| t.length_sectors.duration_ms())
            .sum()
    }
}

impl TryFrom<TocParts> for Toc {
    type Error = ModelError;
    fn try_from(parts: TocParts) -> Result<Self, ModelError> {
        Toc::new(parts.tracks, parts.lead_out)
    }
}

impl From<Toc> for TocParts {
    fn from(toc: Toc) -> TocParts {
        TocParts {
            tracks: toc.tracks,
            lead_out: toc.lead_out,
        }
    }
}

/// The drive's identity as reported by SCSI INQUIRY (trimmed).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveIdentity {
    /// Vendor, e.g. `PLEXTOR`.
    pub vendor: String,
    /// Model, e.g. `DVDR   PX-760A` trimmed to `DVDR PX-760A`.
    pub model: String,
    /// Firmware revision, e.g. `1.07`.
    pub revision: String,
}

impl fmt::Display for DriveIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.vendor, self.model)?;
        if !self.revision.is_empty() {
            write!(f, " (firmware {})", self.revision)?;
        }
        Ok(())
    }
}

/// CD-Text strings for the album or one track. Empty fields are `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CdTextFields {
    /// TITLE.
    pub title: Option<String>,
    /// PERFORMER.
    pub performer: Option<String>,
    /// SONGWRITER.
    pub songwriter: Option<String>,
    /// COMPOSER.
    pub composer: Option<String>,
    /// ARRANGER.
    pub arranger: Option<String>,
    /// MESSAGE.
    pub message: Option<String>,
}

/// CD-Text for one track.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CdTextTrack {
    /// Track number.
    pub number: TrackNumber,
    /// The track's strings.
    #[serde(flatten)]
    pub fields: CdTextFields,
}

/// CD-Text read from the disc (first language block only).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CdText {
    /// Album-level strings.
    pub album: CdTextFields,
    /// Per-track strings, in track order.
    pub tracks: Vec<CdTextTrack>,
}

/// Everything known about the disc in the drive before reading audio.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscInfo {
    /// The drive the disc is in.
    pub drive: DriveIdentity,
    /// Table of contents.
    pub toc: Toc,
    /// Media catalogue number, when present and not blank.
    #[serde(default)]
    pub mcn: Option<Mcn>,
    /// CD-Text, when present.
    #[serde(default)]
    pub cd_text: Option<CdText>,
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// How carefully tracks are read.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReadMode {
    /// Full paranoia (overlap + verify, scratch repair, cache defeat), once.
    #[default]
    Secure,
    /// Secure read twice per track; the copy and test CRCs must match.
    TestAndCopy,
}

impl fmt::Display for ReadMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ReadMode::Secure => "Secure",
            ReadMode::TestAndCopy => "Test and copy",
        })
    }
}

/// Where a track's PCM ended up after reading: raw 16-bit little-endian
/// stereo, offset-corrected, exactly `length_sectors × 2352` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pcm {
    /// A temporary file (the normal case for whole discs).
    File(PathBuf),
    /// In memory (tests, short tracks). Cheap to clone.
    Memory(Arc<[u8]>),
}

/// The outcome of reading one track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackReadResult {
    /// Which track.
    pub track: TrackNumber,
    /// The audio data.
    pub pcm: Pcm,
    /// Number of stereo samples in `pcm`.
    pub samples: Samples,
    /// CRC-32 of the copy pass.
    pub copy_crc: Crc32,
    /// CRC-32 of the test pass (test-and-copy mode only).
    pub test_crc: Option<Crc32>,
    /// Sectors that never verified, in ascending order. Never hidden: they
    /// appear in the log and make the rip "with errors".
    pub unrecovered_sectors: Vec<Lba>,
    /// Sectors that read wrong at first and came right on re-reading
    /// (paranoia corrections and test-and-copy repairs). Not errors, but
    /// recorded in the log so a struggling disc is never hidden.
    pub retried_sectors: u32,
    /// Loudest sample.
    pub peak: Peak,
}

impl TrackReadResult {
    /// True when every sector verified and, in test-and-copy mode, the CRCs
    /// matched.
    pub fn is_clean(&self) -> bool {
        self.unrecovered_sectors.is_empty() && self.test_crc.is_none_or(|t| t == self.copy_crc)
    }
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

/// An image to embed and upload as the front cover.
#[derive(Clone, PartialEq, Eq)]
pub struct CoverArt {
    /// Encoded image bytes.
    pub bytes: Arc<[u8]>,
    /// MIME type, e.g. `image/jpeg`.
    pub mime: String,
}

impl fmt::Debug for CoverArt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoverArt")
            .field("bytes", &self.bytes.len())
            .field("mime", &self.mime)
            .finish()
    }
}

/// Metadata for one track, editable by the user before ripping.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackMetadata {
    /// Track number on the disc (1-based); `None` only in an unfilled form.
    pub number: Option<TrackNumber>,
    /// Track title.
    pub title: String,
    /// Track artist credit (may differ from the album artist).
    pub artist: String,
    /// MusicBrainz recording id.
    pub recording_mbid: Option<Mbid>,
    /// MusicBrainz track id (the track on this release).
    pub track_mbid: Option<Mbid>,
    /// MusicBrainz artist ids for the track credit.
    #[serde(default)]
    pub artist_mbids: Vec<Mbid>,
    /// ISRC (from MusicBrainz or the disc).
    pub isrc: Option<Isrc>,
}

/// Album-level metadata for the disc being ripped.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseMetadata {
    /// Album title.
    pub title: String,
    /// Album artist credit.
    pub artist: String,
    /// Album artist sort name, when known.
    pub artist_sort: Option<String>,
    /// MusicBrainz release id.
    pub release_mbid: Option<Mbid>,
    /// MusicBrainz release group id.
    pub release_group_mbid: Option<Mbid>,
    /// MusicBrainz album artist ids.
    #[serde(default)]
    pub artist_mbids: Vec<Mbid>,
    /// MusicBrainz disc id this was matched by.
    pub disc_id: Option<String>,
    /// Release year.
    pub year: Option<u16>,
    /// Full release date as given by MusicBrainz (`YYYY`, `YYYY-MM` or `YYYY-MM-DD`).
    pub date: Option<String>,
    /// Record label.
    pub label: Option<String>,
    /// Catalogue number.
    pub catalog_number: Option<String>,
    /// Barcode (UPC/EAN).
    pub barcode: Option<String>,
    /// Which disc of a multi-disc release this is (1-based).
    pub disc_number: u8,
    /// How many discs the release has.
    pub disc_total: u8,
    /// Tracks of this disc in order.
    pub tracks: Vec<TrackMetadata>,
    /// Front cover, when found or supplied.
    #[serde(skip)]
    pub cover: Option<CoverArt>,
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

/// AccurateRip checksum versions.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccurateRipVersion {
    /// The original checksum.
    V1,
    /// The fixed checksum that covers every bit.
    V2,
}

/// The two AccurateRip checksums of one track.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccurateRipCrcs {
    /// v1 checksum.
    pub v1: AccurateRipCrc,
    /// v2 checksum.
    pub v2: AccurateRipCrc,
}

/// One track's standing in the AccurateRip database.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum AccurateRipStatus {
    /// Matches database entries; `confidence` is the number of submissions.
    Accurate {
        /// Which checksum matched (v2 preferred when both match).
        version: AccurateRipVersion,
        /// How many other rips agree.
        confidence: u32,
        /// `+0` when the match is with rips of this very pressing. Otherwise
        /// the match is with another pressing of the same album whose audio
        /// sits this many samples later on its disc (earlier when negative):
        /// the audio is identical, only positioned differently, so the rip
        /// is still an exact copy. Absent in older reports, read as `+0`.
        #[serde(default, rename = "pressingOffset")]
        pressing_offset: SampleOffset,
    },
    /// The disc is in the database but this track's checksum is not.
    Mismatch {
        /// The highest confidence among the entries that disagreed.
        best_confidence: u32,
    },
    /// The disc (pressing) is not in the database.
    NotInDatabase,
    /// The database could not be reached; the rip itself may be fine.
    Unavailable {
        /// Plain-English reason, e.g. "no internet connection".
        reason: String,
    },
}

impl AccurateRipStatus {
    /// The phrase shown in the UI and log, e.g. "Accurately ripped
    /// (confidence 8)" or "Accurately ripped (confidence 8, pressing offset
    /// +102)".
    pub fn summary(&self) -> String {
        match self {
            AccurateRipStatus::Accurate {
                confidence,
                pressing_offset,
                ..
            } => {
                if pressing_offset.0 == 0 {
                    format!("Accurately ripped (confidence {confidence})")
                } else {
                    format!(
                        "Accurately ripped (confidence {confidence}, pressing offset {pressing_offset})"
                    )
                }
            }
            AccurateRipStatus::Mismatch { .. } => "Mismatch".to_owned(),
            AccurateRipStatus::NotInDatabase => "Not in database".to_owned(),
            AccurateRipStatus::Unavailable { reason } => {
                format!("Could not check ({reason})")
            }
        }
    }

    /// True for [`AccurateRipStatus::Accurate`].
    pub fn is_accurate(&self) -> bool {
        matches!(self, AccurateRipStatus::Accurate { .. })
    }
}

/// AccurateRip result for one track.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackVerification {
    /// Which track.
    pub track: TrackNumber,
    /// The checksums computed from the rip.
    pub crcs: AccurateRipCrcs,
    /// Database outcome.
    pub accuraterip: AccurateRipStatus,
}

/// The CUETools database result for the whole disc.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum CtdbStatus {
    /// The rip matches an entry.
    Accurate {
        /// Submissions agreeing with this rip.
        confidence: u32,
        /// Total submissions for this TOC.
        total: u32,
        /// `+0` for an entry of this pressing; otherwise the entry is of a
        /// pressing whose audio sits this many samples later (see
        /// [`AccurateRipStatus::Accurate`]). Absent in older reports.
        #[serde(default, rename = "pressingOffset")]
        pressing_offset: SampleOffset,
    },
    /// Entries exist but none match (repairable or damaged rip).
    Mismatch {
        /// Total submissions for this TOC.
        total: u32,
    },
    /// No entry for this TOC.
    NotInDatabase,
    /// The database could not be reached.
    Unavailable {
        /// Plain-English reason.
        reason: String,
    },
}

impl CtdbStatus {
    /// The phrase shown in the UI and log.
    pub fn summary(&self) -> String {
        match self {
            CtdbStatus::Accurate {
                confidence,
                total,
                pressing_offset,
            } => {
                if pressing_offset.0 == 0 {
                    format!("Accurately ripped (confidence {confidence} of {total})")
                } else {
                    format!(
                        "Accurately ripped (confidence {confidence} of {total}, pressing offset {pressing_offset})"
                    )
                }
            }
            CtdbStatus::Mismatch { .. } => "Mismatch".to_owned(),
            CtdbStatus::NotInDatabase => "Not in database".to_owned(),
            CtdbStatus::Unavailable { reason } => format!("Could not check ({reason})"),
        }
    }
}

/// Verification of a whole rip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationResult {
    /// Per audio track, in order.
    pub tracks: Vec<TrackVerification>,
    /// CTDB, disc-wide.
    pub ctdb: CtdbStatus,
}

// ---------------------------------------------------------------------------
// Report (rip.json, rendered into the log)
// ---------------------------------------------------------------------------

/// One track in the rip report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackReport {
    /// Track number.
    pub number: TrackNumber,
    /// Title used for the file.
    pub title: String,
    /// Track start.
    pub start_lba: Lba,
    /// Track length.
    pub length_sectors: Sectors,
    /// Pre-gap length.
    pub pregap: Sectors,
    /// ISRC, if any.
    pub isrc: Option<Isrc>,
    /// Peak level.
    pub peak: Peak,
    /// Copy CRC.
    pub copy_crc: Crc32,
    /// Test CRC (test-and-copy mode).
    pub test_crc: Option<Crc32>,
    /// AccurateRip checksums and result.
    pub accuraterip_crcs: AccurateRipCrcs,
    /// AccurateRip result.
    pub accuraterip: AccurateRipStatus,
    /// Sectors that never verified.
    pub unrecovered_sectors: Vec<Lba>,
    /// Sectors that came right only on re-reading (absent in older
    /// reports, read as 0).
    #[serde(default)]
    pub retried_sectors: u32,
    /// The FLAC file name, relative to the rip folder.
    pub file_name: String,
    /// SHA-256 of the FLAC file.
    pub file_sha256: Option<Sha256Digest>,
}

/// Everything the log, `rip.json` and the Done screen say about a rip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RipReport {
    /// `library-rip` version that made the rip.
    pub app_version: String,
    /// When reading started.
    pub started_at: jiff::Timestamp,
    /// When the report was finished.
    pub finished_at: jiff::Timestamp,
    /// The drive.
    pub drive: DriveIdentity,
    /// Read offset applied.
    pub read_offset: SampleOffset,
    /// Where the offset came from.
    pub offset_source: OffsetSource,
    /// Read mode.
    pub read_mode: ReadMode,
    /// The TOC.
    pub toc: Toc,
    /// Identifiers computed from the TOC.
    pub disc_ids: DiscIds,
    /// MCN, if any.
    pub mcn: Option<Mcn>,
    /// Album metadata as confirmed by the user.
    pub release: ReleaseMetadata,
    /// Per audio track.
    pub tracks: Vec<TrackReport>,
    /// CTDB result.
    pub ctdb: CtdbStatus,
    /// The verdict on the whole disc (format version 2; worked out from
    /// the tracks when an older report is read).
    #[serde(default)]
    pub verdict: DiscVerdict,
}

/// The verdict on a whole rip, from the read results and both databases.
///
/// A rip that matches only another pressing (a non-zero pressing offset) is
/// [`DiscVerdict::Accurate`]: its audio is bit-for-bit what that pressing's
/// rippers got, merely positioned differently on the disc, so it is an exact
/// archival copy of the music. The offsets are listed so nobody mistakes it
/// for a rip of that other pressing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum DiscVerdict {
    /// Some sectors could not be read reliably: not an exact copy.
    ReadErrors,
    /// Every audio track is confirmed by AccurateRip, or the CUETools
    /// database confirms the whole disc.
    Accurate {
        /// Pressing offsets (other than `+0`) of the matches, ascending;
        /// empty when everything matched this pressing.
        #[serde(rename = "otherPressingOffsets")]
        other_pressing_offsets: Vec<SampleOffset>,
    },
    /// Some tracks are confirmed, others are not.
    PartlyAccurate {
        /// Tracks AccurateRip confirms.
        #[serde(rename = "accurateTracks")]
        accurate_tracks: u32,
        /// Audio tracks on the disc.
        tracks: u32,
    },
    /// The databases know the disc but disagree with this rip.
    Mismatch,
    /// Neither database knows the disc.
    NotInDatabase,
    /// The databases could not be asked (or nothing was checked yet).
    #[default]
    Unchecked,
}

impl DiscVerdict {
    /// Works the verdict out from the per-track results and CTDB.
    pub fn from_results(tracks: &[TrackReport], ctdb: &CtdbStatus) -> DiscVerdict {
        let read_errors = tracks.iter().any(|t| {
            !t.unrecovered_sectors.is_empty() || t.test_crc.is_some_and(|test| test != t.copy_crc)
        });
        if read_errors {
            return DiscVerdict::ReadErrors;
        }
        let mut offsets: Vec<SampleOffset> = tracks
            .iter()
            .filter_map(|t| match t.accuraterip {
                AccurateRipStatus::Accurate {
                    pressing_offset, ..
                } => Some(pressing_offset),
                AccurateRipStatus::Mismatch { .. }
                | AccurateRipStatus::NotInDatabase
                | AccurateRipStatus::Unavailable { .. } => None,
            })
            .collect();
        let accurate = offsets.len();
        let ctdb_accurate = match ctdb {
            CtdbStatus::Accurate {
                pressing_offset, ..
            } => {
                offsets.push(*pressing_offset);
                true
            }
            CtdbStatus::Mismatch { .. }
            | CtdbStatus::NotInDatabase
            | CtdbStatus::Unavailable { .. } => false,
        };
        offsets.retain(|o| o.0 != 0);
        offsets.sort();
        offsets.dedup();
        let all = !tracks.is_empty() && accurate == tracks.len();
        if all || ctdb_accurate {
            return DiscVerdict::Accurate {
                other_pressing_offsets: offsets,
            };
        }
        if accurate > 0 {
            return DiscVerdict::PartlyAccurate {
                accurate_tracks: u32::try_from(accurate).unwrap_or(u32::MAX),
                tracks: u32::try_from(tracks.len()).unwrap_or(u32::MAX),
            };
        }
        let mismatch = matches!(ctdb, CtdbStatus::Mismatch { .. })
            || tracks
                .iter()
                .any(|t| matches!(t.accuraterip, AccurateRipStatus::Mismatch { .. }));
        if mismatch {
            return DiscVerdict::Mismatch;
        }
        let unchecked = matches!(ctdb, CtdbStatus::Unavailable { .. })
            || tracks
                .iter()
                .any(|t| matches!(t.accuraterip, AccurateRipStatus::Unavailable { .. }));
        if unchecked || tracks.is_empty() {
            DiscVerdict::Unchecked
        } else {
            DiscVerdict::NotInDatabase
        }
    }
}

impl RipReport {
    /// True when every track read cleanly (verification results aside).
    pub fn read_without_errors(&self) -> bool {
        self.tracks.iter().all(|t| {
            t.unrecovered_sectors.is_empty() && t.test_crc.is_none_or(|test| test == t.copy_crc)
        })
    }
}

// ---------------------------------------------------------------------------
// Upload
// ---------------------------------------------------------------------------

/// Media kinds the server accepts (`kind` upload field).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    /// Playable audio (the FLACs).
    Audio,
    /// Playable video (stage 2).
    Video,
    /// A document (PDF booklet scans, later).
    Document,
    /// Kept for the record, never streamed: log, cue, `rip.json`.
    Archive,
}

impl MediaKind {
    /// The wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Audio => "audio",
            MediaKind::Video => "video",
            MediaKind::Document => "document",
            MediaKind::Archive => "archive",
        }
    }
}

/// One file to upload to `POST /api/rip/items/{id}/media`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadFile {
    /// `audio` for FLACs, `archive` for log, cue and `rip.json`.
    pub kind: MediaKind,
    /// Title shown on the item page.
    pub title: String,
    /// Disc number (1-based).
    pub disc: u8,
    /// Track number for audio; `None` for archive files.
    pub track: Option<TrackNumber>,
    /// Duration for audio.
    pub duration_ms: Option<u64>,
    /// SHA-256 of the file (the server verifies it).
    pub sha256: Sha256Digest,
    /// Local path of the file.
    pub path: PathBuf,
}

/// Body of `POST /api/rip/items` (see docs/rip.md).
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewItem {
    /// Item type slug, `music` for CDs.
    pub type_slug: String,
    /// Title.
    pub title: String,
    /// Creator (album artist).
    pub creator: String,
    /// Year.
    pub year: Option<u16>,
    /// Identifier (barcode).
    pub identifier: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// Tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// MusicBrainz release id.
    pub musicbrainz_release_id: Option<Mbid>,
}

/// Which catalogue item receives the files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "target")]
pub enum ItemTarget {
    /// Create a new item first.
    New(NewItem),
    /// Attach to an existing item.
    Existing {
        /// The item's id on the server.
        id: i64,
    },
}

/// Everything to send to the library after a rip, in upload order: cover,
/// audio files by track, then archive files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadPlan {
    /// Target item.
    pub item: ItemTarget,
    /// Front cover to upload as the item photo.
    pub cover: Option<CoverArt>,
    /// Files in upload order.
    pub files: Vec<UploadFile>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A value that does not satisfy a model invariant.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    /// Track numbers run from 1 to 99.
    #[error("track number {0} is not between 1 and 99")]
    TrackNumber(u8),
    /// Session numbers run from 1 to 99.
    #[error("session number {0} is not between 1 and 99")]
    SessionNumber(u8),
    /// Malformed table of contents.
    #[error("the table of contents does not make sense: {0}")]
    Toc(String),
    /// Malformed ISRC.
    #[error("\"{0}\" is not an ISRC (12 letters and digits)")]
    Isrc(String),
    /// Malformed MCN.
    #[error("\"{0}\" is not a media catalogue number (13 digits)")]
    Mcn(String),
    /// Malformed MusicBrainz id.
    #[error("\"{0}\" is not a MusicBrainz id")]
    Mbid(String),
    /// Malformed SHA-256.
    #[error("\"{0}\" is not a SHA-256 checksum (64 hex digits)")]
    Sha256(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(number: u8, start: u32, length: u32) -> TrackEntry {
        TrackEntry {
            number: TrackNumber::new(number).unwrap(),
            start_lba: Lba(start),
            length_sectors: Sectors(length),
            is_audio: true,
            pregap: Sectors(0),
            isrc: None,
            session: SessionNumber::FIRST,
        }
    }

    #[test]
    fn toc_validates_contiguity() {
        let toc = Toc::new(vec![track(1, 0, 100), track(2, 100, 50)], Lba(150)).unwrap();
        assert_eq!(toc.tracks().len(), 2);
        assert!(Toc::new(vec![track(1, 0, 100), track(2, 101, 49)], Lba(150)).is_err());
        assert!(Toc::new(vec![track(1, 0, 100), track(3, 100, 50)], Lba(150)).is_err());
        assert!(Toc::new(vec![track(1, 0, 100)], Lba(99)).is_err());
        assert!(Toc::new(vec![], Lba(0)).is_err());
    }

    fn data(number: u8, start: u32, length: u32, session: u8) -> TrackEntry {
        TrackEntry {
            is_audio: false,
            session: SessionNumber::new(session).unwrap(),
            ..track(number, start, length)
        }
    }

    #[test]
    fn sessions_allow_a_gap_and_nothing_else() {
        // Enhanced CD: audio to 150, the data track in session 2 at 11 550.
        let toc = Toc::new(
            vec![
                track(1, 0, 100),
                track(2, 100, 50),
                data(3, 11_550, 1_500, 2),
            ],
            Lba(13_050),
        )
        .unwrap();
        assert_eq!(toc.audio_end(), Some(Lba(150)));
        assert_eq!(toc.session_count(), 2);
        assert_eq!(toc.layout(), DiscLayout::Enhanced);
        assert_eq!(toc.data_tracks().count(), 1);
        // A gap within a session is still refused…
        assert!(Toc::new(vec![track(1, 0, 100), data(2, 200, 50, 1)], Lba(250)).is_err());
        // …as is a new session that overlaps the last, skipping a session
        // number, going back a session, or not starting with session 1.
        assert!(Toc::new(vec![track(1, 0, 100), data(2, 100, 50, 2)], Lba(150)).is_err());
        assert!(Toc::new(vec![track(1, 0, 100), data(2, 200, 50, 3)], Lba(250)).is_err());
        let mut later = vec![track(1, 0, 100), data(2, 200, 50, 2), track(3, 250, 10)];
        later[2].session = SessionNumber::new(2).unwrap();
        assert!(Toc::new(later, Lba(260)).is_ok());
        let mut back = vec![track(1, 0, 100), data(2, 200, 50, 2), track(3, 250, 10)];
        back[2].session = SessionNumber::FIRST;
        assert!(Toc::new(back, Lba(260)).is_err());
        assert!(Toc::new(vec![data(1, 0, 100, 2)], Lba(100)).is_err());
        assert!(SessionNumber::new(0).is_err());
        assert!(SessionNumber::new(100).is_err());
    }

    #[test]
    fn layouts() {
        let audio = Toc::new(vec![track(1, 0, 100)], Lba(100)).unwrap();
        assert_eq!(audio.layout(), DiscLayout::Audio);
        let mixed = Toc::new(vec![data(1, 0, 100, 1), track(2, 100, 50)], Lba(150)).unwrap();
        assert_eq!(mixed.layout(), DiscLayout::MixedMode);
        let only_data = Toc::new(vec![data(1, 0, 100, 1)], Lba(100)).unwrap();
        assert_eq!(only_data.layout(), DiscLayout::NoAudio);
        assert_eq!(only_data.audio_end(), None);
    }

    #[test]
    fn a_flat_listing_becomes_an_enhanced_cd() {
        let flat = vec![track(1, 0, 20_000), data(2, 20_000, 5_000, 1)];
        let toc = Toc::new_assuming_enhanced_gap(flat, Lba(25_000)).unwrap();
        assert_eq!(toc.audio_end(), Some(Lba(20_000 - SESSION_GAP_SECTORS)));
        assert_eq!(toc.tracks()[1].session.get(), 2);
        assert_eq!(toc.tracks()[1].start_lba, Lba(20_000));
        // An audio CD and a mixed-mode CD are left as they are.
        let audio = Toc::new_assuming_enhanced_gap(vec![track(1, 0, 100)], Lba(100)).unwrap();
        assert_eq!(audio.audio_end(), Some(Lba(100)));
        let mixed =
            Toc::new_assuming_enhanced_gap(vec![data(1, 0, 100, 1), track(2, 100, 50)], Lba(150))
                .unwrap();
        assert_eq!(mixed.session_count(), 1);
        // A data track too close to the audio for a session gap is refused.
        assert!(
            Toc::new_assuming_enhanced_gap(
                vec![track(1, 0, 5_000), data(2, 5_000, 100, 1)],
                Lba(5_100)
            )
            .is_err()
        );
    }

    #[test]
    fn older_files_without_sessions_still_load() {
        let json = r#"{"tracks":[{"number":1,"startLba":0,"lengthSectors":100,"isAudio":true}],"leadOut":100}"#;
        let toc: Toc = serde_json::from_str(json).unwrap();
        assert_eq!(toc.tracks()[0].session, SessionNumber::FIRST);
        let status: AccurateRipStatus =
            serde_json::from_str(r#"{"status":"accurate","version":"v2","confidence":4}"#).unwrap();
        assert_eq!(
            status,
            AccurateRipStatus::Accurate {
                version: AccurateRipVersion::V2,
                confidence: 4,
                pressing_offset: SampleOffset(0),
            }
        );
    }

    fn report_track(status: AccurateRipStatus) -> TrackReport {
        TrackReport {
            number: TrackNumber::new(1).unwrap(),
            title: String::new(),
            start_lba: Lba(0),
            length_sectors: Sectors(1),
            pregap: Sectors(0),
            isrc: None,
            peak: Peak(0),
            copy_crc: Crc32(1),
            test_crc: None,
            accuraterip_crcs: AccurateRipCrcs {
                v1: AccurateRipCrc(0),
                v2: AccurateRipCrc(0),
            },
            accuraterip: status,
            unrecovered_sectors: Vec::new(),
            retried_sectors: 0,
            file_name: String::new(),
            file_sha256: None,
        }
    }

    #[test]
    fn disc_verdicts() {
        let accurate = |offset: i32| AccurateRipStatus::Accurate {
            version: AccurateRipVersion::V2,
            confidence: 3,
            pressing_offset: SampleOffset(offset),
        };
        let mismatch = AccurateRipStatus::Mismatch { best_confidence: 2 };
        let none = CtdbStatus::NotInDatabase;
        let verdict = |statuses: Vec<AccurateRipStatus>, ctdb: &CtdbStatus| {
            let tracks: Vec<TrackReport> = statuses.into_iter().map(report_track).collect();
            DiscVerdict::from_results(&tracks, ctdb)
        };
        assert_eq!(
            verdict(vec![accurate(0), accurate(0)], &none),
            DiscVerdict::Accurate {
                other_pressing_offsets: Vec::new()
            }
        );
        // Matching only another pressing is still an accurate rip.
        assert_eq!(
            verdict(vec![accurate(102), accurate(102)], &none),
            DiscVerdict::Accurate {
                other_pressing_offsets: vec![SampleOffset(102)]
            }
        );
        assert_eq!(
            verdict(vec![accurate(0), mismatch.clone()], &none),
            DiscVerdict::PartlyAccurate {
                accurate_tracks: 1,
                tracks: 2
            }
        );
        let ctdb = CtdbStatus::Accurate {
            confidence: 2,
            total: 2,
            pressing_offset: SampleOffset(-48),
        };
        assert_eq!(
            verdict(vec![AccurateRipStatus::NotInDatabase], &ctdb),
            DiscVerdict::Accurate {
                other_pressing_offsets: vec![SampleOffset(-48)]
            }
        );
        assert_eq!(verdict(vec![mismatch], &none), DiscVerdict::Mismatch);
        assert_eq!(
            verdict(vec![AccurateRipStatus::NotInDatabase], &none),
            DiscVerdict::NotInDatabase
        );
        assert_eq!(
            verdict(
                vec![AccurateRipStatus::Unavailable {
                    reason: "offline".into()
                }],
                &none
            ),
            DiscVerdict::Unchecked
        );
        let mut broken = report_track(accurate(0));
        broken.unrecovered_sectors.push(Lba(5));
        assert_eq!(
            DiscVerdict::from_results(&[broken], &none),
            DiscVerdict::ReadErrors
        );
    }

    #[test]
    fn toc_round_trips_through_json_and_validates() {
        let toc = Toc::new(vec![track(1, 0, 100)], Lba(100)).unwrap();
        let json = serde_json::to_string(&toc).unwrap();
        assert_eq!(serde_json::from_str::<Toc>(&json).unwrap(), toc);
        let bad = json.replace("\"leadOut\":100", "\"leadOut\":7");
        assert!(serde_json::from_str::<Toc>(&bad).is_err());
    }

    #[test]
    fn msf_and_durations() {
        assert_eq!(Lba(0).to_msf().to_string(), "00:02:00");
        assert_eq!(Sectors(75 * 61 + 3).to_msf().to_string(), "01:01:03");
        assert_eq!(Sectors(75).duration_ms(), 1000);
        assert_eq!(Sectors(1).samples(), Samples(588));
        assert_eq!(Sectors(2).bytes(), 4704);
    }

    #[test]
    fn identifiers_validate() {
        assert!(TrackNumber::new(0).is_err());
        assert!(TrackNumber::new(100).is_err());
        assert_eq!(
            Isrc::new("gb-aye-00-00351").unwrap().as_str(),
            "GBAYE0000351"
        );
        assert!(Isrc::new("short").is_err());
        assert!(Mcn::new("0000000000000").unwrap().is_blank());
        assert!(Mbid::new("B10BBBFC-CF9E-42E0-BE17-E2C3E1D2600D").is_ok());
        assert!(Mbid::new("not-a-uuid").is_err());
    }

    #[test]
    fn sha256_hex_round_trip() {
        let digest = Sha256Digest([0xab; 32]);
        let hex = digest.to_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(Sha256Digest::from_hex(&hex).unwrap(), digest);
        assert!(Sha256Digest::from_hex("zz").is_err());
    }

    #[test]
    fn status_summaries_are_plain_english() {
        let accurate = AccurateRipStatus::Accurate {
            version: AccurateRipVersion::V2,
            confidence: 12,
            pressing_offset: SampleOffset(0),
        };
        assert_eq!(accurate.summary(), "Accurately ripped (confidence 12)");
        let other = AccurateRipStatus::Accurate {
            version: AccurateRipVersion::V1,
            confidence: 8,
            pressing_offset: SampleOffset(102),
        };
        assert_eq!(
            other.summary(),
            "Accurately ripped (confidence 8, pressing offset +102)"
        );
        assert_eq!(
            AccurateRipStatus::NotInDatabase.summary(),
            "Not in database"
        );
        assert_eq!(Peak(32768).percent(), 100.0);
    }
}
