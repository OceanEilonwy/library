//! SQLite access: one writer connection, a small pool of readers (WAL mode
//! lets them run alongside the writer), all used from blocking threads.

pub mod accounts;
pub mod api_keys;
pub mod favourites;
pub mod items;
pub mod loans;
pub mod locations;
pub mod media_files;
pub mod photos;
pub mod playlists;
pub mod settings;
pub mod types;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Once};

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

const SCHEMA_V1: &str = include_str!("schema.sql");
const SCHEMA_V2: &str = include_str!("schema_v2.sql");
const SCHEMA_V3: &str = include_str!("schema_v3.sql");
const READER_POOL_MAX: usize = 8;

/// Registers sqlite-vec with every connection opened afterwards.
fn register_vec_extension() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        // SAFETY: sqlite3_vec_init has the signature SQLite expects of an
        // extension entry point; sqlite3_auto_extension only stores it.
        type Entry = unsafe extern "C" fn(
            *mut rusqlite::ffi::sqlite3,
            *mut *mut std::os::raw::c_char,
            *const rusqlite::ffi::sqlite3_api_routines,
        ) -> i32;
        let entry: Entry =
            std::mem::transmute::<*const (), Entry>(sqlite_vec::sqlite3_vec_init as *const ());
        rusqlite::ffi::sqlite3_auto_extension(Some(entry));
    });
}

#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
}

impl Db {
    /// Opens (creating if needed) the database and brings the schema up to date.
    pub fn open(path: &Path) -> Result<Self> {
        register_vec_extension();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let mut writer = open_connection(path, false)?;
        migrate(&mut writer)?;
        Ok(Self {
            inner: Arc::new(Inner {
                path: path.to_owned(),
                writer: Mutex::new(writer),
                readers: Mutex::new(Vec::new()),
            }),
        })
    }

    /// Runs `f` on a read-only connection on a blocking thread.
    pub async fn read<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.with_reader(f))
            .await
            .context("database task panicked")?
    }

    /// Runs `f` on the single writer connection on a blocking thread.
    pub async fn write<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = inner.writer.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut conn)
        })
        .await
        .context("database task panicked")?
    }

    /// Synchronous access to the writer, for the CLI and tests.
    pub fn write_blocking<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut conn = self.inner.writer.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut conn)
    }

    /// Synchronous access to a reader, for the CLI and tests.
    pub fn read_blocking<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.inner.with_reader(f)
    }
}

impl Inner {
    fn with_reader<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = {
            let mut pool = self.readers.lock().unwrap_or_else(|e| e.into_inner());
            pool.pop()
        };
        let conn = match conn {
            Some(c) => c,
            None => open_connection(&self.path, true)?,
        };
        let out = f(&conn);
        let mut pool = self.readers.lock().unwrap_or_else(|e| e.into_inner());
        if pool.len() < READER_POOL_MAX {
            pool.push(conn);
        }
        out
    }
}

fn open_connection(path: &Path, read_only: bool) -> Result<Connection> {
    let flags = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI
    };
    let conn = Connection::open_with_flags(path, flags)
        .with_context(|| format!("opening {}", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;
         PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = -16000;",
    )?;
    if !read_only {
        conn.pragma_update(None, "journal_mode", "WAL")?;
    }
    conn.set_prepared_statement_cache_capacity(64);
    Ok(conn)
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < 1 {
        let tx = conn.transaction()?;
        tx.execute_batch(SCHEMA_V1).context("applying schema v1")?;
        tx.pragma_update(None, "user_version", 1)?;
        tx.commit()?;
        tracing::info!("database schema created");
    }
    if version < 2 {
        let tx = conn.transaction()?;
        tx.execute_batch(SCHEMA_V2).context("applying schema v2")?;
        tx.pragma_update(None, "user_version", 2)?;
        tx.commit()?;
        tracing::info!("database schema upgraded to version 2 (accounts and digital copies)");
    }
    if version < 3 {
        // media_files is rebuilt: with foreign keys on, dropping the old
        // table would delete every subtitle, playlist entry and favourite
        // pointing at it. The pragma only takes effect outside a
        // transaction, so it is switched off around it.
        conn.pragma_update(None, "foreign_keys", false)?;
        let upgraded = (|| -> Result<()> {
            let tx = conn.transaction()?;
            tx.execute_batch(SCHEMA_V3).context("applying schema v3")?;
            let broken: i64 =
                tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                    r.get(0)
                })?;
            if broken > 0 {
                anyhow::bail!("schema v3 would leave {broken} broken references; not applied");
            }
            tx.pragma_update(None, "user_version", 3)?;
            tx.commit()?;
            Ok(())
        })();
        conn.pragma_update(None, "foreign_keys", true)?;
        upgraded?;
        tracing::info!("database schema upgraded to version 3 (Library Rip)");
    }
    Ok(())
}

/// Seconds since the Unix epoch.
pub fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

/// Ensures the vector table exists for the given embedding model and
/// dimension. If the model changed, the old vectors are dropped and every
/// item is marked for re-embedding.
pub fn ensure_vector_table(conn: &mut Connection, model: &str, dim: usize) -> Result<()> {
    let wanted = format!("{model}:{dim}");
    let current = settings::get(conn, "vector_model")?;
    let exists: bool = conn.query_row(
        "SELECT count(*) > 0 FROM sqlite_master WHERE name = 'item_vectors'",
        [],
        |r| r.get(0),
    )?;
    if exists && current.as_deref() == Some(wanted.as_str()) {
        return Ok(());
    }
    let tx = conn.transaction()?;
    tx.execute_batch("DROP TABLE IF EXISTS item_vectors")?;
    tx.execute_batch(&format!(
        "CREATE VIRTUAL TABLE item_vectors USING vec0(embedding float[{dim}] distance_metric=cosine)"
    ))?;
    tx.execute("UPDATE items SET embed_hash = NULL", [])?;
    settings::set(&tx, "vector_model", &wanted)?;
    tx.commit()?;
    tracing::info!(
        model,
        dim,
        "vector table (re)created; items queued for embedding"
    );
    Ok(())
}

pub fn vector_table_exists(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT count(*) > 0 FROM sqlite_master WHERE name = 'item_vectors'",
        [],
        |r| r.get(0),
    )?)
}

/// Copies the database to `dest` with VACUUM INTO (a consistent snapshot).
pub fn backup(conn: &Connection, dest: &Path) -> Result<()> {
    if dest.exists() {
        anyhow::bail!("{} already exists", dest.display());
    }
    conn.execute("VACUUM INTO ?1", [dest.to_string_lossy()])?;
    Ok(())
}

#[cfg(test)]
pub mod testing {
    use super::*;

    /// A fresh database in a temporary directory (kept alive by the guard).
    pub fn temp_db() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Db::open(&dir.path().join("test.sqlite3")).expect("open db");
        (db, dir)
    }
}

#[cfg(test)]
mod tests {
    use super::testing::temp_db;
    use super::*;

    #[test]
    fn schema_applies_and_seeds_types() {
        let (db, _dir) = temp_db();
        let n: i64 = db
            .read_blocking(|c| {
                Ok(c.query_row("SELECT count(*) FROM media_types", [], |r| r.get(0))?)
            })
            .unwrap();
        assert_eq!(n, 6);
    }

    #[test]
    fn reopening_does_not_reapply_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.sqlite3");
        Db::open(&path).unwrap();
        let db = Db::open(&path).unwrap();
        let v: i64 = db
            .read_blocking(|c| Ok(c.query_row("PRAGMA user_version", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(v, 3);
    }

    #[test]
    fn version_1_database_upgrades_and_keeps_its_items() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.sqlite3");
        {
            // A database as version 1 left it, with an item in it.
            register_vec_extension();
            let mut c = open_connection(&path, false).unwrap();
            let tx = c.transaction().unwrap();
            tx.execute_batch(SCHEMA_V1).unwrap();
            tx.pragma_update(None, "user_version", 1).unwrap();
            tx.execute(
                "INSERT INTO items (type_id, title, created_at, updated_at) VALUES (1, 'Blue', 1, 1)",
                [],
            )
            .unwrap();
            tx.commit().unwrap();
        }
        let db = Db::open(&path).unwrap();
        let (v, restricted, tables): (i64, i64, i64) = db
            .read_blocking(|c| {
                Ok((
                    c.query_row("PRAGMA user_version", [], |r| r.get(0))?,
                    c.query_row(
                        "SELECT restricted FROM items WHERE title = 'Blue'",
                        [],
                        |r| r.get(0),
                    )?,
                    c.query_row(
                        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name IN
                         ('accounts', 'account_sessions', 'media_files', 'subtitles',
                          'playlists', 'playlist_entries', 'favourites')",
                        [],
                        |r| r.get(0),
                    )?,
                ))
            })
            .unwrap();
        assert_eq!(v, 3);
        assert_eq!(restricted, 0, "existing items are not restricted");
        assert_eq!(tables, 7);
    }

    #[test]
    fn version_2_database_upgrades_and_keeps_its_files_and_links() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v2.sqlite3");
        {
            // A database as version 2 left it: an item with a file that has
            // subtitles, a favourite and a playlist entry.
            register_vec_extension();
            let mut c = open_connection(&path, false).unwrap();
            let tx = c.transaction().unwrap();
            tx.execute_batch(SCHEMA_V1).unwrap();
            tx.execute_batch(SCHEMA_V2).unwrap();
            tx.pragma_update(None, "user_version", 2).unwrap();
            tx.execute_batch(
                "INSERT INTO items (id, type_id, title, created_at, updated_at) VALUES (1, 1, 'Blue', 1, 1);
                 INSERT INTO media_files (id, item_id, kind, title, disc, track, file, original_name,
                                          mime, size_bytes, duration_ms, position, created_at)
                     VALUES (5, 1, 'video', 'Blue', 1, NULL, 'a.mp4', 'Blue.mp4', 'video/mp4', 10, 1000, 0, 1);
                 INSERT INTO subtitles (media_file_id, language, label, file, created_at)
                     VALUES (5, 'en', 'English', 'a.vtt', 1);
                 INSERT INTO accounts (id, username, email, password_hash, status, created_at)
                     VALUES (1, 'a', 'a@example.test', 'x', 'approved', 1);
                 INSERT INTO favourites (account_id, media_file_id, created_at) VALUES (1, 5, 1);
                 INSERT INTO playlists (id, account_id, name, created_at, updated_at) VALUES (1, 1, 'P', 1, 1);
                 INSERT INTO playlist_entries (playlist_id, media_file_id, position) VALUES (1, 5, 0);",
            )
            .unwrap();
            tx.commit().unwrap();
        }
        let db = Db::open(&path).unwrap();
        let counts = db
            .write_blocking(|c| {
                let one = |sql: &str| -> Result<i64> { Ok(c.query_row(sql, [], |r| r.get(0))?) };
                let counts = [
                    one("PRAGMA user_version")?,
                    one("SELECT count(*) FROM media_files WHERE id = 5 AND sha256 IS NULL")?,
                    one("SELECT count(*) FROM subtitles")?,
                    one("SELECT count(*) FROM favourites")?,
                    one("SELECT count(*) FROM playlist_entries")?,
                    one("SELECT count(*) FROM api_keys")?,
                    one("PRAGMA foreign_keys")?,
                ];
                // The new kind is accepted, and deleting still cascades.
                c.execute(
                    "INSERT INTO media_files (item_id, kind, title, file, original_name, mime, size_bytes, created_at)
                     VALUES (1, 'archive', 'Rip log', 'b.log', 'b.log', 'text/plain', 1, 1)",
                    [],
                )?;
                c.execute("DELETE FROM media_files WHERE id = 5", [])?;
                let after = one("SELECT count(*) FROM subtitles")? + one("SELECT count(*) FROM favourites")?;
                Ok((counts, after))
            })
            .unwrap();
        assert_eq!(counts.0, [3, 1, 1, 1, 1, 0, 1]);
        assert_eq!(counts.1, 0, "foreign keys still cascade after the rebuild");
    }

    #[test]
    fn vector_table_is_recreated_when_model_changes() {
        let (db, _dir) = temp_db();
        db.write_blocking(|c| {
            ensure_vector_table(c, "a", 4)?;
            c.execute(
                "INSERT INTO item_vectors (rowid, embedding) VALUES (1, ?1)",
                [serde_json::to_string(&[0.1f32, 0.2, 0.3, 0.4])?],
            )?;
            ensure_vector_table(c, "a", 4)?;
            let n: i64 = c.query_row("SELECT count(*) FROM item_vectors", [], |r| r.get(0))?;
            assert_eq!(n, 1, "same model keeps vectors");
            ensure_vector_table(c, "b", 8)?;
            let n: i64 = c.query_row("SELECT count(*) FROM item_vectors", [], |r| r.get(0))?;
            assert_eq!(n, 0, "new model starts empty");
            Ok(())
        })
        .unwrap();
    }

    #[tokio::test]
    async fn async_read_and_write() {
        let (db, _dir) = temp_db();
        db.write(|c| {
            settings::set(c, "k", "v")?;
            Ok(())
        })
        .await
        .unwrap();
        let v = db.read(|c| settings::get(c, "k")).await.unwrap();
        assert_eq!(v.as_deref(), Some("v"));
    }
}
