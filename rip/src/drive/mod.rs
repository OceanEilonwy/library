// SPDX-License-Identifier: AGPL-3.0-or-later
//! Disc drives (lane C).
//!
//! [`DiscDrive`] is one opened drive with a disc; [`DriveProvider`] lists
//! and opens drives. Two implementations:
//!
//! - [`fake::FakeDrive`] reads a simulated disc directory (see
//!   [`crate::fakedisc`]), honouring injected errors and the simulated read
//!   offset. Always available.
//! - `cdio::CdioDrive` uses libcdio + libcdio-paranoia (feature `cdio`).
//!
//! Drives are blocking; the app calls them from `spawn_blocking`.
//! [`reader::read_track`] turns sector reads into a verified, offset-corrected
//! [`TrackReadResult`](crate::model::TrackReadResult).

#[cfg(feature = "cdio")]
pub mod cdio;
pub mod fake;
pub mod reader;

use std::path::PathBuf;

use crate::model::{DiscInfo, Lba, Sectors};

/// Errors from drives.
#[derive(Debug, thiserror::Error)]
pub enum DriveError {
    /// The drive has no disc, or the tray is open.
    #[error("there is no disc in the drive")]
    NoDisc,
    /// The disc has no audio tracks.
    #[error("this disc has no audio tracks")]
    NoAudio,
    /// A mixed-mode disc: a data track before (or among) the audio tracks.
    /// Its audio is not ripped: reading the first audio track's edge would
    /// mean reading the data track as audio, and the databases' rules for
    /// such discs are not settled.
    #[error("this is a mixed-mode CD (its first track is computer data), which cannot be ripped")]
    MixedMode,
    /// The drive could not be opened (missing, busy, no permission).
    #[error("could not open the drive {device}: {reason}")]
    Open {
        /// Device path or id.
        device: String,
        /// Plain-English reason.
        reason: String,
    },
    /// A read failed outright (not a paranoia-reported bad sector).
    #[error("the drive could not read sector {lba}: {reason}")]
    Read {
        /// Where.
        lba: Lba,
        /// Plain-English reason.
        reason: String,
    },
    /// The read was cancelled by the user.
    #[error("reading was cancelled")]
    Cancelled,
    /// A simulated disc is malformed.
    #[error(transparent)]
    FakeDisc(#[from] crate::fakedisc::FakeDiscError),
    /// File system error (temporary PCM files).
    #[error("could not write {path}: {source}")]
    Io {
        /// The file involved.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
}

/// Result type of the drive module.
pub type Result<T, E = DriveError> = std::result::Result<T, E>;

/// A drive the user can pick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveDescriptor {
    /// How to open it again: a device path (`/dev/sr0`, `D:`) or a
    /// simulated disc directory.
    pub device: String,
    /// Friendly label, e.g. "PLEXTOR DVDR PX-760A (/dev/sr0)".
    pub label: String,
}

/// How hard to try when reading sectors.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum ParanoiaMode {
    /// Full paranoia: overlap + verify, scratch repair, never skip, with
    /// drive cache defeat.
    #[default]
    Full,
    /// No checking (used only for quick previews; never for a rip).
    Off,
}

/// Raw sectors returned by [`DiscDrive::read_sectors`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SectorRead {
    /// `count × 2352` bytes of 16-bit LE stereo PCM, **not** offset-corrected.
    pub data: Vec<u8>,
    /// Sectors paranoia could not verify (data for them is its best guess),
    /// in ascending order.
    pub unrecovered: Vec<Lba>,
    /// Sectors that needed more than the normal verification (re-reads,
    /// jitter or scratch fixes) but were recovered in the end.
    pub retried_sectors: u32,
}

/// One opened drive.
pub trait DiscDrive: Send {
    /// Reads the drive identity, TOC, MCN, ISRCs and CD-Text.
    fn disc_info(&mut self) -> Result<DiscInfo>;

    /// Reads `count` sectors from `start` with the given paranoia mode.
    fn read_sectors(
        &mut self,
        start: Lba,
        count: Sectors,
        mode: ParanoiaMode,
    ) -> Result<SectorRead>;

    /// Ejects the disc.
    fn eject(&mut self) -> Result<()>;
}

/// Lists and opens drives.
pub trait DriveProvider: Send + Sync {
    /// Drives currently attached.
    fn list(&self) -> Result<Vec<DriveDescriptor>>;

    /// Opens a drive from [`DriveDescriptor::device`].
    fn open(&self, device: &str) -> Result<Box<dyn DiscDrive>>;
}
