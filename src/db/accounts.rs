//! Player accounts (username, email, password) and their login sessions.
//!
//! Passwords are hashed by the caller (Argon2 is slow, so it runs on a
//! blocking thread in the web layer); this module only stores the hash.
//! Session tokens are random; only their SHA-256 is stored.

use anyhow::Result;
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::items::Invalid;
use super::now;

/// What an account may see: children never get restricted items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    #[default]
    Child,
    Adult,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Access::Child => "child",
            Access::Adult => "adult",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "child" => Some(Access::Child),
            "adult" => Some(Access::Adult),
            _ => None,
        }
    }

    /// May this account see items marked restricted (adults only)?
    pub fn sees_restricted(self) -> bool {
        self == Access::Adult
    }

    pub fn label(self) -> &'static str {
        match self {
            Access::Child => "Child",
            Access::Adult => "Adult",
        }
    }
}

/// Whether a librarian has let an account in yet. Only approved accounts
/// can sign in or use a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    /// Registered from the Player, waiting for a librarian.
    #[default]
    Pending,
    /// Let in by a librarian (or made by one with the CLI).
    Approved,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Approved => "approved",
        }
    }

    /// Reads the stored word; anything unknown counts as pending (safe).
    pub fn parse(s: &str) -> Self {
        match s {
            "approved" => Status::Approved,
            _ => Status::Pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: i64,
    pub username: String,
    pub email: String,
    pub access: Access,
    pub status: Status,
    pub created_at: i64,
}

impl Account {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            username: r.get(1)?,
            email: r.get(2)?,
            access: Access::parse(&r.get::<_, String>(3)?).unwrap_or_default(),
            status: Status::parse(&r.get::<_, String>(4)?),
            created_at: r.get(5)?,
        })
    }

    /// Has a librarian let this account in?
    pub fn approved(&self) -> bool {
        self.status == Status::Approved
    }
}

const COLS: &str = "id, username, email, access, status, created_at";

/// Checks the registration rules; problems are keyed by field name.
pub fn validate(username: &str, email: &str, password: &str) -> Invalid {
    let mut bad = Invalid::default();
    let name_len = username.chars().count();
    if !(3..=32).contains(&name_len) {
        bad.add("username", "Use 3 to 32 characters.");
    } else if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        bad.add(
            "username",
            "Use only letters, numbers, dots, dashes and underscores.",
        );
    }
    let email = email.trim();
    if !email.contains('@') || email.chars().count() > 254 || email.contains(char::is_whitespace) {
        bad.add("email", "That doesn't look like an email address.");
    }
    if password.chars().count() < 8 {
        bad.add("password", "Use at least 8 characters.");
    } else if password.len() > 1024 {
        bad.add("password", "That password is too long.");
    }
    bad
}

/// Creates an account. Fails with [`Invalid`] if the details break the rules
/// or the username or email is already taken. Accounts registered from the
/// Player start [`Status::Pending`]; the CLI makes them approved.
pub fn create(
    conn: &Connection,
    username: &str,
    email: &str,
    password_hash: &str,
    access: Access,
    status: Status,
) -> Result<Account> {
    let username = username.trim();
    let email = email.trim();
    let mut bad = Invalid::default();
    let taken = |col: &str, v: &str| -> Result<bool> {
        Ok(conn.query_row(
            &format!("SELECT count(*) > 0 FROM accounts WHERE {col} = ?1"),
            [v],
            |r| r.get(0),
        )?)
    };
    if taken("username", username)? {
        bad.add("username", "That username is already taken.");
    }
    if taken("email", email)? {
        bad.add("email", "There is already an account with that email.");
    }
    if !bad.is_empty() {
        return Err(bad.into());
    }
    let ts = now();
    let approved_at = (status == Status::Approved).then_some(ts);
    conn.execute(
        "INSERT INTO accounts (username, email, password_hash, access, status, created_at, approved_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            username,
            email,
            password_hash,
            access.as_str(),
            status.as_str(),
            ts,
            approved_at
        ],
    )?;
    get(conn, conn.last_insert_rowid())?.ok_or_else(|| anyhow::anyhow!("account vanished"))
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<Account>> {
    Ok(conn
        .prepare_cached(&format!("SELECT {COLS} FROM accounts WHERE id = ?1"))?
        .query_row([id], Account::from_row)
        .optional()?)
}

pub fn by_username(conn: &Connection, username: &str) -> Result<Option<Account>> {
    Ok(conn
        .prepare_cached(&format!("SELECT {COLS} FROM accounts WHERE username = ?1"))?
        .query_row([username.trim()], Account::from_row)
        .optional()?)
}

/// The account a login name (username or email) refers to, with its hash.
pub fn for_login(conn: &Connection, login: &str) -> Result<Option<(Account, String)>> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT {COLS}, password_hash FROM accounts WHERE username = ?1 OR email = ?1
             ORDER BY username = ?1 DESC LIMIT 1"
        ))?
        .query_row([login.trim()], |r| Ok((Account::from_row(r)?, r.get(6)?)))
        .optional()?)
}

/// Every account, oldest first.
pub fn list(conn: &Connection) -> Result<Vec<Account>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLS} FROM accounts ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([], Account::from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// How many accounts are waiting for a librarian.
pub fn pending_count(conn: &Connection) -> Result<i64> {
    Ok(conn
        .prepare_cached("SELECT count(*) FROM accounts WHERE status = 'pending'")?
        .query_row([], |r| r.get(0))?)
}

/// Lets a pending account in with the given access. False if there is no
/// such account or it was already approved.
pub fn approve(conn: &Connection, id: i64, access: Access) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE accounts SET status = 'approved', access = ?2, approved_at = ?3
         WHERE id = ?1 AND status = 'pending'",
        params![id, access.as_str(), now()],
    )? > 0)
}

/// Turns down a pending account (deletes it). False if there is no such
/// account or it has already been approved.
pub fn reject(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute(
        "DELETE FROM accounts WHERE id = ?1 AND status = 'pending'",
        [id],
    )? > 0)
}

/// Changes an account's access level; false if there is no such account.
pub fn set_access(conn: &Connection, id: i64, access: Access) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE accounts SET access = ?2 WHERE id = ?1",
        params![id, access.as_str()],
    )? > 0)
}

/// Deletes an account with its sessions, playlists and favourites.
pub fn delete(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM accounts WHERE id = ?1", [id])? > 0)
}

// ---------- sessions ----------

fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// Starts a session; returns the token for the cookie.
pub fn create_session(conn: &Connection, account_id: i64) -> Result<String> {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let ts = now();
    conn.execute(
        "INSERT INTO account_sessions (token_hash, account_id, created_at, last_seen)
         VALUES (?1, ?2, ?3, ?3)",
        params![token_hash(&token), account_id, ts],
    )?;
    Ok(token)
}

/// The account a session token belongs to, and when it was last seen.
pub fn session(conn: &Connection, token: &str) -> Result<Option<(Account, i64)>> {
    if token.is_empty() || token.len() > 128 {
        return Ok(None);
    }
    Ok(conn
        .prepare_cached(
            "SELECT a.id, a.username, a.email, a.access, a.status, a.created_at, s.last_seen
             FROM account_sessions s JOIN accounts a ON a.id = s.account_id
             WHERE s.token_hash = ?1",
        )?
        .query_row([token_hash(token)], |r| {
            Ok((Account::from_row(r)?, r.get(6)?))
        })
        .optional()?)
}

/// Records that a session was used now.
pub fn touch_session(conn: &Connection, token: &str) -> Result<()> {
    conn.execute(
        "UPDATE account_sessions SET last_seen = ?2 WHERE token_hash = ?1",
        params![token_hash(token), now()],
    )?;
    Ok(())
}

/// Ends a session (logging out).
pub fn delete_session(conn: &Connection, token: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM account_sessions WHERE token_hash = ?1",
        [token_hash(token)],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testing::temp_db;

    #[test]
    fn registration_rules() {
        assert!(validate("alice", "alice@example.org", "long enough").is_empty());
        let bad = validate("al", "nope", "short");
        assert!(bad.get("username").is_some());
        assert!(bad.get("email").is_some());
        assert!(bad.get("password").is_some());
        assert!(
            validate("al ice", "a@b", "12345678")
                .get("username")
                .is_some()
        );
        assert!(validate("a.l-i_ce", "a@b", "12345678").is_empty());
    }

    #[test]
    fn create_find_and_sessions() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let a = create(
                c,
                "Alice",
                "alice@example.org",
                "hash",
                Access::Child,
                Status::Approved,
            )?;
            assert_eq!(a.access, Access::Child);
            assert!(a.approved());
            let err = create(
                c,
                "alice",
                "ALICE@example.org",
                "h",
                Access::Adult,
                Status::Pending,
            )
            .unwrap_err();
            let bad = err.downcast_ref::<Invalid>().expect("invalid");
            assert!(bad.get("username").is_some() && bad.get("email").is_some());
            assert_eq!(for_login(c, "ALICE")?.unwrap().0.id, a.id);
            assert_eq!(for_login(c, "alice@example.org")?.unwrap().1, "hash");
            assert!(for_login(c, "bob")?.is_none());

            let token = create_session(c, a.id)?;
            assert_eq!(token.len(), 64);
            assert_eq!(session(c, &token)?.unwrap().0.id, a.id);
            assert!(session(c, "nope")?.is_none());
            touch_session(c, &token)?;

            assert!(set_access(c, a.id, Access::Adult)?);
            assert_eq!(get(c, a.id)?.unwrap().access, Access::Adult);
            assert_eq!(list(c)?.len(), 1);

            delete_session(c, &token)?;
            assert!(session(c, &token)?.is_none());
            let again = create_session(c, a.id)?;
            assert!(delete(c, a.id)?);
            assert!(
                session(c, &again)?.is_none(),
                "sessions go with the account"
            );
            assert!(!delete(c, a.id)?);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn approving_and_rejecting() {
        let (db, _d) = temp_db();
        db.write_blocking(|c| {
            let p = create(
                c,
                "pat",
                "pat@example.org",
                "h",
                Access::Child,
                Status::Pending,
            )?;
            let q = create(
                c,
                "quin",
                "q@example.org",
                "h",
                Access::Child,
                Status::Pending,
            )?;
            assert!(!p.approved());
            assert_eq!(pending_count(c)?, 2);
            let approved_at: Option<i64> = c.query_row(
                "SELECT approved_at FROM accounts WHERE id = ?1",
                [p.id],
                |r| r.get(0),
            )?;
            assert!(approved_at.is_none());

            assert!(approve(c, p.id, Access::Adult)?);
            let p = get(c, p.id)?.unwrap();
            assert!(p.approved() && p.access == Access::Adult);
            assert!(!approve(c, p.id, Access::Child)?, "only once");
            assert!(!reject(c, p.id)?, "approved accounts aren't rejected");
            assert_eq!(pending_count(c)?, 1);

            assert!(reject(c, q.id)?);
            assert!(get(c, q.id)?.is_none());
            assert_eq!(pending_count(c)?, 0);
            assert!(Status::parse("nonsense") == Status::Pending);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn access_words() {
        assert_eq!(Access::parse("Adult"), Some(Access::Adult));
        assert_eq!(Access::parse("teen"), None);
        assert!(Access::Adult.sees_restricted());
        assert!(!Access::Child.sees_restricted());
    }
}
