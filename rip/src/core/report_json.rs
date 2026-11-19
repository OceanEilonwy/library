// SPDX-License-Identifier: AGPL-3.0-or-later
//! The machine-readable `rip.json`.
//!
//! The file is the [`RipReport`] (camelCase keys, in field order) with two
//! extra keys first: `"format": "library-rip/rip-report"` and
//! `"formatVersion": 2`. The version goes up only when a reader of the old
//! format would misread the new one; adding fields does not change it.
//!
//! - **Version 1:** the first format.
//! - **Version 2:** an accurate track may have matched *another pressing*
//!   (`accuraterip.pressingOffset`, likewise `ctdb.pressingOffset`), which a
//!   version 1 reader would take for a match with this pressing; the disc
//!   `verdict`; and TOC entries carry their `session`, with an enhanced
//!   CD's last audio track ending at the audio session's end instead of at
//!   the data track. Version 1 files still parse: the missing fields read
//!   as `+0` and session 1, and the verdict is worked out from the tracks.

use serde::{Deserialize, Serialize};

use super::{CoreError, Result};
use crate::model::{DiscVerdict, RipReport};

/// File name used for the report.
pub const FILE_NAME: &str = "rip.json";

/// The `format` value identifying a rip report.
pub const FORMAT: &str = "library-rip/rip-report";

/// The current `formatVersion`.
pub const FORMAT_VERSION: u32 = 2;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope<'a> {
    format: &'a str,
    format_version: u32,
    #[serde(flatten)]
    report: &'a RipReport,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OwnedEnvelope {
    format: String,
    format_version: u32,
    #[serde(flatten)]
    report: RipReport,
}

/// Serialises the report as pretty JSON (camelCase keys, stable order),
/// ending with a newline.
pub fn render(report: &RipReport) -> Result<String> {
    let mut text = serde_json::to_string_pretty(&Envelope {
        format: FORMAT,
        format_version: FORMAT_VERSION,
        report,
    })?;
    text.push('\n');
    Ok(text)
}

/// Parses a report written by [`render`] (this or an earlier format version).
pub fn parse(text: &str) -> Result<RipReport> {
    let envelope: OwnedEnvelope = serde_json::from_str(text)?;
    if envelope.format != FORMAT {
        return Err(CoreError::Invalid(format!(
            "this is not a Library Rip report (format \"{}\")",
            envelope.format
        )));
    }
    if envelope.format_version > FORMAT_VERSION {
        return Err(CoreError::Invalid(format!(
            "this rip report was written by a newer Library Rip (format version {}); please update",
            envelope.format_version
        )));
    }
    let mut report = envelope.report;
    if envelope.format_version < 2 {
        report.verdict = DiscVerdict::from_results(&report.tracks, &report.ctdb);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::log::tests::sample_report;

    #[test]
    fn round_trips_and_is_versioned() {
        let report = sample_report();
        let text = render(&report).unwrap();
        assert!(text.starts_with("{\n  \"format\": \"library-rip/rip-report\",\n  \"formatVersion\": 2,\n  \"appVersion\""));
        assert!(text.ends_with("}\n"));
        assert_eq!(parse(&text).unwrap(), report);
        // Deterministic.
        assert_eq!(render(&report).unwrap(), text);
    }

    #[test]
    fn refuses_other_formats_and_newer_versions() {
        let text = render(&sample_report()).unwrap();
        let newer = text.replace("\"formatVersion\": 2", "\"formatVersion\": 3");
        assert!(matches!(parse(&newer), Err(CoreError::Invalid(_))));
        let other = text.replace("library-rip/rip-report", "something-else");
        assert!(matches!(parse(&other), Err(CoreError::Invalid(_))));
        assert!(parse("{}").is_err());
    }

    #[test]
    fn version_one_reports_still_parse() {
        // A version 1 report: no sessions, no pressing offsets, no verdict.
        let report = sample_report();
        let mut value: serde_json::Value = serde_json::from_str(&render(&report).unwrap()).unwrap();
        value["formatVersion"] = 1.into();
        let object = value.as_object_mut().unwrap();
        object.remove("verdict");
        for track in object["toc"]["tracks"].as_array_mut().unwrap() {
            track.as_object_mut().unwrap().remove("session");
        }
        for track in object["tracks"].as_array_mut().unwrap() {
            track["accuraterip"]
                .as_object_mut()
                .unwrap()
                .remove("pressingOffset");
        }
        object["ctdb"]
            .as_object_mut()
            .unwrap()
            .remove("pressingOffset");
        let old = serde_json::to_string(&value).unwrap();
        assert!(!old.contains("pressingOffset") && !old.contains("session"));
        let parsed = parse(&old).unwrap();
        assert_eq!(
            parsed, report,
            "missing fields read as +0, session 1, verdict worked out"
        );
    }

    #[test]
    fn key_fields_are_camel_case() {
        let text = render(&sample_report()).unwrap();
        for key in [
            "\"readOffset\"",
            "\"offsetSource\"",
            "\"discIds\"",
            "\"copyCrc\"",
            "\"accurateripCrcs\"",
            "\"unrecoveredSectors\"",
            "\"pressingOffset\"",
            "\"verdict\"",
            "\"session\"",
        ] {
            assert!(text.contains(key), "missing {key}");
        }
    }
}
