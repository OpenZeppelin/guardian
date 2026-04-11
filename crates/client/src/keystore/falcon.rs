use guardian_shared::SignatureScheme;
use guardian_shared::hex::IntoHex;
use miden_protocol::Word;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::utils::serde::Serializable;

use super::Signer;

/// In-memory Falcon signer used for GUARDIAN authentication and multisig signing.
///
/// The underlying Miden secret key zeroizes on drop. This type avoids exposing
/// the key material through cloning or accessor APIs.
pub struct FalconKeyStore {
    secret_key: SecretKey,
    commitment: Word,
    commitment_hex: String,
    public_key_hex: String,
}

impl FalconKeyStore {
    /// Creates a new signer from a Falcon secret key.
    pub fn new(secret_key: SecretKey) -> Self {
        let public_key = secret_key.public_key();
        let commitment = public_key.to_commitment();
        let commitment_hex = format!("0x{}", hex::encode(commitment.to_bytes()));
        let public_key_hex = (&public_key).into_hex();

        Self {
            secret_key,
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

impl Signer for FalconKeyStore {
    fn scheme(&self) -> SignatureScheme {
        SignatureScheme::Falcon
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
        self.secret_key.sign(message).into_hex()
    }
}

#[cfg(test)]
mod tests {
    use guardian_shared::hex::FromHex;
    use miden_protocol::crypto::dsa::falcon512_poseidon2::{PublicKey, Signature};

    use super::*;

    #[test]
    fn generate_creates_valid_signer() {
        let signer = FalconKeyStore::generate();
        assert!(signer.commitment_hex().starts_with("0x"));
        assert_eq!(signer.commitment_hex().len(), 66);
    }

    #[test]
    fn new_from_secret_key_derives_correct_commitment() {
        let secret_key = SecretKey::new();
        let expected_commitment = secret_key.public_key().to_commitment();
        let signer = FalconKeyStore::new(secret_key);
        assert_eq!(signer.commitment(), expected_commitment);
    }

    #[test]
    fn sign_word_produces_verifiable_signature() {
        let signer = FalconKeyStore::generate();
        let message = Word::default();
        let signature_hex = signer.sign_word_hex(message);
        let signature = Signature::from_hex(&signature_hex).unwrap();
        let public_key = PublicKey::from_hex(&signer.public_key_hex()).unwrap();
        assert!(public_key.verify(message, &signature));
    }

    #[test]
    fn public_key_hex_roundtrips() {
        let signer = FalconKeyStore::generate();
        let public_key = PublicKey::from_hex(&signer.public_key_hex()).unwrap();
        assert_eq!(public_key.to_commitment(), signer.commitment());
    }
}
