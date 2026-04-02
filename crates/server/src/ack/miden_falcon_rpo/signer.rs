use crate::delta_object::DeltaObject;
use crate::error::{GuardianError, MidenFalconRpoResult as Result};
use guardian_shared::{FromJson, hex::IntoHex};
use miden_keystore::{FilesystemKeyStore, KeyStore};
use miden_protocol::{
    Word,
    crypto::dsa::falcon512_rpo::{SecretKey, Signature},
    transaction::TransactionSummary,
    utils::Serializable,
};
use rand_chacha::ChaCha20Rng;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
pub struct MidenFalconRpoSigner {
    keystore: Arc<FilesystemKeyStore<ChaCha20Rng>>,
    server_pubkey_word: Word,
    pubkey_hex: String,
    commitment_hex: String,
}

impl MidenFalconRpoSigner {
    pub fn new(keystore_path: PathBuf, secret_key: Option<&SecretKey>) -> Result<Self> {
        let keystore = Arc::new(FilesystemKeyStore::<ChaCha20Rng>::new(keystore_path)?);
        let (server_pubkey_word, public_key) = match secret_key {
            Some(secret_key) => {
                let server_pubkey_word = secret_key.public_key().to_commitment();
                keystore.add_key(secret_key)?;
                (server_pubkey_word, secret_key.public_key())
            }
            None => {
                let server_pubkey_word = keystore.generate_key()?;
                let public_key = keystore.get_key(server_pubkey_word)?.public_key();
                (server_pubkey_word, public_key)
            }
        };
        let pubkey_hex = (&public_key).into_hex();
        let commitment_hex = format!("0x{}", hex::encode(public_key.to_commitment().to_bytes()));

        Ok(Self {
            keystore,
            server_pubkey_word,
            pubkey_hex,
            commitment_hex,
        })
    }
}

impl MidenFalconRpoSigner {
    pub(crate) fn sign_with_server_key(&self, message: Word) -> crate::ack::Result<Signature> {
        Ok(self.keystore.sign(self.server_pubkey_word, message)?)
    }

    pub(crate) fn pubkey_hex(&self) -> String {
        self.pubkey_hex.clone()
    }

    pub(crate) fn commitment_hex(&self) -> String {
        self.commitment_hex.clone()
    }

    pub(crate) fn ack_delta(&self, mut delta: DeltaObject) -> crate::ack::Result<DeltaObject> {
        let tx_summary = TransactionSummary::from_json(&delta.delta_payload).map_err(|e| {
            GuardianError::InvalidDelta(format!("Failed to deserialize TransactionSummary: {e}"))
        })?;

        let tx_commitment = tx_summary.to_commitment();
        let signature = self.sign_with_server_key(tx_commitment)?;
        delta.ack_sig = hex::encode(signature.to_bytes());
        Ok(delta)
    }
}
