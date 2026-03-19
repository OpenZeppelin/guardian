use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use miden_protocol::Word;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{PublicKey, Signature};
use miden_protocol::utils::{Deserializable, Serializable};

/// Verify an ECDSA secp256k1 signature for a request with timestamp.
///
/// The public key is recovered from the signature when possible. If recovery
/// fails or yields a commitment outside the authorized set, the server falls
/// back to the caller-provided public key from `x-pubkey` for compatibility
/// with wallet providers that use a different recovery encoding.
pub fn verify_request_signature(
    account_id: &str,
    timestamp: i64,
    authorized_commitments: &[String],
    signature: &str,
    pubkey_hex: &str,
    request_payload: &AuthRequestPayload,
) -> Result<(), String> {
    let message = account_id_timestamp_to_digest(account_id, timestamp, request_payload)?;
    let sig = parse_signature(signature)?;

    let (public_key, commitment_hex) = resolve_authorized_public_key(
        account_id,
        authorized_commitments,
        &message,
        &sig,
        pubkey_hex,
    )?;

    verify_with_public_key(
        account_id,
        timestamp,
        &message,
        &sig,
        &public_key,
        &commitment_hex,
    )
}

/// Convert an account ID and timestamp to a message digest (Word)
///
/// Uses the same digest construction as Falcon to ensure consistency across schemes.
fn account_id_timestamp_to_digest(
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
                "Invalid account ID hex in ECDSA account_id_timestamp_to_digest"
            );
            e
        })
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

fn parse_public_key(hex_str: &str) -> Result<PublicKey, String> {
    let hex_str = hex_str.trim_start_matches("0x");
    let bytes = hex::decode(hex_str).map_err(|e| {
        tracing::error!(
            public_key = %hex_str,
            error = %e,
            "Invalid ECDSA public key hex"
        );
        format!("Invalid ECDSA public key hex: {e}")
    })?;
    PublicKey::read_from_bytes(&bytes).map_err(|e| {
        tracing::error!(
            error = %e,
            "Failed to deserialize ECDSA public key"
        );
        format!("Failed to deserialize ECDSA public key: {e}")
    })
}

fn resolve_authorized_public_key(
    account_id: &str,
    authorized_commitments: &[String],
    message: &Word,
    signature: &Signature,
    provided_pubkey_hex: &str,
) -> Result<(PublicKey, String), String> {
    if let Some(recovered_key) =
        recover_authorized_public_key(account_id, authorized_commitments, message, signature)
    {
        let commitment_hex = commitment_hex(&recovered_key);
        return Ok((recovered_key, commitment_hex));
    }

    authorized_public_key_from_header(account_id, authorized_commitments, provided_pubkey_hex)
}

fn recover_authorized_public_key(
    account_id: &str,
    authorized_commitments: &[String],
    message: &Word,
    signature: &Signature,
) -> Option<PublicKey> {
    match PublicKey::recover_from(*message, signature) {
        Ok(recovered_key) => {
            let recovered_commitment = commitment_hex(&recovered_key);
            if authorized_commitments.contains(&recovered_commitment) {
                return Some(recovered_key);
            }

            tracing::warn!(
                account_id = %account_id,
                recovered_commitment = %recovered_commitment,
                authorized_count = authorized_commitments.len(),
                "Recovered ECDSA public key commitment not authorized; trying provided x-pubkey"
            );
            None
        }
        Err(_) => {
            tracing::warn!(
                account_id = %account_id,
                "ECDSA public key recovery failed; trying provided x-pubkey"
            );
            None
        }
    }
}

fn authorized_public_key_from_header(
    account_id: &str,
    authorized_commitments: &[String],
    provided_pubkey_hex: &str,
) -> Result<(PublicKey, String), String> {
    let provided_key = parse_public_key(provided_pubkey_hex)?;
    let provided_commitment = commitment_hex(&provided_key);

    if !authorized_commitments.contains(&provided_commitment) {
        tracing::error!(
            account_id = %account_id,
            provided_commitment = %provided_commitment,
            authorized_count = authorized_commitments.len(),
            "ECDSA signature verification failed: provided public key commitment not authorized"
        );
        return Err(format!(
            "Signature verification failed: public key commitment '{}...' not authorized",
            &provided_commitment[..18]
        ));
    }

    Ok((provided_key, provided_commitment))
}

fn commitment_hex(public_key: &PublicKey) -> String {
    format!("0x{}", hex::encode(public_key.to_commitment().to_bytes()))
}

fn verify_with_public_key(
    account_id: &str,
    timestamp: i64,
    message: &Word,
    signature: &Signature,
    public_key: &PublicKey,
    commitment_hex: &str,
) -> Result<(), String> {
    if public_key.verify(*message, signature) {
        Ok(())
    } else {
        tracing::error!(
            account_id = %account_id,
            timestamp = %timestamp,
            sig_commitment = %commitment_hex,
            "ECDSA signature verification failed: invalid signature"
        );
        Err("Signature verification failed: invalid signature".to_string())
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use miden_protocol::account::AccountId;
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
        let request_payload = AuthRequestPayload::empty();

        let message = account_id_timestamp_to_digest(&account_id_hex, timestamp, &request_payload)
            .expect("Failed to create message digest");

        let signature = secret_key.sign(message);

        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));

        let signature_bytes = signature.to_bytes();
        let signature_hex = format!("0x{}", hex::encode(&signature_bytes));
        let pubkey_hex = format!("0x{}", hex::encode(public_key.to_bytes()));

        let result = verify_request_signature(
            &account_id_hex,
            timestamp,
            &[commitment_hex],
            &signature_hex,
            &pubkey_hex,
            &request_payload,
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
        let request_payload = AuthRequestPayload::empty();

        let message = account_id_timestamp_to_digest(&account_id_hex, timestamp, &request_payload)
            .expect("Failed to create message digest");

        let signature = secret_key1.sign(message);

        let commitment2 = public_key2.to_commitment();
        let commitment2_hex = format!("0x{}", hex::encode(commitment2.to_bytes()));
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));
        let pubkey_hex = format!("0x{}", hex::encode(public_key2.to_bytes()));

        let result = verify_request_signature(
            &account_id_hex,
            timestamp,
            &[commitment2_hex],
            &signature_hex,
            &pubkey_hex,
            &request_payload,
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
        let request_payload = AuthRequestPayload::empty();

        let message = account_id_timestamp_to_digest(&account_id_hex, timestamp1, &request_payload)
            .expect("Failed to create message digest");
        let signature = secret_key.sign(message);

        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));
        let pubkey_hex = format!("0x{}", hex::encode(public_key.to_bytes()));

        let result = verify_request_signature(
            &account_id_hex,
            timestamp2,
            &[commitment_hex],
            &signature_hex,
            &pubkey_hex,
            &request_payload,
        );

        assert!(
            result.is_err(),
            "ECDSA signature verification should fail with wrong timestamp"
        );
    }

    #[test]
    fn test_ecdsa_verify_falls_back_to_provided_pubkey_when_recovery_commitment_mismatches() {
        use miden_protocol::account::{AccountIdVersion, AccountStorageMode, AccountType};

        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();

        let account_id = AccountId::dummy(
            [7u8; 15],
            AccountIdVersion::Version0,
            AccountType::RegularAccountImmutableCode,
            AccountStorageMode::Private,
        );
        let account_id_hex = account_id.to_hex();
        let timestamp: i64 = 1700000000;
        let request_payload = AuthRequestPayload::empty();

        let message = account_id_timestamp_to_digest(&account_id_hex, timestamp, &request_payload)
            .expect("Failed to create message digest");
        let signature = secret_key.sign(message);

        let mut signature_bytes = signature.to_bytes();
        let last_index = signature_bytes.len() - 1;
        signature_bytes[last_index] ^= 1;

        let commitment_hex = format!("0x{}", hex::encode(public_key.to_commitment().to_bytes()));
        let signature_hex = format!("0x{}", hex::encode(signature_bytes));
        let pubkey_hex = format!("0x{}", hex::encode(public_key.to_bytes()));

        let result = verify_request_signature(
            &account_id_hex,
            timestamp,
            &[commitment_hex],
            &signature_hex,
            &pubkey_hex,
            &request_payload,
        );

        assert!(
            result.is_ok(),
            "ECDSA signature verification should fall back to provided pubkey: {result:?}"
        );
    }

    #[test]
    fn test_ecdsa_verify_with_wrong_payload() {
        use miden_protocol::account::{AccountIdVersion, AccountStorageMode, AccountType};

        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();

        let account_id = AccountId::dummy(
            [8u8; 15],
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
        let pubkey_hex = format!("0x{}", hex::encode(public_key.to_bytes()));

        let result = verify_request_signature(
            &account_id_hex,
            timestamp,
            &[commitment_hex],
            &signature_hex,
            &pubkey_hex,
            &wrong_payload,
        );

        assert!(result.is_err());
    }
}
