// SPDX-License-Identifier: AGPL-3.0-or-later
//! Test support: generates simulated discs of sine tones.
//!
//! Compiled for unit tests and with the `testsupport` feature (on by
//! default; it also powers `library-rip make-fake-disc`).

use std::f64::consts::TAU;
use std::path::Path;

use crate::fakedisc::{FakeDisc, FakeDiscError, FakeDiscSpec, InjectedError, track_file_name};
use crate::model::{
    DriveIdentity, Lba, ModelError, SAMPLE_RATE, SECTORS_PER_SECOND, SampleOffset, Sectors, Toc,
    TrackEntry, TrackNumber,
};

/// What to generate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakeDiscOptions {
    /// Number of audio tracks (1–99).
    pub tracks: u8,
    /// Length of each track in whole seconds (at least 1).
    pub seconds: u32,
    /// Simulated drive read offset.
    pub read_offset: SampleOffset,
    /// Read errors to inject.
    pub errors: Vec<InjectedError>,
}

impl Default for FakeDiscOptions {
    fn default() -> Self {
        FakeDiscOptions {
            tracks: 3,
            seconds: 20,
            read_offset: SampleOffset(6),
            errors: Vec::new(),
        }
    }
}

/// Problems generating a simulated disc.
#[derive(Debug, thiserror::Error)]
pub enum GenerateError {
    /// Invalid options.
    #[error("cannot make that disc: {0}")]
    Options(String),
    /// Invalid TOC (should not happen for valid options).
    #[error(transparent)]
    Model(#[from] ModelError),
    /// Writing files failed.
    #[error(transparent)]
    Disc(#[from] FakeDiscError),
}

/// Frequency of track `number`'s tone: A4 for track 1, rising a semitone
/// per track; left and right differ so channel swaps are caught.
fn tone_hz(number: u8) -> (f64, f64) {
    let left = 440.0 * 2f64.powf(f64::from(number.saturating_sub(1)) / 12.0);
    (left, left * 1.5)
}

/// Generates the PCM for one track (`sectors` long), starting at absolute
/// sample `first_sample` so tones are continuous across track boundaries.
pub fn sine_pcm(number: u8, sectors: Sectors, first_sample: u64) -> Vec<u8> {
    let (left_hz, right_hz) = tone_hz(number);
    let samples = sectors.samples().0;
    let mut out = Vec::with_capacity(usize::try_from(sectors.bytes()).unwrap_or(0));
    for index in 0..samples {
        // Lossless enough for a test signal; precision loss is irrelevant.
        let t = (first_sample + index) as f64 / f64::from(SAMPLE_RATE);
        let left = ((TAU * left_hz * t).sin() * 12_000.0).round() as i16;
        let right = ((TAU * right_hz * t).sin() * 8_000.0).round() as i16;
        out.extend_from_slice(&left.to_le_bytes());
        out.extend_from_slice(&right.to_le_bytes());
    }
    out
}

/// Writes a simulated disc into `dir` (created if missing) and returns it.
pub fn make_fake_disc(dir: &Path, options: &FakeDiscOptions) -> Result<FakeDisc, GenerateError> {
    if options.tracks == 0 || options.tracks > 99 {
        return Err(GenerateError::Options(
            "a disc has between 1 and 99 tracks".into(),
        ));
    }
    if options.seconds == 0 {
        return Err(GenerateError::Options(
            "each track must be at least one second long".into(),
        ));
    }
    let length = options
        .seconds
        .checked_mul(SECTORS_PER_SECOND)
        .map(Sectors)
        .ok_or_else(|| GenerateError::Options("tracks that long do not fit on a disc".into()))?;

    std::fs::create_dir_all(dir).map_err(|source| FakeDiscError::Io {
        path: dir.to_owned(),
        source,
    })?;

    let mut entries = Vec::with_capacity(usize::from(options.tracks));
    let mut start = Lba(0);
    for number in 1..=options.tracks {
        let track = TrackNumber::new(number)?;
        let pcm = sine_pcm(number, length, Sectors(start.0).samples().0);
        let path = dir.join(track_file_name(track));
        std::fs::write(&path, &pcm).map_err(|source| FakeDiscError::Io { path, source })?;
        entries.push(TrackEntry {
            number: track,
            start_lba: start,
            length_sectors: length,
            is_audio: true,
            pregap: Sectors(0),
            isrc: None,
        });
        start = start
            .checked_add(length)
            .ok_or_else(|| GenerateError::Options("the disc is too long".into()))?;
    }
    let spec = FakeDiscSpec {
        drive: DriveIdentity {
            vendor: "LIBRARY".into(),
            model: "Simulated Drive".into(),
            revision: "1.0".into(),
        },
        read_offset: options.read_offset,
        toc: Toc::new(entries, start)?,
        mcn: None,
        cd_text: None,
        errors: options.errors.clone(),
    };
    FakeDisc::save_spec(dir, &spec)?;
    Ok(FakeDisc::load(dir)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BYTES_PER_SECTOR;

    #[test]
    fn generates_a_loadable_disc() {
        let dir = tempfile::tempdir().unwrap();
        let options = FakeDiscOptions {
            tracks: 2,
            seconds: 1,
            ..FakeDiscOptions::default()
        };
        let disc = make_fake_disc(dir.path(), &options).unwrap();
        assert_eq!(disc.spec.toc.tracks().len(), 2);
        assert_eq!(disc.spec.toc.lead_out(), Lba(150));
        let reloaded = FakeDisc::load(dir.path()).unwrap();
        assert_eq!(reloaded, disc);
        let bytes = std::fs::read(disc.track_path(TrackNumber::new(2).unwrap())).unwrap();
        assert_eq!(bytes.len(), 75 * BYTES_PER_SECTOR);
        assert!(bytes.iter().any(|b| *b != 0));
    }

    #[test]
    fn rejects_bad_options() {
        let dir = tempfile::tempdir().unwrap();
        let none = FakeDiscOptions {
            tracks: 0,
            ..FakeDiscOptions::default()
        };
        assert!(make_fake_disc(dir.path(), &none).is_err());
    }

    #[test]
    fn detects_truncated_track_files() {
        let dir = tempfile::tempdir().unwrap();
        let options = FakeDiscOptions {
            tracks: 1,
            seconds: 1,
            ..FakeDiscOptions::default()
        };
        let disc = make_fake_disc(dir.path(), &options).unwrap();
        std::fs::write(disc.track_path(TrackNumber::new(1).unwrap()), [0u8; 10]).unwrap();
        assert!(matches!(
            FakeDisc::load(dir.path()),
            Err(FakeDiscError::TrackSize { .. })
        ));
    }
}
