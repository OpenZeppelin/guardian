use super::EcdsaSignerBackend;
use crate::error::Result;
use async_trait::async_trait;
use miden_keystore::{EcdsaKeyStore, FilesystemEcdsaKeyStore};
use miden_protocol::Word;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{PublicKey, SecretKey, Signature};
use std::path::PathBuf;
use std::sync::Arc;

pub struct InMemoryEcdsaBackend {
    keystore: Arc<FilesystemEcdsaKeyStore>,
    server_pubkey_word: Word,
    public_key: PublicKey,
}

impl InMemoryEcdsaBackend {
    pub fn new(keystore_path: PathBuf, secret_key: Option<&SecretKey>) -> Result<Self> {
        let keystore = Arc::new(FilesystemEcdsaKeyStore::new(keystore_path)?);
        let (server_pubkey_word, public_key) = match secret_key {
            Some(secret_key) => {
                let server_pubkey_word = secret_key.public_key().to_commitment();
                keystore.add_ecdsa_key(secret_key)?;
                (server_pubkey_word, secret_key.public_key())
            }
            None => {
                let server_pubkey_word = keystore.generate_ecdsa_key()?;
                let public_key = keystore.get_ecdsa_key(server_pubkey_word)?.public_key();
                (server_pubkey_word, public_key)
            }
        };

        Ok(Self {
            keystore,
            server_pubkey_word,
            public_key,
        })
    }
}

#[async_trait]
impl EcdsaSignerBackend for InMemoryEcdsaBackend {
    fn public_key(&self) -> &PublicKey {
        &self.public_key
    }

    async fn sign(&self, message: Word) -> Result<Signature> {
        // miden-crypto 0.23.0 implements `ZeroizeOnDrop for SecretKey` (delegating
        // to k256), and miden-keystore wraps its file-read buffer in
        // `zeroize::Zeroizing`, so the per-call loaded key material is wiped when
        // this signing frame returns.
        Ok(self.keystore.ecdsa_sign(self.server_pubkey_word, message)?)
    }

    fn backend_id(&self) -> &'static str {
        "in-memory"
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use miden_keystore::ecdsa_commitment_hex;

    fn create_test_backend() -> (InMemoryEcdsaBackend, PathBuf) {
        let temp_dir = std::env::temp_dir().join(format!(
            "guardian_test_in_memory_ecdsa_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let backend = InMemoryEcdsaBackend::new(temp_dir.clone(), None).unwrap();
        (backend, temp_dir)
    }

    #[tokio::test]
    async fn sign_produces_signature_verifiable_against_public_key() {
        let (backend, dir) = create_test_backend();
        let message = Word::default();
        let signature = backend.sign(message).await.unwrap();
        assert!(backend.public_key().verify(message, &signature));
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn signature_serializes_to_sixty_five_bytes() {
        use miden_protocol::utils::serde::Serializable;
        let (backend, dir) = create_test_backend();
        let signature = backend.sign(Word::default()).await.unwrap();
        assert_eq!(signature.to_bytes().len(), 65);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn imports_provided_secret_key() {
        let temp_dir = std::env::temp_dir().join(format!(
            "guardian_test_in_memory_ecdsa_import_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let secret = SecretKey::new();
        let backend = InMemoryEcdsaBackend::new(temp_dir.clone(), Some(&secret)).unwrap();
        assert_eq!(
            ecdsa_commitment_hex(backend.public_key()),
            ecdsa_commitment_hex(&secret.public_key())
        );
        std::fs::remove_dir_all(temp_dir).ok();
    }

    #[test]
    fn backend_id_is_in_memory() {
        let (backend, dir) = create_test_backend();
        assert_eq!(backend.backend_id(), "in-memory");
        std::fs::remove_dir_all(dir).ok();
    }
}
