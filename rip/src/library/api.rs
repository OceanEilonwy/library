// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wire types of the rip API (docs/rip.md), camelCase JSON.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::LibraryError;
use crate::model::{MediaKind, Sha256Digest};

/// An API key (`lrk_…`). Never printed in full by `Debug`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ApiKey(String);

impl ApiKey {
    /// The prefix every key starts with.
    pub const PREFIX: &'static str = "lrk_";

    /// Validates the prefix and that the rest is non-empty base64url.
    pub fn new(key: &str) -> Result<Self, LibraryError> {
        let rest = key.strip_prefix(Self::PREFIX).unwrap_or_default();
        let valid = !rest.is_empty()
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if valid {
            Ok(ApiKey(key.to_owned()))
        } else {
            Err(LibraryError::KeyStore(
                "the stored key is not a Library Rip key".into(),
            ))
        }
    }

    /// The secret, for the `Authorization: Bearer` header and storage only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(lrk_…)")
    }
}

impl TryFrom<String> for ApiKey {
    type Error = LibraryError;
    fn try_from(key: String) -> Result<Self, LibraryError> {
        ApiKey::new(&key)
    }
}

impl From<ApiKey> for String {
    fn from(key: ApiKey) -> String {
        key.0
    }
}

/// `GET /api/rip/info`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryInfo {
    /// Library name, e.g. "The Library".
    pub name: String,
    /// Server version.
    pub version: String,
    /// Oldest ripper version the server accepts.
    pub min_ripper_version: String,
}

/// The library as named in token and whoami responses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryRef {
    /// Library name.
    pub name: String,
    /// Its public base URL.
    pub url: String,
}

/// `POST /api/rip/token` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "grantType")]
pub enum TokenRequest {
    /// Loopback flow.
    #[serde(rename = "authorization_code", rename_all = "camelCase")]
    AuthorizationCode {
        /// One-time code from the callback.
        code: String,
        /// PKCE verifier.
        code_verifier: String,
        /// Must equal the redirect URI used in `/connect`.
        redirect_uri: String,
    },
    /// Device-code flow.
    #[serde(rename = "device_code", rename_all = "camelCase")]
    DeviceCode {
        /// From `POST /api/rip/device`.
        device_code: String,
    },
}

/// `POST /api/rip/token` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenResponse {
    /// The new key.
    pub api_key: ApiKey,
    /// Its id.
    pub key_id: i64,
    /// Its name (the computer name).
    pub key_name: String,
    /// The library.
    pub library: LibraryRef,
}

/// `POST /api/rip/device` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRequest {
    /// Computer name shown to the librarian.
    pub name: String,
}

/// `POST /api/rip/device` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceResponse {
    /// Secret code to poll with.
    pub device_code: String,
    /// Code the librarian types, e.g. `WXYZ-1234`.
    pub user_code: String,
    /// Where the librarian types it.
    pub verification_uri: String,
    /// Seconds between polls.
    pub interval: u64,
    /// Seconds until the codes expire.
    pub expires_in: u64,
}

/// `GET /api/rip/whoami`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WhoAmI {
    /// Key id.
    pub key_id: i64,
    /// Key name.
    pub key_name: String,
    /// The library.
    pub library: LibraryRef,
}

/// One entry of `GET /api/rip/types`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemType {
    /// Type id.
    pub id: i64,
    /// Display name.
    pub name: String,
    /// Slug, e.g. `music`.
    pub slug: String,
    /// Lookup provider name.
    pub lookup: Option<String>,
}

/// One result of `GET /api/rip/items`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemSummary {
    /// Item id.
    pub id: i64,
    /// Title.
    pub title: String,
    /// Creator.
    pub creator: Option<String>,
    /// Year.
    pub year: Option<i64>,
    /// Identifier (barcode, ISBN…).
    pub identifier: Option<String>,
    /// Digital files already attached.
    pub file_count: i64,
    /// Link to the item page.
    pub url: String,
}

/// `POST /api/rip/items` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedItem {
    /// New item id.
    pub id: i64,
    /// Link to the item page.
    pub url: String,
}

/// `POST /api/rip/items/{id}/media` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedMedia {
    /// New media file id.
    pub id: i64,
}

/// One entry of `GET /api/rip/items/{id}/media`: a file already on an
/// item. `sha256` is `None` for files added before checksums were kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExistingMedia {
    /// Media file id.
    pub id: i64,
    /// Its kind.
    pub kind: MediaKind,
    /// Its title.
    pub title: String,
    /// Disc number, when set.
    #[serde(default)]
    pub disc: Option<i64>,
    /// Track number, when set.
    #[serde(default)]
    pub track: Option<i64>,
    /// Its SHA-256, when known.
    #[serde(default)]
    pub sha256: Option<Sha256Digest>,
}

/// Error bodies (`{"error": "…"}`; 422 answers may add `fields`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Machine-readable code or message.
    pub error: String,
    /// Per-field messages on validation errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<std::collections::BTreeMap<String, String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_validates_and_hides() {
        let key = ApiKey::new("lrk_abc-DEF_123").unwrap();
        assert_eq!(format!("{key:?}"), "ApiKey(lrk_…)");
        assert!(ApiKey::new("abc").is_err());
        assert!(ApiKey::new("lrk_").is_err());
    }

    #[test]
    fn token_request_matches_the_contract() {
        let request = TokenRequest::AuthorizationCode {
            code: "c".into(),
            code_verifier: "v".into(),
            redirect_uri: "http://127.0.0.1:1/callback".into(),
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"grantType": "authorization_code", "code": "c",
                "codeVerifier": "v", "redirectUri": "http://127.0.0.1:1/callback"})
        );
        let device = serde_json::to_value(TokenRequest::DeviceCode {
            device_code: "d".into(),
        })
        .unwrap();
        assert_eq!(
            device,
            serde_json::json!({"grantType": "device_code", "deviceCode": "d"})
        );
    }
}
