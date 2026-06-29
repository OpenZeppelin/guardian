//! Local-file ACK secret provider: a stable Guardian identity without AWS.
//!
//! Reads hex-encoded Falcon and ECDSA ACK secret keys from files whose paths
//! come from `GUARDIAN_ACK_FALCON_SECRET_PATH` and
//! `GUARDIAN_ACK_ECDSA_SECRET_PATH`. This lets a self-hosted Guardian keep a
//! fixed identity across restarts without AWS Secrets Manager. The file format
//! is the hex string emitted by the `ack-keygen` binary — identical to what the
//! Secrets Manager path stores — so the same key material is portable between
//! the two.
//!
//! The alternative (no provider) mints a fresh keypair on every boot, which
//! changes the on-chain ack-key commitment and freezes any account that pinned
//! the old one (recovery then requires a per-account `SwitchGuardian`).

use crate::error::{GuardianError, Result};
use crate::secret::SecretString;
use async_trait::async_trait;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey as EcdsaSecretKey;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey as FalconSecretKey;
use miden_protocol::utils::serde::Deserializable;
use std::path::{Path, PathBuf};

use super::secrets_manager::{AckSecretProvider, decode_secret_key};

const ENV_ACK_FALCON_SECRET_PATH: &str = "GUARDIAN_ACK_FALCON_SECRET_PATH";
const ENV_ACK_ECDSA_SECRET_PATH: &str = "GUARDIAN_ACK_ECDSA_SECRET_PATH";

/// Reads the ACK secrets from local files. Construct with [`from_env`].
///
/// [`from_env`]: FileSecretProvider::from_env
pub struct FileSecretProvider {
    falcon_secret_path: PathBuf,
    ecdsa_secret_path: PathBuf,
}

impl FileSecretProvider {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            falcon_secret_path: required_path(ENV_ACK_FALCON_SECRET_PATH)?,
            ecdsa_secret_path: required_path(ENV_ACK_ECDSA_SECRET_PATH)?,
        })
    }

    fn parsed_secret_key<T, F>(&self, path: &Path, parser: F) -> Result<T>
    where
        F: FnOnce(&[u8]) -> std::result::Result<T, String>,
    {
        let contents = std::fs::read_to_string(path).map_err(|error| {
            GuardianError::ConfigurationError(format!(
                "Failed to read ack secret file {}: {error}",
                path.display()
            ))
        })?;
        decode_secret_key(
            &format!("Ack secret file {}", path.display()),
            &SecretString::new(contents),
            parser,
        )
    }
}

#[async_trait]
impl AckSecretProvider for FileSecretProvider {
    async fn falcon_secret_key(&self) -> Result<FalconSecretKey> {
        self.parsed_secret_key(&self.falcon_secret_path, |secret_bytes| {
            FalconSecretKey::read_from_bytes(secret_bytes).map_err(|error| error.to_string())
        })
    }

    async fn ecdsa_secret_key(&self) -> Result<EcdsaSecretKey> {
        self.parsed_secret_key(&self.ecdsa_secret_path, |secret_bytes| {
            EcdsaSecretKey::read_from_bytes(secret_bytes).map_err(|error| error.to_string())
        })
    }
}

fn required_path(env_var: &str) -> Result<PathBuf> {
    match std::env::var(env_var) {
        Ok(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Err(GuardianError::ConfigurationError(format!(
                    "{env_var} must not be blank when GUARDIAN_ACK_SECRET_PROVIDER=file"
                )))
            } else {
                Ok(PathBuf::from(trimmed))
            }
        }
        Err(std::env::VarError::NotPresent) => Err(GuardianError::ConfigurationError(format!(
            "{env_var} is required when GUARDIAN_ACK_SECRET_PROVIDER=file"
        ))),
        Err(std::env::VarError::NotUnicode(_)) => Err(GuardianError::ConfigurationError(format!(
            "{env_var} must contain valid UTF-8"
        ))),
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use miden_protocol::utils::serde::Serializable;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "guardian_file_provider_{tag}_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_hex(path: &Path, bytes: &[u8]) {
        std::fs::write(path, hex::encode(bytes)).unwrap();
    }

    #[tokio::test]
    async fn reads_falcon_and_ecdsa_secrets_from_files() {
        let dir = temp_dir("read");
        let falcon = FalconSecretKey::new();
        let ecdsa = EcdsaSecretKey::new();
        let falcon_path = dir.join("falcon");
        let ecdsa_path = dir.join("ecdsa");
        write_hex(&falcon_path, &falcon.to_bytes());
        write_hex(&ecdsa_path, &ecdsa.to_bytes());
        let provider = FileSecretProvider {
            falcon_secret_path: falcon_path,
            ecdsa_secret_path: ecdsa_path,
        };

        assert_eq!(
            provider.falcon_secret_key().await.unwrap().to_bytes(),
            falcon.to_bytes()
        );
        assert_eq!(
            provider.ecdsa_secret_key().await.unwrap().to_bytes(),
            ecdsa.to_bytes()
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn same_file_yields_same_identity_across_reads() {
        let dir = temp_dir("stable");
        let falcon = FalconSecretKey::new();
        let falcon_path = dir.join("falcon");
        write_hex(&falcon_path, &falcon.to_bytes());
        let provider = FileSecretProvider {
            falcon_secret_path: falcon_path,
            ecdsa_secret_path: dir.join("ecdsa"),
        };

        let first = provider.falcon_secret_key().await.unwrap();
        let second = provider.falcon_secret_key().await.unwrap();
        assert_eq!(
            first.public_key().to_commitment(),
            second.public_key().to_commitment()
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn tolerates_surrounding_whitespace_in_file() {
        let dir = temp_dir("trim");
        let ecdsa = EcdsaSecretKey::new();
        let ecdsa_path = dir.join("ecdsa");
        std::fs::write(
            &ecdsa_path,
            format!("  {}\n", hex::encode(ecdsa.to_bytes())),
        )
        .unwrap();
        let provider = FileSecretProvider {
            falcon_secret_path: dir.join("falcon"),
            ecdsa_secret_path: ecdsa_path,
        };

        assert_eq!(
            provider.ecdsa_secret_key().await.unwrap().to_bytes(),
            ecdsa.to_bytes()
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn missing_file_is_configuration_error() {
        let provider = FileSecretProvider {
            falcon_secret_path: PathBuf::from("/nonexistent/guardian/ack-falcon"),
            ecdsa_secret_path: PathBuf::from("/nonexistent/guardian/ack-ecdsa"),
        };
        assert!(matches!(
            provider.falcon_secret_key().await,
            Err(GuardianError::ConfigurationError(_))
        ));
    }

    #[tokio::test]
    async fn invalid_hex_is_configuration_error() {
        let dir = temp_dir("badhex");
        let falcon_path = dir.join("falcon");
        std::fs::write(&falcon_path, "nothex!!").unwrap();
        let provider = FileSecretProvider {
            falcon_secret_path: falcon_path,
            ecdsa_secret_path: dir.join("ecdsa"),
        };

        let err = provider.falcon_secret_key().await.unwrap_err();
        assert!(
            matches!(err, GuardianError::ConfigurationError(message) if message.contains("hex"))
        );
        std::fs::remove_dir_all(dir).ok();
    }
}
