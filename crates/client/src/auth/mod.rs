//! Authentication types for PSM client requests.

pub mod miden_ecdsa;
pub mod miden_falcon_rpo;

pub use miden_ecdsa::EcdsaSigner;
pub use miden_falcon_rpo::{
    FalconRpoSigner, account_id_timestamp_to_word, verify_commitment_signature,
};

use miden_protocol::account::AccountId;
use private_state_manager_shared::auth_request_payload::AuthRequestPayload;

/// Authentication provider for PSM requests.
pub enum Auth {
    /// Falcon-based authentication using RPO hashing.
    FalconRpoSigner(FalconRpoSigner),
    /// ECDSA secp256k1-based authentication.
    EcdsaSigner(EcdsaSigner),
}

impl Auth {
    /// Returns the hex-encoded public key for this authentication provider.
    pub fn public_key_hex(&self) -> String {
        match self {
            Auth::FalconRpoSigner(signer) => signer.public_key_hex(),
            Auth::EcdsaSigner(signer) => signer.public_key_hex(),
        }
    }

    /// Signs an account ID with a timestamp.
    ///
    /// This compatibility helper is kept for callers that still construct the
    /// legacy digest without binding request bytes.
    pub fn sign_account_id_with_timestamp(&self, account_id: &AccountId, timestamp: i64) -> String {
        match self {
            Auth::FalconRpoSigner(signer) => {
                signer.sign_account_id_with_timestamp(account_id, timestamp)
            }
            Auth::EcdsaSigner(signer) => {
                signer.sign_account_id_with_timestamp(account_id, timestamp)
            }
        }
    }

    /// Signs the request-bound auth message for this request.
    pub fn sign_request_message(
        &self,
        account_id: &AccountId,
        timestamp: i64,
        payload: AuthRequestPayload,
    ) -> String {
        match self {
            Auth::FalconRpoSigner(signer) => {
                signer.sign_request_message(account_id, timestamp, payload)
            }
            Auth::EcdsaSigner(signer) => {
                signer.sign_request_message(account_id, timestamp, payload)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::miden_falcon_rpo::account_id_timestamp_to_word;
    use miden_protocol::crypto::dsa::falcon512_rpo::SecretKey;
    use miden_protocol::crypto::dsa::falcon512_rpo::Signature;
    use miden_protocol::utils::Deserializable;
    use private_state_manager_shared::auth_request_message::AuthRequestMessage;
    use private_state_manager_shared::auth_request_payload::AuthRequestPayload;

    #[test]
    fn test_auth_enum_falcon_signer_with_timestamp() {
        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();
        let auth = Auth::FalconRpoSigner(FalconRpoSigner::new(secret_key));

        let account_id = AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f").unwrap();
        let timestamp: i64 = 1700000000;
        let signature_hex = auth.sign_account_id_with_timestamp(&account_id, timestamp);

        assert!(signature_hex.starts_with("0x"));

        let sig_bytes = hex::decode(signature_hex.strip_prefix("0x").unwrap()).unwrap();
        let signature = Signature::read_from_bytes(&sig_bytes).unwrap();
        let message = account_id_timestamp_to_word(account_id, timestamp);

        assert!(public_key.verify(message, &signature));
    }

    #[test]
    fn test_auth_enum_ecdsa_request_bound_signing() {
        let secret_key = miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey::new();
        let public_key = secret_key.public_key();
        let auth = Auth::EcdsaSigner(EcdsaSigner::new(secret_key));

        let account_id = AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f").unwrap();
        let timestamp: i64 = 1700000000;
        let payload = AuthRequestPayload::from_json_bytes(br#"{"op":"push_delta"}"#).unwrap();
        let signature_hex = auth.sign_request_message(&account_id, timestamp, payload.clone());

        let sig_bytes = hex::decode(signature_hex.strip_prefix("0x").unwrap()).unwrap();
        let signature =
            miden_protocol::crypto::dsa::ecdsa_k256_keccak::Signature::read_from_bytes(&sig_bytes)
                .unwrap();
        let message = AuthRequestMessage::new(account_id, timestamp, payload).to_word();

        assert!(public_key.verify(message, &signature));
    }
}
