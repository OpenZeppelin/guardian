use crate::delta_object::DeltaObject;
use crate::error::PsmError;
use miden_keystore::{EcdsaKeyStore, FilesystemEcdsaKeyStore, ecdsa_commitment_hex};
use miden_objects::{
    Word, crypto::dsa::ecdsa_k256_keccak::Signature, transaction::TransactionSummary,
    utils::Serializable,
};
use private_state_manager_shared::FromJson;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
pub struct MidenEcdsaSigner {
    keystore: Arc<FilesystemEcdsaKeyStore>,
    server_pubkey_word: Word,
}

impl MidenEcdsaSigner {
    pub fn new(keystore_path: PathBuf) -> crate::ack::Result<Self> {
        let keystore = FilesystemEcdsaKeyStore::new(keystore_path)?;
        let keystore = Arc::new(keystore);
        let server_pubkey_word = keystore.generate_ecdsa_key()?;

        Ok(Self {
            keystore,
            server_pubkey_word,
        })
    }
}

impl MidenEcdsaSigner {
    pub(crate) fn sign_with_server_key(&self, message: Word) -> crate::ack::Result<Signature> {
        Ok(self.keystore.ecdsa_sign(self.server_pubkey_word, message)?)
    }

    pub(crate) fn pubkey_hex(&self) -> String {
        let secret_key = self
            .keystore
            .get_ecdsa_key(self.server_pubkey_word)
            .expect("Server key must exist in keystore");
        let pub_key = secret_key.public_key();
        format!("0x{}", hex::encode(pub_key.to_bytes()))
    }

    pub(crate) fn commitment_hex(&self) -> String {
        let secret_key = self
            .keystore
            .get_ecdsa_key(self.server_pubkey_word)
            .expect("Server key must exist in keystore");
        ecdsa_commitment_hex(&secret_key.public_key())
    }

    pub(crate) fn ack_delta(&self, mut delta: DeltaObject) -> crate::ack::Result<DeltaObject> {
        let tx_summary = TransactionSummary::from_json(&delta.delta_payload).map_err(|e| {
            PsmError::InvalidDelta(format!("Failed to deserialize TransactionSummary: {e}"))
        })?;

        let tx_commitment = tx_summary.to_commitment();
        let signature = self.sign_with_server_key(tx_commitment)?;
        delta.ack_sig = hex::encode(signature.to_bytes());
        Ok(delta)
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use miden_keystore::EcdsaKeyStore;

    fn create_test_signer() -> (MidenEcdsaSigner, PathBuf) {
        let temp_dir =
            std::env::temp_dir().join(format!("psm_test_ecdsa_signer_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let signer = MidenEcdsaSigner::new(temp_dir.clone()).unwrap();
        (signer, temp_dir)
    }

    #[test]
    fn new_creates_signer_with_key() {
        let (signer, dir) = create_test_signer();
        assert_ne!(signer.server_pubkey_word, Word::default());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn sign_with_server_key_produces_verifiable_signature() {
        let (signer, dir) = create_test_signer();
        let message = Word::default();
        let sig = signer.sign_with_server_key(message).unwrap();

        let sk = signer
            .keystore
            .get_ecdsa_key(signer.server_pubkey_word)
            .unwrap();
        let pk = sk.public_key();
        assert!(pk.verify(message, &sig));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn pubkey_hex_returns_valid_hex() {
        let (signer, dir) = create_test_signer();
        let pk_hex = signer.pubkey_hex();
        assert!(pk_hex.starts_with("0x"));
        assert!(hex::decode(pk_hex.strip_prefix("0x").unwrap()).is_ok());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn commitment_hex_returns_valid_hex() {
        let (signer, dir) = create_test_signer();
        let commitment = signer.commitment_hex();
        assert!(commitment.starts_with("0x"));
        assert_eq!(commitment.len(), 66);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn commitment_hex_matches_pubkey_commitment() {
        let (signer, dir) = create_test_signer();
        let commitment = signer.commitment_hex();
        let sk = signer
            .keystore
            .get_ecdsa_key(signer.server_pubkey_word)
            .unwrap();
        let expected = ecdsa_commitment_hex(&sk.public_key());
        assert_eq!(commitment, expected);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn pubkey_hex_is_consistent() {
        let (signer, dir) = create_test_signer();
        let hex1 = signer.pubkey_hex();
        let hex2 = signer.pubkey_hex();
        assert_eq!(hex1, hex2);
        std::fs::remove_dir_all(dir).ok();
    }
}
