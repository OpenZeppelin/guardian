use miden_protocol::Word;
use miden_protocol::crypto::dsa::falcon512_rpo::{SecretKey, Signature};
use miden_protocol::utils::{Deserializable, Serializable};
use rand::{RngCore, SeedableRng};
use std::fs::{self, OpenOptions};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
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
        fs::create_dir_all(&keys_directory).map_err(|error| {
            KeyStoreError::StorageError(format!(
                "Failed to create keys directory {}: {error}",
                keys_directory.display()
            ))
        })?;

        Ok(Self {
            rng: Arc::new(RwLock::new(rng)),
            keys_directory,
        })
    }

    fn file_path_for_commitment(&self, pub_key: Word) -> PathBuf {
        self.keys_directory.join(hash_pub_key(pub_key))
    }

    fn write_key_file(&self, file_path: &Path, filename: &str, key_bytes: &[u8]) -> Result<()> {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(file_path)
            .map_err(|error| {
                KeyStoreError::StorageError(format!("Failed to open key file {filename}: {error}"))
            })?;

        let mut writer = BufWriter::new(file);
        writer
            .write_all(hex::encode(key_bytes).as_bytes())
            .map_err(|error| {
                KeyStoreError::StorageError(format!(
                    "Failed to write key to file {filename}: {error}"
                ))
            })?;

        writer.flush().map_err(|error| {
            KeyStoreError::StorageError(format!("Failed to flush key file {filename}: {error}"))
        })?;

        Ok(())
    }

    fn read_key_file(&self, pub_key: Word) -> Result<SecretKey> {
        let filename = hash_pub_key(pub_key);
        let file_path = self.file_path_for_commitment(pub_key);

        let file = OpenOptions::new()
            .read(true)
            .open(&file_path)
            .map_err(|error| {
                KeyStoreError::KeyNotFound(format!("Key file {filename} not found: {error}"))
            })?;

        let mut reader = BufReader::new(file);
        let mut hex_encoded = String::new();
        reader.read_line(&mut hex_encoded).map_err(|error| {
            KeyStoreError::StorageError(format!("Failed to read key from file {filename}: {error}"))
        })?;

        let key_bytes = hex::decode(hex_encoded.trim()).map_err(|error| {
            KeyStoreError::DecodingError(format!(
                "Failed to decode hex key from file {filename}: {error}"
            ))
        })?;

        SecretKey::read_from_bytes(&key_bytes).map_err(|error| {
            KeyStoreError::DecodingError(format!(
                "Failed to deserialize key from file {filename}: {error}"
            ))
        })
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
        let pub_key = key.public_key().to_commitment();
        let filename = hash_pub_key(pub_key);
        let file_path = self.file_path_for_commitment(pub_key);

        match self.get_key(pub_key) {
            Ok(existing_key) if existing_key.to_bytes() == key.to_bytes() => Ok(()),
            Ok(_) => Err(KeyStoreError::StorageError(format!(
                "Key file {filename} already exists with different key material"
            ))),
            Err(KeyStoreError::KeyNotFound(_)) => {
                self.write_key_file(&file_path, &filename, &key.to_bytes())
            }
            Err(error) => Err(error),
        }
    }

    fn get_key(&self, pub_key: Word) -> Result<SecretKey> {
        self.read_key_file(pub_key)
    }

    fn sign(&self, pub_key: Word, message: Word) -> Result<Signature> {
        let secret_key = self.get_key(pub_key)?;
        let mut rng_guard = self
            .rng
            .write()
            .map_err(|error| KeyStoreError::StorageError(format!("Failed to lock RNG: {error}")))?;
        Ok(secret_key.sign_with_rng::<R>(message, &mut *rng_guard))
    }

    fn generate_key(&self) -> Result<Word> {
        let secret_key = SecretKey::new();
        let pub_key = secret_key.public_key().to_commitment();
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
        let pub_key = secret_key.public_key().to_commitment();

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

        let retrieved_pubkey = retrieved_key.public_key().to_commitment();
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

    #[test]
    fn test_add_key_is_idempotent() {
        let temp_dir = TempDir::new().unwrap();
        let keystore =
            FilesystemKeyStore::<ChaCha20Rng>::new(temp_dir.path().to_path_buf()).unwrap();
        let secret_key = SecretKey::new();
        let commitment = secret_key.public_key().to_commitment();

        keystore.add_key(&secret_key).unwrap();
        keystore.add_key(&secret_key).unwrap();

        assert_eq!(
            keystore.get_key(commitment).unwrap().to_bytes(),
            secret_key.to_bytes()
        );
    }

    #[test]
    fn test_add_key_rejects_mismatched_existing_material() {
        let temp_dir = TempDir::new().unwrap();
        let keystore =
            FilesystemKeyStore::<ChaCha20Rng>::new(temp_dir.path().to_path_buf()).unwrap();
        let secret_key = SecretKey::new();
        let commitment = secret_key.public_key().to_commitment();
        let filename = hash_pub_key(commitment);
        let file_path = temp_dir.path().join(&filename);
        let different_key = SecretKey::new();

        write_test_key_file(&file_path, &different_key.to_bytes());

        let error = keystore.add_key(&secret_key).unwrap_err();
        assert!(
            matches!(error, KeyStoreError::StorageError(message) if message.contains("different key material"))
        );
    }

    fn write_test_key_file(file_path: &Path, key_bytes: &[u8]) {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(file_path)
            .unwrap();
        file.write_all(hex::encode(key_bytes).as_bytes()).unwrap();
        file.flush().unwrap();
    }
}
