use guardian_shared::SignatureScheme;
use miden_protocol::Word;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey;
use miden_protocol::utils::serde::Serializable;

use super::Signer;

/// In-memory ECDSA signer used for GUARDIAN authentication and multisig signing.
pub struct EcdsaKeyStore {
    secret_key: std::sync::Mutex<SecretKey>,
    commitment: Word,
    commitment_hex: String,
    public_key_hex: String,
}

impl EcdsaKeyStore {
    /// Creates a new signer from an ECDSA secret key.
    pub fn new(secret_key: SecretKey) -> Self {
        let public_key = secret_key.public_key();
        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));
        let public_key_hex = format!("0x{}", hex::encode(public_key.to_bytes()));

        Self {
            secret_key: std::sync::Mutex::new(secret_key),
            commitment,
            commitment_hex,
            public_key_hex,
        }
    }

    /// Generates a new random signer.
    pub fn generate() -> Self {
        Self::new(SecretKey::new())
    }
}

impl Signer for EcdsaKeyStore {
    fn scheme(&self) -> SignatureScheme {
        SignatureScheme::Ecdsa
    }

    fn commitment(&self) -> Word {
        self.commitment
    }

    fn commitment_hex(&self) -> String {
        self.commitment_hex.clone()
    }

    fn public_key_hex(&self) -> String {
        self.public_key_hex.clone()
    }

    fn sign_word_hex(&self, message: Word) -> String {
        format!(
            "0x{}",
            hex::encode(self.secret_key.lock().unwrap().sign(message).to_bytes())
        )
    }
}

#[cfg(test)]
mod tests {
    use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{PublicKey, Signature};
    use miden_protocol::utils::serde::Deserializable;

    use super::*;

    #[test]
    fn generate_creates_valid_signer() {
        let signer = EcdsaKeyStore::generate();
        assert!(signer.commitment_hex().starts_with("0x"));
        assert_eq!(signer.commitment_hex().len(), 66);
    }

    #[test]
    fn new_from_secret_key_derives_correct_commitment() {
        let secret_key = SecretKey::new();
        let expected_commitment = secret_key.public_key().to_commitment();
        let signer = EcdsaKeyStore::new(secret_key);
        assert_eq!(signer.commitment(), expected_commitment);
    }

    #[test]
    fn sign_word_hex_produces_verifiable_signature() {
        let signer = EcdsaKeyStore::generate();
        let message = Word::default();
        let signature_hex = signer.sign_word_hex(message);
        let signature_bytes = hex::decode(signature_hex.trim_start_matches("0x")).unwrap();
        let signature = Signature::read_from_bytes(&signature_bytes).unwrap();
        let public_key_bytes =
            hex::decode(signer.public_key_hex().trim_start_matches("0x")).unwrap();
        let public_key = PublicKey::read_from_bytes(&public_key_bytes).unwrap();
        assert!(public_key.verify(message, &signature));
    }
}
