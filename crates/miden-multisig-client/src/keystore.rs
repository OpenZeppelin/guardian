//! Key management for PSM authentication.

use miden_client::Serializable;
use miden_objects::crypto::dsa::rpo_falcon512::{PublicKey, SecretKey};
use miden_objects::{FieldElement, Word};
use private_state_manager_shared::SignatureScheme;

/// Scheme-specific secret key for creating PSM auth providers.
pub enum SchemeSecretKey {
    Falcon(SecretKey),
    Ecdsa(miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey),
}

/// Trait for managing keys used in PSM authentication and transaction signing.
pub trait KeyManager: Send + Sync {
    /// Returns the signature scheme used by this key manager.
    fn scheme(&self) -> SignatureScheme {
        SignatureScheme::Falcon
    }

    /// Returns the public key commitment as a Word.
    fn commitment(&self) -> Word;

    /// Returns the public key commitment as a hex string with 0x prefix.
    fn commitment_hex(&self) -> String;

    /// Signs a message and returns the hex-encoded signature with 0x prefix.
    fn sign_hex(&self, message: Word) -> String;

    /// Returns the scheme-specific secret key for creating auth providers.
    fn secret_key(&self) -> SchemeSecretKey;

    /// Returns the hex-encoded public key (with 0x prefix), if available.
    ///
    /// Required for ECDSA signatures where the public key must be passed
    /// explicitly for advice preparation. Returns `None` for Falcon.
    fn public_key_hex(&self) -> Option<String> {
        None
    }
}

/// Default key store implementation using Falcon keys.
///
/// This stores a Falcon secret key and provides signing capabilities
/// for PSM authentication and transaction signing.
pub struct PsmKeyStore {
    secret_key: SecretKey,
    public_key: PublicKey,
    commitment: Word,
    commitment_hex: String,
}

impl PsmKeyStore {
    /// Creates a new key store with the given secret key.
    pub fn new(secret_key: SecretKey) -> Self {
        let public_key = secret_key.public_key();
        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));

        Self {
            secret_key,
            public_key,
            commitment,
            commitment_hex,
        }
    }

    /// Generates a new random key store.
    pub fn generate() -> Self {
        let secret_key = SecretKey::new();
        Self::new(secret_key)
    }

    /// Returns a reference to the Falcon secret key.
    pub fn falcon_secret_key(&self) -> &SecretKey {
        &self.secret_key
    }

    /// Returns a reference to the public key.
    pub fn public_key(&self) -> &PublicKey {
        &self.public_key
    }
}

impl KeyManager for PsmKeyStore {
    fn commitment(&self) -> Word {
        self.commitment
    }

    fn commitment_hex(&self) -> String {
        self.commitment_hex.clone()
    }

    fn sign_hex(&self, message: Word) -> String {
        let sig = self.secret_key.sign(message);
        format!("0x{}", hex::encode(sig.to_bytes()))
    }

    fn secret_key(&self) -> SchemeSecretKey {
        SchemeSecretKey::Falcon(self.secret_key.clone())
    }
}

/// ECDSA key store implementation using secp256k1 keys.
pub struct EcdsaPsmKeyStore {
    secret_key: std::sync::Mutex<miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey>,
    commitment: Word,
    commitment_hex: String,
}

impl EcdsaPsmKeyStore {
    /// Creates a new ECDSA key store with the given secret key.
    pub fn new(secret_key: miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey) -> Self {
        let public_key = secret_key.public_key();
        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));

        Self {
            secret_key: std::sync::Mutex::new(secret_key),
            commitment,
            commitment_hex,
        }
    }

    /// Generates a new random ECDSA key store.
    pub fn generate() -> Self {
        let secret_key = miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey::new();
        Self::new(secret_key)
    }

    /// Returns the ECDSA public key.
    pub fn public_key(&self) -> miden_objects::crypto::dsa::ecdsa_k256_keccak::PublicKey {
        self.secret_key.lock().unwrap().public_key()
    }

    /// Returns a clone of the ECDSA secret key.
    pub fn clone_ecdsa_secret_key(
        &self,
    ) -> miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey {
        self.secret_key.lock().unwrap().clone()
    }
}

impl KeyManager for EcdsaPsmKeyStore {
    fn scheme(&self) -> SignatureScheme {
        SignatureScheme::Ecdsa
    }

    fn commitment(&self) -> Word {
        self.commitment
    }

    fn commitment_hex(&self) -> String {
        self.commitment_hex.clone()
    }

    fn sign_hex(&self, message: Word) -> String {
        let sig = self.secret_key.lock().unwrap().sign(message);
        format!("0x{}", hex::encode(sig.to_bytes()))
    }

    fn secret_key(&self) -> SchemeSecretKey {
        SchemeSecretKey::Ecdsa(self.secret_key.lock().unwrap().clone())
    }

    fn public_key_hex(&self) -> Option<String> {
        let pk = self.public_key();
        Some(format!("0x{}", hex::encode(pk.to_bytes())))
    }
}

/// Strips the "0x" prefix from a hex string if present.
///
/// # Example
/// ```ignore
/// assert_eq!(strip_hex_prefix("0xabcd"), "abcd");
/// assert_eq!(strip_hex_prefix("abcd"), "abcd");
/// ```
pub fn strip_hex_prefix(input: &str) -> &str {
    input.strip_prefix("0x").unwrap_or(input)
}

/// Ensures the hex string has a "0x" prefix.
///
/// # Example
/// ```ignore
/// assert_eq!(ensure_hex_prefix("abcd"), "0xabcd");
/// assert_eq!(ensure_hex_prefix("0xabcd"), "0xabcd");
/// ```
pub fn ensure_hex_prefix(input: &str) -> String {
    if input.starts_with("0x") {
        input.to_string()
    } else {
        format!("0x{}", input)
    }
}

/// Validates that a string is valid commitment hex (64 hex chars, optionally with 0x prefix).
///
/// # Example
/// ```ignore
/// validate_commitment_hex("0x1234...").unwrap(); // 64 hex chars after 0x
/// validate_commitment_hex("abc").unwrap_err();   // too short
/// ```
pub fn validate_commitment_hex(input: &str) -> Result<(), String> {
    let stripped = strip_hex_prefix(input);
    if stripped.len() != 64 {
        return Err(format!(
            "invalid commitment length: expected 64 hex chars, got {}",
            stripped.len()
        ));
    }
    hex::decode(stripped).map_err(|e| format!("invalid hex: {}", e))?;
    Ok(())
}

/// Parses a hex-encoded commitment string to a Word.
pub fn commitment_from_hex(hex_str: &str) -> Result<Word, String> {
    let trimmed = strip_hex_prefix(hex_str);
    let bytes = hex::decode(trimmed).map_err(|e| format!("invalid hex: {}", e))?;

    if bytes.len() != 32 {
        return Err(format!(
            "invalid commitment length: expected 32 bytes, got {}",
            bytes.len()
        ));
    }

    let mut felts = [miden_objects::Felt::ZERO; 4];
    #[allow(clippy::needless_range_loop)]
    for (i, chunk) in bytes.chunks(8).enumerate() {
        let mut arr = [0u8; 8];
        arr.copy_from_slice(chunk);
        felts[i] = miden_objects::Felt::new(u64::from_le_bytes(arr));
    }

    Ok(felts.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_creates_valid_keystore() {
        let keystore = PsmKeyStore::generate();
        assert!(keystore.commitment_hex().starts_with("0x"));
        assert_eq!(keystore.commitment_hex().len(), 66);
    }

    #[test]
    fn new_from_secret_key_derives_correct_commitment() {
        let secret_key = SecretKey::new();
        let expected_commitment = secret_key.public_key().to_commitment();
        let keystore = PsmKeyStore::new(secret_key);
        assert_eq!(keystore.commitment(), expected_commitment);
    }

    #[test]
    fn commitment_hex_is_consistent() {
        let keystore = PsmKeyStore::generate();
        let hex1 = keystore.commitment_hex();
        let hex2 = keystore.commitment_hex();
        assert_eq!(hex1, hex2);
    }

    #[test]
    fn commitment_roundtrip_via_hex() {
        let keystore = PsmKeyStore::generate();
        let hex = keystore.commitment_hex();
        let parsed = commitment_from_hex(&hex).unwrap();
        assert_eq!(parsed, keystore.commitment());
    }

    #[test]
    fn sign_produces_verifiable_signature() {
        let keystore = PsmKeyStore::generate();
        let message = Word::default();
        let signature = keystore.falcon_secret_key().sign(message);
        let result = keystore.public_key().verify(message, &signature);
        assert!(result);
    }

    #[test]
    fn sign_hex_returns_hex_encoded_signature() {
        let keystore = PsmKeyStore::generate();
        let message = Word::default();
        let sig_hex = keystore.sign_hex(message);
        assert!(sig_hex.starts_with("0x"));
        assert!(hex::decode(sig_hex.strip_prefix("0x").unwrap()).is_ok());
    }

    #[test]
    fn secret_key_returns_falcon_variant() {
        let keystore = PsmKeyStore::generate();
        let message = Word::default();
        match keystore.secret_key() {
            SchemeSecretKey::Falcon(sk) => {
                let sig = sk.sign(message);
                assert!(keystore.public_key().verify(message, &sig));
            }
            SchemeSecretKey::Ecdsa(_) => panic!("expected Falcon variant"),
        }
    }

    #[test]
    fn falcon_secret_key_accessor_returns_key() {
        let keystore = PsmKeyStore::generate();
        let key = keystore.falcon_secret_key();
        assert!(key.public_key().to_commitment() == keystore.commitment());
    }

    #[test]
    fn public_key_accessor_returns_key() {
        let keystore = PsmKeyStore::generate();
        let pubkey = keystore.public_key();
        assert_eq!(pubkey.to_commitment(), keystore.commitment());
    }

    #[test]
    fn strip_hex_prefix_with_prefix() {
        assert_eq!(strip_hex_prefix("0xabcd"), "abcd");
    }

    #[test]
    fn strip_hex_prefix_without_prefix() {
        assert_eq!(strip_hex_prefix("abcd"), "abcd");
    }

    #[test]
    fn strip_hex_prefix_empty_after_prefix() {
        assert_eq!(strip_hex_prefix("0x"), "");
    }

    #[test]
    fn strip_hex_prefix_empty_string() {
        assert_eq!(strip_hex_prefix(""), "");
    }

    #[test]
    fn ensure_hex_prefix_adds_prefix() {
        assert_eq!(ensure_hex_prefix("abcd"), "0xabcd");
    }

    #[test]
    fn ensure_hex_prefix_preserves_existing() {
        assert_eq!(ensure_hex_prefix("0xabcd"), "0xabcd");
    }

    #[test]
    fn ensure_hex_prefix_empty_string() {
        assert_eq!(ensure_hex_prefix(""), "0x");
    }

    #[test]
    fn validate_commitment_hex_valid_without_prefix() {
        let valid = "a".repeat(64);
        assert!(validate_commitment_hex(&valid).is_ok());
    }

    #[test]
    fn validate_commitment_hex_valid_with_prefix() {
        let valid = format!("0x{}", "b".repeat(64));
        assert!(validate_commitment_hex(&valid).is_ok());
    }

    #[test]
    fn validate_commitment_hex_too_short() {
        let err = validate_commitment_hex("abcd").unwrap_err();
        assert!(err.contains("expected 64"));
    }

    #[test]
    fn validate_commitment_hex_too_long() {
        let too_long = "c".repeat(65);
        let err = validate_commitment_hex(&too_long).unwrap_err();
        assert!(err.contains("expected 64"));
    }

    #[test]
    fn validate_commitment_hex_invalid_chars() {
        let not_hex = "g".repeat(64);
        let err = validate_commitment_hex(&not_hex).unwrap_err();
        assert!(err.contains("invalid hex"));
    }

    #[test]
    fn commitment_from_hex_valid_with_prefix() {
        let hex = format!("0x{}", "a".repeat(64));
        let result = commitment_from_hex(&hex);
        assert!(result.is_ok());
    }

    #[test]
    fn commitment_from_hex_valid_without_prefix() {
        let hex = "b".repeat(64);
        let result = commitment_from_hex(&hex);
        assert!(result.is_ok());
    }

    #[test]
    fn commitment_from_hex_invalid_length() {
        let hex = "abcd";
        let err = commitment_from_hex(hex).unwrap_err();
        assert!(err.contains("expected 32 bytes"));
    }

    #[test]
    fn commitment_from_hex_invalid_chars() {
        let hex = "g".repeat(64);
        let err = commitment_from_hex(&hex).unwrap_err();
        assert!(err.contains("invalid hex"));
    }

    #[test]
    fn commitment_from_hex_roundtrip() {
        let original = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
        let word = commitment_from_hex(original).unwrap();
        let bytes: Vec<u8> = word.iter().flat_map(|f| f.as_int().to_le_bytes()).collect();
        let result = hex::encode(bytes);
        assert_eq!(original, result);
    }

    // --- EcdsaPsmKeyStore tests ---

    #[test]
    fn ecdsa_generate_creates_valid_keystore() {
        let keystore = EcdsaPsmKeyStore::generate();
        assert!(keystore.commitment_hex().starts_with("0x"));
        assert_eq!(keystore.commitment_hex().len(), 66);
    }

    #[test]
    fn ecdsa_new_from_secret_key_derives_correct_commitment() {
        let secret_key = miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey::new();
        let expected_commitment = secret_key.public_key().to_commitment();
        let keystore = EcdsaPsmKeyStore::new(secret_key);
        assert_eq!(keystore.commitment(), expected_commitment);
    }

    #[test]
    fn ecdsa_commitment_hex_is_consistent() {
        let keystore = EcdsaPsmKeyStore::generate();
        let hex1 = keystore.commitment_hex();
        let hex2 = keystore.commitment_hex();
        assert_eq!(hex1, hex2);
    }

    #[test]
    fn ecdsa_commitment_roundtrip_via_hex() {
        let keystore = EcdsaPsmKeyStore::generate();
        let hex = keystore.commitment_hex();
        let parsed = commitment_from_hex(&hex).unwrap();
        assert_eq!(parsed, keystore.commitment());
    }

    #[test]
    fn ecdsa_sign_hex_returns_hex_encoded_signature() {
        let keystore = EcdsaPsmKeyStore::generate();
        let message = Word::default();
        let sig_hex = keystore.sign_hex(message);
        assert!(sig_hex.starts_with("0x"));
        assert!(hex::decode(sig_hex.strip_prefix("0x").unwrap()).is_ok());
    }

    #[test]
    fn ecdsa_sign_produces_verifiable_signature() {
        use miden_objects::utils::Deserializable;

        let keystore = EcdsaPsmKeyStore::generate();
        let message = Word::default();
        let sig_hex = keystore.sign_hex(message);

        let sig_bytes = hex::decode(sig_hex.strip_prefix("0x").unwrap()).unwrap();
        let signature =
            miden_objects::crypto::dsa::ecdsa_k256_keccak::Signature::read_from_bytes(&sig_bytes)
                .unwrap();
        let pk = keystore.public_key();
        assert!(pk.verify(message, &signature));
    }

    #[test]
    fn ecdsa_secret_key_returns_ecdsa_variant() {
        let keystore = EcdsaPsmKeyStore::generate();
        match keystore.secret_key() {
            SchemeSecretKey::Ecdsa(_) => {}
            SchemeSecretKey::Falcon(_) => panic!("expected Ecdsa variant"),
        }
    }

    #[test]
    fn ecdsa_scheme_returns_ecdsa() {
        let keystore = EcdsaPsmKeyStore::generate();
        assert_eq!(keystore.scheme(), SignatureScheme::Ecdsa);
    }

    #[test]
    fn ecdsa_public_key_hex_returns_some() {
        let keystore = EcdsaPsmKeyStore::generate();
        let pk_hex = keystore.public_key_hex();
        assert!(pk_hex.is_some());
        let hex_str = pk_hex.unwrap();
        assert!(hex_str.starts_with("0x"));
        assert!(hex::decode(hex_str.strip_prefix("0x").unwrap()).is_ok());
    }

    #[test]
    fn falcon_public_key_hex_returns_none() {
        let keystore = PsmKeyStore::generate();
        assert!(keystore.public_key_hex().is_none());
    }

    #[test]
    fn falcon_scheme_returns_falcon() {
        let keystore = PsmKeyStore::generate();
        assert_eq!(keystore.scheme(), SignatureScheme::Falcon);
    }

    #[test]
    fn ecdsa_clone_secret_key_produces_same_public_key() {
        let keystore = EcdsaPsmKeyStore::generate();
        let sk = keystore.clone_ecdsa_secret_key();
        assert_eq!(sk.public_key().to_commitment(), keystore.commitment());
    }

    #[test]
    fn ecdsa_public_key_matches_commitment() {
        let keystore = EcdsaPsmKeyStore::generate();
        let pk = keystore.public_key();
        assert_eq!(pk.to_commitment(), keystore.commitment());
    }
}
