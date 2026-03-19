//! ECDSA secp256k1 signature-based authentication.

use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use miden_protocol::account::AccountId;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey;
use miden_protocol::utils::Serializable;

use super::miden_falcon_rpo::account_id_timestamp_to_word;

/// A signer that uses ECDSA secp256k1 signatures with RPO hashing.
pub struct EcdsaSigner {
    secret_key: std::sync::Mutex<SecretKey>,
    public_key_hex: String,
}

impl EcdsaSigner {
    /// Creates a new ECDSA signer from a secret key.
    pub fn new(secret_key: SecretKey) -> Self {
        let public_key = secret_key.public_key();
        let public_key_hex = format!("0x{}", hex::encode(public_key.to_bytes()));
        Self {
            secret_key: std::sync::Mutex::new(secret_key),
            public_key_hex,
        }
    }

    /// Returns the hex-encoded public key.
    pub fn public_key_hex(&self) -> String {
        self.public_key_hex.clone()
    }

    /// Signs an account ID with a timestamp and returns the hex-encoded signature.
    pub fn sign_account_id_with_timestamp(&self, account_id: &AccountId, timestamp: i64) -> String {
        let message = account_id_timestamp_to_word(*account_id, timestamp);
        let signature = self.secret_key.lock().unwrap().sign(message);
        format!("0x{}", hex::encode(signature.to_bytes()))
    }

    /// Signs the request-bound auth message used by authenticated GUARDIAN requests.
    pub fn sign_request_message(
        &self,
        account_id: &AccountId,
        timestamp: i64,
        payload: AuthRequestPayload,
    ) -> String {
        let message = AuthRequestMessage::new(*account_id, timestamp, payload).to_word();
        let signature = self.secret_key.lock().unwrap().sign(message);
        format!("0x{}", hex::encode(signature.to_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_protocol::utils::Deserializable;

    #[test]
    fn test_ecdsa_signer_creates_valid_signature_with_timestamp() {
        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();
        let signer = EcdsaSigner::new(secret_key);

        let account_id = AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f").unwrap();
        let timestamp: i64 = 1700000000;
        let signature_hex = signer.sign_account_id_with_timestamp(&account_id, timestamp);

        assert!(signature_hex.starts_with("0x"));

        // Verify the signature by recovering public key
        let sig_bytes = hex::decode(signature_hex.strip_prefix("0x").unwrap()).unwrap();
        let signature =
            miden_protocol::crypto::dsa::ecdsa_k256_keccak::Signature::read_from_bytes(&sig_bytes)
                .unwrap();

        let message = account_id_timestamp_to_word(account_id, timestamp);
        assert!(
            public_key.verify(message, &signature),
            "ECDSA signature verification failed"
        );
    }

    #[test]
    fn test_ecdsa_signer_creates_valid_request_bound_signature() {
        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();
        let signer = EcdsaSigner::new(secret_key);

        let account_id = AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f").unwrap();
        let timestamp: i64 = 1700000000;
        let payload = AuthRequestPayload::from_json_bytes(br#"{"op":"push_delta"}"#).unwrap();
        let signature_hex = signer.sign_request_message(&account_id, timestamp, payload.clone());

        assert!(signature_hex.starts_with("0x"));

        let sig_bytes = hex::decode(signature_hex.strip_prefix("0x").unwrap()).unwrap();
        let signature =
            miden_protocol::crypto::dsa::ecdsa_k256_keccak::Signature::read_from_bytes(&sig_bytes)
                .unwrap();

        let message = AuthRequestMessage::new(account_id, timestamp, payload).to_word();
        assert!(
            public_key.verify(message, &signature),
            "ECDSA request-bound signature verification failed"
        );
    }
}
