//! The admin password and the "unlocked" session cookie.
//!
//! There are no accounts. Entering the admin password sets a signed session
//! cookie (no expiry date, so the browser drops it when it closes; the server
//! also refuses cookies older than 30 days).

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use argon2::Argon2;
use argon2::password_hash::{
    PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng,
};
use axum::http::HeaderMap;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;

pub const COOKIE: &str = "library_admin";
const MAX_AGE_SECS: i64 = 30 * 24 * 3600;
const FAIL_WINDOW: Duration = Duration::from_secs(10 * 60);
const FAIL_LIMIT: usize = 10;

pub struct Auth {
    key: [u8; 32],
    password_hash: Option<String>,
    failures: Mutex<VecDeque<Instant>>,
}

pub enum Login {
    Ok(String),
    Wrong,
    TooMany,
    NotConfigured,
}

impl Auth {
    pub fn new(key: [u8; 32], password_hash: Option<String>) -> Self {
        Self {
            key,
            password_hash,
            failures: Mutex::new(VecDeque::new()),
        }
    }

    /// Loads the cookie-signing key from `path`, creating it on first run.
    pub fn load_or_create_key(path: &Path) -> Result<[u8; 32]> {
        if let Ok(text) = std::fs::read_to_string(path) {
            let bytes = hex_decode(text.trim()).context("session key file is not valid hex")?;
            if let Ok(key) = <[u8; 32]>::try_from(bytes.as_slice()) {
                return Ok(key);
            }
        }
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, hex_encode(&key))
            .with_context(|| format!("writing {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(key)
    }

    pub fn configured(&self) -> bool {
        self.password_hash.is_some()
    }

    /// Checks a password; on success returns the cookie value to set.
    /// Runs Argon2, so call it from a blocking thread.
    pub fn login(&self, password: &str) -> Login {
        let Some(hash) = &self.password_hash else {
            return Login::NotConfigured;
        };
        {
            let mut f = self.failures.lock().unwrap_or_else(|e| e.into_inner());
            while f.front().is_some_and(|t| t.elapsed() > FAIL_WINDOW) {
                f.pop_front();
            }
            if f.len() >= FAIL_LIMIT {
                return Login::TooMany;
            }
        }
        if verify_password(hash, password) {
            Login::Ok(self.issue(jiff::Timestamp::now().as_second()))
        } else {
            self.failures
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_back(Instant::now());
            Login::Wrong
        }
    }

    fn mac(&self, msg: &str) -> Vec<u8> {
        let mut m = <Hmac<Sha256>>::new_from_slice(&self.key).expect("any key length");
        m.update(msg.as_bytes());
        m.finalize().into_bytes().to_vec()
    }

    fn issue(&self, ts: i64) -> String {
        let msg = format!("admin:{ts}");
        format!("{ts}.{}", URL_SAFE_NO_PAD.encode(self.mac(&msg)))
    }

    pub fn verify_cookie(&self, value: &str, now: i64) -> bool {
        let Some((ts, sig)) = value.split_once('.') else {
            return false;
        };
        let Ok(ts) = ts.parse::<i64>() else {
            return false;
        };
        if ts > now + 60 || now - ts > MAX_AGE_SECS {
            return false;
        }
        let Ok(sig) = URL_SAFE_NO_PAD.decode(sig) else {
            return false;
        };
        let mut m = <Hmac<Sha256>>::new_from_slice(&self.key).expect("any key length");
        m.update(format!("admin:{ts}").as_bytes());
        m.verify_slice(&sig).is_ok()
    }

    /// True if the request carries a valid admin cookie.
    pub fn is_admin(&self, headers: &HeaderMap) -> bool {
        cookie(headers, COOKIE)
            .is_some_and(|v| self.verify_cookie(&v, jiff::Timestamp::now().as_second()))
    }
}

pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hashing failed: {e}"))?
        .to_string())
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| {
            Argon2::default()
                .verify_password(password.as_bytes(), &h)
                .is_ok()
        })
        .unwrap_or(false)
}

/// Reads one cookie from the request headers.
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
}

/// The Set-Cookie header value for an unlocked session.
pub fn session_cookie(value: &str, secure: bool) -> String {
    format!(
        "{COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict{}",
        if secure { "; Secure" } else { "" }
    )
}

/// Only allow redirects to paths on this site.
pub fn safe_next(next: &str) -> String {
    if next.starts_with('/')
        && !next.starts_with("//")
        && !next.starts_with("/\\")
        && !next.contains("://")
    {
        next.to_owned()
    } else {
        "/".to_owned()
    }
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth() -> Auth {
        Auth::new([7u8; 32], Some(hash_password("open sesame").unwrap()))
    }

    #[test]
    fn password_hash_round_trip() {
        let h = hash_password("pw").unwrap();
        assert!(verify_password(&h, "pw"));
        assert!(!verify_password(&h, "PW"));
        assert!(!verify_password("not a hash", "pw"));
    }

    #[test]
    fn login_issues_verifiable_cookie() {
        let a = auth();
        let Login::Ok(c) = a.login("open sesame") else {
            panic!("login failed")
        };
        let now = jiff::Timestamp::now().as_second();
        assert!(a.verify_cookie(&c, now));
        assert!(!a.verify_cookie(&c, now + MAX_AGE_SECS + 10), "expires");
        let other = Auth::new([8u8; 32], None);
        assert!(!other.verify_cookie(&c, now), "different key");
        let (ts, sig) = c.split_once('.').unwrap();
        let forged = format!("{}.{sig}", ts.parse::<i64>().unwrap() + 1);
        assert!(!a.verify_cookie(&forged, now), "timestamp is signed");
        assert!(!a.verify_cookie("garbage", now));
        assert!(!a.verify_cookie("1.%%%", now));
    }

    #[test]
    fn repeated_failures_are_throttled() {
        let a = auth();
        for _ in 0..FAIL_LIMIT {
            assert!(matches!(a.login("nope"), Login::Wrong));
        }
        assert!(matches!(a.login("open sesame"), Login::TooMany));
    }

    #[test]
    fn unconfigured_never_unlocks() {
        let a = Auth::new([1u8; 32], None);
        assert!(matches!(a.login(""), Login::NotConfigured));
    }

    #[test]
    fn key_file_is_created_then_reused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("k");
        let a = Auth::load_or_create_key(&p).unwrap();
        let b = Auth::load_or_create_key(&p).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn cookie_parsing_and_next() {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::COOKIE,
            "a=1; library_admin=xyz; b=2".parse().unwrap(),
        );
        assert_eq!(cookie(&h, COOKIE).as_deref(), Some("xyz"));
        assert_eq!(cookie(&h, "zzz"), None);
        assert_eq!(safe_next("/items/3"), "/items/3");
        assert_eq!(safe_next("//evil.com"), "/");
        assert_eq!(safe_next("https://evil.com"), "/");
        assert_eq!(safe_next("/\\evil.com"), "/");
        assert!(session_cookie("v", true).ends_with("; Secure"));
    }
}
