use miden_protocol::account::AccountId;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{PublicKey, Signature};
use miden_protocol::crypto::hash::rpo::Rpo256;
use miden_protocol::utils::{Deserializable, Serializable};
use miden_protocol::{Felt, FieldElement, Word};

/// Verify an ECDSA secp256k1 signature for a request with timestamp.
///
/// The public key is recovered from the signature (secp256k1 supports recovery).
/// The recovered key's commitment is checked against the authorized list.
pub fn verify_request_signature(
    account_id: &str,
    timestamp: i64,
    authorized_commitments: &[String],
    signature: &str,
) -> Result<(), String> {
    let message = account_id_timestamp_to_digest(account_id, timestamp)?;
    let sig = parse_signature(signature)?;

    let public_key = PublicKey::recover_from(message, &sig).map_err(|e| {
        tracing::error!(account_id = %account_id, error = ?e, "ECDSA public key recovery failed");
        format!("ECDSA public key recovery failed: {e}")
    })?;

    let sig_pubkey_commitment = public_key.to_commitment();
    let sig_commitment_hex = format!("0x{}", hex::encode(sig_pubkey_commitment.to_bytes()));

    if !authorized_commitments.contains(&sig_commitment_hex) {
        tracing::error!(
            account_id = %account_id,
            sig_commitment = %sig_commitment_hex,
            authorized_count = authorized_commitments.len(),
            "ECDSA signature verification failed: public key commitment not authorized"
        );
        return Err(format!(
            "Signature verification failed: public key commitment '{}...' not authorized",
            &sig_commitment_hex[..18]
        ));
    }

    if public_key.verify(message, &sig) {
        Ok(())
    } else {
        tracing::error!(
            account_id = %account_id,
            timestamp = %timestamp,
            sig_commitment = %sig_commitment_hex,
            "ECDSA signature verification failed: invalid signature"
        );
        Err("Signature verification failed: invalid signature".to_string())
    }
}

/// Convert an account ID and timestamp to a message digest (Word)
///
/// Uses the same digest construction as Falcon to ensure consistency across schemes.
fn account_id_timestamp_to_digest(account_id_hex: &str, timestamp: i64) -> Result<Word, String> {
    let account_id = AccountId::from_hex(account_id_hex).map_err(|e| {
        tracing::error!(
            account_id = %account_id_hex,
            error = %e,
            "Invalid account ID hex in ECDSA account_id_timestamp_to_digest"
        );
        format!("Invalid account ID hex: {e}")
    })?;

    let account_id_felts: [Felt; 2] = account_id.into();
    let timestamp_felt = Felt::new(timestamp as u64);

    let message_elements = vec![
        account_id_felts[0],
        account_id_felts[1],
        timestamp_felt,
        Felt::ZERO,
    ];

    let digest = Rpo256::hash_elements(&message_elements);
    Ok(digest)
}

/// Parse a hex-encoded ECDSA signature
fn parse_signature(hex_str: &str) -> Result<Signature, String> {
    let hex_str = hex_str.trim_start_matches("0x");
    let bytes = hex::decode(hex_str).map_err(|e| {
        tracing::error!(
            signature = %hex_str,
            error = %e,
            "Invalid ECDSA signature hex"
        );
        format!("Invalid ECDSA signature hex: {e}")
    })?;
    Signature::read_from_bytes(&bytes).map_err(|e| {
        tracing::error!(
            error = %e,
            "Failed to deserialize ECDSA signature"
        );
        format!("Failed to deserialize ECDSA signature: {e}")
    })
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey;
    use miden_protocol::utils::Serializable;

    #[test]
    fn test_ecdsa_sign_and_verify_account_id_with_timestamp() {
        use miden_protocol::account::{AccountIdVersion, AccountStorageMode, AccountType};

        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();

        let account_id = AccountId::dummy(
            [0u8; 15],
            AccountIdVersion::Version0,
            AccountType::RegularAccountImmutableCode,
            AccountStorageMode::Private,
        );
        let account_id_hex = account_id.to_hex();
        let timestamp: i64 = 1700000000;

        let message = account_id_timestamp_to_digest(&account_id_hex, timestamp)
            .expect("Failed to create message digest");

        let signature = secret_key.sign(message);

        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));

        let signature_bytes = signature.to_bytes();
        let signature_hex = format!("0x{}", hex::encode(&signature_bytes));

        let result = verify_request_signature(
            &account_id_hex,
            timestamp,
            &[commitment_hex],
            &signature_hex,
        );

        assert!(
            result.is_ok(),
            "ECDSA signature verification should succeed: {result:?}"
        );
    }

    #[test]
    fn test_ecdsa_verify_with_wrong_pubkey() {
        use miden_protocol::account::{AccountIdVersion, AccountStorageMode, AccountType};

        let secret_key1 = SecretKey::new();
        let secret_key2 = SecretKey::new();
        let public_key2 = secret_key2.public_key();

        let account_id = AccountId::dummy(
            [1u8; 15],
            AccountIdVersion::Version0,
            AccountType::RegularAccountImmutableCode,
            AccountStorageMode::Private,
        );
        let account_id_hex = account_id.to_hex();
        let timestamp: i64 = 1700000000;

        let message = account_id_timestamp_to_digest(&account_id_hex, timestamp)
            .expect("Failed to create message digest");

        let signature = secret_key1.sign(message);

        let commitment2 = public_key2.to_commitment();
        let commitment2_hex = format!("0x{}", hex::encode(commitment2.to_bytes()));
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

        let result = verify_request_signature(
            &account_id_hex,
            timestamp,
            &[commitment2_hex],
            &signature_hex,
        );

        assert!(
            result.is_err(),
            "ECDSA signature verification should fail with wrong public key commitment"
        );
    }

    #[test]
    fn test_ecdsa_verify_with_wrong_timestamp() {
        use miden_protocol::account::{AccountIdVersion, AccountStorageMode, AccountType};

        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();

        let account_id = AccountId::dummy(
            [4u8; 15],
            AccountIdVersion::Version0,
            AccountType::RegularAccountImmutableCode,
            AccountStorageMode::Private,
        );
        let account_id_hex = account_id.to_hex();
        let timestamp1: i64 = 1700000000;
        let timestamp2: i64 = 1700000001;

        let message = account_id_timestamp_to_digest(&account_id_hex, timestamp1)
            .expect("Failed to create message digest");
        let signature = secret_key.sign(message);

        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

        let result = verify_request_signature(
            &account_id_hex,
            timestamp2,
            &[commitment_hex],
            &signature_hex,
        );

        assert!(
            result.is_err(),
            "ECDSA signature verification should fail with wrong timestamp"
        );
    }
}
