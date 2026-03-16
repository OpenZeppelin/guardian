//! Key management and hex utilities used by the multisig client.

use miden_client::Serializable;
use miden_protocol::crypto::dsa::falcon512_rpo::{PublicKey, SecretKey};
use miden_protocol::{FieldElement, Word};
use private_state_manager_shared::SignatureScheme;

/// Scheme-specific secret key for creating PSM auth providers.
pub enum SchemeSecretKey {
    Falcon(SecretKey),
    Ecdsa(miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey),
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

    /// Returns the hex-encoded public key, if the scheme requires one at execution time.
    fn public_key_hex(&self) -> Option<String> {
        None
    }
}

/// Default Falcon key store implementation.
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
        Self::new(SecretKey::new())
    }

    /// Returns a reference to the Falcon secret key.
    pub fn falcon_secret_key(&self) -> &SecretKey {
        &self.secret_key
    }

    /// Returns a reference to the Falcon public key.
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
        format!(
            "0x{}",
            hex::encode(self.secret_key.sign(message).to_bytes())
        )
    }

    fn secret_key(&self) -> SchemeSecretKey {
        SchemeSecretKey::Falcon(self.secret_key.clone())
    }
}

/// ECDSA key store implementation using secp256k1 keys.
pub struct EcdsaPsmKeyStore {
    secret_key: std::sync::Mutex<miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey>,
    commitment: Word,
    commitment_hex: String,
    public_key_hex: String,
}

impl EcdsaPsmKeyStore {
    /// Creates a new ECDSA key store with the given secret key.
    pub fn new(secret_key: miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey) -> Self {
        let public_key = secret_key.public_key();
        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));
        let public_key_hex = format!("0x{}", hex::encode(public_key.to_bytes()));

        Self {
            secret_key: std::sync::Mutex::new(secret_key),
            commitment,
            commitment_hex,
            public_key_hex,
        }
    }

    /// Generates a new random ECDSA key store.
    pub fn generate() -> Self {
        let secret_key = miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey::new();
        Self::new(secret_key)
    }

    /// Returns the ECDSA public key.
    pub fn public_key(&self) -> miden_protocol::crypto::dsa::ecdsa_k256_keccak::PublicKey {
        self.secret_key.lock().unwrap().public_key()
    }

    /// Returns a clone of the ECDSA secret key.
    pub fn clone_ecdsa_secret_key(
        &self,
    ) -> miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey {
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
        format!(
            "0x{}",
            hex::encode(self.secret_key.lock().unwrap().sign(message).to_bytes())
        )
    }

    fn secret_key(&self) -> SchemeSecretKey {
        SchemeSecretKey::Ecdsa(self.secret_key.lock().unwrap().clone())
    }

    fn public_key_hex(&self) -> Option<String> {
        Some(self.public_key_hex.clone())
    }
}

/// Backward-compatible alias for the Falcon key store.
pub type FalconKeyStore = PsmKeyStore;

/// Strips the "0x" prefix from a hex string if present.
pub fn strip_hex_prefix(input: &str) -> &str {
    input.strip_prefix("0x").unwrap_or(input)
}

/// Ensures the hex string has a "0x" prefix.
pub fn ensure_hex_prefix(input: &str) -> String {
    if input.starts_with("0x") {
        input.to_string()
    } else {
        format!("0x{}", input)
    }
}

/// Validates that a string is valid commitment hex (64 hex chars, optionally with 0x prefix).
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

/// Parses a hex-encoded word string to a Word.
pub fn word_from_hex(hex_str: &str) -> Result<Word, String> {
    let trimmed = strip_hex_prefix(hex_str);
    let bytes = hex::decode(trimmed).map_err(|e| format!("invalid hex: {}", e))?;

    if bytes.len() != 32 {
        return Err(format!(
            "invalid word length: expected 32 bytes, got {}",
            bytes.len()
        ));
    }

    let mut felts = [miden_protocol::Felt::ZERO; 4];
    #[allow(clippy::needless_range_loop)]
    for (i, chunk) in bytes.chunks(8).enumerate() {
        let mut arr = [0u8; 8];
        arr.copy_from_slice(chunk);
        felts[i] = miden_protocol::Felt::try_from(u64::from_le_bytes(arr))
            .map_err(|e| format!("invalid field element in word '{}': {}", hex_str, e))?;
    }

    Ok(felts.into())
}

/// Backward-compatible alias for parsing commitment hex.
pub fn commitment_from_hex(hex_str: &str) -> Result<Word, String> {
    word_from_hex(hex_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_protocol::crypto::dsa::ecdsa_k256_keccak::Signature as EcdsaSignature;
    use miden_protocol::crypto::dsa::falcon512_rpo::PublicKey as FalconPublicKey;
    use miden_protocol::crypto::dsa::falcon512_rpo::SecretKey as FalconSecretKey;
    use miden_protocol::utils::Deserializable;
    use private_state_manager_shared::hex::FromHex;

    #[test]
    fn generate_creates_valid_signer() {
        let signer = FalconKeyStore::generate();
        assert!(signer.commitment_hex().starts_with("0x"));
        assert_eq!(signer.commitment_hex().len(), 66);
    }

    #[test]
    fn new_from_secret_key_derives_correct_commitment() {
        let secret_key = FalconSecretKey::new();
        let expected_commitment = secret_key.public_key().to_commitment();
        let signer = FalconKeyStore::new(secret_key);
        assert_eq!(signer.commitment(), expected_commitment);
    }

    #[test]
    fn commitment_hex_is_consistent() {
        let signer = FalconKeyStore::generate();
        assert_eq!(signer.commitment_hex(), signer.commitment_hex());
    }

    #[test]
    fn commitment_roundtrip_via_hex() {
        let signer = FalconKeyStore::generate();
        let parsed = word_from_hex(&signer.commitment_hex()).unwrap();
        assert_eq!(parsed, signer.commitment());
    }

    #[test]
    fn falcon_sign_produces_verifiable_signature() {
        let signer = FalconKeyStore::generate();
        let message = Word::default();
        let signature_hex = signer.sign_hex(message);
        let public_key = FalconPublicKey::from_hex(&format!(
            "0x{}",
            hex::encode(signer.public_key().to_bytes())
        ))
        .unwrap();
        let signature =
            miden_protocol::crypto::dsa::falcon512_rpo::Signature::from_hex(&signature_hex)
                .unwrap();
        assert!(public_key.verify(message, &signature));
    }

    #[test]
    fn ecdsa_sign_produces_verifiable_signature() {
        let signer = EcdsaPsmKeyStore::generate();
        let message = Word::default();
        let signature_hex = signer.sign_hex(message);
        let signature = EcdsaSignature::read_from_bytes(
            &hex::decode(signature_hex.trim_start_matches("0x")).unwrap(),
        )
        .unwrap();
        assert!(signer.public_key().verify(message, &signature));
    }

    #[test]
    fn public_key_hex_roundtrips() {
        let signer = FalconKeyStore::new(FalconSecretKey::new());
        let public_key = FalconPublicKey::from_hex(&format!(
            "0x{}",
            hex::encode(signer.public_key().to_bytes())
        ))
        .unwrap();
        assert_eq!(public_key.to_commitment(), signer.commitment());
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
    fn ensure_hex_prefix_adds_prefix() {
        assert_eq!(ensure_hex_prefix("abcd"), "0xabcd");
    }

    #[test]
    fn ensure_hex_prefix_preserves_existing() {
        assert_eq!(ensure_hex_prefix("0xabcd"), "0xabcd");
    }

    #[test]
    fn validate_commitment_hex_valid_with_prefix() {
        let valid = format!("0x{}", "b".repeat(64));
        assert!(validate_commitment_hex(&valid).is_ok());
    }

    #[test]
    fn validate_commitment_hex_invalid_chars() {
        let not_hex = "g".repeat(64);
        let err = validate_commitment_hex(&not_hex).unwrap_err();
        assert!(err.contains("invalid hex"));
    }

    #[test]
    fn word_from_hex_valid_with_prefix() {
        let hex = format!("0x{}", "a".repeat(64));
        assert!(word_from_hex(&hex).is_ok());
    }

    #[test]
    fn word_from_hex_invalid_length() {
        let err = word_from_hex("abcd").unwrap_err();
        assert!(err.contains("expected 32 bytes"));
    }

    #[test]
    fn word_from_hex_rejects_non_canonical_felt() {
        let hex = format!("{}{}", "ff".repeat(8), "00".repeat(24));
        let err = word_from_hex(&hex).unwrap_err();
        assert!(err.contains("invalid field element"));
    }
}
