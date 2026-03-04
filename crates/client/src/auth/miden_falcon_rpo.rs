//! Falcon signature-based authentication using RPO hashing.

use miden_protocol::account::AccountId;
use miden_protocol::crypto::dsa::falcon512_rpo::{PublicKey, SecretKey, Signature};
use miden_protocol::crypto::hash::rpo::Rpo256;
use miden_protocol::utils::{Deserializable, Serializable};
use miden_protocol::{Felt, FieldElement, Word};
use private_state_manager_shared::auth_request_message::AuthRequestMessage;
use private_state_manager_shared::auth_request_payload::AuthRequestPayload;
use private_state_manager_shared::hex::{FromHex, IntoHex};

/// A signer that uses Falcon signatures with RPO hashing.
///
/// This is the primary authentication mechanism for PSM requests,
/// compatible with Miden's native signature scheme.
pub struct FalconRpoSigner {
    secret_key: SecretKey,
    public_key: PublicKey,
}

impl FalconRpoSigner {
    /// Creates a new signer from a Falcon secret key.
    pub fn new(secret_key: SecretKey) -> Self {
        let public_key = secret_key.public_key();
        Self {
            secret_key,
            public_key,
        }
    }

    /// Returns the hex-encoded public key.
    pub fn public_key_hex(&self) -> String {
        (&self.public_key).into_hex()
    }

    /// Signs an authenticated request and returns the hex-encoded signature.
    pub fn sign_request(
        &self,
        account_id: &AccountId,
        timestamp: i64,
        request_payload: &AuthRequestPayload,
    ) -> String {
        let message = account_id_request_to_word(*account_id, timestamp, request_payload);
        let signature = self.secret_key.sign(message);
        signature.into_hex()
    }
}

/// Converts account id + timestamp + request payload to a signing message word.
pub fn account_id_request_to_word(
    account_id: AccountId,
    timestamp: i64,
    request_payload: &AuthRequestPayload,
) -> Word {
    AuthRequestMessage::new(account_id, timestamp, request_payload.clone()).to_word()
}

/// Trait for converting types to a [`Word`] for signing.
pub trait IntoWord {
    /// Converts this value into a Word suitable for signing.
    fn into_word(self) -> Word;
}

impl IntoWord for AccountId {
    fn into_word(self) -> Word {
        let account_id_felts: [Felt; 2] = self.into();

        let message_elements = vec![
            account_id_felts[0],
            account_id_felts[1],
            Felt::ZERO,
            Felt::ZERO,
        ];

        Rpo256::hash_elements(&message_elements)
    }
}

/// Verifies a signature using commitment-based authentication.
///
/// This function verifies that a signature was created by a key whose
/// commitment matches the expected server commitment.
pub fn verify_commitment_signature(
    commitment_hex: &str,
    server_commitment_hex: &str,
    signature_hex: &str,
) -> Result<bool, String> {
    let message = commitment_hex.hex_into_word()?;
    let signature = Signature::from_hex(signature_hex)?;

    let pubkey = signature.public_key();
    let sig_pubkey_commitment = pubkey.to_commitment();
    let sig_commitment_hex = format!("0x{}", hex::encode(sig_pubkey_commitment.to_bytes()));

    if sig_commitment_hex != server_commitment_hex {
        return Ok(false);
    }

    Ok(pubkey.verify(message, &signature))
}

/// Trait for parsing hex strings into [`Word`] values.
pub trait HexIntoWord {
    /// Parses this hex string into a Word.
    fn hex_into_word(self) -> Result<Word, String>;
}

impl HexIntoWord for &str {
    fn hex_into_word(self) -> Result<Word, String> {
        let commitment_hex = self.strip_prefix("0x").unwrap_or(self);

        let bytes =
            hex::decode(commitment_hex).map_err(|e| format!("Invalid commitment hex: {e}"))?;

        if bytes.len() != 32 {
            return Err(format!("Commitment must be 32 bytes, got {}", bytes.len()));
        }

        Word::read_from_bytes(&bytes)
            .map_err(|e| format!("Failed to deserialize Word from bytes: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_falcon_signer_creates_valid_signature_for_request() {
        use miden_protocol::utils::Deserializable;

        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();
        let signer = FalconRpoSigner::new(secret_key);

        let account_id = AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f").unwrap();
        let timestamp: i64 = 1700000000;
        let payload =
            AuthRequestPayload::from_json_bytes(br#"{"op":"get_state"}"#).expect("valid payload");
        let signature_hex = signer.sign_request(&account_id, timestamp, &payload);

        assert!(signature_hex.starts_with("0x"));

        let sig_bytes = hex::decode(signature_hex.strip_prefix("0x").unwrap()).unwrap();
        let signature = Signature::read_from_bytes(&sig_bytes).unwrap();
        let message = account_id_request_to_word(account_id, timestamp, &payload);

        assert!(
            public_key.verify(message, &signature),
            "Signature verification failed"
        );
    }

    #[test]
    fn test_signature_fails_with_wrong_timestamp() {
        use miden_protocol::utils::Deserializable;

        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();
        let signer = FalconRpoSigner::new(secret_key);

        let account_id = AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f").unwrap();
        let timestamp1: i64 = 1700000000;
        let timestamp2: i64 = 1700000001;
        let payload =
            AuthRequestPayload::from_json_bytes(br#"{"op":"get_state"}"#).expect("valid payload");
        let signature_hex = signer.sign_request(&account_id, timestamp1, &payload);

        let sig_bytes = hex::decode(signature_hex.strip_prefix("0x").unwrap()).unwrap();
        let signature = Signature::read_from_bytes(&sig_bytes).unwrap();
        let wrong_message = account_id_request_to_word(account_id, timestamp2, &payload);

        assert!(
            !public_key.verify(wrong_message, &signature),
            "Signature verification should fail with wrong timestamp"
        );
    }

    #[test]
    fn test_public_key_from_hex_roundtrip() {
        let secret_key = SecretKey::new();
        let original_pubkey = secret_key.public_key();
        let hex = original_pubkey.into_hex();
        let parsed_pubkey = PublicKey::from_hex(&hex).expect("Failed to parse public key from hex");
        let parsed_hex = parsed_pubkey.into_hex();
        assert_eq!(
            hex, parsed_hex,
            "Roundtrip should produce identical public key"
        );
    }

    #[test]
    fn test_signature_from_hex_roundtrip() {
        let secret_key = SecretKey::new();
        let account_id = AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f").unwrap();
        let timestamp: i64 = 1700000000;
        let payload = AuthRequestPayload::empty();
        let message = account_id_request_to_word(account_id, timestamp, &payload);
        let original_sig = secret_key.sign(message);
        let hex = original_sig.into_hex();
        let parsed_sig = Signature::from_hex(&hex).expect("Failed to parse signature from hex");
        let parsed_hex = parsed_sig.into_hex();
        assert_eq!(
            hex, parsed_hex,
            "Roundtrip should produce identical signature"
        );
    }

    #[test]
    fn test_from_hex_without_prefix() {
        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();
        let hex_with_prefix = public_key.into_hex();
        let hex_without_prefix = hex_with_prefix.strip_prefix("0x").unwrap();
        let pubkey1 = PublicKey::from_hex(&hex_with_prefix).unwrap();
        let pubkey2 = PublicKey::from_hex(hex_without_prefix).unwrap();

        assert_eq!(
            pubkey1.into_hex(),
            pubkey2.into_hex(),
            "Parsing with and without 0x prefix should produce same result"
        );
    }

    #[test]
    fn test_signature_fails_with_wrong_payload() {
        use miden_protocol::utils::Deserializable;

        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();
        let signer = FalconRpoSigner::new(secret_key);

        let account_id = AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f").unwrap();
        let timestamp: i64 = 1700000000;
        let signed_payload = AuthRequestPayload::from_json_bytes(br#"{"op":"get_delta"}"#)
            .expect("valid signed payload");
        let wrong_payload = AuthRequestPayload::from_json_bytes(br#"{"op":"push_delta"}"#)
            .expect("valid wrong payload");
        let signature_hex = signer.sign_request(&account_id, timestamp, &signed_payload);

        let sig_bytes = hex::decode(signature_hex.strip_prefix("0x").unwrap()).unwrap();
        let signature = Signature::read_from_bytes(&sig_bytes).unwrap();

        let wrong_message = account_id_request_to_word(account_id, timestamp, &wrong_payload);
        assert!(
            !public_key.verify(wrong_message, &signature),
            "Signature verification should fail with wrong payload"
        );
    }
}
