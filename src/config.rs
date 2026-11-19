//! Runtime configuration, read from a TOML file.
//!
//! Everything has a sensible default except the admin password hash, which
//! must be set before anyone can edit the catalogue (see `library hash-password`).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::db::accounts::Access;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Address to listen on. Keep it on localhost and put a reverse proxy
    /// with basic auth in front.
    pub listen: SocketAddr,
    /// Where the database, photos and model files live.
    pub data_dir: PathBuf,
    /// Argon2 PHC hash of the admin password (`library hash-password`).
    pub admin_password_hash: Option<String>,
    /// The admin password itself, hashed at startup. Only from the environment
    /// (`LIBRARY_ADMIN_PASSWORD`), for hosts that hand settings over that way.
    #[serde(skip)]
    pub admin_password: Option<String>,
    /// When set, every page asks for this password first (HTTP basic auth,
    /// any user name). For hosts without a reverse proxy doing that.
    pub site_password: Option<String>,
    /// Library title shown until it is changed on the settings page.
    pub default_title: String,
    /// Directory holding hand-drawn art (hero.png, margin-left.png, …).
    /// Defaults to `<data_dir>/art`.
    pub art_dir: Option<PathBuf>,
    pub semantic: SemanticConfig,
    pub lookup: LookupConfig,
    pub accounts: AccountsConfig,
    pub media: MediaConfig,
}

/// Player accounts.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AccountsConfig {
    /// Access level of accounts made from the Player (`child` or `adult`).
    pub default_access: Access,
    /// Let anyone make an account from the Player.
    pub open_registration: bool,
}

/// Digital copies.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MediaConfig {
    /// Largest single file the upload studio accepts, in megabytes.
    pub max_upload_mb: u64,
}

impl Default for AccountsConfig {
    fn default() -> Self {
        Self {
            default_access: Access::Child,
            open_registration: true,
        }
    }
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            max_upload_mb: 20_000,
        }
    }
}

impl MediaConfig {
    /// The upload limit in bytes.
    pub fn max_upload_bytes(&self) -> u64 {
        self.max_upload_mb.saturating_mul(1024 * 1024)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SemanticConfig {
    /// Turn meaning-based search on or off.
    pub enabled: bool,
    /// Hugging Face model repository (a BERT-family sentence embedder).
    pub model: String,
    /// Base URL files are downloaded from on first start.
    pub hub_url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LookupConfig {
    /// Send lookups to the internet at all.
    pub enabled: bool,
    /// MusicBrainz asks clients to identify themselves; a contact URL or
    /// address is appended to the user agent if given.
    pub contact: Option<String>,
    /// The Movie Database API read token (v4). Films are skipped without it.
    pub tmdb_token: Option<String>,
    /// Per-request timeout in seconds.
    pub timeout_secs: u64,
    pub open_library_url: String,
    pub google_books_url: String,
    pub musicbrainz_url: String,
    pub tmdb_url: String,
    pub upc_url: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8080".parse().expect("static address"),
            data_dir: PathBuf::from("data"),
            admin_password_hash: None,
            admin_password: None,
            site_password: None,
            default_title: "The Library".into(),
            art_dir: None,
            semantic: SemanticConfig::default(),
            lookup: LookupConfig::default(),
            accounts: AccountsConfig::default(),
            media: MediaConfig::default(),
        }
    }
}

impl Default for SemanticConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: "BAAI/bge-small-en-v1.5".into(),
            hub_url: "https://huggingface.co".into(),
        }
    }
}

impl Default for LookupConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            contact: None,
            tmdb_token: None,
            timeout_secs: 15,
            open_library_url: "https://openlibrary.org".into(),
            google_books_url: "https://www.googleapis.com/books/v1".into(),
            musicbrainz_url: "https://musicbrainz.org/ws/2".into(),
            tmdb_url: "https://api.themoviedb.org/3".into(),
            upc_url: "https://api.upcitemdb.com/prod/trial".into(),
        }
    }
}

impl Config {
    /// Reads the config file (defaults if it doesn't exist), then applies
    /// `LIBRARY_*` environment variables on top.
    pub fn load(path: &Path) -> Result<Self> {
        let mut cfg = if path.exists() {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            Self::parse(&text).with_context(|| format!("parsing {}", path.display()))?
        } else {
            tracing::info!(path = %path.display(), "no config file, using defaults");
            Self::default()
        };
        cfg.apply_env(|k| std::env::var(k).ok());
        Ok(cfg)
    }

    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// Overrides from the environment (empty values are ignored):
    /// `LIBRARY_ADMIN_PASSWORD_HASH`, `LIBRARY_ADMIN_PASSWORD`, `LIBRARY_SITE_PASSWORD`,
    /// `LIBRARY_TITLE`, `LIBRARY_TMDB_TOKEN`, `LIBRARY_LOOKUP_CONTACT`,
    /// `LIBRARY_SEMANTIC` and `LIBRARY_LOOKUPS` (on/off).
    pub fn apply_env(&mut self, get: impl Fn(&str) -> Option<String>) {
        let get = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        let flag = |v: String| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "off" | "false" | "no"
            )
        };
        if let Some(v) = get("LIBRARY_ADMIN_PASSWORD_HASH") {
            self.admin_password_hash = Some(v);
        }
        if let Some(v) = get("LIBRARY_ADMIN_PASSWORD") {
            self.admin_password = Some(v);
        }
        if let Some(v) = get("LIBRARY_SITE_PASSWORD") {
            self.site_password = Some(v);
        }
        if let Some(v) = get("LIBRARY_TITLE") {
            self.default_title = v;
        }
        if let Some(v) = get("LIBRARY_TMDB_TOKEN") {
            self.lookup.tmdb_token = Some(v);
        }
        if let Some(v) = get("LIBRARY_LOOKUP_CONTACT") {
            self.lookup.contact = Some(v);
        }
        if let Some(v) = get("LIBRARY_SEMANTIC") {
            self.semantic.enabled = flag(v);
        }
        if let Some(v) = get("LIBRARY_LOOKUPS") {
            self.lookup.enabled = flag(v);
        }
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("library.sqlite3")
    }

    pub fn photos_dir(&self) -> PathBuf {
        self.data_dir.join("photos")
    }

    /// Where digital copies (media files and subtitles) are stored.
    pub fn media_dir(&self) -> PathBuf {
        self.data_dir.join("media")
    }

    pub fn models_dir(&self) -> PathBuf {
        self.data_dir.join("models")
    }

    pub fn art_dir(&self) -> PathBuf {
        self.art_dir
            .clone()
            .unwrap_or_else(|| self.data_dir.join("art"))
    }

    pub fn secret_path(&self) -> PathBuf {
        self.data_dir.join("session.key")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_uses_defaults() {
        let cfg = Config::parse("").unwrap();
        assert_eq!(cfg.listen.port(), 8080);
        assert!(cfg.semantic.enabled);
        assert_eq!(cfg.db_path(), PathBuf::from("data/library.sqlite3"));
        assert_eq!(cfg.art_dir(), PathBuf::from("data/art"));
        assert_eq!(cfg.media_dir(), PathBuf::from("data/media"));
        assert_eq!(cfg.accounts.default_access, Access::Child);
        assert!(cfg.accounts.open_registration);
        assert_eq!(cfg.media.max_upload_mb, 20_000);
    }

    #[test]
    fn player_sections_parse() {
        let cfg = Config::parse(
            r#"
            [accounts]
            default_access = "adult"
            open_registration = false
            [media]
            max_upload_mb = 5
            "#,
        )
        .unwrap();
        assert_eq!(cfg.accounts.default_access, Access::Adult);
        assert!(!cfg.accounts.open_registration);
        assert_eq!(cfg.media.max_upload_bytes(), 5 * 1024 * 1024);
        assert!(Config::parse("[accounts]\ndefault_access = \"teen\"").is_err());
    }

    #[test]
    fn nested_sections_parse() {
        let cfg = Config::parse(
            r#"
            listen = "0.0.0.0:9000"
            data_dir = "/var/lib/library"
            default_title = "Tallow Lane"
            [semantic]
            enabled = false
            [lookup]
            tmdb_token = "abc"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.listen.port(), 9000);
        assert!(!cfg.semantic.enabled);
        assert_eq!(cfg.lookup.tmdb_token.as_deref(), Some("abc"));
        assert_eq!(cfg.lookup.timeout_secs, 15);
        assert_eq!(cfg.default_title, "Tallow Lane");
    }

    #[test]
    fn environment_overrides() {
        let mut cfg = Config::parse("site_password = \"from-file\"").unwrap();
        let env: std::collections::HashMap<&str, &str> = [
            ("LIBRARY_ADMIN_PASSWORD", "hunter22"),
            ("LIBRARY_TITLE", "Hollingworth Library"),
            ("LIBRARY_SEMANTIC", "off"),
            ("LIBRARY_LOOKUPS", "on"),
            ("LIBRARY_TMDB_TOKEN", " "),
        ]
        .into();
        cfg.apply_env(|k| env.get(k).map(|v| v.to_string()));
        assert_eq!(cfg.admin_password.as_deref(), Some("hunter22"));
        assert_eq!(
            cfg.site_password.as_deref(),
            Some("from-file"),
            "unset keeps the file's value"
        );
        assert_eq!(cfg.default_title, "Hollingworth Library");
        assert!(!cfg.semantic.enabled);
        assert!(cfg.lookup.enabled);
        assert_eq!(cfg.lookup.tmdb_token, None, "blank values are ignored");
        assert!(
            Config::parse("admin_password = \"x\"").is_err(),
            "plain passwords only from the environment"
        );
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::parse("typo_key = 1").is_err());
    }
}
