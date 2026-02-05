use crate::delta_object::DeltaObject;
use crate::error::{MidenFalconRpoResult as Result, PsmError};
use miden_keystore::{FilesystemKeyStore, KeyStore};
use miden_objects::{
    Word,
    crypto::dsa::rpo_falcon512::{PublicKey, Signature},
    transaction::TransactionSummary,
    utils::Serializable,
};
use private_state_manager_shared::{FromJson, hex::IntoHex};
use rand_chacha::ChaCha20Rng;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
pub struct MidenFalconRpoSigner {
    keystore: Arc<FilesystemKeyStore<ChaCha20Rng>>,
    server_pubkey_word: Word,
}

impl MidenFalconRpoSigner {
    pub fn new(keystore_path: PathBuf) -> Result<Self> {
        let keystore = FilesystemKeyStore::<ChaCha20Rng>::new(keystore_path)?;
        let keystore = Arc::new(keystore);
        let server_pubkey_word = keystore.generate_key()?;

        Ok(Self {
            keystore,
            server_pubkey_word,
        })
    }
}

impl MidenFalconRpoSigner {
    pub(crate) fn sign_with_server_key(&self, message: Word) -> crate::ack::Result<Signature> {
        Ok(self.keystore.sign(self.server_pubkey_word, message)?)
    }

    pub(crate) fn pubkey(&self) -> PublicKey {
        let secret_key = self
            .keystore
            .get_key(self.server_pubkey_word)
            .expect("Server key must exist in keystore");
        secret_key.public_key()
    }

    pub(crate) fn pubkey_hex(&self) -> String {
        self.pubkey().into_hex()
    }

    pub(crate) fn commitment_hex(&self) -> String {
        let commitment = self.pubkey().to_commitment();
        format!("0x{}", hex::encode(commitment.to_bytes()))
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
