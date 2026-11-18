//! Runtime configuration, read from a TOML file.
//!
//! Everything has a sensible default except the admin password hash, which
//! must be set before anyone can edit the catalogue (see `library hash-password`).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

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
    /// Library title shown until it is changed on the settings page.
    pub default_title: String,
    /// Directory holding hand-drawn art (hero.png, margin-left.png, …).
    /// Defaults to `<data_dir>/art`.
    pub art_dir: Option<PathBuf>,
    pub semantic: SemanticConfig,
    pub lookup: LookupConfig,
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
            default_title: "The Library".into(),
            art_dir: None,
            semantic: SemanticConfig::default(),
            lookup: LookupConfig::default(),
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
            timeout_secs: 8,
            open_library_url: "https://openlibrary.org".into(),
            google_books_url: "https://www.googleapis.com/books/v1".into(),
            musicbrainz_url: "https://musicbrainz.org/ws/2".into(),
            tmdb_url: "https://api.themoviedb.org/3".into(),
            upc_url: "https://api.upcitemdb.com/prod/trial".into(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            tracing::warn!(path = %path.display(), "config file not found, using defaults");
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let mut cfg: Config = toml::from_str(text)?;
        if let Ok(hash) = std::env::var("LIBRARY_ADMIN_PASSWORD_HASH") {
            cfg.admin_password_hash = Some(hash);
        }
        Ok(cfg)
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("library.sqlite3")
    }

    pub fn photos_dir(&self) -> PathBuf {
        self.data_dir.join("photos")
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
        assert_eq!(cfg.lookup.timeout_secs, 8);
        assert_eq!(cfg.default_title, "Tallow Lane");
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::parse("typo_key = 1").is_err());
    }
}
