use miden_objects::Word;
use miden_objects::crypto::dsa::ecdsa_k256_keccak::{PublicKey, SecretKey, Signature};
use miden_objects::utils::{Deserializable, Serializable};
use std::fs::{self, OpenOptions};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use crate::KeyStoreError;

type Result<T> = std::result::Result<T, KeyStoreError>;

pub trait EcdsaKeyStore {
    fn add_ecdsa_key(&self, key: &SecretKey) -> Result<()>;
    fn get_ecdsa_key(&self, pub_key: Word) -> Result<SecretKey>;
    fn ecdsa_sign(&self, pub_key: Word, message: Word) -> Result<Signature>;
    fn generate_ecdsa_key(&self) -> Result<Word>;
}

#[derive(Debug)]
pub struct FilesystemEcdsaKeyStore {
    keys_directory: PathBuf,
    /// Lock to serialize signing operations.
    sign_lock: Mutex<()>,
}

impl Clone for FilesystemEcdsaKeyStore {
    fn clone(&self) -> Self {
        Self {
            keys_directory: self.keys_directory.clone(),
            sign_lock: Mutex::new(()),
        }
    }
}

impl FilesystemEcdsaKeyStore {
    pub fn new(keys_directory: PathBuf) -> Result<Self> {
        fs::create_dir_all(&keys_directory).map_err(|e| {
            KeyStoreError::StorageError(format!("Failed to create keys directory: {e}"))
        })?;

        Ok(Self {
            keys_directory,
            sign_lock: Mutex::new(()),
        })
    }
}

impl EcdsaKeyStore for FilesystemEcdsaKeyStore {
    fn add_ecdsa_key(&self, key: &SecretKey) -> Result<()> {
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

    fn get_ecdsa_key(&self, pub_key: Word) -> Result<SecretKey> {
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
                "Failed to deserialize ECDSA key from file {filename}: {e}"
            ))
        })
    }

    fn ecdsa_sign(&self, pub_key: Word, message: Word) -> Result<Signature> {
        let secret_key = self.get_ecdsa_key(pub_key)?;
        let _lock = self.sign_lock.lock().unwrap();
        Ok(secret_key.sign(message))
    }

    fn generate_ecdsa_key(&self) -> Result<Word> {
        let secret_key = SecretKey::new();
        let pub_key: Word = secret_key.public_key().to_commitment();

        self.add_ecdsa_key(&secret_key)?;

        Ok(pub_key)
    }
}

/// Compute the public key commitment for an ECDSA public key.
pub fn ecdsa_commitment_hex(pub_key: &PublicKey) -> String {
    let commitment = pub_key.to_commitment();
    format!("0x{}", hex::encode(commitment.to_bytes()))
}

fn hash_pub_key(pub_key: Word) -> String {
    let mut hasher = DefaultHasher::new();
    pub_key.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_ecdsa_add_and_get_key() {
        let temp_dir = TempDir::new().unwrap();
        let keystore = FilesystemEcdsaKeyStore::new(temp_dir.path().to_path_buf()).unwrap();

        let secret_key = SecretKey::new();
        let pub_key: Word = secret_key.public_key().to_commitment();

        keystore.add_ecdsa_key(&secret_key).unwrap();
        let retrieved_key = keystore.get_ecdsa_key(pub_key).unwrap();

        assert_eq!(secret_key.to_bytes(), retrieved_key.to_bytes());
    }

    #[test]
    fn test_ecdsa_generate_key() {
        let temp_dir = TempDir::new().unwrap();
        let keystore = FilesystemEcdsaKeyStore::new(temp_dir.path().to_path_buf()).unwrap();

        let pub_key = keystore.generate_ecdsa_key().unwrap();
        let retrieved_key = keystore.get_ecdsa_key(pub_key).unwrap();

        let retrieved_pubkey: Word = retrieved_key.public_key().to_commitment();
        assert_eq!(retrieved_pubkey, pub_key);
    }

    #[test]
    fn test_ecdsa_sign() {
        let temp_dir = TempDir::new().unwrap();
        let keystore = FilesystemEcdsaKeyStore::new(temp_dir.path().to_path_buf()).unwrap();

        let pub_key = keystore.generate_ecdsa_key().unwrap();
        let message = Word::from([1u32, 2, 3, 4]);

        let signature = keystore.ecdsa_sign(pub_key, message).unwrap();

        let secret_key = keystore.get_ecdsa_key(pub_key).unwrap();
        let public_key = secret_key.public_key();
        assert!(public_key.verify(message, &signature));
    }
}
