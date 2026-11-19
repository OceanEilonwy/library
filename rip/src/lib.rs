// SPDX-License-Identifier: AGPL-3.0-or-later
//! Library Rip: an archival disc ripper for The Library.
//!
//! Module map (lanes in parentheses; see README.md):
//!
//! - [`model`] — shared types, the integration contract (Phase 0).
//! - [`fakedisc`] — the simulated disc format (Phase 0).
//! - [`core`] — disc ids, AccurateRip, CTDB, offsets, cue, log, `rip.json` (B).
//! - [`drive`] — `DiscDrive`, `FakeDrive`, libcdio-paranoia, secure reader (C).
//! - [`encode`] — FLAC through libFLAC, tags, cover (C).
//! - [`library`] — rip API client, connect handshake, key store (D).
//! - [`musicbrainz`] — disc-ID lookup, release metadata, cover art (D).
//! - [`app`] — state machine, pipeline, settings, headless runner, GUI (E).

#![warn(missing_docs)]

pub mod app;
pub mod core;
pub mod drive;
pub mod encode;
pub mod fakedisc;
pub mod library;
#[cfg(any(test, feature = "testsupport"))]
pub mod mockservices;
pub mod model;
pub mod musicbrainz;
#[cfg(any(test, feature = "testsupport"))]
pub mod testsupport;

/// This build's version (`CARGO_PKG_VERSION`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where the source code lives (AGPL: shown in About and the log).
pub const SOURCE_URL: &str = "https://github.com/OceanEilonwy/library";
