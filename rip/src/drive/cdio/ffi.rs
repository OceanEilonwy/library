// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hand-written bindings to libcdio, libcdio_cdda and libcdio_paranoia
//! (linked by build.rs through pkg-config), and the safe wrappers around
//! them. **Every `unsafe` block of the drive code is in this file.**
//!
//! Invariants the wrappers keep:
//!
//! - A [`Device`] owns its `CdIo_t`, and the `cdrom_drive_t` and
//!   `cdrom_paranoia_t` made from it, exclusively. They are freed in reverse
//!   order of creation (paranoia, drive, CdIo) exactly once, in `Drop` or
//!   in [`Device::eject`].
//! - Pointers are checked for NULL before use; NULL results become `None`
//!   or an error string.
//! - Strings libcdio allocates (`cdio_get_mcn`, `cdio_get_track_isrc`, the
//!   device list) are copied into Rust and freed with the matching libcdio
//!   function. Strings libcdio owns (`cdtext_get_const`, hardware info in a
//!   caller buffer) are only copied.
//! - Paranoia's sector buffer is valid only until the next read on the same
//!   object; it is copied immediately.
//! - The paranoia callback has no user-data pointer, so it records the
//!   sectors it is told about (its position, in words, over
//!   `CD_FRAMEWORDS`) into a thread-local set. It only uses
//!   `RefCell::try_borrow_mut` and set inserts: it cannot panic or unwind
//!   into C.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ffi::{CStr, CString, c_char, c_int, c_long, c_void};
use std::ptr::NonNull;

/// Opaque `CdIo_t`.
#[repr(C)]
struct CdIo {
    _private: [u8; 0],
}

/// Opaque `cdrom_drive_t`.
#[repr(C)]
struct CdromDrive {
    _private: [u8; 0],
}

/// Opaque `cdrom_paranoia_t`.
#[repr(C)]
struct CdromParanoia {
    _private: [u8; 0],
}

/// Opaque `cdtext_t`.
#[repr(C)]
struct CdText {
    _private: [u8; 0],
}

/// `mmc_cdb_t`: a SCSI command descriptor block.
#[repr(C)]
struct MmcCdb {
    field: [u8; MAX_CDB_LEN],
}

/// `cdio_hwinfo_t` (lengths 8, 16 and 4 plus a terminating NUL).
#[repr(C)]
struct HwInfo {
    vendor: [c_char; 9],
    model: [c_char; 17],
    revision: [c_char; 5],
}

/// `driver_id_t::DRIVER_DEVICE`: any hardware driver.
const DRIVER_DEVICE: c_int = 11;
/// `driver_return_code_t::DRIVER_OP_SUCCESS`.
const DRIVER_OP_SUCCESS: c_int = 0;
/// `track_format_t::TRACK_FORMAT_AUDIO`.
const TRACK_FORMAT_AUDIO: c_int = 0;
/// `CDIO_CDROM_LEADOUT_TRACK`.
const LEADOUT_TRACK: u8 = 0xAA;
/// `CDIO_INVALID_TRACK`.
const INVALID_TRACK: u8 = 0xFF;
/// `CDIO_INVALID_LSN`.
const INVALID_LSN: i32 = -45_301;
/// `MAX_CDB_LEN`: bytes in an `mmc_cdb_t`.
const MAX_CDB_LEN: usize = 12;
/// `cdio_mmc_direction_t::SCSI_MMC_DATA_READ`.
const SCSI_MMC_DATA_READ: c_int = 0;
/// `CDIO_MMC_GPCMD_READ_TOC`.
const MMC_READ_TOC: u8 = 0x43;
/// `CDIO_MMC_READTOC_FMT_FULTOC`: the full TOC, with sessions.
const MMC_READTOC_FULL_TOC: u8 = 2;
/// How long the drive may take to answer READ TOC.
const MMC_TIMEOUT_MS: u32 = 10_000;
/// Bytes asked for: the 4-byte header and 11 bytes per descriptor, enough
/// for 99 tracks and the A0–A2 points of several sessions.
const FULL_TOC_BYTES: u16 = 4 + 11 * 140;
/// `CDDA_MESSAGE_FORGETIT`.
const CDDA_MESSAGE_FORGETIT: c_int = 0;
/// `PARANOIA_MODE_FULL`: verify, overlap, scratch repair, never skip.
const PARANOIA_MODE_FULL: c_int = 0xff;
/// `PARANOIA_MODE_DISABLE`.
const PARANOIA_MODE_DISABLE: c_int = 0;
/// `SEEK_SET`.
const SEEK_SET: c_int = 0;
/// 16-bit words per raw sector (`CD_FRAMEWORDS`).
const FRAME_WORDS: usize = 1176;
/// [`FRAME_WORDS`] for signed position maths.
const FRAME_WORDS_I64: i64 = 1176;
/// Paranoia's default cache model size; set explicitly so cache defeat is
/// on regardless of library defaults.
const CACHE_MODEL_SECTORS: c_int = 1200;

// `cdtext_field_t`.
const CDTEXT_FIELD_TITLE: c_int = 0;
const CDTEXT_FIELD_PERFORMER: c_int = 1;
const CDTEXT_FIELD_SONGWRITER: c_int = 2;
const CDTEXT_FIELD_COMPOSER: c_int = 3;
const CDTEXT_FIELD_MESSAGE: c_int = 4;
const CDTEXT_FIELD_ARRANGER: c_int = 5;

// `paranoia_cb_mode_t`.
const CB_FIXUP_EDGE: c_int = 2;
const CB_FIXUP_ATOM: c_int = 3;
const CB_SCRATCH: c_int = 4;
const CB_REPAIR: c_int = 5;
const CB_SKIP: c_int = 6;
const CB_DRIFT: c_int = 7;
const CB_FIXUP_DROPPED: c_int = 10;
const CB_FIXUP_DUPED: c_int = 11;
const CB_READERR: c_int = 12;
const CB_CACHEERR: c_int = 13;

type ParanoiaCallback = extern "C" fn(c_long, c_int);

unsafe extern "C" {
    fn cdio_get_devices(driver: c_int) -> *mut *mut c_char;
    fn cdio_free_device_list(list: *mut *mut c_char);
    fn cdio_open(source: *const c_char, driver: c_int) -> *mut CdIo;
    fn cdio_destroy(cdio: *mut CdIo);
    fn cdio_eject_media(cdio: *mut *mut CdIo) -> c_int;
    fn cdio_get_hwinfo(cdio: *const CdIo, info: *mut HwInfo) -> bool;
    fn cdio_get_first_track_num(cdio: *const CdIo) -> u8;
    fn cdio_get_num_tracks(cdio: *const CdIo) -> u8;
    fn cdio_get_track_lsn(cdio: *const CdIo, track: u8) -> i32;
    fn cdio_get_track_pregap_lsn(cdio: *const CdIo, track: u8) -> i32;
    fn cdio_get_track_format(cdio: *const CdIo, track: u8) -> c_int;
    fn cdio_get_track_isrc(cdio: *const CdIo, track: u8) -> *mut c_char;
    fn cdio_get_mcn(cdio: *const CdIo) -> *mut c_char;
    fn cdio_get_cdtext(cdio: *mut CdIo) -> *mut CdText;
    fn cdtext_get_const(cdtext: *const CdText, field: c_int, track: u8) -> *const c_char;
    fn cdio_free(memory: *mut c_void);
    fn cdio_get_last_session(cdio: *mut CdIo, last_session: *mut i32) -> c_int;
    fn mmc_run_cmd(
        cdio: *const CdIo,
        timeout_ms: u32,
        cdb: *const MmcCdb,
        direction: c_int,
        buffer_length: u32,
        buffer: *mut c_void,
    ) -> c_int;

    fn cdio_cddap_identify_cdio(
        cdio: *mut CdIo,
        message_destination: c_int,
        messages: *mut *mut c_char,
    ) -> *mut CdromDrive;
    fn cdio_cddap_open(drive: *mut CdromDrive) -> c_int;
    fn cdio_cddap_close_no_free_cdio(drive: *mut CdromDrive) -> bool;

    fn cdio_paranoia_init(drive: *mut CdromDrive) -> *mut CdromParanoia;
    fn cdio_paranoia_free(paranoia: *mut CdromParanoia);
    fn cdio_paranoia_modeset(paranoia: *mut CdromParanoia, mode: c_int);
    fn cdio_paranoia_cachemodel_size(paranoia: *mut CdromParanoia, sectors: c_int) -> c_int;
    fn cdio_paranoia_seek(paranoia: *mut CdromParanoia, seek: i32, whence: c_int) -> i32;
    fn cdio_paranoia_read_limited(
        paranoia: *mut CdromParanoia,
        callback: ParanoiaCallback,
        max_retries: c_int,
    ) -> *mut i16;
}

/// Copies a NUL-terminated C string; `None` for NULL or empty.
///
/// Safety: `pointer` is NULL or points to a NUL-terminated string that
/// stays valid for the duration of the call.
unsafe fn copy_c_string(pointer: *const c_char) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: non-NULL and NUL-terminated per the function contract.
    let text = unsafe { CStr::from_ptr(pointer) }.to_string_lossy();
    let trimmed = text.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Copies and frees a string allocated by libcdio.
///
/// Safety: `pointer` is NULL or a string libcdio allocated for the caller.
unsafe fn take_cdio_string(pointer: *mut c_char) -> Option<String> {
    // SAFETY: per the contract the string is valid until freed below.
    let text = unsafe { copy_c_string(pointer) };
    if !pointer.is_null() {
        // SAFETY: allocated by libcdio for us; freed exactly once.
        unsafe { cdio_free(pointer.cast()) };
    }
    text
}

/// Copies a fixed-size `char` field (possibly not NUL-terminated).
fn copy_fixed(field: &[c_char]) -> String {
    let bytes: Vec<u8> = field
        .iter()
        .map(|c| c.to_ne_bytes()[0])
        .take_while(|b| *b != 0)
        .collect();
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

/// Device paths of the optical drives libcdio finds (`/dev/sr0`, `D:`…).
pub(super) fn list_devices() -> Vec<String> {
    // SAFETY: no preconditions; returns NULL or a NULL-terminated array.
    let list = unsafe { cdio_get_devices(DRIVER_DEVICE) };
    if list.is_null() {
        return Vec::new();
    }
    let mut devices = Vec::new();
    let mut index = 0usize;
    loop {
        // SAFETY: the array is NULL-terminated and we stop at the NULL.
        let entry = unsafe { *list.add(index) };
        if entry.is_null() {
            break;
        }
        // SAFETY: each entry is a NUL-terminated string owned by the list.
        if let Some(device) = unsafe { copy_c_string(entry) } {
            devices.push(device);
        }
        index = index.saturating_add(1);
    }
    // SAFETY: the list came from cdio_get_devices and is freed once.
    unsafe { cdio_free_device_list(list) };
    devices
}

/// What libcdio says about one track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RawTrack {
    /// Track number.
    pub number: u8,
    /// Start (INDEX 01) as an LSN.
    pub lsn: i32,
    /// Start of the pre-gap (INDEX 00), when the driver knows it.
    pub pregap_lsn: Option<i32>,
    /// Audio (as opposed to data).
    pub is_audio: bool,
    /// ISRC as read from the subchannel, unchecked.
    pub isrc: Option<String>,
}

/// One CD-Text block's strings, unchecked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct RawCdTextFields {
    pub title: Option<String>,
    pub performer: Option<String>,
    pub songwriter: Option<String>,
    pub composer: Option<String>,
    pub arranger: Option<String>,
    pub message: Option<String>,
}

/// The result of reading sectors through paranoia.
#[derive(Debug, Default)]
pub(super) struct RawRead {
    /// `count × 2352` bytes, 16-bit little-endian stereo.
    pub data: Vec<u8>,
    /// Offsets (from the first sector read) paranoia skipped.
    pub skipped: Vec<u32>,
    /// Sectors that needed corrections but were not skipped.
    pub corrected: u32,
}

/// Paranoia events seen during one [`Device::read_audio`], by the sector
/// paranoia named (not the sector being returned when it fired: paranoia
/// reads around the requested sector for its overlap checks).
#[derive(Debug, Default, PartialEq, Eq)]
struct ParanoiaEvents {
    /// Sectors paranoia gave up verifying.
    skipped: BTreeSet<i64>,
    /// Sectors paranoia had to correct.
    corrected: BTreeSet<i64>,
}

impl ParanoiaEvents {
    /// Records callback `mode` at callback `position`, which paranoia gives
    /// in 16-bit words from the start of the disc (`CD_FRAMEWORDS` per
    /// sector).
    fn record(&mut self, position: i64, mode: c_int) {
        let sector = position.div_euclid(FRAME_WORDS_I64);
        match mode {
            CB_SKIP => {
                self.skipped.insert(sector);
            }
            CB_FIXUP_EDGE | CB_FIXUP_ATOM | CB_SCRATCH | CB_REPAIR | CB_DRIFT
            | CB_FIXUP_DROPPED | CB_FIXUP_DUPED | CB_READERR | CB_CACHEERR => {
                self.corrected.insert(sector);
            }
            // Routine progress (read, verify, overlap): not a problem.
            _ => {}
        }
    }

    /// The events that fall on the `count` sectors from `start`: indexes
    /// (from `start`) of skipped sectors, and how many other sectors were
    /// corrected. Events outside the range are about sectors paranoia only
    /// looked at for overlap; they are verified again when those sectors
    /// are read (every read seeks, which resets paranoia's cache).
    fn attribute(&self, start: i64, count: u32) -> (Vec<u32>, u32) {
        let end = start.saturating_add(i64::from(count));
        let index = |sector: &i64| u32::try_from(sector - start).ok();
        let skipped: Vec<u32> = self.skipped.range(start..end).filter_map(index).collect();
        let corrected = self
            .corrected
            .range(start..end)
            .filter(|sector| !self.skipped.contains(sector))
            .count();
        (skipped, u32::try_from(corrected).unwrap_or(u32::MAX))
    }
}

thread_local! {
    /// Paranoia events seen during the current [`Device::read_audio`].
    static EVENTS: RefCell<ParanoiaEvents> = RefCell::new(ParanoiaEvents::default());
}

/// Paranoia's progress callback: records skips and corrections by the
/// position paranoia reports.
extern "C" fn paranoia_callback(position: c_long, mode: c_int) {
    EVENTS.with(|events| {
        // Never re-entered; if it somehow were, drop the event rather than
        // unwind across the FFI boundary.
        if let Ok(mut events) = events.try_borrow_mut() {
            // `c_long` is 32 bits on Windows and 64 elsewhere.
            #[allow(clippy::useless_conversion)]
            let position = i64::from(position);
            events.record(position, mode);
        }
    });
}

/// An open optical drive.
#[derive(Debug)]
pub(super) struct Device {
    /// `None` after a successful eject (libcdio frees the handle).
    cdio: Option<NonNull<CdIo>>,
    drive: Option<NonNull<CdromDrive>>,
    paranoia: Option<NonNull<CdromParanoia>>,
}

// SAFETY: the handles are owned exclusively by this value and libcdio has
// no thread affinity; `&mut self` on every operation that touches them
// means they are never used from two threads at once.
unsafe impl Send for Device {}

impl Device {
    /// Opens a device path.
    pub fn open(device: &str) -> Result<Self, String> {
        let path = CString::new(device).map_err(|_| "the device name contains a NUL".to_owned())?;
        // SAFETY: `path` is a valid C string for the duration of the call.
        let cdio = unsafe { cdio_open(path.as_ptr(), DRIVER_DEVICE) };
        let cdio = NonNull::new(cdio).ok_or_else(|| {
            "it does not exist, is not an optical drive, or this user may not use it".to_owned()
        })?;
        Ok(Device {
            cdio: Some(cdio),
            drive: None,
            paranoia: None,
        })
    }

    fn handle(&self) -> Result<NonNull<CdIo>, String> {
        self.cdio
            .ok_or_else(|| "the disc has been ejected".to_owned())
    }

    /// Vendor, model and firmware revision from a SCSI INQUIRY.
    pub fn hardware(&self) -> Option<(String, String, String)> {
        let cdio = self.handle().ok()?;
        let mut info = HwInfo {
            vendor: [0; 9],
            model: [0; 17],
            revision: [0; 5],
        };
        // SAFETY: `cdio` is a live handle; `info` is a correctly sized
        // out-parameter.
        let ok = unsafe { cdio_get_hwinfo(cdio.as_ptr(), &mut info) };
        ok.then(|| {
            (
                copy_fixed(&info.vendor),
                copy_fixed(&info.model),
                copy_fixed(&info.revision),
            )
        })
    }

    /// The tracks and the lead-out LSN, or `None` when there is no disc.
    pub fn tracks(&self) -> Result<Option<(Vec<RawTrack>, i32)>, String> {
        let cdio = self.handle()?.as_ptr();
        // SAFETY: live handle for all calls below.
        let first = unsafe { cdio_get_first_track_num(cdio) };
        // SAFETY: as above.
        let count = unsafe { cdio_get_num_tracks(cdio) };
        if first == INVALID_TRACK || count == INVALID_TRACK || count == 0 {
            return Ok(None);
        }
        let mut tracks = Vec::with_capacity(usize::from(count));
        for number in (0..count).filter_map(|index| first.checked_add(index)) {
            // SAFETY: live handle; libcdio range-checks the track number.
            let lsn = unsafe { cdio_get_track_lsn(cdio, number) };
            if lsn == INVALID_LSN {
                return Err(format!("the drive gave no start for track {number}"));
            }
            // SAFETY: as above.
            let pregap = unsafe { cdio_get_track_pregap_lsn(cdio, number) };
            // SAFETY: as above.
            let format = unsafe { cdio_get_track_format(cdio, number) };
            // SAFETY: as above; the returned string is ours to free.
            let isrc = unsafe { take_cdio_string(cdio_get_track_isrc(cdio, number)) };
            tracks.push(RawTrack {
                number,
                lsn,
                pregap_lsn: (pregap != INVALID_LSN).then_some(pregap),
                is_audio: format == TRACK_FORMAT_AUDIO,
                isrc,
            });
        }
        // SAFETY: live handle.
        let lead_out = unsafe { cdio_get_track_lsn(cdio, LEADOUT_TRACK) };
        if lead_out == INVALID_LSN {
            return Err("the drive gave no lead-out position".into());
        }
        Ok(Some((tracks, lead_out)))
    }

    /// The start (LSN) of the first track of the last session, as libcdio
    /// reports it (`cdio_get_last_session`); `None` when the drive cannot
    /// tell.
    pub fn last_session_start(&self) -> Option<i32> {
        let cdio = self.handle().ok()?;
        let mut lsn: i32 = INVALID_LSN;
        // SAFETY: live handle; `lsn` is a valid out-parameter.
        let status = unsafe { cdio_get_last_session(cdio.as_ptr(), &mut lsn) };
        (status == DRIVER_OP_SUCCESS && lsn != INVALID_LSN).then_some(lsn)
    }

    /// The raw answer to MMC READ TOC/PMA/ATIP in full-TOC format (format
    /// 2, from session 1), or `None` when the drive refuses it. Parsed by
    /// the caller.
    pub fn full_toc(&self) -> Option<Vec<u8>> {
        let cdio = self.handle().ok()?;
        let [length_high, length_low] = FULL_TOC_BYTES.to_be_bytes();
        let mut cdb = MmcCdb {
            field: [0; MAX_CDB_LEN],
        };
        // READ TOC: MSF addresses, format 2, starting with session 1.
        cdb.field = [
            MMC_READ_TOC,
            0x02,
            MMC_READTOC_FULL_TOC,
            0,
            0,
            0,
            1,
            length_high,
            length_low,
            0,
            0,
            0,
        ];
        let mut buffer = vec![0u8; usize::from(FULL_TOC_BYTES)];
        // SAFETY: live handle; `cdb` is a valid CDB; `buffer` is writable
        // for exactly the length passed.
        let status = unsafe {
            mmc_run_cmd(
                cdio.as_ptr(),
                MMC_TIMEOUT_MS,
                &cdb,
                SCSI_MMC_DATA_READ,
                u32::from(FULL_TOC_BYTES),
                buffer.as_mut_ptr().cast::<c_void>(),
            )
        };
        (status == DRIVER_OP_SUCCESS).then_some(buffer)
    }

    /// The media catalogue number, unchecked.
    pub fn mcn(&self) -> Option<String> {
        let cdio = self.handle().ok()?;
        // SAFETY: live handle; the returned string is ours to free.
        unsafe { take_cdio_string(cdio_get_mcn(cdio.as_ptr())) }
    }

    /// CD-Text for the album (track 0) and for each given track number.
    pub fn cd_text(
        &mut self,
        tracks: &[u8],
    ) -> Option<(RawCdTextFields, Vec<(u8, RawCdTextFields)>)> {
        let cdio = self.handle().ok()?;
        // SAFETY: live handle; the CD-Text object is owned by the handle.
        let text = unsafe { cdio_get_cdtext(cdio.as_ptr()) };
        if text.is_null() {
            return None;
        }
        let fields = |track: u8| {
            let get = |field: c_int| {
                // SAFETY: `text` is non-NULL and owned by the live handle;
                // the returned string is owned by it too and copied at once.
                unsafe { copy_c_string(cdtext_get_const(text, field, track)) }
            };
            RawCdTextFields {
                title: get(CDTEXT_FIELD_TITLE),
                performer: get(CDTEXT_FIELD_PERFORMER),
                songwriter: get(CDTEXT_FIELD_SONGWRITER),
                composer: get(CDTEXT_FIELD_COMPOSER),
                arranger: get(CDTEXT_FIELD_ARRANGER),
                message: get(CDTEXT_FIELD_MESSAGE),
            }
        };
        let album = fields(0);
        let per_track = tracks.iter().map(|n| (*n, fields(*n))).collect();
        Some((album, per_track))
    }

    /// Sets up paranoia on first use.
    fn paranoia(&mut self) -> Result<NonNull<CdromParanoia>, String> {
        if let Some(paranoia) = self.paranoia {
            return Ok(paranoia);
        }
        let cdio = self.handle()?;
        let drive = match self.drive {
            Some(drive) => drive,
            None => {
                // SAFETY: live handle; messages are discarded so the
                // out-parameter may be NULL.
                let drive = unsafe {
                    cdio_cddap_identify_cdio(
                        cdio.as_ptr(),
                        CDDA_MESSAGE_FORGETIT,
                        std::ptr::null_mut(),
                    )
                };
                let drive = NonNull::new(drive)
                    .ok_or_else(|| "the drive does not hold an audio CD".to_owned())?;
                self.drive = Some(drive);
                // SAFETY: `drive` was just created from the live handle.
                if unsafe { cdio_cddap_open(drive.as_ptr()) } != 0 {
                    return Err("the drive could not read the disc's contents".into());
                }
                drive
            }
        };
        // SAFETY: `drive` is open.
        let paranoia = unsafe { cdio_paranoia_init(drive.as_ptr()) };
        let paranoia =
            NonNull::new(paranoia).ok_or_else(|| "could not start careful reading".to_owned())?;
        // SAFETY: `paranoia` was just created.
        unsafe { cdio_paranoia_cachemodel_size(paranoia.as_ptr(), CACHE_MODEL_SECTORS) };
        self.paranoia = Some(paranoia);
        Ok(paranoia)
    }

    /// Reads `count` sectors from `start` through paranoia (full mode with
    /// cache defeat, or no checking), retrying each bad sector up to
    /// `max_retries` times.
    pub fn read_audio(
        &mut self,
        start: i32,
        count: u32,
        full: bool,
        max_retries: c_int,
    ) -> Result<RawRead, String> {
        let paranoia = self.paranoia()?.as_ptr();
        let mode = if full {
            PARANOIA_MODE_FULL
        } else {
            PARANOIA_MODE_DISABLE
        };
        // SAFETY: live paranoia object.
        unsafe { cdio_paranoia_modeset(paranoia, mode) };
        // SAFETY: as above; the position is a sector number.
        unsafe { cdio_paranoia_seek(paranoia, start, SEEK_SET) };
        let bytes = usize::try_from(count)
            .ok()
            .and_then(|n| n.checked_mul(FRAME_WORDS * 2))
            .ok_or_else(|| "too many sectors in one read".to_owned())?;
        let mut read = RawRead {
            data: Vec::with_capacity(bytes),
            ..RawRead::default()
        };
        EVENTS.with(|events| {
            if let Ok(mut events) = events.try_borrow_mut() {
                *events = ParanoiaEvents::default();
            }
        });
        for index in 0..count {
            // SAFETY: live paranoia object; the callback cannot unwind.
            let samples =
                unsafe { cdio_paranoia_read_limited(paranoia, paranoia_callback, max_retries) };
            if samples.is_null() {
                return Err(format!(
                    "the drive stopped responding at sector {}",
                    i64::from(start) + i64::from(index)
                ));
            }
            // SAFETY: paranoia returns a buffer of CD_FRAMEWORDS 16-bit
            // samples, valid until the next read; copied immediately.
            let words = unsafe { std::slice::from_raw_parts(samples, FRAME_WORDS) };
            for word in words {
                // Paranoia returns host-endian samples; files are LE.
                read.data.extend_from_slice(&word.to_le_bytes());
            }
        }
        let (skipped, corrected) = EVENTS.with(|events| {
            events
                .try_borrow()
                .map(|events| events.attribute(i64::from(start), count))
                .unwrap_or_default()
        });
        read.skipped = skipped;
        read.corrected = corrected;
        Ok(read)
    }

    /// Frees paranoia and the cdda drive, keeping the `CdIo_t`.
    fn close_reader(&mut self) {
        if let Some(paranoia) = self.paranoia.take() {
            // SAFETY: created by cdio_paranoia_init, freed once.
            unsafe { cdio_paranoia_free(paranoia.as_ptr()) };
        }
        if let Some(drive) = self.drive.take() {
            // SAFETY: created by cdio_cddap_identify_cdio, closed once; the
            // CdIo_t it borrowed is freed separately.
            unsafe { cdio_cddap_close_no_free_cdio(drive.as_ptr()) };
        }
    }

    /// Ejects the disc. On success libcdio frees the handle, so the device
    /// is unusable afterwards.
    pub fn eject(&mut self) -> Result<(), String> {
        self.close_reader();
        let cdio = self.handle()?;
        let mut pointer = cdio.as_ptr();
        // SAFETY: live handle passed by address; on success libcdio frees
        // it and sets `pointer` to NULL.
        let status = unsafe { cdio_eject_media(&mut pointer) };
        self.cdio = NonNull::new(pointer);
        if status == DRIVER_OP_SUCCESS {
            Ok(())
        } else {
            Err("the drive refused to open (is the disc in use?)".into())
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.close_reader();
        if let Some(cdio) = self.cdio.take() {
            // SAFETY: created by cdio_open, freed exactly once.
            unsafe { cdio_destroy(cdio.as_ptr()) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_fields_stop_at_nul_and_trim() {
        let mut field = [0 as c_char; 9];
        for (slot, byte) in field.iter_mut().zip(b"PLEXTOR ") {
            *slot = *byte as c_char;
        }
        assert_eq!(copy_fixed(&field), "PLEXTOR");
        let full = [b'A' as c_char; 5];
        assert_eq!(copy_fixed(&full), "AAAAA");
    }

    #[test]
    fn opening_a_missing_device_is_an_error() {
        assert!(Device::open("/nonexistent/library-rip-test-drive").is_err());
        assert!(Device::open("bad\0name").is_err());
    }

    #[test]
    fn listing_devices_does_not_crash_without_drives() {
        // This machine may have no drive; the call must still be safe.
        let _ = list_devices();
    }

    const WORDS: c_long = 1176;

    #[test]
    fn callback_records_events_by_the_sector_paranoia_names() {
        EVENTS.with(|events| *events.borrow_mut() = ParanoiaEvents::default());
        paranoia_callback(100 * WORDS + 5, CB_SKIP);
        paranoia_callback(101 * WORDS, CB_FIXUP_EDGE);
        paranoia_callback(101 * WORDS + WORDS - 1, CB_READERR); // same sector
        paranoia_callback(102 * WORDS, 1); // VERIFY: routine, not recorded
        EVENTS.with(|events| {
            let events = events.borrow();
            assert_eq!(events.skipped, BTreeSet::from([100]));
            assert_eq!(events.corrected, BTreeSet::from([101]));
        });
    }

    #[test]
    fn events_are_attributed_by_position_not_by_the_sector_being_read() {
        let mut events = ParanoiaEvents::default();
        // While returning sector 10, paranoia looked ahead and skipped 12
        // and corrected 11; it also corrected 9, outside this read.
        events.record(12 * 1176, CB_SKIP);
        events.record(11 * 1176 + 40, CB_DRIFT);
        events.record(9 * 1176, CB_SCRATCH);
        // A sector both corrected and skipped counts once, as skipped.
        events.record(13 * 1176, CB_REPAIR);
        events.record(13 * 1176 + 1, CB_SKIP);
        assert_eq!(events.attribute(10, 4), (vec![2, 3], 1));
        // Nothing falls on a read of other sectors.
        assert_eq!(events.attribute(20, 5), (vec![], 0));
        // Positions before the disc's start stay out of range.
        let mut early = ParanoiaEvents::default();
        early.record(-1, CB_SKIP);
        assert_eq!(early.skipped, BTreeSet::from([-1]));
        assert_eq!(early.attribute(0, 10), (vec![], 0));
    }
}
