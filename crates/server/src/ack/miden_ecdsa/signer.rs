use crate::delta_object::DeltaObject;
use crate::error::PsmError;
use miden_keystore::{EcdsaKeyStore, FilesystemEcdsaKeyStore, ecdsa_commitment_hex};
use miden_objects::{
    Word,
    crypto::dsa::ecdsa_k256_keccak::Signature,
    transaction::TransactionSummary,
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
