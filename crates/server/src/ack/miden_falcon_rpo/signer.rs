use crate::error::{MidenFalconRpoError, PsmError};
use crate::storage::DeltaObject;
use miden_keystore::{FilesystemKeyStore, KeyStore};
use miden_objects::{
    Felt, Word,
    crypto::dsa::rpo_falcon512::{PublicKey, Signature},
    crypto::hash::rpo::Rpo256,
    utils::Serializable,
};
use rand_chacha::ChaCha20Rng;
use std::path::PathBuf;
use std::sync::Arc;

type Result<T> = std::result::Result<T, MidenFalconRpoError>;

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
        let pubkey = self.pubkey();
        let pubkey_word: Word = pubkey.into();
        format!("0x{}", hex::encode(pubkey_word.to_bytes()))
    }

    pub(crate) fn ack_delta(&self, mut delta: DeltaObject) -> crate::ack::Result<DeltaObject> {
        let commitment_digest = self.commitment_to_digest(&delta.new_commitment)?;
        let signature = self.sign_with_server_key(commitment_digest)?;
        delta.ack_sig = Some(hex::encode(signature.to_bytes()));
        Ok(delta)
    }

    fn commitment_to_digest(&self, commitment_hex: &str) -> crate::ack::Result<Word> {
        let commitment_hex = commitment_hex.strip_prefix("0x").unwrap_or(commitment_hex);

        let bytes = hex::decode(commitment_hex)
            .map_err(|e| PsmError::InvalidCommitment(format!("Invalid hex: {e}")))?;

        if bytes.len() != 32 {
            return Err(PsmError::InvalidCommitment(format!(
                "Commitment must be 32 bytes, got {}",
                bytes.len()
            )));
        }

        let mut felts = Vec::new();
        for chunk in bytes.chunks(8) {
            let mut arr = [0u8; 8];
            arr[..chunk.len()].copy_from_slice(chunk);
            let value = u64::from_le_bytes(arr);
            felts.push(
                Felt::try_from(value).map_err(|e| {
                    PsmError::InvalidCommitment(format!("Invalid field element: {e}"))
                })?,
            );
        }

        let message_elements = vec![felts[0], felts[1], felts[2], felts[3]];

        let digest = Rpo256::hash_elements(&message_elements);
        Ok(digest)
    }
}
