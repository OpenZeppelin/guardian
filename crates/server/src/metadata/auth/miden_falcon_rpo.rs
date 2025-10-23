use miden_objects::account::AccountId;
use miden_objects::crypto::dsa::rpo_falcon512::{PublicKey, Signature};
use miden_objects::crypto::hash::rpo::Rpo256;
use miden_objects::utils::Deserializable;
use miden_objects::{Felt, FieldElement, Word};

/// Verify a Falcon RPO signature for a request
///
/// # Arguments
/// * `account_id` - The account ID (hex-encoded)
/// * `pubkey` - The public key (hex-encoded)
/// * `signature` - The signature to verify (hex-encoded)
pub fn verify_request_signature(
    account_id: &str,
    pubkey: &str,
    signature: &str,
) -> Result<(), String> {
    let message = account_id_to_digest(account_id)?;

    let pubkey = parse_public_key(pubkey)?;

    let sig = parse_signature(signature)?;

    if pubkey.verify(message, &sig) {
        Ok(())
    } else {
        Err("Invalid signature".to_string())
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
    let account_id =
        AccountId::from_hex(account_id_hex).map_err(|e| format!("Invalid account ID hex: {e}"))?;

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

/// Parse a hex-encoded public key
///
/// # Arguments
/// * `hex_str` - Hex-encoded public key, format: "0x" + 64 hex chars (32 bytes)
fn parse_public_key(hex_str: &str) -> Result<PublicKey, String> {
    let word = Word::try_from(hex_str).map_err(|e| format!("Invalid public key hex: {e}"))?;

    // Convert Word to PublicKey
    Ok(PublicKey::new(word))
}

/// Parse a hex-encoded signature
///
/// # Arguments
/// * `hex_str` - Hex-encoded signature
fn parse_signature(hex_str: &str) -> Result<Signature, String> {
    let hex_str = hex_str.trim_start_matches("0x");
    let bytes = hex::decode(hex_str).map_err(|e| format!("Invalid signature hex: {e}"))?;

    // Miden Signature is 1563 bytes when serialized:
    // 1 byte header + 40 bytes nonce + 625 bytes s2 polynomial + 897 bytes h polynomial
    const EXPECTED_SIG_LEN: usize = 1563;
    if bytes.len() != EXPECTED_SIG_LEN {
        return Err(format!(
            "Signature must be exactly {EXPECTED_SIG_LEN} bytes, got {} bytes",
            bytes.len()
        ));
    }

    Signature::read_from_bytes(&bytes).map_err(|e| format!("Failed to deserialize signature: {e}"))
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

        let pubkey_word: Word = public_key.into();
        let pubkey_hex = format!("0x{}", hex::encode(pubkey_word.to_bytes()));

        let signature_bytes = signature.to_bytes();
        let signature_hex = format!("0x{}", hex::encode(&signature_bytes));

        assert_eq!(
            signature_bytes.len(),
            1563,
            "Signature should be 1563 bytes"
        );

        let result = verify_request_signature(&account_id_hex, &pubkey_hex, &signature_hex);

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

        // Try to verify with public_key2 (wrong key)
        let pubkey_word: Word = public_key2.into();
        let pubkey_hex = format!("0x{}", hex::encode(pubkey_word.to_bytes()));
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

        let result = verify_request_signature(&account_id_hex, &pubkey_hex, &signature_hex);

        assert!(
            result.is_err(),
            "Signature verification should fail with wrong public key"
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

        // Try to verify signature for account_id2 (different message)
        let pubkey_word: Word = public_key.into();
        let pubkey_hex = format!("0x{}", hex::encode(pubkey_word.to_bytes()));
        let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));

        let result = verify_request_signature(&account_id2_hex, &pubkey_hex, &signature_hex);

        assert!(
            result.is_err(),
            "Signature verification should fail with wrong message"
        );
    }
}
