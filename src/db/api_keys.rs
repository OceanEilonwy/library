// SPDX-License-Identifier: AGPL-3.0-or-later
//! API keys for Library Rip (docs/rip.md): `lrk_` plus 32 random bytes in
//! base64url. Only the key's SHA-256 is stored, so a stolen database gives
//! nobody a working key. Keys are revoked rather than deleted, so the desk
//! can say which computer was disconnected.

use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension, Row, params};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::now;
use crate::util;

/// Every key starts with this, so it is easy to recognise (and to find if
/// one is ever pasted somewhere it shouldn't be).
pub const PREFIX: &str = "lrk_";

/// The longest key name kept.
const MAX_NAME: usize = 60;

/// A stored key (never the secret itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKey {
    pub id: i64,
    pub name: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

impl ApiKey {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            name: r.get(1)?,
            created_at: r.get(2)?,
            last_used_at: r.get(3)?,
            revoked_at: r.get(4)?,
        })
    }

    /// Still allowed in?
    pub fn active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

const COLS: &str = "id, name, created_at, last_used_at, revoked_at";

fn digest(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

/// Tidies a computer name for storing and showing.
pub fn clean_name(raw: &str) -> String {
    let spaced = raw
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    match util::clean(&spaced, MAX_NAME) {
        n if n.is_empty() => "Library Rip".to_owned(),
        n => n,
    }
}

/// Makes a key; returns its row and the secret, which is shown only once.
pub fn create(conn: &Connection, name: &str) -> Result<(ApiKey, String)> {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let secret = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes));
    conn.execute(
        "INSERT INTO api_keys (name, key_hash, created_at) VALUES (?1, ?2, ?3)",
        params![clean_name(name), digest(&secret).as_slice(), now()],
    )?;
    let key =
        get(conn, conn.last_insert_rowid())?.ok_or_else(|| anyhow::anyhow!("api key vanished"))?;
    Ok((key, secret))
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<ApiKey>> {
    Ok(conn
        .prepare_cached(&format!("SELECT {COLS} FROM api_keys WHERE id = ?1"))?
        .query_row([id], ApiKey::from_row)
        .optional()?)
}

/// The active key a secret belongs to. The lookup is by SHA-256 (so how
/// long it takes says nothing about the secret), and the stored digest is
/// compared again in constant time.
pub fn for_secret(conn: &Connection, secret: &str) -> Result<Option<ApiKey>> {
    let looks_right = secret.starts_with(PREFIX)
        && (PREFIX.len() + 20..=PREFIX.len() + 100).contains(&secret.len());
    if !looks_right {
        return Ok(None);
    }
    let wanted = digest(secret);
    let found = conn
        .prepare_cached(&format!(
            "SELECT {COLS}, key_hash FROM api_keys WHERE key_hash = ?1"
        ))?
        .query_row([wanted.as_slice()], |r| {
            Ok((ApiKey::from_row(r)?, r.get::<_, Vec<u8>>(5)?))
        })
        .optional()?;
    Ok(found
        .filter(|(_, hash)| bool::from(hash.as_slice().ct_eq(wanted.as_slice())))
        .map(|(key, _)| key)
        .filter(ApiKey::active))
}

/// Records that a key was used now.
pub fn touch(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE api_keys SET last_used_at = ?2 WHERE id = ?1",
        params![id, now()],
    )?;
    Ok(())
}

/// Revokes a key. False if there is no such key or it was already revoked.
pub fn revoke(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE api_keys SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
        params![id, now()],
    )? > 0)
}

/// Keys still connected, newest first.
pub fn active(conn: &Connection) -> Result<Vec<ApiKey>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLS} FROM api_keys WHERE revoked_at IS NULL ORDER BY created_at DESC, id DESC"
    ))?;
    let rows = stmt.query_map([], ApiKey::from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testing::temp_db;

    #[test]
    fn create_find_touch_revoke() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let (key, secret) = create(c, "  Henry's   desktop ")?;
            assert!(secret.starts_with("lrk_"));
            assert_eq!(secret.len(), 4 + 43, "32 bytes of base64url");
            assert_eq!(key.name, "Henry's desktop");
            assert_eq!(key.last_used_at, None);
            let stored: Vec<u8> = c.query_row("SELECT key_hash FROM api_keys", [], |r| r.get(0))?;
            assert_eq!(stored.len(), 32);
            assert!(
                !stored.windows(4).any(|w| w == b"lrk_"),
                "the secret is not stored"
            );

            assert_eq!(for_secret(c, &secret)?.map(|k| k.id), Some(key.id));
            assert_eq!(for_secret(c, "lrk_wrongwrongwrongwrongwrong")?, None);
            assert_eq!(for_secret(c, "")?, None);
            assert_eq!(for_secret(c, &secret[4..])?, None, "prefix required");

            touch(c, key.id)?;
            assert!(get(c, key.id)?.unwrap().last_used_at.is_some());
            assert_eq!(active(c)?.len(), 1);

            assert!(revoke(c, key.id)?);
            assert!(!revoke(c, key.id)?, "only once");
            assert_eq!(for_secret(c, &secret)?, None, "revoked keys stop working");
            assert!(active(c)?.is_empty());
            assert!(!get(c, key.id)?.unwrap().active());

            let (blank, _) = create(c, "   ")?;
            assert_eq!(blank.name, "Library Rip");
            Ok(())
        })
        .unwrap();
    }
}
