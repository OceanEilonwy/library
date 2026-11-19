// SPDX-License-Identifier: AGPL-3.0-or-later
//! The machine-readable `rip.json`.
//!
//! The file is the [`RipReport`] (camelCase keys, in field order) with two
//! extra keys first: `"format": "library-rip/rip-report"` and
//! `"formatVersion": 1`. The version goes up only when a reader of the old
//! format would misread the new one; adding fields does not change it.

use serde::{Deserialize, Serialize};

use super::{CoreError, Result};
use crate::model::RipReport;

/// File name used for the report.
pub const FILE_NAME: &str = "rip.json";

/// The `format` value identifying a rip report.
pub const FORMAT: &str = "library-rip/rip-report";

/// The current `formatVersion`.
pub const FORMAT_VERSION: u32 = 1;

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
    Ok(envelope.report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::log::tests::sample_report;

    #[test]
    fn round_trips_and_is_versioned() {
        let report = sample_report();
        let text = render(&report).unwrap();
        assert!(text.starts_with("{\n  \"format\": \"library-rip/rip-report\",\n  \"formatVersion\": 1,\n  \"appVersion\""));
        assert!(text.ends_with("}\n"));
        assert_eq!(parse(&text).unwrap(), report);
        // Deterministic.
        assert_eq!(render(&report).unwrap(), text);
    }

    #[test]
    fn refuses_other_formats_and_newer_versions() {
        let text = render(&sample_report()).unwrap();
        let newer = text.replace("\"formatVersion\": 1", "\"formatVersion\": 2");
        assert!(matches!(parse(&newer), Err(CoreError::Invalid(_))));
        let other = text.replace("library-rip/rip-report", "something-else");
        assert!(matches!(parse(&other), Err(CoreError::Invalid(_))));
        assert!(parse("{}").is_err());
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
        ] {
            assert!(text.contains(key), "missing {key}");
        }
    }
}
