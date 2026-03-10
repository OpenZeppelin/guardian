//! Signer re-exports and hex utilities used by the multisig client.

use miden_protocol::{FieldElement, Word};

pub use private_state_manager_client::{FalconKeyStore, Signer};

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

#[cfg(test)]
mod tests {
    use super::*;
    use miden_protocol::crypto::dsa::falcon512_rpo::PublicKey;
    use miden_protocol::crypto::dsa::falcon512_rpo::SecretKey;
    use private_state_manager_shared::hex::FromHex;
    use private_state_manager_shared::hex::IntoHex;

    #[test]
    fn generate_creates_valid_signer() {
        let signer = FalconKeyStore::generate();
        assert!(signer.commitment_hex().starts_with("0x"));
        assert_eq!(signer.commitment_hex().len(), 66);
    }

    #[test]
    fn new_from_secret_key_derives_correct_commitment() {
        let secret_key = SecretKey::new();
        let expected_commitment = secret_key.public_key().to_commitment();
        let signer = FalconKeyStore::new(secret_key);
        assert_eq!(signer.commitment(), expected_commitment);
    }

    #[test]
    fn commitment_hex_is_consistent() {
        let signer = FalconKeyStore::generate();
        let hex1 = signer.commitment_hex();
        let hex2 = signer.commitment_hex();
        assert_eq!(hex1, hex2);
    }

    #[test]
    fn commitment_roundtrip_via_hex() {
        let signer = FalconKeyStore::generate();
        let hex = signer.commitment_hex();
        let parsed = word_from_hex(&hex).unwrap();
        assert_eq!(parsed, signer.commitment());
    }

    #[test]
    fn sign_produces_verifiable_signature() {
        let signer = FalconKeyStore::generate();
        let message = Word::default();
        let signature = signer.sign_word(message);
        let public_key = PublicKey::from_hex(&signer.public_key_hex()).unwrap();
        assert!(public_key.verify(message, &signature));
    }

    #[test]
    fn sign_hex_returns_hex_encoded_signature() {
        let signer = FalconKeyStore::generate();
        let message = Word::default();
        let sig_hex = signer.sign_word(message).into_hex();
        assert!(sig_hex.starts_with("0x"));
        assert!(hex::decode(sig_hex.strip_prefix("0x").unwrap()).is_ok());
    }

    #[test]
    fn public_key_hex_roundtrips() {
        let signer = FalconKeyStore::new(SecretKey::new());
        let public_key = PublicKey::from_hex(&signer.public_key_hex()).unwrap();
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
    fn word_from_hex_valid_with_prefix() {
        let hex = format!("0x{}", "a".repeat(64));
        let result = word_from_hex(&hex);
        assert!(result.is_ok());
    }

    #[test]
    fn word_from_hex_valid_without_prefix() {
        let hex = "b".repeat(64);
        let result = word_from_hex(&hex);
        assert!(result.is_ok());
    }

    #[test]
    fn word_from_hex_invalid_length() {
        let hex = "abcd";
        let err = word_from_hex(hex).unwrap_err();
        assert!(err.contains("expected 32 bytes"));
    }

    #[test]
    fn word_from_hex_invalid_chars() {
        let hex = "g".repeat(64);
        let err = word_from_hex(&hex).unwrap_err();
        assert!(err.contains("invalid hex"));
    }

    #[test]
    fn word_from_hex_roundtrip() {
        let original = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
        let word = word_from_hex(original).unwrap();
        let bytes: Vec<u8> = word.iter().flat_map(|f| f.as_int().to_le_bytes()).collect();
        let result = hex::encode(bytes);
        assert_eq!(original, result);
    }

    #[test]
    fn word_from_hex_rejects_non_canonical_felt() {
        let hex = format!("{}{}", "ff".repeat(8), "00".repeat(24));
        let err = word_from_hex(&hex).unwrap_err();
        assert!(err.contains("invalid field element"));
    }
}
