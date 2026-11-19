// SPDX-License-Identifier: AGPL-3.0-or-later
//! PKCE (RFC 7636) verifier/challenge and the random `state`.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// A PKCE pair: the secret verifier and its S256 challenge.
#[derive(Clone, PartialEq, Eq)]
pub struct Pkce {
    verifier: String,
    challenge: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce")
            .field("challenge", &self.challenge)
            .finish()
    }
}

impl Pkce {
    /// A fresh pair: 32 random bytes → 43-character base64url verifier.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        Self::from_verifier(URL_SAFE_NO_PAD.encode(bytes))
    }

    /// A pair for a known verifier (tests, RFC vectors).
    pub fn from_verifier(verifier: String) -> Self {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Pkce {
            verifier,
            challenge,
        }
    }

    /// The verifier, sent only to `/api/rip/token`.
    pub fn verifier(&self) -> &str {
        &self.verifier
    }

    /// The challenge, sent in the browser URL.
    pub fn challenge(&self) -> &str {
        &self.challenge
    }
}

/// A random `state` value (base64url of 16 bytes).
pub fn random_state() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc7636_appendix_b_vector() {
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(
            pkce.challenge(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_verifier_length_is_valid() {
        let pkce = Pkce::generate();
        assert!((43..=128).contains(&pkce.verifier().len()));
    }
}
