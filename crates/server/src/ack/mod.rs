pub mod miden_ecdsa;
pub mod miden_falcon_rpo;
mod secrets_manager;

use crate::delta_object::DeltaObject;
use crate::error::{GuardianError, Result};
use guardian_shared::SignatureScheme;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey;
use miden_protocol::crypto::dsa::falcon512_rpo::SecretKey as FalconSecretKey;
use std::path::PathBuf;

use self::secrets_manager::{AckSecretProvider, AwsSecretsManagerProvider};

pub use miden_ecdsa::MidenEcdsaSigner;
pub use miden_falcon_rpo::MidenFalconRpoSigner;

const ENV_GUARDIAN_ENV: &str = "GUARDIAN_ENV";
const PROD_ENV: &str = "prod";

#[derive(Clone)]
pub struct AckRegistry {
    falcon: MidenFalconRpoSigner,
    ecdsa: MidenEcdsaSigner,
}

impl AckRegistry {
    pub async fn new(keystore_path: PathBuf) -> Result<Self> {
        if is_prod_environment()? {
            let provider = AwsSecretsManagerProvider::from_env().await?;
            Self::from_secret_provider(keystore_path, &provider).await
        } else {
            Self::new_filesystem(keystore_path)
        }
    }

    pub fn pubkey(&self, scheme: &SignatureScheme) -> String {
        match scheme {
            SignatureScheme::Falcon => self.falcon.pubkey_hex(),
            SignatureScheme::Ecdsa => self.ecdsa.pubkey_hex(),
        }
    }

    pub fn commitment(&self, scheme: &SignatureScheme) -> String {
        match scheme {
            SignatureScheme::Falcon => self.falcon.commitment_hex(),
            SignatureScheme::Ecdsa => self.ecdsa.commitment_hex(),
        }
    }

    pub fn ack_delta(&self, delta: DeltaObject, scheme: &SignatureScheme) -> Result<DeltaObject> {
        match scheme {
            SignatureScheme::Falcon => Ok(self.falcon.ack_delta(delta)?),
            SignatureScheme::Ecdsa => Ok(self.ecdsa.ack_delta(delta)?),
        }
    }

    fn new_filesystem(keystore_path: PathBuf) -> Result<Self> {
        let falcon = MidenFalconRpoSigner::new(keystore_path.clone(), None)?;
        let ecdsa = MidenEcdsaSigner::new(keystore_path, None)?;
        Ok(Self { falcon, ecdsa })
    }

    async fn from_secret_provider<P: AckSecretProvider>(
        keystore_path: PathBuf,
        provider: &P,
    ) -> Result<Self> {
        let (falcon_secret, ecdsa_secret) =
            tokio::try_join!(provider.falcon_secret_key(), provider.ecdsa_secret_key())?;

        Self::from_secret_keys(keystore_path, &falcon_secret, &ecdsa_secret)
    }

    fn from_secret_keys(
        keystore_path: PathBuf,
        falcon_secret: &FalconSecretKey,
        ecdsa_secret: &EcdsaSecretKey,
    ) -> Result<Self> {
        let falcon = MidenFalconRpoSigner::new(keystore_path.clone(), Some(falcon_secret))?;
        let ecdsa = MidenEcdsaSigner::new(keystore_path, Some(ecdsa_secret))?;
        Ok(Self { falcon, ecdsa })
    }
}

fn is_prod_environment() -> Result<bool> {
    match std::env::var(ENV_GUARDIAN_ENV) {
        Ok(value) => Ok(value.eq_ignore_ascii_case(PROD_ENV)),
        Err(std::env::VarError::NotPresent) => Ok(false),
        Err(std::env::VarError::NotUnicode(_)) => Err(GuardianError::ConfigurationError(format!(
            "{ENV_GUARDIAN_ENV} must contain valid UTF-8"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use miden_keystore::{EcdsaKeyStore, FilesystemEcdsaKeyStore, FilesystemKeyStore, KeyStore};
    use miden_protocol::utils::Serializable;
    use rand_chacha::ChaCha20Rng;

    struct MockAckSecretProvider {
        falcon_secret: Option<FalconSecretKey>,
        ecdsa_secret: Option<EcdsaSecretKey>,
    }

    impl MockAckSecretProvider {
        fn new(
            falcon_secret: Option<FalconSecretKey>,
            ecdsa_secret: Option<EcdsaSecretKey>,
        ) -> Self {
            Self {
                falcon_secret,
                ecdsa_secret,
            }
        }
    }

    #[async_trait]
    impl AckSecretProvider for MockAckSecretProvider {
        async fn falcon_secret_key(&self) -> Result<FalconSecretKey> {
            self.falcon_secret.clone().ok_or_else(|| {
                GuardianError::ConfigurationError(
                    "Secret guardian-prod/server/ack-falcon-secret-key not found".to_string(),
                )
            })
        }

        async fn ecdsa_secret_key(&self) -> Result<EcdsaSecretKey> {
            self.ecdsa_secret.clone().ok_or_else(|| {
                GuardianError::ConfigurationError(
                    "Secret guardian-prod/server/ack-ecdsa-secret-key not found".to_string(),
                )
            })
        }
    }

    #[tokio::test]
    async fn aws_secretsmanager_backend_imports_keys_into_filesystem_keystore() {
        let temp_dir = std::env::temp_dir().join(format!(
            "guardian_ack_registry_test_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let falcon_secret = FalconSecretKey::new();
        let ecdsa_secret = EcdsaSecretKey::new();
        let provider =
            MockAckSecretProvider::new(Some(falcon_secret.clone()), Some(ecdsa_secret.clone()));

        let registry = AckRegistry::from_secret_provider(temp_dir.clone(), &provider)
            .await
            .unwrap();

        assert_eq!(
            registry.commitment(&SignatureScheme::Falcon),
            format!(
                "0x{}",
                hex::encode(falcon_secret.public_key().to_commitment().to_bytes())
            )
        );
        assert_eq!(
            registry.commitment(&SignatureScheme::Ecdsa),
            format!(
                "0x{}",
                hex::encode(ecdsa_secret.public_key().to_commitment().to_bytes())
            )
        );

        let falcon_keystore = FilesystemKeyStore::<ChaCha20Rng>::new(temp_dir.clone()).unwrap();
        let ecdsa_keystore = FilesystemEcdsaKeyStore::new(temp_dir.clone()).unwrap();

        assert_eq!(
            falcon_keystore
                .get_key(falcon_secret.public_key().to_commitment())
                .unwrap()
                .to_bytes(),
            falcon_secret.to_bytes()
        );
        assert_eq!(
            ecdsa_keystore
                .get_ecdsa_key(ecdsa_secret.public_key().to_commitment())
                .unwrap()
                .to_bytes(),
            ecdsa_secret.to_bytes()
        );
        std::fs::remove_dir_all(temp_dir).ok();
    }

    #[tokio::test]
    async fn aws_secretsmanager_backend_requires_both_secret_values() {
        let temp_dir = std::env::temp_dir().join(format!(
            "guardian_ack_registry_test_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let falcon_secret = FalconSecretKey::new();
        let provider = MockAckSecretProvider::new(Some(falcon_secret), None);

        let result = AckRegistry::from_secret_provider(temp_dir.clone(), &provider).await;

        assert!(
            matches!(result, Err(GuardianError::ConfigurationError(message)) if message.contains("ack-ecdsa-secret-key"))
        );
        std::fs::remove_dir_all(temp_dir).ok();
    }
}
