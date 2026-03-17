//! Signer re-exports and hex utilities used by the multisig client.

use miden_protocol::{FieldElement, Word};
use private_state_manager_shared::SignatureScheme;

pub use private_state_manager_client::{
    EcdsaKeyStore as EcdsaPsmKeyStore, FalconKeyStore, Signer as KeyManager,
};

/// Backward-compatible alias for the Falcon key store.
pub type PsmKeyStore = FalconKeyStore;

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

/// Returns the public key hex needed for exported ECDSA signatures.
pub fn proposal_public_key_hex(key_manager: &dyn KeyManager) -> Option<String> {
    match key_manager.scheme() {
        SignatureScheme::Falcon => None,
        SignatureScheme::Ecdsa => Some(key_manager.public_key_hex()),
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
    use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{
        PublicKey as EcdsaPublicKey, Signature as EcdsaSignature,
    };
    use miden_protocol::crypto::dsa::falcon512_rpo::{
        PublicKey as FalconPublicKey, Signature as FalconSignature,
    };
    use miden_protocol::utils::Deserializable;
    use private_state_manager_shared::hex::FromHex;

    #[test]
    fn falcon_signer_commitment_roundtrip_via_hex() {
        let signer = FalconKeyStore::generate();
        let hex = signer.commitment_hex();
        let parsed = commitment_from_hex(&hex).unwrap();
        assert_eq!(parsed, signer.commitment());
    }

    #[test]
    fn ecdsa_signer_commitment_roundtrip_via_hex() {
        let signer = EcdsaPsmKeyStore::generate();
        let hex = signer.commitment_hex();
        let parsed = commitment_from_hex(&hex).unwrap();
        assert_eq!(parsed, signer.commitment());
    }

    #[test]
    fn falcon_signer_signature_hex_is_verifiable() {
        let signer = FalconKeyStore::generate();
        let message = Word::default();
        let signature = FalconSignature::from_hex(&signer.sign_word_hex(message)).unwrap();
        let public_key = FalconPublicKey::from_hex(&signer.public_key_hex()).unwrap();
        assert!(public_key.verify(message, &signature));
    }

    #[test]
    fn ecdsa_signer_signature_hex_is_verifiable() {
        let signer = EcdsaPsmKeyStore::generate();
        let message = Word::default();
        let signature_bytes =
            hex::decode(signer.sign_word_hex(message).trim_start_matches("0x")).unwrap();
        let signature = EcdsaSignature::read_from_bytes(&signature_bytes).unwrap();
        let public_key_bytes =
            hex::decode(signer.public_key_hex().trim_start_matches("0x")).unwrap();
        let public_key = EcdsaPublicKey::read_from_bytes(&public_key_bytes).unwrap();
        assert!(public_key.verify(message, &signature));
    }

    #[test]
    fn proposal_public_key_hex_is_none_for_falcon() {
        let signer = FalconKeyStore::generate();
        assert_eq!(proposal_public_key_hex(&signer), None);
    }

    #[test]
    fn proposal_public_key_hex_is_present_for_ecdsa() {
        let signer = EcdsaPsmKeyStore::generate();
        assert_eq!(
            proposal_public_key_hex(&signer),
            Some(signer.public_key_hex())
        );
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
    fn validate_commitment_hex_invalid_chars() {
        let not_hex = "g".repeat(64);
        let err = validate_commitment_hex(&not_hex).unwrap_err();
        assert!(err.contains("invalid hex"));
    }
}
