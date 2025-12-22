//! Key management for PSM authentication.
//!
//! This module provides key management functionality separate from miden-client's keystore
//! because miden-client's keystore doesn't expose direct signing methods.

use miden_client::Serializable;
use miden_objects::crypto::dsa::rpo_falcon512::{PublicKey, SecretKey, Signature};
use miden_objects::{FieldElement, Word};

/// Trait for managing keys used in PSM authentication and transaction signing.
pub trait KeyManager: Send + Sync {
    /// Returns the public key commitment as a Word.
    fn commitment(&self) -> Word;

    /// Returns the public key commitment as a hex string with 0x prefix.
    fn commitment_hex(&self) -> String;

    /// Signs a message (Word) and returns the signature.
    fn sign(&self, message: Word) -> Signature;

    /// Signs a message and returns the hex-encoded signature with 0x prefix.
    fn sign_hex(&self, message: Word) -> String {
        let sig = self.sign(message);
        format!("0x{}", hex::encode(sig.to_bytes()))
    }

    /// Returns a clone of the secret key for PSM authentication.
    ///
    /// This is needed to create `Auth` for PSM client requests.
    fn clone_secret_key(&self) -> SecretKey;
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
        // Use the same commitment computation as miden-objects (SequentialCommit trait)
        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.as_bytes()));

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

    /// Returns a reference to the secret key.
    pub fn secret_key(&self) -> &SecretKey {
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

    fn sign(&self, message: Word) -> Signature {
        self.secret_key.sign(message)
    }

    fn clone_secret_key(&self) -> SecretKey {
        self.secret_key.clone()
    }
}

// ============================================================================
// Hex Utilities
// ============================================================================

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
///
/// Accepts hex strings with or without the "0x" prefix.
/// This is the primary function for converting user input to a commitment Word.
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
        let signature = keystore.sign(message);
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
    fn clone_secret_key_produces_equivalent_key() {
        let keystore = PsmKeyStore::generate();
        let cloned = keystore.clone_secret_key();
        let message = Word::default();
        let sig1 = keystore.sign(message);
        let sig2 = cloned.sign(message);
        assert!(keystore.public_key().verify(message, &sig1));
        assert!(keystore.public_key().verify(message, &sig2));
    }

    #[test]
    fn secret_key_accessor_returns_key() {
        let keystore = PsmKeyStore::generate();
        let key = keystore.secret_key();
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
}
