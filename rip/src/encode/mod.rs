// SPDX-License-Identifier: AGPL-3.0-or-later
//! FLAC encoding, tagging and cover embedding through libFLAC (lane C).
//!
//! Every track becomes one FLAC: compression level 8, verify mode on (libFLAC
//! decodes as it encodes and fails on any difference), Vorbis comments from
//! [`tags::vorbis_comments`], and the front cover as a PICTURE block.

mod ffi;
pub mod tags;

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::model::{BYTES_PER_SAMPLE, CoverArt, Pcm, SAMPLE_RATE, Sha256Digest};

/// Errors from encoding.
#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    /// libFLAC reported a problem (including a verify mismatch).
    #[error("the FLAC encoder failed: {0}")]
    Flac(String),
    /// The PCM is not a whole number of stereo samples.
    #[error("the audio data has an odd length ({0} bytes)")]
    PcmLength(u64),
    /// File system error.
    #[error("could not use {path}: {source}")]
    Io {
        /// The file involved.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
}

/// Result type of the encode module.
pub type Result<T, E = EncodeError> = std::result::Result<T, E>;

/// One Vorbis comment (`NAME=value`); names are upper-case ASCII.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VorbisComment {
    /// Field name, e.g. `TITLE`, `MUSICBRAINZ_TRACKID`.
    pub name: String,
    /// Value (UTF-8).
    pub value: String,
}

/// What to put in a FLAC besides the audio.
#[derive(Clone, Debug, Default)]
pub struct FlacMetadata {
    /// Vorbis comments in order.
    pub comments: Vec<VorbisComment>,
    /// Front cover to embed.
    pub cover: Option<CoverArt>,
}

/// A written FLAC file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedFile {
    /// Where it was written.
    pub path: PathBuf,
    /// Size in bytes.
    pub size_bytes: u64,
    /// SHA-256 of the whole file (sent with the upload).
    pub sha256: Sha256Digest,
}

/// FLAC compression level (the slowest, smallest standard preset).
pub const COMPRESSION_LEVEL: u32 = 8;

/// A seek point every this many samples (ten seconds), so players can jump
/// around a streamed file quickly.
pub const SEEK_POINT_SPACING: u32 = 10 * SAMPLE_RATE;

/// Padding left after the tags so they can be edited later without
/// rewriting the file.
pub const PADDING_BYTES: u32 = 4096;

/// FLAC picture type for the front cover.
pub const PICTURE_FRONT_COVER: u32 = 3;

/// Samples handed to libFLAC at a time (one second).
const SAMPLES_PER_CHUNK: usize = SAMPLE_RATE as usize;

/// Encodes 16-bit LE stereo 44.1 kHz PCM to `output` with the metadata:
/// compression level 8, verify mode on (libFLAC decodes every frame as it
/// goes and fails on any difference), a VORBIS_COMMENT block, the front
/// cover as a PICTURE block, a seek table and some padding.
///
/// On failure the partial output file is removed.
pub fn encode_flac(pcm: &Pcm, output: &Path, metadata: &FlacMetadata) -> Result<EncodedFile> {
    let result = write_flac(pcm, output, metadata);
    if result.is_err() {
        // Best effort: a half-written FLAC must not be mistaken for a rip.
        let _ = std::fs::remove_file(output);
    }
    result?;
    let size_bytes = std::fs::metadata(output)
        .map_err(|source| EncodeError::Io {
            path: output.to_owned(),
            source,
        })?
        .len();
    Ok(EncodedFile {
        path: output.to_owned(),
        size_bytes,
        sha256: sha256_file(output)?,
    })
}

fn write_flac(pcm: &Pcm, output: &Path, metadata: &FlacMetadata) -> Result<()> {
    let flac_error = |error: ffi::FlacError| match error {
        ffi::FlacError::Library(message) => EncodeError::Flac(message),
        ffi::FlacError::Io(source) => EncodeError::Io {
            path: output.to_owned(),
            source,
        },
    };
    let mut source = PcmSource::open(pcm)?;
    let total_bytes = source.length();
    if total_bytes % BYTES_PER_SAMPLE as u64 != 0 {
        return Err(EncodeError::PcmLength(total_bytes));
    }
    let total_samples = total_bytes / BYTES_PER_SAMPLE as u64;

    let mut blocks = Vec::with_capacity(4);
    blocks.push(
        ffi::Block::vorbis_comment(
            metadata
                .comments
                .iter()
                .map(|comment| (comment.name.as_str(), comment.value.as_str())),
        )
        .map_err(flac_error)?,
    );
    if let Some(cover) = &metadata.cover {
        blocks.push(
            ffi::Block::picture(
                PICTURE_FRONT_COVER,
                &cover.mime,
                "Front cover",
                image_dimensions(&cover.bytes),
                &cover.bytes,
            )
            .map_err(flac_error)?,
        );
    }
    if total_samples > 0 {
        blocks.push(ffi::Block::seek_table(SEEK_POINT_SPACING, total_samples).map_err(flac_error)?);
    }
    blocks.push(ffi::Block::padding(PADDING_BYTES).map_err(flac_error)?);

    let file = File::create(output).map_err(|source| EncodeError::Io {
        path: output.to_owned(),
        source,
    })?;
    let settings = ffi::Settings {
        channels: 2,
        bits_per_sample: 16,
        sample_rate: SAMPLE_RATE,
        compression_level: COMPRESSION_LEVEL,
        total_samples,
    };
    let mut encoder = ffi::Encoder::new(file, settings, blocks).map_err(flac_error)?;
    let mut bytes = vec![0u8; SAMPLES_PER_CHUNK * BYTES_PER_SAMPLE];
    let mut samples = Vec::with_capacity(SAMPLES_PER_CHUNK * 2);
    loop {
        let filled = source.fill(&mut bytes)?;
        let Some(chunk) = bytes.get(..filled).filter(|chunk| !chunk.is_empty()) else {
            break;
        };
        samples.clear();
        samples.extend(
            chunk
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| i32::from(i16::from_le_bytes(*pair))),
        );
        let count = u32::try_from(chunk.len() / BYTES_PER_SAMPLE)
            .map_err(|_| EncodeError::Flac("too many samples at once".into()))?;
        encoder.process(&samples, count).map_err(flac_error)?;
    }
    encoder.finish().map_err(flac_error)
}

/// Reads PCM from a file or memory in whole-sample pieces.
enum PcmSource<'a> {
    File {
        file: File,
        path: &'a Path,
        length: u64,
    },
    Memory {
        bytes: &'a [u8],
        position: usize,
    },
}

impl<'a> PcmSource<'a> {
    fn open(pcm: &'a Pcm) -> Result<Self> {
        match pcm {
            Pcm::File(path) => {
                let io_error = |source| EncodeError::Io {
                    path: path.clone(),
                    source,
                };
                let file = File::open(path).map_err(io_error)?;
                let length = file.metadata().map_err(io_error)?.len();
                Ok(PcmSource::File { file, path, length })
            }
            Pcm::Memory(bytes) => Ok(PcmSource::Memory { bytes, position: 0 }),
        }
    }

    fn length(&self) -> u64 {
        match self {
            PcmSource::File { length, .. } => *length,
            PcmSource::Memory { bytes, .. } => u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        }
    }

    /// Fills `buffer` as far as possible; returns the bytes filled (0 at
    /// the end).
    fn fill(&mut self, buffer: &mut [u8]) -> Result<usize> {
        match self {
            PcmSource::File { file, path, .. } => {
                let mut filled = 0;
                while let Some(space) = buffer.get_mut(filled..).filter(|s| !s.is_empty()) {
                    let read = file.read(space).map_err(|source| EncodeError::Io {
                        path: path.to_path_buf(),
                        source,
                    })?;
                    if read == 0 {
                        break;
                    }
                    filled += read;
                }
                Ok(filled)
            }
            PcmSource::Memory { bytes, position } => {
                let rest = bytes.get(*position..).unwrap_or(&[]);
                let count = rest.len().min(buffer.len());
                if let (Some(target), Some(source)) = (buffer.get_mut(..count), rest.get(..count)) {
                    target.copy_from_slice(source);
                }
                *position += count;
                Ok(count)
            }
        }
    }
}

/// Width, height and bits per pixel of a PNG or JPEG image, when they can
/// be read from its header (FLAC PICTURE blocks record them).
pub fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32, u32)> {
    png_dimensions(bytes).or_else(|| jpeg_dimensions(bytes))
}

fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32, u32)> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if !bytes.starts_with(SIGNATURE) || bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    let be = |range: std::ops::Range<usize>| -> Option<u32> {
        Some(u32::from_be_bytes(bytes.get(range)?.try_into().ok()?))
    };
    let width = be(16..20)?;
    let height = be(20..24)?;
    let bit_depth = u32::from(*bytes.get(24)?);
    let channels = match bytes.get(25)? {
        0 | 3 => 1, // greyscale, palette
        2 => 3,     // RGB
        4 => 2,     // grey + alpha
        6 => 4,     // RGBA
        _ => return None,
    };
    Some((width, height, bit_depth * channels))
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32, u32)> {
    if bytes.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut position = 2usize;
    loop {
        if *bytes.get(position)? != 0xFF {
            return None;
        }
        let marker = *bytes.get(position + 1)?;
        if marker == 0xFF {
            position += 1; // fill byte
            continue;
        }
        let length = usize::from(u16::from_be_bytes([
            *bytes.get(position + 2)?,
            *bytes.get(position + 3)?,
        ]));
        // Start-of-frame markers (not DHT, JPG or DAC).
        if matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let precision = u32::from(*bytes.get(position + 4)?);
            let height = u32::from(u16::from_be_bytes([
                *bytes.get(position + 5)?,
                *bytes.get(position + 6)?,
            ]));
            let width = u32::from(u16::from_be_bytes([
                *bytes.get(position + 7)?,
                *bytes.get(position + 8)?,
            ]));
            let components = u32::from(*bytes.get(position + 9)?);
            return Some((width, height, precision * components));
        }
        if marker == 0xDA || length < 2 {
            return None; // start of scan before any frame header
        }
        position = position.checked_add(2 + length)?;
    }
}

/// A picture embedded in a FLAC file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddedPicture {
    /// FLAC picture type ([`PICTURE_FRONT_COVER`] for the front cover).
    pub picture_type: u32,
    /// MIME type, e.g. `image/jpeg`.
    pub mime: String,
    /// Width in pixels (0 when unknown).
    pub width: u32,
    /// Height in pixels (0 when unknown).
    pub height: u32,
    /// The image bytes.
    pub data: Vec<u8>,
}

/// A FLAC file decoded back into PCM and tags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedFlac {
    /// 16-bit little-endian interleaved PCM.
    pub pcm: Vec<u8>,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Number of channels.
    pub channels: u32,
    /// Bits per sample.
    pub bits_per_sample: u32,
    /// Vorbis comments in file order (names upper-cased).
    pub comments: Vec<VorbisComment>,
    /// Embedded pictures.
    pub pictures: Vec<EmbeddedPicture>,
}

/// Decodes a whole 16-bit FLAC file into memory, checking its MD5
/// signature. Used to double-check rips (and by the tests).
pub fn decode_flac(path: &Path) -> Result<DecodedFlac> {
    let file = File::open(path).map_err(|source| EncodeError::Io {
        path: path.to_owned(),
        source,
    })?;
    let decoded = ffi::decode(file).map_err(|error| match error {
        ffi::FlacError::Library(message) => EncodeError::Flac(message),
        ffi::FlacError::Io(source) => EncodeError::Io {
            path: path.to_owned(),
            source,
        },
    })?;
    let comments = decoded
        .comments
        .iter()
        .filter_map(|entry| {
            let (name, value) = entry.split_once('=')?;
            Some(VorbisComment {
                name: name.to_ascii_uppercase(),
                value: value.to_owned(),
            })
        })
        .collect();
    let pictures = decoded
        .pictures
        .into_iter()
        .map(|picture| EmbeddedPicture {
            picture_type: picture.picture_type,
            mime: picture.mime,
            width: picture.width,
            height: picture.height,
            data: picture.data,
        })
        .collect();
    Ok(DecodedFlac {
        pcm: decoded.pcm,
        sample_rate: decoded.sample_rate,
        channels: decoded.channels,
        bits_per_sample: decoded.bits_per_sample,
        comments,
        pictures,
    })
}

/// SHA-256 of a file, streamed.
pub fn sha256_file(path: &Path) -> Result<Sha256Digest> {
    use sha2::{Digest, Sha256};
    let io_error = |source| EncodeError::Io {
        path: path.to_owned(),
        source,
    };
    let mut file = std::fs::File::open(path).map_err(io_error)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = file.read(&mut buffer).map_err(io_error)?;
        let Some(chunk) = buffer.get(..read) else {
            break;
        };
        if chunk.is_empty() {
            break;
        }
        hasher.update(chunk);
    }
    Ok(Sha256Digest(hasher.finalize().into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Sectors, TrackNumber};
    use crate::testsupport::sine_pcm;
    use std::sync::Arc;

    fn comment(name: &str, value: &str) -> VorbisComment {
        VorbisComment {
            name: name.into(),
            value: value.into(),
        }
    }

    /// A tiny but valid PNG header (only the header is read).
    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 2, 0, 0, 0]);
        bytes.extend_from_slice(&[0xAB; 64]);
        bytes
    }

    #[test]
    fn round_trips_bit_exact_with_tags_and_cover() {
        let dir = tempfile::tempdir().unwrap();
        let pcm_bytes = sine_pcm(3, Sectors(75 * 3 + 17), 1234);
        let pcm_path = dir.path().join("track.pcm");
        std::fs::write(&pcm_path, &pcm_bytes).unwrap();
        let cover = png_bytes(600, 500);
        let metadata = FlacMetadata {
            comments: vec![
                comment("TITLE", "Café au lait – “quoted”"),
                comment("ARTIST", "Somebody"),
                comment("MUSICBRAINZ_ARTISTID", "a"),
                comment("MUSICBRAINZ_ARTISTID", "b"),
            ],
            cover: Some(CoverArt {
                bytes: Arc::from(cover.clone()),
                mime: "image/png".into(),
            }),
        };
        let output = dir.path().join("track.flac");
        let encoded = encode_flac(&Pcm::File(pcm_path), &output, &metadata).unwrap();
        assert_eq!(encoded.path, output);
        assert_eq!(
            encoded.size_bytes,
            std::fs::metadata(&output).unwrap().len()
        );
        assert_eq!(encoded.sha256, sha256_file(&output).unwrap());
        assert!(encoded.size_bytes < pcm_bytes.len() as u64);

        let decoded = decode_flac(&output).unwrap();
        assert_eq!(decoded.pcm, pcm_bytes, "decoded audio is bit-exact");
        assert_eq!(
            (
                decoded.sample_rate,
                decoded.channels,
                decoded.bits_per_sample
            ),
            (44_100, 2, 16)
        );
        assert_eq!(decoded.comments, metadata.comments);
        assert_eq!(decoded.pictures.len(), 1);
        let picture = &decoded.pictures[0];
        assert_eq!(picture.picture_type, PICTURE_FRONT_COVER);
        assert_eq!(picture.mime, "image/png");
        assert_eq!((picture.width, picture.height), (600, 500));
        assert_eq!(picture.data, cover);
    }

    #[test]
    fn encodes_from_memory_and_handles_silence() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("silence.flac");
        let pcm = Pcm::Memory(Arc::from(vec![0u8; 2352 * 10]));
        encode_flac(&pcm, &output, &FlacMetadata::default()).unwrap();
        let decoded = decode_flac(&output).unwrap();
        assert_eq!(decoded.pcm, vec![0u8; 2352 * 10]);
        assert!(decoded.pictures.is_empty());
    }

    #[test]
    fn rejects_partial_samples_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("odd.flac");
        let pcm = Pcm::Memory(Arc::from(vec![0u8; 6]));
        let error = encode_flac(&pcm, &output, &FlacMetadata::default()).unwrap_err();
        assert!(matches!(error, EncodeError::PcmLength(6)));
        assert!(!output.exists());
    }

    #[test]
    fn rejects_bad_tag_names() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("bad.flac");
        let metadata = FlacMetadata {
            comments: vec![comment("BAD=NAME", "x")],
            cover: None,
        };
        let pcm = Pcm::Memory(Arc::from(vec![0u8; 8]));
        assert!(matches!(
            encode_flac(&pcm, &output, &metadata),
            Err(EncodeError::Flac(_))
        ));
        assert!(!output.exists());
    }

    #[test]
    fn missing_input_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let pcm = Pcm::File(dir.path().join("missing.pcm"));
        let result = encode_flac(&pcm, &dir.path().join("x.flac"), &FlacMetadata::default());
        assert!(matches!(result, Err(EncodeError::Io { .. })));
    }

    #[test]
    fn decoding_garbage_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("garbage.flac");
        std::fs::write(&path, b"this is not a flac file at all").unwrap();
        assert!(decode_flac(&path).is_err());
    }

    #[test]
    fn tagged_track_matches_the_flac_command_line_tools() {
        // Cross-check with the reference tools when they are installed.
        let has = |tool: &str| {
            std::process::Command::new(tool)
                .arg("--version")
                .output()
                .is_ok_and(|out| out.status.success())
        };
        if !has("flac") || !has("metaflac") {
            eprintln!("flac/metaflac not installed; skipping the cross-check");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pcm_bytes = sine_pcm(1, Sectors(150), 0);
        let release = crate::model::ReleaseMetadata {
            title: "Album".into(),
            artist: "Artist".into(),
            disc_number: 1,
            disc_total: 1,
            ..Default::default()
        };
        let track = crate::model::TrackMetadata {
            number: Some(TrackNumber::new(1).unwrap()),
            title: "First".into(),
            ..Default::default()
        };
        let metadata = FlacMetadata {
            comments: tags::vorbis_comments(&release, &track),
            cover: Some(CoverArt {
                bytes: Arc::from(png_bytes(10, 20)),
                mime: "image/png".into(),
            }),
        };
        let output = dir.path().join("check.flac");
        encode_flac(
            &Pcm::Memory(Arc::from(pcm_bytes.clone())),
            &output,
            &metadata,
        )
        .unwrap();

        let test = std::process::Command::new("flac")
            .args(["--test", "--silent"])
            .arg(&output)
            .status()
            .unwrap();
        assert!(test.success(), "flac --test accepts the file");
        let raw = dir.path().join("check.raw");
        let decode = std::process::Command::new("flac")
            .args([
                "--decode",
                "--silent",
                "--force-raw-format",
                "--endian=little",
                "--sign=signed",
                "-o",
            ])
            .arg(&raw)
            .arg(&output)
            .status()
            .unwrap();
        assert!(decode.success());
        assert_eq!(std::fs::read(&raw).unwrap(), pcm_bytes);
        let tags = std::process::Command::new("metaflac")
            .arg("--export-tags-to=-")
            .arg(&output)
            .output()
            .unwrap();
        let tags = String::from_utf8(tags.stdout).unwrap();
        assert!(tags.contains("TITLE=First"));
        assert!(tags.contains("ALBUM=Album"));
        assert!(tags.contains("MEDIA=CD"));
        let list = std::process::Command::new("metaflac")
            .args(["--list", "--block-type=PICTURE"])
            .arg(&output)
            .output()
            .unwrap();
        let list = String::from_utf8(list.stdout).unwrap();
        assert!(list.contains("type: 3 (Cover (front))"), "{list}");
        assert!(list.contains("MIME type: image/png"));
        let streaminfo = std::process::Command::new("metaflac")
            .args(["--show-total-samples", "--show-sample-rate"])
            .arg(&output)
            .output()
            .unwrap();
        // metaflac ends lines with \r\n on Windows.
        assert_eq!(
            String::from_utf8(streaminfo.stdout)
                .unwrap()
                .replace("\r\n", "\n"),
            "88200\n44100\n"
        );
    }

    #[test]
    fn simulated_disc_to_flac_is_bit_exact() {
        use crate::drive::DiscDrive;
        use crate::drive::fake::FakeDrive;
        use crate::drive::reader::{ReadOptions, read_track};
        use crate::fakedisc::{InjectedError, InjectedErrorKind};
        use crate::model::{Lba, ReadMode, SampleOffset};
        use crate::testsupport::{FakeDiscOptions, make_fake_disc};
        use std::sync::atomic::AtomicBool;

        let dir = tempfile::tempdir().unwrap();
        let disc_dir = dir.path().join("disc");
        let errors = vec![InjectedError {
            lba: Lba(100),
            kind: InjectedErrorKind::Jitter,
        }];
        let options = FakeDiscOptions {
            tracks: 3,
            seconds: 2,
            read_offset: SampleOffset(-48),
            errors,
            enhanced: false,
        };
        let disc = make_fake_disc(&disc_dir, &options).unwrap();
        let mut drive = FakeDrive::open(&disc_dir).unwrap();
        let info = drive.disc_info().unwrap();
        let track = info.toc.tracks()[0].clone();
        let pcm_path = dir.path().join("track01.pcm");
        let read_options = ReadOptions {
            mode: ReadMode::TestAndCopy,
            offset: SampleOffset(-48),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let lead_out = Sectors(info.toc.lead_out().0);
        let read = read_track(
            &mut drive,
            &track,
            lead_out,
            &read_options,
            &pcm_path,
            &mut |_| {},
        )
        .unwrap();
        assert!(read.is_clean());

        let output = dir.path().join("01.flac");
        encode_flac(&read.pcm, &output, &FlacMetadata::default()).unwrap();
        let truth = std::fs::read(disc.track_path(TrackNumber::new(1).unwrap())).unwrap();
        let decoded = decode_flac(&output).unwrap().pcm;
        // A negative offset reads 48 samples before the disc start: silence.
        assert!(decoded[..192].iter().all(|b| *b == 0));
        assert_eq!(decoded[192..], truth[192..]);
    }

    #[test]
    fn reads_image_dimensions() {
        assert_eq!(image_dimensions(&png_bytes(300, 200)), Some((300, 200, 24)));
        let jpeg = [
            0xFF, 0xD8, // SOI
            0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, // APP0, length 4
            0xFF, 0xC0, 0x00, 0x11, 0x08, 0x01, 0xF4, 0x02, 0x58, 0x03, // SOF0 600×500
        ];
        assert_eq!(image_dimensions(&jpeg), Some((600, 500, 24)));
        assert_eq!(image_dimensions(b"GIF89a"), None);
        assert_eq!(image_dimensions(&[0xFF, 0xD8, 0xFF]), None);
    }

    #[test]
    fn sha256_of_known_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc.txt");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap().to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
