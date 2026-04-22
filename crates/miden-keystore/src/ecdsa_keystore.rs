use crate::KeyStoreError;
use miden_protocol::Word;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{PublicKey, SecretKey, Signature};
use miden_protocol::utils::serde::{Deserializable, Serializable};
use std::fs::{self, OpenOptions};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
        fs::create_dir_all(&keys_directory).map_err(|error| {
            KeyStoreError::StorageError(format!(
                "Failed to create keys directory {}: {error}",
                keys_directory.display()
            ))
        })?;

        Ok(Self {
            keys_directory,
            sign_lock: Mutex::new(()),
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
                "Failed to deserialize ECDSA key from file {filename}: {error}"
            ))
        })
    }
}

impl EcdsaKeyStore for FilesystemEcdsaKeyStore {
    fn add_ecdsa_key(&self, key: &SecretKey) -> Result<()> {
        let pub_key = key.public_key().to_commitment();
        let filename = hash_pub_key(pub_key);
        let file_path = self.file_path_for_commitment(pub_key);

        match self.get_ecdsa_key(pub_key) {
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

    fn get_ecdsa_key(&self, pub_key: Word) -> Result<SecretKey> {
        self.read_key_file(pub_key)
    }

    fn ecdsa_sign(&self, pub_key: Word, message: Word) -> Result<Signature> {
        let secret_key = self.get_ecdsa_key(pub_key)?;
        let _lock = self.sign_lock.lock().map_err(|error| {
            KeyStoreError::StorageError(format!("Failed to lock signer: {error}"))
        })?;
        Ok(secret_key.sign(message))
    }

    fn generate_ecdsa_key(&self) -> Result<Word> {
        let secret_key = SecretKey::new();
        let pub_key = secret_key.public_key().to_commitment();
        self.add_ecdsa_key(&secret_key)?;
        Ok(pub_key)
    }
}

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
        let pub_key = secret_key.public_key().to_commitment();

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

        let retrieved_pubkey = retrieved_key.public_key().to_commitment();
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

    #[test]
    fn test_add_ecdsa_key_is_idempotent() {
        let temp_dir = TempDir::new().unwrap();
        let keystore = FilesystemEcdsaKeyStore::new(temp_dir.path().to_path_buf()).unwrap();
        let secret_key = SecretKey::new();
        let commitment = secret_key.public_key().to_commitment();

        keystore.add_ecdsa_key(&secret_key).unwrap();
        keystore.add_ecdsa_key(&secret_key).unwrap();

        assert_eq!(
            keystore.get_ecdsa_key(commitment).unwrap().to_bytes(),
            secret_key.to_bytes()
        );
    }

    #[test]
    fn test_add_ecdsa_key_rejects_mismatched_existing_material() {
        let temp_dir = TempDir::new().unwrap();
        let keystore = FilesystemEcdsaKeyStore::new(temp_dir.path().to_path_buf()).unwrap();
        let secret_key = SecretKey::new();
        let commitment = secret_key.public_key().to_commitment();
        let filename = hash_pub_key(commitment);
        let file_path = temp_dir.path().join(&filename);
        let different_key = SecretKey::new();

        write_test_key_file(&file_path, &different_key.to_bytes());

        let error = keystore.add_ecdsa_key(&secret_key).unwrap_err();
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
