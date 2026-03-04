//! Authentication types for PSM client requests.

pub mod miden_falcon_rpo;

pub use miden_falcon_rpo::{
    FalconRpoSigner, account_id_request_to_word, verify_commitment_signature,
};
use miden_protocol::account::AccountId;
use private_state_manager_shared::auth_request_payload::AuthRequestPayload;

/// Authentication provider for PSM requests.
///
/// Wraps different signing implementations that can authenticate requests
/// to the PSM server.
pub enum Auth {
    /// Falcon-based authentication using RPO hashing.
    FalconRpoSigner(FalconRpoSigner),
}

impl Auth {
    /// Returns the hex-encoded public key for this authentication provider.
    pub fn public_key_hex(&self) -> String {
        match self {
            Auth::FalconRpoSigner(signer) => signer.public_key_hex(),
        }
    }

    /// Signs an authenticated request and returns the hex-encoded signature.
    pub fn sign_request(
        &self,
        account_id: &AccountId,
        timestamp: i64,
        request_payload: &AuthRequestPayload,
    ) -> String {
        match self {
            Auth::FalconRpoSigner(signer) => {
                signer.sign_request(account_id, timestamp, request_payload)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::miden_falcon_rpo::account_id_request_to_word;
    use miden_protocol::crypto::dsa::falcon512_rpo::SecretKey;
    use miden_protocol::crypto::dsa::falcon512_rpo::Signature;
    use miden_protocol::utils::Deserializable;
    use private_state_manager_shared::auth_request_payload::AuthRequestPayload;

    #[test]
    fn test_auth_enum_falcon_signer_with_timestamp() {
        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();
        let auth = Auth::FalconRpoSigner(FalconRpoSigner::new(secret_key));

        let account_id = AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f").unwrap();
        let timestamp: i64 = 1700000000;
        let payload =
            AuthRequestPayload::from_json_bytes(br#"{"op":"get_state"}"#).expect("valid payload");
        let signature_hex = auth.sign_request(&account_id, timestamp, &payload);

        assert!(signature_hex.starts_with("0x"));

        // Verify the signature is valid
        let sig_bytes = hex::decode(signature_hex.strip_prefix("0x").unwrap()).unwrap();
        let signature = Signature::read_from_bytes(&sig_bytes).unwrap();

        let message = account_id_request_to_word(account_id, timestamp, &payload);

        // Verify signature with public key
        assert!(
            public_key.verify(message, &signature),
            "Signature verification failed"
        );
    }
}
