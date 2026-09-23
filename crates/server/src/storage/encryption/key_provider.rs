use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::secret::FixedKey;

pub(crate) const ENV_KEY: &str = "GUARDIAN_STORAGE_ENCRYPTION_KEY";
pub(crate) const ENV_KEY_ID: &str = "GUARDIAN_STORAGE_ENCRYPTION_KEY_ID";
pub(crate) const ENV_KEY_FILE: &str = "GUARDIAN_STORAGE_ENCRYPTION_KEY_FILE";
pub(crate) const ENV_SECRET_ID: &str = "GUARDIAN_STORAGE_ENCRYPTION_KEY_SECRET_ID";
pub(crate) const DEFAULT_KID: &str = "k1";

#[derive(Debug)]
pub(crate) enum KeyProviderError {
    MultipleKeySources,
    InvalidKeyEncoding,
    InvalidKeyLength,
    MalformedSecret,
    UnknownKeyId(String),
    KeyStoreUnavailable(String),
    KeyFileAccessible(String),
}

impl fmt::Display for KeyProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyProviderError::MultipleKeySources => {
                write!(f, "more than one storage encryption key source configured")
            }
            KeyProviderError::InvalidKeyEncoding => {
                write!(f, "storage encryption key is not valid base64")
            }
            KeyProviderError::InvalidKeyLength => {
                write!(f, "storage encryption key must decode to exactly 32 bytes")
            }
            KeyProviderError::MalformedSecret => {
                write!(f, "storage encryption key secret is malformed")
            }
            KeyProviderError::UnknownKeyId(kid) => {
                write!(f, "storage encryption key id '{kid}' is not available")
            }
            KeyProviderError::KeyStoreUnavailable(detail) => {
                write!(f, "storage encryption key store unavailable: {detail}")
            }
            KeyProviderError::KeyFileAccessible(path) => {
                write!(
                    f,
                    "storage encryption key file {path} must not be accessible by group or others (set its permissions to 0600)"
                )
            }
        }
    }
}

impl std::error::Error for KeyProviderError {}

/// Resolves storage encryption keys by id. Keys are loaded once at construction
/// and held in memory; resolution never calls an external key store.
pub(crate) trait StorageKeyProvider: Send + Sync {
    /// Key id stamped onto envelopes for new writes.
    fn active_key_id(&self) -> &str;

    /// Resolve the key for a given envelope key id. An unknown id is an error,
    /// never a wrong key.
    fn key(&self, kid: &str) -> Result<FixedKey<32>, KeyProviderError>;
}

/// In-memory provider backing both the dev and Secrets Manager sources.
#[derive(Debug)]
pub(crate) struct InMemoryKeyProvider {
    active: String,
    keys: HashMap<String, FixedKey<32>>,
}

impl InMemoryKeyProvider {
    pub(crate) fn new(
        active: String,
        keys: HashMap<String, FixedKey<32>>,
    ) -> Result<Self, KeyProviderError> {
        if !keys.contains_key(&active) {
            return Err(KeyProviderError::MalformedSecret);
        }
        Ok(Self { active, keys })
    }

    /// Build a single-key provider from a base64-encoded 32-byte dev key.
    pub(crate) fn from_dev_key(key_b64: &str, kid: &str) -> Result<Self, KeyProviderError> {
        let mut keys = HashMap::new();
        keys.insert(kid.to_string(), decode_key(key_b64)?);
        Self::new(kid.to_string(), keys)
    }

    /// Build a provider from the structured key document
    /// `{ "active": kid, "keys": { kid: base64-32-bytes } }`, whether it came
    /// from Secrets Manager or from a mounted file.
    pub(crate) fn from_secret_json(secret: &str) -> Result<Self, KeyProviderError> {
        let doc: SecretDocument =
            serde_json::from_str(secret).map_err(|_| KeyProviderError::MalformedSecret)?;
        let mut keys = HashMap::with_capacity(doc.keys.len());
        for (kid, key_b64) in &doc.keys {
            keys.insert(kid.clone(), decode_key(key_b64)?);
        }
        Self::new(doc.active, keys)
    }

    /// Build a provider from a key document on disk. The file holds long-lived
    /// key material, so like the ACK key files it must be owner-only on Unix.
    pub(crate) fn from_document_file(path: &Path) -> Result<Self, KeyProviderError> {
        ensure_owner_only(path)?;
        let document = Zeroizing::new(std::fs::read_to_string(path).map_err(|error| {
            KeyProviderError::KeyStoreUnavailable(format!("file {}: {error}", path.display()))
        })?);
        Self::from_secret_json(&document)
    }
}

#[cfg(unix)]
fn ensure_owner_only(path: &Path) -> Result<(), KeyProviderError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(path)
        .map_err(|error| {
            KeyProviderError::KeyStoreUnavailable(format!("file {}: {error}", path.display()))
        })?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        return Err(KeyProviderError::KeyFileAccessible(
            path.display().to_string(),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_owner_only(_path: &Path) -> Result<(), KeyProviderError> {
    Ok(())
}

impl StorageKeyProvider for InMemoryKeyProvider {
    fn active_key_id(&self) -> &str {
        &self.active
    }

    fn key(&self, kid: &str) -> Result<FixedKey<32>, KeyProviderError> {
        self.keys
            .get(kid)
            .cloned()
            .ok_or_else(|| KeyProviderError::UnknownKeyId(kid.to_string()))
    }
}

#[derive(Deserialize)]
struct SecretDocument {
    active: String,
    keys: HashMap<String, String>,
}

fn decode_key(key_b64: &str) -> Result<FixedKey<32>, KeyProviderError> {
    let bytes = Zeroizing::new(
        BASE64
            .decode(key_b64.trim())
            .map_err(|_| KeyProviderError::InvalidKeyEncoding)?,
    );
    let array: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| KeyProviderError::InvalidKeyLength)?;
    Ok(FixedKey::new(array))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_b64(byte: u8) -> String {
        BASE64.encode([byte; 32])
    }

    #[test]
    fn dev_key_roundtrips() {
        let provider = InMemoryKeyProvider::from_dev_key(&key_b64(7), "k1").unwrap();
        assert_eq!(provider.active_key_id(), "k1");
        assert_eq!(provider.key("k1").unwrap().expose_secret(), &[7u8; 32]);
    }

    #[test]
    fn rejects_bad_base64() {
        let err = InMemoryKeyProvider::from_dev_key("not base64!!!", "k1").unwrap_err();
        assert!(matches!(err, KeyProviderError::InvalidKeyEncoding));
    }

    #[test]
    fn rejects_wrong_length() {
        let short = BASE64.encode([1u8; 16]);
        let err = InMemoryKeyProvider::from_dev_key(&short, "k1").unwrap_err();
        assert!(matches!(err, KeyProviderError::InvalidKeyLength));
    }

    #[test]
    fn unknown_kid_is_error() {
        let provider = InMemoryKeyProvider::from_dev_key(&key_b64(1), "k1").unwrap();
        let err = provider.key("k9").unwrap_err();
        assert!(matches!(err, KeyProviderError::UnknownKeyId(kid) if kid == "k9"));
    }

    #[test]
    fn structured_secret_supports_multiple_keys() {
        let secret = format!(
            r#"{{"active":"k2","keys":{{"k1":"{}","k2":"{}"}}}}"#,
            key_b64(1),
            key_b64(2)
        );
        let provider = InMemoryKeyProvider::from_secret_json(&secret).unwrap();
        assert_eq!(provider.active_key_id(), "k2");
        assert_eq!(provider.key("k1").unwrap().expose_secret(), &[1u8; 32]);
        assert_eq!(provider.key("k2").unwrap().expose_secret(), &[2u8; 32]);
    }

    #[test]
    fn active_kid_must_be_present_in_keys() {
        let secret = format!(r#"{{"active":"missing","keys":{{"k1":"{}"}}}}"#, key_b64(1));
        let err = InMemoryKeyProvider::from_secret_json(&secret).unwrap_err();
        assert!(matches!(err, KeyProviderError::MalformedSecret));
    }

    #[test]
    fn malformed_secret_json_is_error() {
        let err = InMemoryKeyProvider::from_secret_json("{not json").unwrap_err();
        assert!(matches!(err, KeyProviderError::MalformedSecret));
    }

    fn write_document(dir: &Path, mode: u32) -> std::path::PathBuf {
        let path = dir.join("storage-encryption-keys.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"active":"k2","keys":{{"k1":"{}","k2":"{}"}}}}"#,
                key_b64(1),
                key_b64(2)
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let _ = mode;
        path
    }

    #[test]
    fn document_file_loads_multiple_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_document(dir.path(), 0o600);
        let provider = InMemoryKeyProvider::from_document_file(&path).unwrap();
        assert_eq!(provider.active_key_id(), "k2");
        assert_eq!(provider.key("k1").unwrap().expose_secret(), &[1u8; 32]);
    }

    #[cfg(unix)]
    #[test]
    fn document_file_must_be_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_document(dir.path(), 0o644);
        let err = InMemoryKeyProvider::from_document_file(&path).unwrap_err();
        assert!(matches!(err, KeyProviderError::KeyFileAccessible(_)));
    }

    #[test]
    fn missing_document_file_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let err =
            InMemoryKeyProvider::from_document_file(&dir.path().join("absent.json")).unwrap_err();
        assert!(matches!(err, KeyProviderError::KeyStoreUnavailable(_)));
    }
}
