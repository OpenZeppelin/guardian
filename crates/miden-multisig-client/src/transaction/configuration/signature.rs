//! Signature advice entry building for multisig transactions.

use miden_objects::account::auth::Signature;
use miden_objects::crypto::dsa::ecdsa_k256_keccak;
use miden_objects::utils::{Deserializable, Serializable};
use miden_objects::{Felt, Hasher, Word};

use crate::error::{MultisigError, Result};

/// Packs raw bytes into field elements as packed u32 values (little-endian).
/// This matches the encoding used by `miden_stdlib::encode_ecdsa_signature`.
fn bytes_to_packed_u32_felts(bytes: &[u8]) -> Vec<Felt> {
    bytes
        .chunks(4)
        .map(|chunk| {
            let mut packed = [0u8; 4];
            packed[..chunk.len()].copy_from_slice(chunk);
            Felt::from(u32::from_le_bytes(packed))
        })
        .collect()
}

/// Encodes an ECDSA public key and signature into field elements in the format
/// expected by the Miden VM's ECDSA verification procedure.
///
/// This replicates `miden_stdlib::encode_ecdsa_signature` to avoid a direct dependency.
fn encode_ecdsa_signature_felts(
    pk: &ecdsa_k256_keccak::PublicKey,
    sig: &ecdsa_k256_keccak::Signature,
) -> Vec<Felt> {
    let mut out = Vec::new();
    out.extend(bytes_to_packed_u32_felts(&pk.to_bytes()));
    out.extend(bytes_to_packed_u32_felts(&sig.to_bytes()));
    out
}

/// Builds an advice entry for a signature.
///
/// The key is Hash(pubkey_commitment, message) and the value is the prepared signature.
///
/// For Falcon signatures, uses the standard `to_prepared_signature` method.
/// For ECDSA signatures, requires the public key bytes to encode directly (since
/// ECDSA public key recovery from signature is unreliable and panics).
pub fn build_signature_advice_entry(
    pubkey_commitment: Word,
    message: Word,
    signature: &Signature,
    ecdsa_pubkey: Option<&ecdsa_k256_keccak::PublicKey>,
) -> (Word, Vec<Felt>) {
    let mut elements = Vec::with_capacity(8);
    elements.extend_from_slice(pubkey_commitment.as_elements());
    elements.extend_from_slice(message.as_elements());
    let key: Word = Hasher::hash_elements(&elements);

    let values = match signature {
        Signature::RpoFalcon512(_) => signature.to_prepared_signature(message),
        Signature::EcdsaK256Keccak(ecdsa_sig) => {
            let pk = ecdsa_pubkey
                .expect("ECDSA public key must be provided for ECDSA signature advice preparation");
            let encoded = encode_ecdsa_signature_felts(pk, ecdsa_sig);
            let mut reversed = encoded;
            reversed.reverse();
            reversed
        }
    };

    (key, values)
}

/// Builds an advice entry for an ECDSA signature from raw hex-encoded public key bytes.
///
/// This is a convenience wrapper that parses the public key from hex before calling
/// `build_signature_advice_entry`.
pub fn build_ecdsa_signature_advice_entry(
    pubkey_commitment: Word,
    message: Word,
    signature: &Signature,
    pubkey_hex: &str,
) -> Result<(Word, Vec<Felt>)> {
    let hex_str = pubkey_hex.trim_start_matches("0x");
    let pk_bytes = hex::decode(hex_str)
        .map_err(|e| MultisigError::Signature(format!("invalid ECDSA public key hex: {}", e)))?;
    let pk = ecdsa_k256_keccak::PublicKey::read_from_bytes(&pk_bytes).map_err(|e| {
        MultisigError::Signature(format!("failed to deserialize ECDSA public key: {}", e))
    })?;
    Ok(build_signature_advice_entry(
        pubkey_commitment,
        message,
        signature,
        Some(&pk),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_objects::account::auth::Signature as AccountSignature;
    use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;

    #[test]
    fn ecdsa_commitment_matches_packed_u32_hash() {
        use miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey;

        let sk = EcdsaSecretKey::new();
        let pk = sk.public_key();

        let commitment_via_trait = pk.to_commitment();

        let pk_bytes = pk.to_bytes();
        let packed_felts = bytes_to_packed_u32_felts(&pk_bytes);
        let commitment_via_manual: Word = Hasher::hash_elements(&packed_felts);

        assert_eq!(
            commitment_via_trait, commitment_via_manual,
            "PublicKey::to_commitment() must match manual packed-u32 hash"
        );
    }

    #[test]
    fn signature_advice_key_matches_hash_elements_concat() {
        let pubkey_commitment =
            Word::from([Felt::new(1), Felt::new(2), Felt::new(3), Felt::new(4)]);
        let message = Word::from([Felt::new(5), Felt::new(6), Felt::new(7), Felt::new(8)]);

        let secret_key = SecretKey::new();
        let rpo_sig = secret_key.sign(message);
        let signature = AccountSignature::from(rpo_sig);
        let (key, _) = build_signature_advice_entry(pubkey_commitment, message, &signature, None);

        let mut elements = Vec::with_capacity(8);
        elements.extend_from_slice(pubkey_commitment.as_elements());
        elements.extend_from_slice(message.as_elements());
        let expected: Word = Hasher::hash_elements(&elements);

        assert_eq!(key, expected);
    }

    #[test]
    fn falcon_advice_entry_produces_non_empty_values() {
        let pubkey_commitment =
            Word::from([Felt::new(1), Felt::new(2), Felt::new(3), Felt::new(4)]);
        let message = Word::from([Felt::new(5), Felt::new(6), Felt::new(7), Felt::new(8)]);

        let secret_key = SecretKey::new();
        let rpo_sig = secret_key.sign(message);
        let signature = AccountSignature::from(rpo_sig);
        let (_key, values) =
            build_signature_advice_entry(pubkey_commitment, message, &signature, None);

        assert!(!values.is_empty());
    }

    #[test]
    fn ecdsa_advice_entry_produces_non_empty_values() {
        use miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey;

        let sk = EcdsaSecretKey::new();
        let pk = sk.public_key();
        let commitment = pk.to_commitment();
        let message = Word::from([Felt::new(10), Felt::new(20), Felt::new(30), Felt::new(40)]);
        let sig = sk.sign(message);
        let account_sig = AccountSignature::EcdsaK256Keccak(sig);

        let (_key, values) =
            build_signature_advice_entry(commitment, message, &account_sig, Some(&pk));

        assert!(!values.is_empty());
    }

    #[test]
    fn ecdsa_advice_entry_key_matches_expected() {
        use miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey;

        let sk = EcdsaSecretKey::new();
        let pk = sk.public_key();
        let commitment = pk.to_commitment();
        let message = Word::from([Felt::new(1), Felt::new(2), Felt::new(3), Felt::new(4)]);
        let sig = sk.sign(message);
        let account_sig = AccountSignature::EcdsaK256Keccak(sig);

        let (key, _) = build_signature_advice_entry(commitment, message, &account_sig, Some(&pk));

        let mut elements = Vec::with_capacity(8);
        elements.extend_from_slice(commitment.as_elements());
        elements.extend_from_slice(message.as_elements());
        let expected: Word = Hasher::hash_elements(&elements);

        assert_eq!(key, expected);
    }

    #[test]
    fn build_ecdsa_signature_advice_entry_valid() {
        use miden_objects::crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey;

        let sk = EcdsaSecretKey::new();
        let pk = sk.public_key();
        let pk_hex = format!("0x{}", hex::encode(pk.to_bytes()));
        let commitment = pk.to_commitment();
        let message = Word::from([Felt::new(1), Felt::new(2), Felt::new(3), Felt::new(4)]);
        let sig = sk.sign(message);
        let account_sig = AccountSignature::EcdsaK256Keccak(sig);

        let result = build_ecdsa_signature_advice_entry(commitment, message, &account_sig, &pk_hex);
        assert!(result.is_ok());
        let (key, values) = result.unwrap();
        assert!(!values.is_empty());

        // key should match direct build
        let (expected_key, _) =
            build_signature_advice_entry(commitment, message, &account_sig, Some(&pk));
        assert_eq!(key, expected_key);
    }

    #[test]
    fn build_ecdsa_signature_advice_entry_invalid_hex() {
        let commitment = Word::default();
        let message = Word::default();

        let secret_key = SecretKey::new();
        let rpo_sig = secret_key.sign(message);
        let signature = AccountSignature::from(rpo_sig);

        let result =
            build_ecdsa_signature_advice_entry(commitment, message, &signature, "0xinvalid!!!");
        assert!(result.is_err());
    }

    #[test]
    fn build_ecdsa_signature_advice_entry_wrong_key_bytes() {
        let commitment = Word::default();
        let message = Word::default();

        let secret_key = SecretKey::new();
        let rpo_sig = secret_key.sign(message);
        let signature = AccountSignature::from(rpo_sig);

        // Valid hex but not a valid ECDSA public key
        let bad_hex = format!("0x{}", "ab".repeat(33));
        let result = build_ecdsa_signature_advice_entry(commitment, message, &signature, &bad_hex);
        assert!(result.is_err());
    }

    #[test]
    fn bytes_to_packed_u32_felts_basic() {
        let bytes = [1u8, 0, 0, 0, 2, 0, 0, 0];
        let felts = bytes_to_packed_u32_felts(&bytes);
        assert_eq!(felts.len(), 2);
        assert_eq!(felts[0], Felt::from(1u32));
        assert_eq!(felts[1], Felt::from(2u32));
    }

    #[test]
    fn bytes_to_packed_u32_felts_partial_chunk() {
        let bytes = [1u8, 2, 3]; // less than 4 bytes
        let felts = bytes_to_packed_u32_felts(&bytes);
        assert_eq!(felts.len(), 1);
        // [1, 2, 3, 0] as u32 little-endian = 0x00030201 = 197121
        assert_eq!(felts[0], Felt::from(0x00030201u32));
    }

    #[test]
    fn bytes_to_packed_u32_felts_empty() {
        let felts = bytes_to_packed_u32_felts(&[]);
        assert!(felts.is_empty());
    }
}
