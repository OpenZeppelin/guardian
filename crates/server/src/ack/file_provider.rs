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
    /// `None` when `GUARDIAN_ACK_ECDSA_SECRET_PATH` is unset. The ECDSA secret
    /// file is only read for the in-memory ECDSA backend; an `aws-kms` backend
    /// signs with the KMS key and never calls [`ecdsa_secret_key`], so the path
    /// is optional and only required to exist when actually consulted.
    ///
    /// [`ecdsa_secret_key`]: AckSecretProvider::ecdsa_secret_key
    ecdsa_secret_path: Option<PathBuf>,
}

impl FileSecretProvider {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            falcon_secret_path: required_path(ENV_ACK_FALCON_SECRET_PATH)?,
            ecdsa_secret_path: optional_path(ENV_ACK_ECDSA_SECRET_PATH)?,
        })
    }

    fn parsed_secret_key<T, F>(&self, path: &Path, parser: F) -> Result<T>
    where
        F: FnOnce(&[u8]) -> std::result::Result<T, String>,
    {
        ensure_owner_only(path)?;
        // Read-and-wrap in one expression so the key bytes never bind to a bare
        // `String` (CONTRIBUTING.md, "Secrets in server memory").
        let contents = SecretString::new(std::fs::read_to_string(path).map_err(|error| {
            GuardianError::ConfigurationError(format!(
                "Failed to read ack secret file {}: {error}",
                path.display()
            ))
        })?);
        decode_secret_key(
            &format!("Ack secret file {}", path.display()),
            &contents,
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
        let path = self.ecdsa_secret_path.as_deref().ok_or_else(|| {
            GuardianError::ConfigurationError(format!(
                "{ENV_ACK_ECDSA_SECRET_PATH} is required for the in-memory ECDSA backend; set it or use GUARDIAN_ACK_ECDSA_BACKEND=aws-kms"
            ))
        })?;
        self.parsed_secret_key(path, |secret_bytes| {
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

/// Like [`required_path`] but absent is allowed (`Ok(None)`); a set-but-blank
/// value is still a misconfiguration.
fn optional_path(env_var: &str) -> Result<Option<PathBuf>> {
    match std::env::var(env_var) {
        Ok(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Err(GuardianError::ConfigurationError(format!(
                    "{env_var} must not be blank when set"
                )))
            } else {
                Ok(Some(PathBuf::from(trimmed)))
            }
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(GuardianError::ConfigurationError(format!(
            "{env_var} must contain valid UTF-8"
        ))),
    }
}

/// Reject a secret file that any principal other than the owner can touch, the
/// way OpenSSH guards private keys. These hold long-lived ACK signing keys, so a
/// group/other-accessible file fails startup rather than loading silently.
/// Unix-only; a no-op elsewhere (Windows ACLs are not modeled here).
#[cfg(unix)]
fn ensure_owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(path)
        .map_err(|error| {
            GuardianError::ConfigurationError(format!(
                "Failed to inspect ack secret file {}: {error}",
                path.display()
            ))
        })?
        .permissions()
        .mode();

    if mode & 0o077 != 0 {
        return Err(GuardianError::ConfigurationError(format!(
            "Ack secret file {} must not be accessible by group or others (set its permissions to 0600)",
            path.display()
        )));
    }

    Ok(())
}

#[cfg(not(unix))]
fn ensure_owner_only(_path: &Path) -> Result<()> {
    Ok(())
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

    /// Write a secret file with owner-only (`0600`) permissions so it passes
    /// [`ensure_owner_only`].
    fn write_secret(path: &Path, contents: impl AsRef<[u8]>) {
        std::fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn write_hex(path: &Path, bytes: &[u8]) {
        write_secret(path, hex::encode(bytes));
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
            ecdsa_secret_path: Some(ecdsa_path),
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
            ecdsa_secret_path: None,
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
        write_secret(
            &ecdsa_path,
            format!("  {}\n", hex::encode(ecdsa.to_bytes())),
        );
        let provider = FileSecretProvider {
            falcon_secret_path: dir.join("falcon"),
            ecdsa_secret_path: Some(ecdsa_path),
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
            ecdsa_secret_path: Some(PathBuf::from("/nonexistent/guardian/ack-ecdsa")),
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
        write_secret(&falcon_path, "nothex!!");
        let provider = FileSecretProvider {
            falcon_secret_path: falcon_path,
            ecdsa_secret_path: None,
        };

        let err = provider.falcon_secret_key().await.unwrap_err();
        assert!(
            matches!(err, GuardianError::ConfigurationError(message) if message.contains("hex"))
        );
        std::fs::remove_dir_all(dir).ok();
    }

    // The ECDSA file is only consulted by the in-memory backend; an aws-kms
    // backend never calls `ecdsa_secret_key`, so an unset path must not block
    // loading the Falcon key, yet must surface a clear error if consulted.
    #[tokio::test]
    async fn unset_ecdsa_path_loads_falcon_but_errors_only_when_ecdsa_consulted() {
        let dir = temp_dir("ecdsa_opt");
        let falcon = FalconSecretKey::new();
        let falcon_path = dir.join("falcon");
        write_hex(&falcon_path, &falcon.to_bytes());
        let provider = FileSecretProvider {
            falcon_secret_path: falcon_path,
            ecdsa_secret_path: None,
        };

        assert_eq!(
            provider.falcon_secret_key().await.unwrap().to_bytes(),
            falcon.to_bytes()
        );
        let err = provider.ecdsa_secret_key().await.unwrap_err();
        assert!(matches!(
            err,
            GuardianError::ConfigurationError(message)
                if message.contains(ENV_ACK_ECDSA_SECRET_PATH)
        ));
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_group_or_world_accessible_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("perms");
        let falcon = FalconSecretKey::new();
        let falcon_path = dir.join("falcon");
        std::fs::write(&falcon_path, hex::encode(falcon.to_bytes())).unwrap();
        std::fs::set_permissions(&falcon_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let provider = FileSecretProvider {
            falcon_secret_path: falcon_path,
            ecdsa_secret_path: None,
        };

        let err = provider.falcon_secret_key().await.unwrap_err();
        assert!(
            matches!(err, GuardianError::ConfigurationError(message) if message.contains("0600"))
        );
        std::fs::remove_dir_all(dir).ok();
    }
}
