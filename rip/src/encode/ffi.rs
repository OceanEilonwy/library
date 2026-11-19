// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hand-written bindings to libFLAC (stream encoder, stream decoder and
//! metadata objects), linked by build.rs through pkg-config, and the safe
//! wrappers around them. **Every `unsafe` block of the encoder is in this
//! file.**
//!
//! Invariants the wrappers keep:
//!
//! - [`Block`] owns one `FLAC__StreamMetadata` from
//!   `FLAC__metadata_object_new` and deletes it once. Only the fields this
//!   file declares are touched, through the `repr(C)` mirror of the header
//!   layout (header of three 32-bit fields, then the data union aligned
//!   like a `u64`).
//! - [`Encoder`] keeps its metadata blocks and its boxed sink alive for as
//!   long as the libFLAC encoder exists; libFLAC only borrows them.
//! - Callbacks receive a pointer to a boxed client struct that outlives the
//!   encoder/decoder. They never unwind: every Rust error is stored in the
//!   client struct and reported to libFLAC as a status code.

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::ptr::NonNull;

/// Opaque `FLAC__StreamEncoder`.
#[repr(C)]
struct StreamEncoder {
    _private: [u8; 0],
}

/// Opaque `FLAC__StreamDecoder`.
#[repr(C)]
struct StreamDecoder {
    _private: [u8; 0],
}

/// `FLAC__StreamMetadata_VorbisComment_Entry`.
#[repr(C)]
#[derive(Clone, Copy)]
struct VorbisEntry {
    length: u32,
    entry: *mut u8,
}

/// `FLAC__StreamMetadata_VorbisComment`.
#[repr(C)]
#[derive(Clone, Copy)]
struct VorbisCommentData {
    vendor_string: VorbisEntry,
    num_comments: u32,
    comments: *mut VorbisEntry,
}

/// `FLAC__StreamMetadata_Picture`.
#[repr(C)]
#[derive(Clone, Copy)]
struct PictureData {
    picture_type: c_int,
    mime_type: *mut c_char,
    description: *mut u8,
    width: u32,
    height: u32,
    depth: u32,
    colors: u32,
    data_length: u32,
    data: *mut u8,
}

/// The parts of the `FLAC__StreamMetadata` data union this file uses. The
/// `u64` member gives it the same alignment as the C union (whose
/// STREAMINFO member holds a `FLAC__uint64`).
#[repr(C)]
union MetadataData {
    vorbis_comment: VorbisCommentData,
    picture: PictureData,
    _align: u64,
}

/// `FLAC__StreamMetadata`.
#[repr(C)]
struct StreamMetadata {
    kind: c_int,
    is_last: c_int,
    length: u32,
    data: MetadataData,
}

/// The first fields of `FLAC__FrameHeader` (the start of `FLAC__Frame`).
#[repr(C)]
struct FrameHeaderStart {
    blocksize: u32,
    sample_rate: u32,
    channels: u32,
    channel_assignment: c_int,
    bits_per_sample: u32,
}

/// `FLAC__MetadataType` values.
const METADATA_PADDING: c_int = 1;
const METADATA_SEEKTABLE: c_int = 3;
const METADATA_VORBIS_COMMENT: c_int = 4;
const METADATA_PICTURE: c_int = 6;

const ENCODER_INIT_OK: c_int = 0;
const ENCODER_WRITE_OK: c_int = 0;
const ENCODER_WRITE_FATAL: c_int = 1;
const ENCODER_SEEK_OK: c_int = 0;
const ENCODER_SEEK_ERROR: c_int = 1;
const ENCODER_TELL_OK: c_int = 0;
const ENCODER_TELL_ERROR: c_int = 1;

const DECODER_INIT_OK: c_int = 0;
const DECODER_READ_CONTINUE: c_int = 0;
const DECODER_READ_END: c_int = 1;
const DECODER_READ_ABORT: c_int = 2;
const DECODER_WRITE_CONTINUE: c_int = 0;
const DECODER_WRITE_ABORT: c_int = 1;
const DECODER_END_OF_STREAM: c_int = 4;

type EncoderWrite =
    extern "C" fn(*const StreamEncoder, *const u8, usize, c_uint, c_uint, *mut c_void) -> c_int;
type EncoderSeek = extern "C" fn(*const StreamEncoder, u64, *mut c_void) -> c_int;
type EncoderTell = extern "C" fn(*const StreamEncoder, *mut u64, *mut c_void) -> c_int;
type EncoderMetadata = extern "C" fn(*const StreamEncoder, *const StreamMetadata, *mut c_void);

type DecoderRead = extern "C" fn(*const StreamDecoder, *mut u8, *mut usize, *mut c_void) -> c_int;
type DecoderWrite = extern "C" fn(
    *const StreamDecoder,
    *const FrameHeaderStart,
    *const *const i32,
    *mut c_void,
) -> c_int;
type DecoderMetadata = extern "C" fn(*const StreamDecoder, *const StreamMetadata, *mut c_void);
type DecoderError = extern "C" fn(*const StreamDecoder, c_int, *mut c_void);

unsafe extern "C" {
    fn FLAC__stream_encoder_new() -> *mut StreamEncoder;
    fn FLAC__stream_encoder_delete(encoder: *mut StreamEncoder);
    fn FLAC__stream_encoder_set_verify(encoder: *mut StreamEncoder, value: c_int) -> c_int;
    fn FLAC__stream_encoder_set_channels(encoder: *mut StreamEncoder, value: c_uint) -> c_int;
    fn FLAC__stream_encoder_set_bits_per_sample(
        encoder: *mut StreamEncoder,
        value: c_uint,
    ) -> c_int;
    fn FLAC__stream_encoder_set_sample_rate(encoder: *mut StreamEncoder, value: c_uint) -> c_int;
    fn FLAC__stream_encoder_set_compression_level(
        encoder: *mut StreamEncoder,
        value: c_uint,
    ) -> c_int;
    fn FLAC__stream_encoder_set_total_samples_estimate(
        encoder: *mut StreamEncoder,
        value: u64,
    ) -> c_int;
    fn FLAC__stream_encoder_set_metadata(
        encoder: *mut StreamEncoder,
        metadata: *mut *mut StreamMetadata,
        count: c_uint,
    ) -> c_int;
    fn FLAC__stream_encoder_init_stream(
        encoder: *mut StreamEncoder,
        write: EncoderWrite,
        seek: Option<EncoderSeek>,
        tell: Option<EncoderTell>,
        metadata: Option<EncoderMetadata>,
        client: *mut c_void,
    ) -> c_int;
    fn FLAC__stream_encoder_process_interleaved(
        encoder: *mut StreamEncoder,
        buffer: *const i32,
        samples: c_uint,
    ) -> c_int;
    fn FLAC__stream_encoder_finish(encoder: *mut StreamEncoder) -> c_int;
    fn FLAC__stream_encoder_get_resolved_state_string(
        encoder: *const StreamEncoder,
    ) -> *const c_char;

    fn FLAC__stream_decoder_new() -> *mut StreamDecoder;
    fn FLAC__stream_decoder_delete(decoder: *mut StreamDecoder);
    fn FLAC__stream_decoder_set_metadata_respond(decoder: *mut StreamDecoder, kind: c_int)
    -> c_int;
    fn FLAC__stream_decoder_set_md5_checking(decoder: *mut StreamDecoder, value: c_int) -> c_int;
    fn FLAC__stream_decoder_init_stream(
        decoder: *mut StreamDecoder,
        read: DecoderRead,
        seek: *const c_void,
        tell: *const c_void,
        length: *const c_void,
        eof: *const c_void,
        write: DecoderWrite,
        metadata: Option<DecoderMetadata>,
        error: DecoderError,
        client: *mut c_void,
    ) -> c_int;
    fn FLAC__stream_decoder_process_until_end_of_stream(decoder: *mut StreamDecoder) -> c_int;
    fn FLAC__stream_decoder_finish(decoder: *mut StreamDecoder) -> c_int;
    fn FLAC__stream_decoder_get_state(decoder: *const StreamDecoder) -> c_int;
    fn FLAC__stream_decoder_get_resolved_state_string(
        decoder: *const StreamDecoder,
    ) -> *const c_char;

    fn FLAC__metadata_object_new(kind: c_int) -> *mut StreamMetadata;
    fn FLAC__metadata_object_delete(object: *mut StreamMetadata);
    fn FLAC__metadata_object_vorbiscomment_entry_from_name_value_pair(
        entry: *mut VorbisEntry,
        name: *const c_char,
        value: *const c_char,
    ) -> c_int;
    fn FLAC__metadata_object_vorbiscomment_append_comment(
        object: *mut StreamMetadata,
        entry: VorbisEntry,
        copy: c_int,
    ) -> c_int;
    fn FLAC__metadata_object_picture_set_mime_type(
        object: *mut StreamMetadata,
        mime: *mut c_char,
        copy: c_int,
    ) -> c_int;
    fn FLAC__metadata_object_picture_set_description(
        object: *mut StreamMetadata,
        description: *mut u8,
        copy: c_int,
    ) -> c_int;
    fn FLAC__metadata_object_picture_set_data(
        object: *mut StreamMetadata,
        data: *mut u8,
        length: u32,
        copy: c_int,
    ) -> c_int;
    fn FLAC__metadata_object_seektable_template_append_spaced_points_by_samples(
        object: *mut StreamMetadata,
        samples: c_uint,
        total_samples: u64,
    ) -> c_int;
}

/// An error from libFLAC or from the file behind it.
#[derive(Debug)]
pub(super) enum FlacError {
    /// libFLAC's own description.
    Library(String),
    /// Reading or writing the file failed.
    Io(std::io::Error),
}

fn library(message: &str) -> FlacError {
    FlacError::Library(message.to_owned())
}

/// Copies a static C string from libFLAC.
fn static_string(pointer: *const c_char) -> String {
    if pointer.is_null() {
        return "unknown error".into();
    }
    // SAFETY: libFLAC state strings are static NUL-terminated strings.
    unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned()
}

/// One owned metadata block.
pub(super) struct Block(NonNull<StreamMetadata>);

impl Block {
    fn new(kind: c_int) -> Result<Self, FlacError> {
        // SAFETY: no preconditions; NULL on allocation failure.
        let object = unsafe { FLAC__metadata_object_new(kind) };
        NonNull::new(object)
            .map(Block)
            .ok_or_else(|| library("out of memory making a metadata block"))
    }

    /// A VORBIS_COMMENT block with these `(NAME, value)` pairs, in order.
    pub fn vorbis_comment<'a>(
        comments: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Result<Self, FlacError> {
        let block = Block::new(METADATA_VORBIS_COMMENT)?;
        for (name, value) in comments {
            let name = CString::new(name).map_err(|_| library("a tag name contains a NUL"))?;
            let value = CString::new(value).map_err(|_| library("a tag value contains a NUL"))?;
            let mut entry = VorbisEntry {
                length: 0,
                entry: std::ptr::null_mut(),
            };
            // SAFETY: valid C strings; `entry` is an out-parameter that
            // libFLAC fills with a freshly allocated buffer.
            let made = unsafe {
                FLAC__metadata_object_vorbiscomment_entry_from_name_value_pair(
                    &mut entry,
                    name.as_ptr(),
                    value.as_ptr(),
                )
            };
            if made == 0 {
                return Err(library("a tag name is not allowed in FLAC"));
            }
            // SAFETY: `block` is a VORBIS_COMMENT object; with copy=false
            // libFLAC takes ownership of the entry buffer (and frees it on
            // failure as well).
            let appended = unsafe {
                FLAC__metadata_object_vorbiscomment_append_comment(block.0.as_ptr(), entry, 0)
            };
            if appended == 0 {
                return Err(library("out of memory adding a tag"));
            }
        }
        Ok(block)
    }

    /// A PICTURE block.
    pub fn picture(
        picture_type: u32,
        mime: &str,
        description: &str,
        dimensions: Option<(u32, u32, u32)>,
        data: &[u8],
    ) -> Result<Self, FlacError> {
        let block = Block::new(METADATA_PICTURE)?;
        let mime = CString::new(mime).map_err(|_| library("the cover type contains a NUL"))?;
        let description = CString::new(description)
            .map_err(|_| library("the cover description contains a NUL"))?;
        let length =
            u32::try_from(data.len()).map_err(|_| library("the cover picture is too large"))?;
        let object = block.0.as_ptr();
        // SAFETY: `object` is a PICTURE object. With copy=true libFLAC
        // copies the strings and data, so the borrowed buffers may be
        // passed as `*mut` (they are not written through).
        let ok = unsafe {
            FLAC__metadata_object_picture_set_mime_type(object, mime.as_ptr().cast_mut(), 1) != 0
                && FLAC__metadata_object_picture_set_description(
                    object,
                    description.as_ptr().cast_mut().cast(),
                    1,
                ) != 0
                && FLAC__metadata_object_picture_set_data(
                    object,
                    data.as_ptr().cast_mut(),
                    length,
                    1,
                ) != 0
        };
        if !ok {
            return Err(library("out of memory adding the cover picture"));
        }
        let picture_type =
            c_int::try_from(picture_type).map_err(|_| library("bad picture type"))?;
        let (width, height, depth) = dimensions.unwrap_or((0, 0, 0));
        // SAFETY: `object` is a PICTURE object, so the `picture` member of
        // the union is the active one; plain integer fields are written.
        unsafe {
            let picture = &mut (*object).data.picture;
            picture.picture_type = picture_type;
            picture.width = width;
            picture.height = height;
            picture.depth = depth;
            picture.colors = 0;
        }
        Ok(block)
    }

    /// A SEEKTABLE template with a point every `spacing` samples; libFLAC
    /// fills in the offsets when encoding finishes.
    pub fn seek_table(spacing: u32, total_samples: u64) -> Result<Self, FlacError> {
        let block = Block::new(METADATA_SEEKTABLE)?;
        // SAFETY: `block` is a SEEKTABLE object.
        let ok = unsafe {
            FLAC__metadata_object_seektable_template_append_spaced_points_by_samples(
                block.0.as_ptr(),
                spacing,
                total_samples,
            )
        };
        if ok == 0 {
            return Err(library("out of memory making the seek table"));
        }
        Ok(block)
    }

    /// A PADDING block of `length` bytes (room to edit tags in place).
    pub fn padding(length: u32) -> Result<Self, FlacError> {
        let block = Block::new(METADATA_PADDING)?;
        // SAFETY: plain header field of a block we own.
        unsafe { (*block.0.as_ptr()).length = length };
        Ok(block)
    }
}

impl Drop for Block {
    fn drop(&mut self) {
        // SAFETY: created by FLAC__metadata_object_new, deleted once.
        unsafe { FLAC__metadata_object_delete(self.0.as_ptr()) };
    }
}

/// Encoder settings.
#[derive(Clone, Copy, Debug)]
pub(super) struct Settings {
    pub channels: u32,
    pub bits_per_sample: u32,
    pub sample_rate: u32,
    pub compression_level: u32,
    pub total_samples: u64,
}

/// Where encoded bytes go; errors are kept for after libFLAC returns.
struct Sink {
    writer: BufWriter<File>,
    error: Option<std::io::Error>,
}

extern "C" fn encoder_write(
    _encoder: *const StreamEncoder,
    buffer: *const u8,
    bytes: usize,
    _samples: c_uint,
    _frame: c_uint,
    client: *mut c_void,
) -> c_int {
    // SAFETY: `client` is the boxed Sink, alive for the encoder's lifetime.
    let sink = unsafe { &mut *client.cast::<Sink>() };
    if bytes == 0 {
        return ENCODER_WRITE_OK;
    }
    if buffer.is_null() {
        return ENCODER_WRITE_FATAL;
    }
    // SAFETY: libFLAC passes `bytes` valid bytes at `buffer`.
    let data = unsafe { std::slice::from_raw_parts(buffer, bytes) };
    match sink.writer.write_all(data) {
        Ok(()) => ENCODER_WRITE_OK,
        Err(error) => {
            sink.error = Some(error);
            ENCODER_WRITE_FATAL
        }
    }
}

extern "C" fn encoder_seek(
    _encoder: *const StreamEncoder,
    offset: u64,
    client: *mut c_void,
) -> c_int {
    // SAFETY: as in `encoder_write`.
    let sink = unsafe { &mut *client.cast::<Sink>() };
    match sink.writer.seek(SeekFrom::Start(offset)) {
        Ok(_) => ENCODER_SEEK_OK,
        Err(error) => {
            sink.error = Some(error);
            ENCODER_SEEK_ERROR
        }
    }
}

extern "C" fn encoder_tell(
    _encoder: *const StreamEncoder,
    offset: *mut u64,
    client: *mut c_void,
) -> c_int {
    // SAFETY: as in `encoder_write`.
    let sink = unsafe { &mut *client.cast::<Sink>() };
    match sink.writer.stream_position() {
        Ok(position) if !offset.is_null() => {
            // SAFETY: non-NULL out-parameter from libFLAC.
            unsafe { *offset = position };
            ENCODER_TELL_OK
        }
        Ok(_) => ENCODER_TELL_ERROR,
        Err(error) => {
            sink.error = Some(error);
            ENCODER_TELL_ERROR
        }
    }
}

/// A libFLAC stream encoder writing to a file.
pub(super) struct Encoder {
    raw: NonNull<StreamEncoder>,
    sink: Box<Sink>,
    // Kept alive until the encoder is deleted: libFLAC borrows them.
    _blocks: Vec<Block>,
    _pointers: Vec<*mut StreamMetadata>,
    channels: usize,
    finished: bool,
}

impl Encoder {
    /// Starts encoding into `file` with verify mode on.
    pub fn new(file: File, settings: Settings, blocks: Vec<Block>) -> Result<Self, FlacError> {
        // SAFETY: no preconditions; NULL on allocation failure.
        let raw = NonNull::new(unsafe { FLAC__stream_encoder_new() })
            .ok_or_else(|| library("out of memory starting the encoder"))?;
        let mut encoder = Encoder {
            raw,
            sink: Box::new(Sink {
                writer: BufWriter::new(file),
                error: None,
            }),
            _pointers: blocks.iter().map(|block| block.0.as_ptr()).collect(),
            _blocks: blocks,
            channels: usize::try_from(settings.channels).unwrap_or(usize::MAX),
            finished: true, // nothing to finish until init succeeds
        };
        let pointer = encoder.raw.as_ptr();
        let count = c_uint::try_from(encoder._pointers.len())
            .map_err(|_| library("too many metadata blocks"))?;
        // SAFETY: `pointer` is a fresh, uninitialised encoder; setters are
        // valid in that state. The metadata array and blocks live in
        // `encoder` and so outlive the libFLAC encoder.
        let configured = unsafe {
            FLAC__stream_encoder_set_verify(pointer, 1) != 0
                && FLAC__stream_encoder_set_channels(pointer, settings.channels) != 0
                && FLAC__stream_encoder_set_bits_per_sample(pointer, settings.bits_per_sample) != 0
                && FLAC__stream_encoder_set_sample_rate(pointer, settings.sample_rate) != 0
                && FLAC__stream_encoder_set_compression_level(pointer, settings.compression_level)
                    != 0
                && FLAC__stream_encoder_set_total_samples_estimate(pointer, settings.total_samples)
                    != 0
                && FLAC__stream_encoder_set_metadata(pointer, encoder._pointers.as_mut_ptr(), count)
                    != 0
        };
        if !configured {
            return Err(library("the encoder refused its settings"));
        }
        let client: *mut Sink = &mut *encoder.sink;
        // SAFETY: configured encoder; callbacks match libFLAC's signatures
        // and `client` (the boxed sink) outlives the encoder.
        let status = unsafe {
            FLAC__stream_encoder_init_stream(
                pointer,
                encoder_write,
                Some(encoder_seek),
                Some(encoder_tell),
                None,
                client.cast(),
            )
        };
        if status != ENCODER_INIT_OK {
            return Err(encoder.failure());
        }
        encoder.finished = false;
        Ok(encoder)
    }

    /// The error to report: a file error if one happened, else libFLAC's.
    fn failure(&mut self) -> FlacError {
        if let Some(error) = self.sink.error.take() {
            return FlacError::Io(error);
        }
        // SAFETY: live encoder.
        let state = unsafe { FLAC__stream_encoder_get_resolved_state_string(self.raw.as_ptr()) };
        FlacError::Library(static_string(state))
    }

    /// Encodes interleaved samples (`channels` values per sample).
    pub fn process(&mut self, interleaved: &[i32], samples: u32) -> Result<(), FlacError> {
        let needed = usize::try_from(samples)
            .ok()
            .and_then(|n| n.checked_mul(self.channels))
            .ok_or_else(|| library("too many samples at once"))?;
        if interleaved.len() < needed {
            return Err(library("fewer sample values than samples"));
        }
        // SAFETY: initialised encoder; `interleaved` holds at least
        // `samples × channels` values (checked above).
        let ok = unsafe {
            FLAC__stream_encoder_process_interleaved(
                self.raw.as_ptr(),
                interleaved.as_ptr(),
                samples,
            )
        };
        if ok == 0 {
            return Err(self.failure());
        }
        Ok(())
    }

    /// Finishes the stream (rewriting STREAMINFO and the seek table) and
    /// flushes the file.
    pub fn finish(mut self) -> Result<(), FlacError> {
        self.finished = true;
        // SAFETY: initialised encoder; finished once.
        let ok = unsafe { FLAC__stream_encoder_finish(self.raw.as_ptr()) };
        if ok == 0 {
            return Err(self.failure());
        }
        if let Some(error) = self.sink.error.take() {
            return Err(FlacError::Io(error));
        }
        self.sink.writer.flush().map_err(FlacError::Io)?;
        self.sink.writer.get_ref().sync_all().map_err(FlacError::Io)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        if !self.finished {
            // SAFETY: initialised encoder that was never finished; the
            // result is irrelevant as the output is being abandoned.
            unsafe { FLAC__stream_encoder_finish(self.raw.as_ptr()) };
        }
        // SAFETY: created by FLAC__stream_encoder_new, deleted once, before
        // the sink and blocks it borrows are dropped.
        unsafe { FLAC__stream_encoder_delete(self.raw.as_ptr()) };
    }
}

/// A picture found while decoding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RawPicture {
    pub picture_type: u32,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// What decoding a FLAC file produced.
#[derive(Debug, Default)]
pub(super) struct Decoded {
    /// Interleaved samples as 16-bit little-endian bytes.
    pub pcm: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u32,
    pub bits_per_sample: u32,
    /// Raw `NAME=value` comment strings.
    pub comments: Vec<String>,
    pub pictures: Vec<RawPicture>,
}

struct DecodeClient {
    reader: File,
    decoded: Decoded,
    error: Option<FlacError>,
}

extern "C" fn decoder_read(
    _decoder: *const StreamDecoder,
    buffer: *mut u8,
    bytes: *mut usize,
    client: *mut c_void,
) -> c_int {
    // SAFETY: `client` is the boxed DecodeClient, alive for the decoder.
    let client = unsafe { &mut *client.cast::<DecodeClient>() };
    if buffer.is_null() || bytes.is_null() {
        return DECODER_READ_ABORT;
    }
    // SAFETY: libFLAC gives a writable buffer of `*bytes` bytes.
    let space = unsafe { std::slice::from_raw_parts_mut(buffer, *bytes) };
    match client.reader.read(space) {
        Ok(0) => {
            // SAFETY: non-NULL out-parameter.
            unsafe { *bytes = 0 };
            DECODER_READ_END
        }
        Ok(read) => {
            // SAFETY: as above.
            unsafe { *bytes = read };
            DECODER_READ_CONTINUE
        }
        Err(error) => {
            client.error = Some(FlacError::Io(error));
            DECODER_READ_ABORT
        }
    }
}

extern "C" fn decoder_write(
    _decoder: *const StreamDecoder,
    frame: *const FrameHeaderStart,
    buffer: *const *const i32,
    client: *mut c_void,
) -> c_int {
    // SAFETY: as in `decoder_read`.
    let client = unsafe { &mut *client.cast::<DecodeClient>() };
    if frame.is_null() || buffer.is_null() {
        return DECODER_WRITE_ABORT;
    }
    // SAFETY: `frame` points to a FLAC__Frame, which starts with the header
    // fields mirrored by FrameHeaderStart.
    let header = unsafe { &*frame };
    if header.bits_per_sample != 16 {
        client.error = Some(library("only 16-bit audio is expected"));
        return DECODER_WRITE_ABORT;
    }
    let (Ok(blocksize), Ok(channels)) = (
        usize::try_from(header.blocksize),
        usize::try_from(header.channels),
    ) else {
        return DECODER_WRITE_ABORT;
    };
    let decoded = &mut client.decoded;
    decoded.sample_rate = header.sample_rate;
    decoded.channels = header.channels;
    decoded.bits_per_sample = header.bits_per_sample;
    let mut planes = Vec::with_capacity(channels);
    for channel in 0..channels {
        // SAFETY: `buffer` holds one pointer per channel, each to
        // `blocksize` samples, valid during this callback.
        let plane = unsafe {
            let pointer = *buffer.add(channel);
            if pointer.is_null() {
                return DECODER_WRITE_ABORT;
            }
            std::slice::from_raw_parts(pointer, blocksize)
        };
        planes.push(plane);
    }
    decoded.pcm.reserve(blocksize * channels * 2);
    for index in 0..blocksize {
        for plane in &planes {
            let Some(sample) = plane.get(index).and_then(|s| i16::try_from(*s).ok()) else {
                return DECODER_WRITE_ABORT;
            };
            decoded.pcm.extend_from_slice(&sample.to_le_bytes());
        }
    }
    DECODER_WRITE_CONTINUE
}

/// Copies `length` bytes from a libFLAC buffer.
///
/// Safety: `pointer` is NULL or valid for `length` bytes.
unsafe fn copy_bytes(pointer: *const u8, length: u32) -> Vec<u8> {
    match usize::try_from(length) {
        Ok(length) if !pointer.is_null() && length > 0 => {
            // SAFETY: per the contract.
            unsafe { std::slice::from_raw_parts(pointer, length) }.to_vec()
        }
        _ => Vec::new(),
    }
}

extern "C" fn decoder_metadata(
    _decoder: *const StreamDecoder,
    metadata: *const StreamMetadata,
    client: *mut c_void,
) {
    // SAFETY: as in `decoder_read`.
    let client = unsafe { &mut *client.cast::<DecodeClient>() };
    if metadata.is_null() {
        return;
    }
    // SAFETY: libFLAC passes a valid block for the duration of the call.
    let block = unsafe { &*metadata };
    match block.kind {
        METADATA_VORBIS_COMMENT => {
            // SAFETY: the kind says `vorbis_comment` is the active member.
            let comments = unsafe { block.data.vorbis_comment };
            let count = usize::try_from(comments.num_comments).unwrap_or(0);
            for index in 0..count {
                // SAFETY: `comments` points to `num_comments` entries, each
                // `length` bytes long.
                let bytes = unsafe {
                    let entry = *comments.comments.add(index);
                    copy_bytes(entry.entry, entry.length)
                };
                client
                    .decoded
                    .comments
                    .push(String::from_utf8_lossy(&bytes).into_owned());
            }
        }
        METADATA_PICTURE => {
            // SAFETY: the kind says `picture` is the active member; its
            // strings are NUL-terminated and its data `data_length` long.
            let picture = unsafe { block.data.picture };
            let mime = if picture.mime_type.is_null() {
                String::new()
            } else {
                // SAFETY: non-NULL, NUL-terminated.
                unsafe { CStr::from_ptr(picture.mime_type) }
                    .to_string_lossy()
                    .into_owned()
            };
            client.decoded.pictures.push(RawPicture {
                picture_type: u32::try_from(picture.picture_type).unwrap_or(0),
                mime,
                width: picture.width,
                height: picture.height,
                // SAFETY: as above.
                data: unsafe { copy_bytes(picture.data, picture.data_length) },
            });
        }
        _ => {}
    }
}

extern "C" fn decoder_error(_decoder: *const StreamDecoder, status: c_int, client: *mut c_void) {
    // SAFETY: as in `decoder_read`.
    let client = unsafe { &mut *client.cast::<DecodeClient>() };
    if client.error.is_none() {
        client.error = Some(FlacError::Library(format!(
            "the file is damaged (decoder error {status})"
        )));
    }
}

/// Decodes a whole FLAC file into memory.
pub(super) fn decode(reader: File) -> Result<Decoded, FlacError> {
    struct DecoderHandle(NonNull<StreamDecoder>);
    impl Drop for DecoderHandle {
        fn drop(&mut self) {
            // SAFETY: created by FLAC__stream_decoder_new, deleted once
            // (delete also finishes an initialised decoder).
            unsafe { FLAC__stream_decoder_delete(self.0.as_ptr()) };
        }
    }

    // Declared before the decoder handle so it is dropped after it: the
    // decoder borrows it as client data.
    let mut client = Box::new(DecodeClient {
        reader,
        decoded: Decoded::default(),
        error: None,
    });
    let client_pointer: *mut DecodeClient = &mut *client;
    // SAFETY: no preconditions; NULL on allocation failure.
    let handle = NonNull::new(unsafe { FLAC__stream_decoder_new() })
        .map(DecoderHandle)
        .ok_or_else(|| library("out of memory starting the decoder"))?;
    let decoder = handle.0.as_ptr();
    // SAFETY: fresh decoder; settings are made before init; the client
    // outlives the decoder (see above).
    let status = unsafe {
        FLAC__stream_decoder_set_md5_checking(decoder, 1);
        FLAC__stream_decoder_set_metadata_respond(decoder, METADATA_VORBIS_COMMENT);
        FLAC__stream_decoder_set_metadata_respond(decoder, METADATA_PICTURE);
        FLAC__stream_decoder_init_stream(
            decoder,
            decoder_read,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            decoder_write,
            Some(decoder_metadata),
            decoder_error,
            client_pointer.cast(),
        )
    };
    if status != DECODER_INIT_OK {
        return Err(library("could not start the decoder"));
    }
    // SAFETY: initialised decoder.
    let ok = unsafe { FLAC__stream_decoder_process_until_end_of_stream(decoder) };
    // SAFETY: initialised decoder.
    let state = unsafe { FLAC__stream_decoder_get_state(decoder) };
    let failure = (ok == 0 || state != DECODER_END_OF_STREAM).then(|| {
        // SAFETY: live decoder; the string is static.
        static_string(unsafe { FLAC__stream_decoder_get_resolved_state_string(decoder) })
    });
    // SAFETY: initialised decoder; with MD5 checking on, returns false when
    // the decoded audio does not match the signature in STREAMINFO.
    let md5_ok = unsafe { FLAC__stream_decoder_finish(decoder) };
    drop(handle);
    if let Some(error) = client.error.take() {
        return Err(error);
    }
    if let Some(message) = failure {
        return Err(FlacError::Library(message));
    }
    if md5_ok == 0 {
        return Err(library("the audio does not match the file's MD5 signature"));
    }
    Ok(std::mem::take(&mut client.decoded))
}
