use miden_objects::account::AccountId;
use miden_objects::crypto::dsa::rpo_falcon512::Signature;
use miden_objects::crypto::hash::rpo::Rpo256;
use miden_objects::utils::{Deserializable, Serializable};
use miden_objects::{Felt, FieldElement, Word};

/// Verify a Falcon RPO signature for a request
///
/// # Arguments
/// * `account_id` - The account ID (hex-encoded)
/// * `authorized_commitments` - List of authorized public key commitments
/// * `signature` - The signature to verify
pub fn verify_request_signature(
    account_id: &str,
    authorized_commitments: &[String],
    signature: &str,
) -> Result<(), String> {
    let message = account_id_to_digest(account_id)?;
    let sig = parse_signature(signature)?;

    // Extract the public key from the signature
    let public_key = sig.public_key();

    // Compute the commitment of the extracted public key
    let sig_pubkey_commitment = public_key.to_commitment();
    let sig_commitment_hex = format!("0x{}", hex::encode(sig_pubkey_commitment.to_bytes()));

    // Check if this commitment is in the authorized list
    if !authorized_commitments.contains(&sig_commitment_hex) {
        tracing::error!(
            account_id = %account_id,
            sig_commitment = %sig_commitment_hex,
            authorized_count = authorized_commitments.len(),
            "Signature verification failed: public key commitment not authorized"
        );
        return Err(format!(
            "Signature verification failed: public key commitment '{}...' not authorized",
            &sig_commitment_hex[..18]
        ));
    }

    // Verify the signature cryptographically
    if public_key.verify(message, &sig) {
        Ok(())
    } else {
        tracing::error!(
            account_id = %account_id,
            sig_commitment = %sig_commitment_hex,
            "Signature verification failed: invalid signature"
        );
        Err("Signature verification failed: invalid signature".to_string())
    }
}

/// Convert an account ID hex string to a message digest (Word)
///
/// This parses the account ID from hex format and converts it to its
/// field element representation, which is then hashed to produce the
/// message to be signed.
///
/// # Arguments
/// * `account_id_hex` - The account ID in hex format (e.g., "0x1234...")
fn account_id_to_digest(account_id_hex: &str) -> Result<Word, String> {
    let account_id = AccountId::from_hex(account_id_hex).map_err(|e| {
        tracing::error!(
            account_id = %account_id_hex,
            error = %e,
            "Invalid account ID hex in account_id_to_digest"
        );
        format!("Invalid account ID hex: {e}")
    })?;

    // Convert AccountId to its field element representation [prefix, suffix]
    let account_id_felts: [Felt; 2] = account_id.into();

    // We use 4 elements to fill a full rate (pad with zeros)
    let message_elements = vec![
        account_id_felts[0],
        account_id_felts[1],
        Felt::ZERO,
        Felt::ZERO,
    ];

    // Hash using RPO256 and return as Word
    let digest = Rpo256::hash_elements(&message_elements);
    Ok(digest)
}

/// Parse a hex-encoded signature
fn parse_signature(hex_str: &str) -> Result<Signature, String> {
    let hex_str = hex_str.trim_start_matches("0x");
    let bytes = hex::decode(hex_str).map_err(|e| {
        tracing::error!(
            signature = %hex_str,
            error = %e,
            "Invalid signature hex"
        );
        format!("Invalid signature hex: {e}")
    })?;
    Signature::read_from_bytes(&bytes).map_err(|e| {
        tracing::error!(
            error = %e,
            "Failed to deserialize signature"
        );
        format!("Failed to deserialize signature: {e}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;
    use miden_objects::utils::Serializable;

    #[test]
    fn test_falcon_sign_and_verify_account_id() {
        use miden_objects::account::{AccountIdVersion, AccountStorageMode, AccountType};

        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();

        let account_id = AccountId::dummy(
            [0u8; 15],
            AccountIdVersion::Version0,
            AccountType::RegularAccountImmutableCode,
            AccountStorageMode::Private,
        );
        let account_id_hex = account_id.to_hex();

        let message =
            account_id_to_digest(&account_id_hex).expect("Failed to create message digest");

        let signature = secret_key.sign(message);

        // Compute commitment from public key
        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));

        let signature_bytes = signature.to_bytes();
        let signature_hex = format!("0x{}", hex::encode(&signature_bytes));

        let result = verify_request_signature(&account_id_hex, &[commitment_hex], &signature_hex);

        assert!(
            result.is_ok(),
            "Signature verification should succeed: {result:?}"
        );
    }

    #[test]
    fn test_falcon_verify_with_wrong_pubkey() {
        use miden_objects::account::{AccountIdVersion, AccountStorageMode, AccountType};

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

        let message =
            account_id_to_digest(&account_id_hex).expect("Failed to create message digest");

        // Sign with secret_key1
        let signature = secret_key1.sign(message);

        // Try to verify with commitment from public_key2 (wrong key)
        let commitment2 = public_key2.to_commitment();
        let commitment2_hex = format!("0x{}", hex::encode(commitment2.to_bytes()));
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

        let result = verify_request_signature(&account_id_hex, &[commitment2_hex], &signature_hex);

        assert!(
            result.is_err(),
            "Signature verification should fail with wrong public key commitment"
        );
    }

    #[test]
    fn test_falcon_verify_with_wrong_message() {
        use miden_objects::account::{AccountIdVersion, AccountStorageMode, AccountType};

        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();

        let account_id1 = AccountId::dummy(
            [2u8; 15],
            AccountIdVersion::Version0,
            AccountType::RegularAccountImmutableCode,
            AccountStorageMode::Private,
        );
        let account_id2 = AccountId::dummy(
            [3u8; 15],
            AccountIdVersion::Version0,
            AccountType::RegularAccountImmutableCode,
            AccountStorageMode::Private,
        );
        let account_id1_hex = account_id1.to_hex();
        let account_id2_hex = account_id2.to_hex();

        // Sign account_id1
        let message1 =
            account_id_to_digest(&account_id1_hex).expect("Failed to create message digest");
        let signature = secret_key.sign(message1);

        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

        // Try to verify with account_id2 (wrong message)
        let result = verify_request_signature(&account_id2_hex, &[commitment_hex], &signature_hex);

        assert!(
            result.is_err(),
            "Signature verification should fail with wrong message"
        );
    }
}
