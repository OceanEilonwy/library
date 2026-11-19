// SPDX-License-Identifier: AGPL-3.0-or-later
//! User settings, kept as JSON in the OS config directory.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{AppError, Result};
use crate::model::{ReadMode, SampleOffset};

/// What the Settings screen edits.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Read offset typed by the user; `None` uses the AccurateRip database.
    pub read_offset: Option<SampleOffset>,
    /// Secure or test-and-copy.
    pub read_mode: ReadMode,
    /// Keep a copy of each rip in this folder too.
    pub keep_local_copy: Option<PathBuf>,
    /// How far, in samples either way, to look for another pressing when a
    /// track does not match the databases at our own position; `None` for
    /// the default ([`crate::core::pressing::DEFAULT_MAX_SHIFT`], 2,939),
    /// `0` to not look. Not on the Settings screen; `pressingSearch` in
    /// `settings.json`.
    pub pressing_search: Option<u32>,
}

/// The config directory (`~/.config/library-rip` on Linux).
pub fn config_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("org", "The Library", "library-rip")
        .map(|dirs| dirs.config_dir().to_owned())
}

impl Settings {
    /// The pressing search window in samples.
    pub fn pressing_search(&self) -> u32 {
        self.pressing_search
            .unwrap_or(crate::core::pressing::DEFAULT_MAX_SHIFT)
    }

    /// Loads settings from `path`; a missing file gives the defaults.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|error| AppError::Settings {
                path: path.to_owned(),
                reason: error.to_string(),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(error) => Err(AppError::Settings {
                path: path.to_owned(),
                reason: error.to_string(),
            }),
        }
    }

    /// Saves settings to `path`, creating the folder.
    pub fn save(&self, path: &Path) -> Result<()> {
        let fail = |reason: String| AppError::Settings {
            path: path.to_owned(),
            reason,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| fail(e.to_string()))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| fail(e.to_string()))?;
        std::fs::write(path, text).map_err(|e| fail(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_default_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/settings.json");
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());
        let settings = Settings {
            read_offset: Some(SampleOffset(6)),
            read_mode: ReadMode::TestAndCopy,
            keep_local_copy: None,
            pressing_search: Some(100),
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), settings);
    }
}
