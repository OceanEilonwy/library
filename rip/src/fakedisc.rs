// SPDX-License-Identifier: AGPL-3.0-or-later
//! The simulated disc format read by [`crate::drive::fake::FakeDrive`] and
//! written by the test generator.
//!
//! A simulated disc is a directory:
//!
//! ```text
//! disc.json      FakeDiscSpec: drive identity, TOC, MCN, CD-Text, injected errors
//! track01.raw    16-bit little-endian stereo 44.1 kHz PCM for track 1
//! track02.raw    …one file per audio track, named by track number
//! ```
//!
//! Each `trackNN.raw` holds exactly `length_sectors × 2352` bytes, as the
//! audio would be read by a drive with **zero** read offset. Data tracks
//! (`isAudio: false`) have no file; reading them, or the gap before a later
//! session's first track, fails as it would on a real drive. An enhanced CD
//! puts its data track in `"session": 2` (see [`crate::model::Toc`]).
//!
//! `disc.json` example:
//!
//! ```json
//! {
//!   "drive": { "vendor": "LIBRARY", "model": "Simulated Drive", "revision": "1.0" },
//!   "readOffset": 6,
//!   "toc": { "tracks": [ { "number": 1, "startLba": 0, "lengthSectors": 1500,
//!                          "isAudio": true, "pregap": 0, "isrc": null } ],
//!            "leadOut": 1500 },
//!   "mcn": null,
//!   "cdText": null,
//!   "errors": [ { "lba": 700, "kind": "unreadable" } ]
//! }
//! ```
//!
//! `readOffset` simulates the drive's offset: a [`FakeDrive`] returns data
//! shifted by `-readOffset` samples so that applying the AccurateRip
//! correction of `+readOffset` recovers the files exactly.
//!
//! [`FakeDrive`]: crate::drive::fake::FakeDrive

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{CdText, DriveIdentity, Lba, Mcn, SampleOffset, Toc, TrackNumber};

/// File name of the disc description inside a simulated disc directory.
pub const SPEC_FILE: &str = "disc.json";

/// A kind of read problem to simulate at one sector.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectedErrorKind {
    /// Never reads correctly: paranoia gives up and reports the sector.
    Unreadable,
    /// Each read returns the data shifted by a few samples (jitter); full
    /// paranoia's overlap check must correct it.
    Jitter,
    /// The first read of the sector returns corrupted data; later reads
    /// are correct (a verify pass must catch it).
    FlakyOnce,
}

/// A read problem at one sector.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InjectedError {
    /// The sector affected.
    pub lba: Lba,
    /// What goes wrong.
    pub kind: InjectedErrorKind,
}

/// The contents of `disc.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FakeDiscSpec {
    /// The simulated drive.
    pub drive: DriveIdentity,
    /// The simulated drive's read offset (see the module docs).
    #[serde(default)]
    pub read_offset: SampleOffset,
    /// Table of contents.
    pub toc: Toc,
    /// Media catalogue number.
    #[serde(default)]
    pub mcn: Option<Mcn>,
    /// CD-Text.
    #[serde(default)]
    pub cd_text: Option<CdText>,
    /// Injected read errors.
    #[serde(default)]
    pub errors: Vec<InjectedError>,
}

/// A simulated disc directory with its parsed spec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakeDisc {
    /// The directory.
    pub dir: PathBuf,
    /// Its `disc.json`.
    pub spec: FakeDiscSpec,
}

/// Problems loading or saving a simulated disc.
#[derive(Debug, thiserror::Error)]
pub enum FakeDiscError {
    /// File system error.
    #[error("could not use the simulated disc file {path}: {source}")]
    Io {
        /// The file involved.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// `disc.json` is not valid.
    #[error("the simulated disc description {path} is not valid: {source}")]
    Spec {
        /// The file involved.
        path: PathBuf,
        /// The underlying error.
        source: serde_json::Error,
    },
    /// A track file has the wrong size.
    #[error("{path} should hold {expected} bytes of audio but holds {actual}")]
    TrackSize {
        /// The file involved.
        path: PathBuf,
        /// Bytes required by the TOC.
        expected: u64,
        /// Bytes found.
        actual: u64,
    },
}

/// File name of a track's PCM, e.g. `track01.raw`.
pub fn track_file_name(number: TrackNumber) -> String {
    format!("track{:02}.raw", number.get())
}

impl FakeDisc {
    /// Loads `dir/disc.json` and checks that every audio track file exists
    /// with the size the TOC requires.
    pub fn load(dir: &Path) -> Result<Self, FakeDiscError> {
        let spec_path = dir.join(SPEC_FILE);
        let text = std::fs::read_to_string(&spec_path).map_err(|source| FakeDiscError::Io {
            path: spec_path.clone(),
            source,
        })?;
        let spec: FakeDiscSpec =
            serde_json::from_str(&text).map_err(|source| FakeDiscError::Spec {
                path: spec_path,
                source,
            })?;
        let disc = FakeDisc {
            dir: dir.to_owned(),
            spec,
        };
        for track in disc.spec.toc.audio_tracks() {
            let path = disc.track_path(track.number);
            let actual = std::fs::metadata(&path)
                .map_err(|source| FakeDiscError::Io {
                    path: path.clone(),
                    source,
                })?
                .len();
            let expected = track.length_sectors.bytes();
            if actual != expected {
                return Err(FakeDiscError::TrackSize {
                    path,
                    expected,
                    actual,
                });
            }
        }
        Ok(disc)
    }

    /// Writes `disc.json` (track files are written by the caller).
    pub fn save_spec(dir: &Path, spec: &FakeDiscSpec) -> Result<(), FakeDiscError> {
        let path = dir.join(SPEC_FILE);
        let text = serde_json::to_string_pretty(spec).map_err(|source| FakeDiscError::Spec {
            path: path.clone(),
            source,
        })?;
        std::fs::write(&path, text).map_err(|source| FakeDiscError::Io { path, source })
    }

    /// Path of a track's PCM file.
    pub fn track_path(&self, number: TrackNumber) -> PathBuf {
        self.dir.join(track_file_name(number))
    }
}
