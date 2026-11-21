// SPDX-License-Identifier: AGPL-3.0-or-later
//! Film discs (stage 2): the user-installed `makemkvcon` copies a title to a
//! lossless MKV master, and the user-installed `ffmpeg` makes a WebM (AV1 and
//! Opus) viewing copy the Player can stream.
//!
//! Nothing here is bundled: both programs are found on the user's computer
//! ([`makemkv::discover`], [`ffmpeg::discover`]) and the app explains how to
//! install them when they are missing.
//!
//! - [`robot`]: parses `makemkvcon -r` output lines.
//! - [`scan`]: turns those lines into a disc and its titles.
//! - [`makemkv`]: finds and runs `makemkvcon`.
//! - [`ffmpeg`]: finds and runs `ffmpeg`.
//! - [`process`]: runs a child, line by line, and kills it on cancel.

pub mod ffmpeg;
pub mod makemkv;
pub mod process;
pub mod robot;
pub mod scan;

use std::path::PathBuf;

/// What can go wrong with film discs.
#[derive(Debug, thiserror::Error)]
pub enum VideoError {
    /// `makemkvcon` was not found.
    #[error("MakeMKV is not installed")]
    MakeMkvMissing,
    /// `makemkvcon` ran but could not do the job.
    #[error("MakeMKV could not read the disc: {}", .0.join(" "))]
    MakeMkv(Vec<String>),
    /// MakeMKV found no drive that matches.
    #[error("MakeMKV cannot see this drive")]
    NoMakeMkvDrive,
    /// MakeMKV found nothing to copy on the disc.
    #[error("there is nothing on this disc to copy")]
    NoTitles,
    /// A copy finished but left no MKV file.
    #[error("MakeMKV finished without writing a file for title {0}")]
    NoOutput(usize),
    /// `ffmpeg` was not found.
    #[error("ffmpeg is not installed")]
    FfmpegMissing,
    /// `ffmpeg` has no AV1 or Opus encoder.
    #[error("this ffmpeg has no AV1 and Opus encoders: {0}")]
    NoEncoders(String),
    /// `ffmpeg` failed.
    #[error("ffmpeg could not make the viewing copy: {0}")]
    Encode(String),
    /// A program could not be started.
    #[error("could not start {program}: {source}")]
    Start {
        /// The program.
        program: String,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A file problem.
    #[error("could not use {path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The user stopped it.
    #[error("cancelled")]
    Cancelled,
}

/// Result of the video module.
pub type Result<T, E = VideoError> = std::result::Result<T, E>;
