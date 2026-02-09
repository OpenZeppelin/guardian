use miden_protocol::Word;
use miden_protocol::crypto::dsa::falcon512_rpo::{SecretKey, Signature};
use miden_protocol::utils::{Deserializable, Serializable};
use rand::{RngCore, SeedableRng};
use std::fs::{self, OpenOptions};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum KeyStoreError {
    #[error("Storage error: {0}")]
    StorageError(String),

    #[error("Decoding error: {0}")]
    DecodingError(String),

    #[error("Key not found: {0}")]
    KeyNotFound(String),
}

pub type Result<T> = std::result::Result<T, KeyStoreError>;

pub trait KeyStore {
    fn add_key(&self, key: &SecretKey) -> Result<()>;
    fn get_key(&self, pub_key: Word) -> Result<SecretKey>;
    fn sign(&self, pub_key: Word, message: Word) -> Result<Signature>;
    fn generate_key(&self) -> Result<Word>;
}

#[derive(Debug, Clone)]
pub struct FilesystemKeyStore<R: RngCore + Send + Sync> {
    rng: Arc<RwLock<R>>,
    keys_directory: PathBuf,
}

impl<R: RngCore + Send + Sync> FilesystemKeyStore<R> {
    pub fn with_rng(keys_directory: PathBuf, rng: R) -> Result<Self> {
        fs::create_dir_all(&keys_directory).map_err(|e| {
            KeyStoreError::StorageError(format!("Failed to create keys directory: {e}"))
        })?;

        Ok(Self {
            rng: Arc::new(RwLock::new(rng)),
            keys_directory,
        })
    }

    pub fn keys_directory(&self) -> &PathBuf {
        &self.keys_directory
    }
}

impl<R: RngCore + SeedableRng + Send + Sync> FilesystemKeyStore<R> {
    pub fn new(keys_directory: PathBuf) -> Result<Self> {
        let rng = R::seed_from_u64(rand::random());
        Self::with_rng(keys_directory, rng)
    }
}

impl<R: RngCore + Send + Sync> KeyStore for FilesystemKeyStore<R> {
    fn add_key(&self, key: &SecretKey) -> Result<()> {
        let pub_key = key.public_key();
        let pub_key_word: Word = pub_key.to_commitment();
        let filename = hash_pub_key(pub_key_word);
        let file_path = self.keys_directory.join(&filename);

        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&file_path)
            .map_err(|e| {
                KeyStoreError::StorageError(format!("Failed to open key file {filename}: {e}"))
            })?;

        let mut writer = BufWriter::new(file);
        let key_bytes = key.to_bytes();
        let hex_encoded = hex::encode(key_bytes);

        writer.write_all(hex_encoded.as_bytes()).map_err(|e| {
            KeyStoreError::StorageError(format!("Failed to write key to file {filename}: {e}"))
        })?;

        writer.flush().map_err(|e| {
            KeyStoreError::StorageError(format!("Failed to flush key file {filename}: {e}"))
        })?;

        Ok(())
    }

    fn get_key(&self, pub_key: Word) -> Result<SecretKey> {
        let filename = hash_pub_key(pub_key);
        let file_path = self.keys_directory.join(&filename);

        let file = OpenOptions::new()
            .read(true)
            .open(&file_path)
            .map_err(|e| {
                KeyStoreError::KeyNotFound(format!("Key file {filename} not found: {e}"))
            })?;

        let mut reader = BufReader::new(file);
        let mut hex_encoded = String::new();

        reader.read_line(&mut hex_encoded).map_err(|e| {
            KeyStoreError::StorageError(format!("Failed to read key from file {filename}: {e}"))
        })?;

        let key_bytes = hex::decode(hex_encoded.trim()).map_err(|e| {
            KeyStoreError::DecodingError(format!(
                "Failed to decode hex key from file {filename}: {e}"
            ))
        })?;

        SecretKey::read_from_bytes(&key_bytes).map_err(|e| {
            KeyStoreError::DecodingError(format!(
                "Failed to deserialize key from file {filename}: {e}"
            ))
        })
    }

    fn sign(&self, pub_key: Word, message: Word) -> Result<Signature> {
        let secret_key = self.get_key(pub_key)?;
        let mut rng_guard = self.rng.write().unwrap();
        Ok(secret_key.sign_with_rng::<R>(message, &mut *rng_guard))
    }

    fn generate_key(&self) -> Result<Word> {
        let secret_key = SecretKey::new();
        let pub_key: Word = secret_key.public_key().to_commitment();

        self.add_key(&secret_key)?;

        Ok(pub_key)
    }
}

fn hash_pub_key(pub_key: Word) -> String {
    let mut hasher = DefaultHasher::new();
    pub_key.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_chacha::ChaCha20Rng;
    use tempfile::TempDir;

    #[test]
    fn test_add_and_get_key() {
        let temp_dir = TempDir::new().unwrap();
        let keystore =
            FilesystemKeyStore::<ChaCha20Rng>::new(temp_dir.path().to_path_buf()).unwrap();

        let secret_key = SecretKey::new();
        let pub_key: Word = secret_key.public_key().to_commitment();

        keystore.add_key(&secret_key).unwrap();
        let retrieved_key = keystore.get_key(pub_key).unwrap();

        assert_eq!(secret_key.to_bytes(), retrieved_key.to_bytes());
    }

    #[test]
    fn test_generate_key() {
        let temp_dir = TempDir::new().unwrap();
        let keystore =
            FilesystemKeyStore::<ChaCha20Rng>::new(temp_dir.path().to_path_buf()).unwrap();

        let pub_key = keystore.generate_key().unwrap();
        let retrieved_key = keystore.get_key(pub_key).unwrap();

        let retrieved_pubkey: Word = retrieved_key.public_key().to_commitment();
        assert_eq!(retrieved_pubkey, pub_key);
    }

    #[test]
    fn test_sign() {
        let temp_dir = TempDir::new().unwrap();
        let keystore =
            FilesystemKeyStore::<ChaCha20Rng>::new(temp_dir.path().to_path_buf()).unwrap();

        let pub_key = keystore.generate_key().unwrap();
        let message = Word::from([1u32, 2, 3, 4]);

        let signature = keystore.sign(pub_key, message).unwrap();

        let secret_key = keystore.get_key(pub_key).unwrap();
        let public_key = secret_key.public_key();
        assert!(public_key.verify(message, &signature));
    }

    #[test]
    fn test_hash_pub_key() {
        use miden_protocol::Felt;
        let pub_key = [Felt::new(1), Felt::new(2), Felt::new(3), Felt::new(4)];
        let hash1 = hash_pub_key(pub_key.into());
        let hash2 = hash_pub_key(pub_key.into());

        assert_eq!(hash1, hash2);
        assert!(!hash1.is_empty());
    }
}
