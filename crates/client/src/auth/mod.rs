//! Authentication types for PSM client requests.

pub mod miden_falcon_rpo;

pub use miden_falcon_rpo::{FalconRpoSigner, verify_commitment_signature};
use miden_objects::account::AccountId;

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

    /// Signs an account ID and returns the hex-encoded signature.
    pub fn sign_account_id(&self, account_id: &AccountId) -> String {
        match self {
            Auth::FalconRpoSigner(signer) => signer.sign_account_id(account_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::miden_falcon_rpo::IntoWord;
    use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;
    use miden_objects::crypto::dsa::rpo_falcon512::Signature;
    use miden_objects::utils::Deserializable;

    #[test]
    fn test_auth_enum_falcon_signer() {
        let secret_key = SecretKey::new();
        let public_key = secret_key.public_key();
        let auth = Auth::FalconRpoSigner(FalconRpoSigner::new(secret_key));

        let account_id = AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f").unwrap();
        let signature_hex = auth.sign_account_id(&account_id);

        assert!(signature_hex.starts_with("0x"));

        // Verify the signature is valid
        let sig_bytes = hex::decode(signature_hex.strip_prefix("0x").unwrap()).unwrap();
        let signature = Signature::read_from_bytes(&sig_bytes).unwrap();

        let message = account_id.into_word();

        // Verify signature with public key
        assert!(
            public_key.verify(message, &signature),
            "Signature verification failed"
        );
    }
}
