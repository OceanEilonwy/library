// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where the connection is kept: the OS keyring (`keyring` crate) for the
//! key, falling back to a 0600 file in the config directory.
//!
//! The keyring entry uses service [`KEYRING_SERVICE`] and the library's
//! address as the account. Every call here is blocking (the keyring talks
//! to the OS); a GUI runs them off the UI thread.

use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::api::{ApiKey, TokenResponse};
use super::{LibraryError, Result};

/// The keyring service name.
pub const KEYRING_SERVICE: &str = "library-rip";
/// File (in the config directory) with the connection details beside a
/// keyring-held key.
pub const DETAILS_FILE: &str = "connection.json";
/// File (in the config directory) holding the whole connection, key
/// included, when the keyring is unavailable.
pub const FALLBACK_FILE: &str = "connection-key.json";

/// A saved connection to one library.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredConnection {
    /// Library base address.
    pub library_url: url::Url,
    /// Library name.
    pub library_name: String,
    /// Key id on the server.
    pub key_id: i64,
    /// Key name (computer name).
    pub key_name: String,
    /// The key itself.
    pub api_key: ApiKey,
}

impl StoredConnection {
    /// The connection to keep after a successful handshake with the
    /// library at `library_url` (the address the user typed, which is
    /// what the app talks to even if the library names another).
    pub fn from_token(library_url: url::Url, token: TokenResponse) -> Self {
        StoredConnection {
            library_url,
            library_name: token.library.name,
            key_id: token.key_id,
            key_name: token.key_name,
            api_key: token.api_key,
        }
    }
}

/// Saves, loads and forgets the connection.
pub trait KeyStore: Send + Sync {
    /// The saved connection, if any.
    fn load(&self) -> Result<Option<StoredConnection>>;
    /// Saves (replacing any earlier one).
    fn save(&self, connection: &StoredConnection) -> Result<()>;
    /// Forgets it.
    fn clear(&self) -> Result<()>;
}

/// The store the app uses: the OS keyring, or a 0600 file in
/// `config_dir` when the keyring is unavailable (for example a Linux
/// desktop without a Secret Service).
pub fn default_store(config_dir: &Path) -> FallbackStore {
    FallbackStore {
        keyring: KeyringStore::new(KEYRING_SERVICE, config_dir.join(DETAILS_FILE)),
        file: FileStore::new(config_dir.join(FALLBACK_FILE)),
    }
}

// ---------------------------------------------------------------------------
// Keyring
// ---------------------------------------------------------------------------

/// Secret storage by (service, account); the OS keyring in the app.
trait SecretBackend: Send + Sync + fmt::Debug {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>>;
    fn set(&self, service: &str, account: &str, secret: &str) -> Result<()>;
    fn delete(&self, service: &str, account: &str) -> Result<()>;
}

/// The `keyring` crate.
#[derive(Debug)]
struct OsKeyring;

fn keyring_error(error: keyring::Error) -> LibraryError {
    LibraryError::KeyStore(format!("the system keyring said: {error}"))
}

impl SecretBackend for OsKeyring {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>> {
        let entry = keyring::Entry::new(service, account).map_err(keyring_error)?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(keyring_error(error)),
        }
    }

    fn set(&self, service: &str, account: &str, secret: &str) -> Result<()> {
        keyring::Entry::new(service, account)
            .and_then(|entry| entry.set_password(secret))
            .map_err(keyring_error)
    }

    fn delete(&self, service: &str, account: &str) -> Result<()> {
        let entry = keyring::Entry::new(service, account).map_err(keyring_error)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(keyring_error(error)),
        }
    }
}

/// The connection without its key (kept in a file beside the keyring).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionDetails {
    library_url: url::Url,
    library_name: String,
    key_id: i64,
    key_name: String,
}

/// The OS keyring (Secret Service, macOS Keychain, Windows Credential
/// Manager), with connection details beside it in a config file.
#[derive(Clone, Debug)]
pub struct KeyringStore {
    service: String,
    details_path: PathBuf,
    backend: Arc<dyn SecretBackend>,
}

impl KeyringStore {
    /// A store under `service` with details at `details_path`.
    pub fn new(service: &str, details_path: PathBuf) -> Self {
        KeyringStore {
            service: service.to_owned(),
            details_path,
            backend: Arc::new(OsKeyring),
        }
    }

    fn details(&self) -> Result<Option<ConnectionDetails>> {
        read_json_file(&self.details_path)
    }
}

impl KeyStore for KeyringStore {
    fn load(&self) -> Result<Option<StoredConnection>> {
        let Some(details) = self.details()? else {
            return Ok(None);
        };
        let Some(secret) = self
            .backend
            .get(&self.service, details.library_url.as_str())?
        else {
            return Ok(None);
        };
        Ok(Some(StoredConnection {
            api_key: ApiKey::new(&secret)?,
            library_url: details.library_url,
            library_name: details.library_name,
            key_id: details.key_id,
            key_name: details.key_name,
        }))
    }

    fn save(&self, connection: &StoredConnection) -> Result<()> {
        let account = connection.library_url.as_str();
        // Forget a key kept for a different library.
        if let Ok(Some(old)) = self.details()
            && old.library_url.as_str() != account
        {
            self.backend
                .delete(&self.service, old.library_url.as_str())?;
        }
        self.backend
            .set(&self.service, account, connection.api_key.expose())?;
        let details = ConnectionDetails {
            library_url: connection.library_url.clone(),
            library_name: connection.library_name.clone(),
            key_id: connection.key_id,
            key_name: connection.key_name.clone(),
        };
        write_private_json(&self.details_path, &details)
    }

    fn clear(&self) -> Result<()> {
        if let Some(details) = self.details().ok().flatten() {
            self.backend
                .delete(&self.service, details.library_url.as_str())?;
        }
        remove_file(&self.details_path)
    }
}

// ---------------------------------------------------------------------------
// File
// ---------------------------------------------------------------------------

/// A JSON file with 0600 permissions (fallback, and used by tests).
#[derive(Clone, Debug)]
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    /// A store at `path`.
    pub fn new(path: PathBuf) -> Self {
        FileStore { path }
    }
}

impl KeyStore for FileStore {
    fn load(&self) -> Result<Option<StoredConnection>> {
        read_json_file(&self.path)
    }

    fn save(&self, connection: &StoredConnection) -> Result<()> {
        write_private_json(&self.path, connection)
    }

    fn clear(&self) -> Result<()> {
        remove_file(&self.path)
    }
}

// ---------------------------------------------------------------------------
// Keyring with file fallback
// ---------------------------------------------------------------------------

/// The keyring when it works, otherwise a 0600 file (see [`default_store`]).
#[derive(Clone, Debug)]
pub struct FallbackStore {
    keyring: KeyringStore,
    file: FileStore,
}

impl KeyStore for FallbackStore {
    fn load(&self) -> Result<Option<StoredConnection>> {
        // The file exists only when the keyring failed at save time.
        if let Some(connection) = self.file.load()? {
            return Ok(Some(connection));
        }
        match self.keyring.load() {
            Ok(connection) => Ok(connection),
            Err(LibraryError::KeyStore(reason)) => {
                tracing::warn!(%reason, "the system keyring is unavailable");
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn save(&self, connection: &StoredConnection) -> Result<()> {
        match self.keyring.save(connection) {
            Ok(()) => self.file.clear(),
            Err(LibraryError::KeyStore(reason)) => {
                tracing::warn!(%reason, "keeping the key in a private file instead");
                self.file.save(connection)
            }
            Err(error) => Err(error),
        }
    }

    fn clear(&self) -> Result<()> {
        let keyring = self.keyring.clear();
        self.file.clear()?;
        match keyring {
            Err(LibraryError::KeyStore(reason)) => {
                tracing::warn!(%reason, "the system keyring is unavailable");
                // The details file must go even when the keyring is down.
                remove_file(&self.keyring.details_path)
            }
            other => other,
        }
    }
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> LibraryError + '_ {
    move |source| LibraryError::Io {
        path: path.to_owned(),
        source,
    }
}

/// Reads JSON from `path`; `None` when there is no file.
fn read_json_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(path)(error)),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| LibraryError::KeyStore(format!("{} is damaged: {error}", path.display())))
}

/// Writes JSON to `path` readable by this user only (0600 on Unix),
/// replacing it atomically.
fn write_private_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_error(parent))?;
    }
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| LibraryError::KeyStore(error.to_string()))?;
    let temporary = path.with_extension("tmp");
    // A leftover temporary file might have looser permissions.
    remove_file(&temporary)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(io_error(&temporary))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(io_error(&temporary))?;
    drop(file);
    std::fs::rename(&temporary, path).map_err(io_error(path))
}

/// Removes `path` if it exists.
fn remove_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(path)(error)),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;

    /// An in-memory keyring.
    #[derive(Debug, Default)]
    struct MemoryKeyring(Mutex<BTreeMap<(String, String), String>>);

    impl SecretBackend for MemoryKeyring {
        fn get(&self, service: &str, account: &str) -> Result<Option<String>> {
            let map = self.0.lock().unwrap();
            Ok(map.get(&(service.into(), account.into())).cloned())
        }
        fn set(&self, service: &str, account: &str, secret: &str) -> Result<()> {
            let mut map = self.0.lock().unwrap();
            map.insert((service.into(), account.into()), secret.into());
            Ok(())
        }
        fn delete(&self, service: &str, account: &str) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .remove(&(service.into(), account.into()));
            Ok(())
        }
    }

    /// A keyring that is never available.
    #[derive(Debug)]
    struct BrokenKeyring;

    impl SecretBackend for BrokenKeyring {
        fn get(&self, _: &str, _: &str) -> Result<Option<String>> {
            Err(LibraryError::KeyStore("no keyring".into()))
        }
        fn set(&self, _: &str, _: &str, _: &str) -> Result<()> {
            Err(LibraryError::KeyStore("no keyring".into()))
        }
        fn delete(&self, _: &str, _: &str) -> Result<()> {
            Err(LibraryError::KeyStore("no keyring".into()))
        }
    }

    fn connection(url: &str) -> StoredConnection {
        StoredConnection {
            library_url: url::Url::parse(url).unwrap(),
            library_name: "The Library".into(),
            key_id: 7,
            key_name: "Henry's desktop".into(),
            api_key: ApiKey::new("lrk_secret").unwrap(),
        }
    }

    fn keyring_store(dir: &Path, backend: Arc<dyn SecretBackend>) -> KeyringStore {
        KeyringStore {
            service: KEYRING_SERVICE.into(),
            details_path: dir.join(DETAILS_FILE),
            backend,
        }
    }

    #[test]
    fn file_store_round_trips_privately() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path().join("nested/connection-key.json"));
        assert_eq!(store.load().unwrap(), None);
        store.save(&connection("http://library.local/")).unwrap();
        assert_eq!(
            store.load().unwrap(),
            Some(connection("http://library.local/"))
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.path().join("nested/connection-key.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        store.clear().unwrap();
        assert_eq!(store.load().unwrap(), None);
        store.clear().unwrap();
    }

    #[test]
    fn keyring_store_keeps_the_key_out_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(MemoryKeyring::default());
        let store = keyring_store(dir.path(), backend.clone());
        store.save(&connection("http://library.local/")).unwrap();
        let details = std::fs::read_to_string(dir.path().join(DETAILS_FILE)).unwrap();
        assert!(!details.contains("lrk_secret"));
        assert_eq!(
            backend
                .get(KEYRING_SERVICE, "http://library.local/")
                .unwrap()
                .as_deref(),
            Some("lrk_secret")
        );
        assert_eq!(
            store.load().unwrap(),
            Some(connection("http://library.local/"))
        );

        // Connecting to another library forgets the first key.
        store.save(&connection("http://other.local/")).unwrap();
        assert_eq!(
            backend
                .get(KEYRING_SERVICE, "http://library.local/")
                .unwrap(),
            None
        );

        store.clear().unwrap();
        assert_eq!(store.load().unwrap(), None);
        assert_eq!(
            backend.get(KEYRING_SERVICE, "http://other.local/").unwrap(),
            None
        );
    }

    #[test]
    fn fallback_uses_the_file_only_without_a_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let broken = FallbackStore {
            keyring: keyring_store(dir.path(), Arc::new(BrokenKeyring)),
            file: FileStore::new(dir.path().join(FALLBACK_FILE)),
        };
        assert_eq!(broken.load().unwrap(), None);
        broken.save(&connection("http://library.local/")).unwrap();
        assert!(dir.path().join(FALLBACK_FILE).exists());
        assert_eq!(
            broken.load().unwrap(),
            Some(connection("http://library.local/"))
        );
        broken.clear().unwrap();
        assert_eq!(broken.load().unwrap(), None);

        // A working keyring removes any old plain file.
        broken.file.save(&connection("http://old.local/")).unwrap();
        let working = FallbackStore {
            keyring: keyring_store(dir.path(), Arc::new(MemoryKeyring::default())),
            file: FileStore::new(dir.path().join(FALLBACK_FILE)),
        };
        working.save(&connection("http://library.local/")).unwrap();
        assert!(!dir.path().join(FALLBACK_FILE).exists());
        assert_eq!(
            working.load().unwrap(),
            Some(connection("http://library.local/"))
        );
        working.clear().unwrap();
        assert_eq!(working.load().unwrap(), None);
    }

    #[test]
    fn default_store_paths() {
        let store = default_store(Path::new("/config"));
        assert_eq!(
            store.keyring.details_path,
            Path::new("/config/connection.json")
        );
        assert_eq!(store.keyring.service, "library-rip");
    }
}
