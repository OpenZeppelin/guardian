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
    fn test_generate_keystore() {
        let keystore = PsmKeyStore::generate();
        assert!(keystore.commitment_hex().starts_with("0x"));
        assert_eq!(keystore.commitment_hex().len(), 66); // 0x + 64 hex chars
    }

    #[test]
    fn test_commitment_roundtrip() {
        let keystore = PsmKeyStore::generate();
        let hex = keystore.commitment_hex();
        let parsed = commitment_from_hex(&hex).unwrap();
        assert_eq!(parsed, keystore.commitment());
    }

    #[test]
    fn test_sign_and_verify() {
        let keystore = PsmKeyStore::generate();
        let message = Word::default();
        let signature = keystore.sign(message);

        // Verify signature is valid
        let result = keystore.public_key().verify(message, &signature);
        assert!(result);
    }

    #[test]
    fn test_strip_hex_prefix() {
        assert_eq!(strip_hex_prefix("0xabcd"), "abcd");
        assert_eq!(strip_hex_prefix("abcd"), "abcd");
        assert_eq!(strip_hex_prefix("0x"), "");
        assert_eq!(strip_hex_prefix(""), "");
    }

    #[test]
    fn test_ensure_hex_prefix() {
        assert_eq!(ensure_hex_prefix("abcd"), "0xabcd");
        assert_eq!(ensure_hex_prefix("0xabcd"), "0xabcd");
        assert_eq!(ensure_hex_prefix(""), "0x");
    }

    #[test]
    fn test_validate_commitment_hex() {
        // Valid: 64 hex chars without prefix
        let valid_no_prefix = "a".repeat(64);
        assert!(validate_commitment_hex(&valid_no_prefix).is_ok());

        // Valid: 64 hex chars with prefix
        let valid_with_prefix = format!("0x{}", "b".repeat(64));
        assert!(validate_commitment_hex(&valid_with_prefix).is_ok());

        // Invalid: too short
        assert!(validate_commitment_hex("abcd").is_err());

        // Invalid: too long
        let too_long = "c".repeat(65);
        assert!(validate_commitment_hex(&too_long).is_err());

        // Invalid: not hex
        let not_hex = "g".repeat(64);
        assert!(validate_commitment_hex(&not_hex).is_err());
    }
}
