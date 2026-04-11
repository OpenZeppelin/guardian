use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use miden_protocol::Word;
use miden_protocol::crypto::dsa::falcon512_poseidon2::Signature;
use miden_protocol::utils::serde::{Deserializable, Serializable};

/// Verify a Falcon RPO signature for a request with timestamp.
///
/// # Arguments
/// * `account_id` - The account ID (hex-encoded)
/// * `timestamp` - Unix timestamp included in the signed payload
/// * `authorized_commitments` - List of authorized public key commitments
/// * `signature` - The signature to verify
/// * `request_payload` - Request payload digest to bind request intent
pub fn verify_request_signature(
    account_id: &str,
    timestamp: i64,
    authorized_commitments: &[String],
    signature: &str,
    request_payload: &AuthRequestPayload,
) -> Result<(), String> {
    let message = account_id_timestamp_to_digest(account_id, timestamp, request_payload)?;
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
            timestamp = %timestamp,
            sig_commitment = %sig_commitment_hex,
            "Signature verification failed: invalid signature"
        );
        Err("Signature verification failed: invalid signature".to_string())
    }
}

/// Convert account ID + timestamp + request payload to a message digest (Word)
///
/// This parses the account ID from hex format and combines it with the timestamp
/// to produce a unique message for signing that prevents replay attacks and binds
/// the signature to a specific request payload.
///
/// # Arguments
/// * `account_id_hex` - The account ID in hex format (e.g., "0x1234...")
/// * `timestamp` - Unix timestamp in milliseconds
pub fn account_id_timestamp_to_digest(
    account_id_hex: &str,
    timestamp: i64,
    request_payload: &AuthRequestPayload,
) -> Result<Word, String> {
    AuthRequestMessage::from_account_id_hex(account_id_hex, timestamp, request_payload.clone())
        .map(|request| request.to_word())
        .map_err(|e| {
            tracing::error!(
                account_id = %account_id_hex,
                error = %e,
                "Invalid account ID hex in account_id_timestamp_to_digest"
            );
            e
        })
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

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use miden_protocol::account::AccountId;
    use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
    use miden_protocol::utils::serde::Serializable;

    #[test]
    fn test_falcon_sign_and_verify_account_id_with_timestamp() {
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
        let timestamp: i64 = 1700000000; // Fixed timestamp for testing

        let request_payload = AuthRequestPayload::empty();
        let message = account_id_timestamp_to_digest(&account_id_hex, timestamp, &request_payload)
            .expect("Failed to create message digest");

        let signature = secret_key.sign(message);

        // Compute commitment from public key
        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));

        let signature_bytes = signature.to_bytes();
        let signature_hex = format!("0x{}", hex::encode(&signature_bytes));

        let result = verify_request_signature(
            &account_id_hex,
            timestamp,
            &[commitment_hex],
            &signature_hex,
            &request_payload,
        );

        assert!(
            result.is_ok(),
            "Signature verification should succeed: {result:?}"
        );
    }

    #[test]
    fn test_falcon_verify_with_wrong_pubkey() {
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

        let request_payload = AuthRequestPayload::empty();
        let message = account_id_timestamp_to_digest(&account_id_hex, timestamp, &request_payload)
            .expect("Failed to create message digest");

        // Sign with secret_key1
        let signature = secret_key1.sign(message);

        // Try to verify with commitment from public_key2 (wrong key)
        let commitment2 = public_key2.to_commitment();
        let commitment2_hex = format!("0x{}", hex::encode(commitment2.to_bytes()));
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

        let result = verify_request_signature(
            &account_id_hex,
            timestamp,
            &[commitment2_hex],
            &signature_hex,
            &request_payload,
        );

        assert!(
            result.is_err(),
            "Signature verification should fail with wrong public key commitment"
        );
    }

    #[test]
    fn test_falcon_verify_with_wrong_message() {
        use miden_protocol::account::{AccountIdVersion, AccountStorageMode, AccountType};

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
        let timestamp: i64 = 1700000000;

        // Sign account_id1
        let request_payload = AuthRequestPayload::empty();
        let message1 =
            account_id_timestamp_to_digest(&account_id1_hex, timestamp, &request_payload)
                .expect("Failed to create message digest");
        let signature = secret_key.sign(message1);

        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

        // Try to verify with account_id2 (wrong message)
        let result = verify_request_signature(
            &account_id2_hex,
            timestamp,
            &[commitment_hex],
            &signature_hex,
            &request_payload,
        );

        assert!(
            result.is_err(),
            "Signature verification should fail with wrong message"
        );
    }

    #[test]
    fn test_falcon_verify_with_wrong_timestamp() {
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
        let timestamp2: i64 = 1700000001; // Different timestamp

        let request_payload = AuthRequestPayload::empty();
        let message = account_id_timestamp_to_digest(&account_id_hex, timestamp1, &request_payload)
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
            &request_payload,
        );

        assert!(
            result.is_err(),
            "Signature verification should fail with wrong timestamp"
        );
    }

    #[test]
    fn test_falcon_verify_with_wrong_payload() {
        use miden_protocol::account::{AccountIdVersion, AccountStorageMode, AccountType};

        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();

        let account_id = AccountId::dummy(
            [5u8; 15],
            AccountIdVersion::Version0,
            AccountType::RegularAccountImmutableCode,
            AccountStorageMode::Private,
        );
        let account_id_hex = account_id.to_hex();
        let timestamp: i64 = 1700000000;

        let signed_payload = AuthRequestPayload::from_json_bytes(br#"{"op":"get_delta"}"#)
            .expect("valid signed payload");
        let wrong_payload = AuthRequestPayload::from_json_bytes(br#"{"op":"push_delta"}"#)
            .expect("valid wrong payload");
        let message = account_id_timestamp_to_digest(&account_id_hex, timestamp, &signed_payload)
            .expect("Failed to create message digest");
        let signature = secret_key.sign(message);

        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

        let result = verify_request_signature(
            &account_id_hex,
            timestamp,
            &[commitment_hex],
            &signature_hex,
            &wrong_payload,
        );

        assert!(
            result.is_err(),
            "Signature verification should fail with wrong payload"
        );
    }
}
