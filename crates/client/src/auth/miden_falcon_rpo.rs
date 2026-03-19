//! Falcon signature-based authentication using request-bound payload hashing.

use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::hex::{FromHex, IntoHex};
use miden_protocol::account::AccountId;
use miden_protocol::crypto::dsa::falcon512_rpo::{PublicKey, SecretKey, Signature};
use miden_protocol::crypto::hash::rpo::Rpo256;
use miden_protocol::utils::{Deserializable, Serializable};
use miden_protocol::{Felt, FieldElement, Word};

/// A signer that uses Falcon signatures with RPO hashing.
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

    /// Signs the legacy account ID + timestamp digest.
    pub fn sign_account_id_with_timestamp(&self, account_id: &AccountId, timestamp: i64) -> String {
        let message = account_id_timestamp_to_word(*account_id, timestamp);
        let signature = self.secret_key.sign(message);
        signature.into_hex()
    }

    /// Signs the request-bound auth message used by the audited server path.
    pub fn sign_request_message(
        &self,
        account_id: &AccountId,
        timestamp: i64,
        payload: AuthRequestPayload,
    ) -> String {
        let message = AuthRequestMessage::new(*account_id, timestamp, payload).to_word();
        self.secret_key.sign(message).into_hex()
    }
}

/// Converts an account ID and timestamp to a Word for signing.
pub fn account_id_timestamp_to_word(account_id: AccountId, timestamp: i64) -> Word {
    let account_id_felts: [Felt; 2] = account_id.into();
    let timestamp_felt = Felt::new(timestamp as u64);

    let message_elements = vec![
        account_id_felts[0],
        account_id_felts[1],
        timestamp_felt,
        Felt::ZERO,
    ];

    Rpo256::hash_elements(&message_elements)
}

/// Verifies a signature using commitment-based authentication.
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

trait HexIntoWord {
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
    fn test_falcon_signer_creates_valid_signature_with_timestamp() {
        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();
        let signer = FalconRpoSigner::new(secret_key);

        let account_id = AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f").unwrap();
        let timestamp: i64 = 1700000000;
        let signature_hex = signer.sign_account_id_with_timestamp(&account_id, timestamp);

        assert!(signature_hex.starts_with("0x"));

        let sig_bytes = hex::decode(signature_hex.strip_prefix("0x").unwrap()).unwrap();
        let signature = Signature::read_from_bytes(&sig_bytes).unwrap();
        let message = account_id_timestamp_to_word(account_id, timestamp);

        assert!(public_key.verify(message, &signature));
    }

    #[test]
    fn test_request_bound_signature_verifies_against_request_message() {
        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();
        let signer = FalconRpoSigner::new(secret_key);

        let account_id = AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f").unwrap();
        let timestamp: i64 = 1700000000;
        let payload = AuthRequestPayload::from_json_bytes(br#"{"op":"push_delta"}"#).unwrap();

        let signature_hex = signer.sign_request_message(&account_id, timestamp, payload.clone());
        let sig_bytes = hex::decode(signature_hex.strip_prefix("0x").unwrap()).unwrap();
        let signature = Signature::read_from_bytes(&sig_bytes).unwrap();
        let message = AuthRequestMessage::new(account_id, timestamp, payload).to_word();

        assert!(public_key.verify(message, &signature));
    }
}
