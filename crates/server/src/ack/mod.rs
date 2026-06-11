//! ACK signing: Guardian's own response signers over delta commitments.
//!
//! [`AckRegistry`] holds both schemes — Falcon and ECDSA — and signs a delta
//! with the scheme the request selects. The ECDSA signer is abstracted over a
//! pluggable backend so its key can live in a hosted service; Falcon stays
//! concrete. The two are built independently at startup: a hosted ECDSA backend
//! must not require an ECDSA secret in Secrets Manager that does not exist.

pub mod miden_ecdsa;
pub mod miden_falcon_rpo;
mod secrets_manager;

use crate::delta_object::DeltaObject;
use crate::error::{GuardianError, Result};
use guardian_shared::SignatureScheme;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey as EcdsaSecretKey;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use self::secrets_manager::{AckSecretProvider, AwsSecretsManagerProvider};

pub(crate) use miden_ecdsa::{
    AwsKmsEcdsaBackend, EcdsaBackendKind, EcdsaSignerBackend, InMemoryEcdsaBackend,
    MidenEcdsaSigner,
};
pub use miden_falcon_rpo::MidenFalconRpoSigner;

const ENV_GUARDIAN_ENV: &str = "GUARDIAN_ENV";
const PROD_ENV: &str = "prod";

/// The ECDSA signer is abstracted over [`EcdsaSignerBackend`] so its key can live
/// in a hosted backend (e.g. AWS KMS); Falcon stays concrete because hosted
/// backends only support the secp256k1 ECDSA scheme.
#[derive(Clone)]
pub struct AckRegistry {
    falcon: MidenFalconRpoSigner,
    ecdsa: MidenEcdsaSigner,
}

impl AckRegistry {
    pub async fn new(keystore_path: PathBuf) -> Result<Self> {
        let ecdsa_backend = EcdsaBackendKind::from_env()?;
        if is_prod_environment()? {
            let provider = AwsSecretsManagerProvider::from_env().await?;
            Self::from_provider(keystore_path, ecdsa_backend, Some(&provider)).await
        } else {
            Self::from_provider(
                keystore_path,
                ecdsa_backend,
                None::<&AwsSecretsManagerProvider>,
            )
            .await
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

    pub(crate) fn ecdsa_backend_id(&self) -> &'static str {
        self.ecdsa.backend_id()
    }

    pub async fn ack_delta(
        &self,
        delta: DeltaObject,
        scheme: &SignatureScheme,
    ) -> Result<DeltaObject> {
        match scheme {
            SignatureScheme::Falcon => Ok(self.falcon.ack_delta(delta)?),
            SignatureScheme::Ecdsa => self.ecdsa.ack_delta(delta).await,
        }
    }

    async fn from_provider<P: AckSecretProvider>(
        keystore_path: PathBuf,
        ecdsa_backend: EcdsaBackendKind,
        provider: Option<&P>,
    ) -> Result<Self> {
        let falcon = build_falcon_signer(&keystore_path, provider).await?;
        let ecdsa = build_ecdsa_signer(keystore_path, ecdsa_backend, provider).await?;
        Ok(Self { falcon, ecdsa })
    }
}

async fn build_falcon_signer<P: AckSecretProvider>(
    keystore_path: &Path,
    provider: Option<&P>,
) -> Result<MidenFalconRpoSigner> {
    let secret = match provider {
        Some(provider) => Some(provider.falcon_secret_key().await?),
        None => None,
    };
    Ok(MidenFalconRpoSigner::new(
        keystore_path.to_path_buf(),
        secret.as_ref(),
    )?)
}

async fn acquire_ecdsa_secret<P: AckSecretProvider>(
    ecdsa_backend: EcdsaBackendKind,
    provider: Option<&P>,
) -> Result<Option<EcdsaSecretKey>> {
    match (ecdsa_backend, provider) {
        (EcdsaBackendKind::InMemory, Some(provider)) => {
            Ok(Some(provider.ecdsa_secret_key().await?))
        }
        _ => Ok(None),
    }
}

async fn build_ecdsa_signer<P: AckSecretProvider>(
    keystore_path: PathBuf,
    ecdsa_backend: EcdsaBackendKind,
    provider: Option<&P>,
) -> Result<MidenEcdsaSigner> {
    let backend: Arc<dyn EcdsaSignerBackend> = match ecdsa_backend {
        EcdsaBackendKind::InMemory => {
            let secret = acquire_ecdsa_secret(ecdsa_backend, provider).await?;
            Arc::new(InMemoryEcdsaBackend::new(keystore_path, secret.as_ref())?)
        }
        EcdsaBackendKind::AwsKms => Arc::new(AwsKmsEcdsaBackend::connect_from_env().await?),
    };
    tracing::info!(backend = backend.backend_id(), "ECDSA ACK signer ready");
    Ok(MidenEcdsaSigner::new(backend))
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

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use miden_keystore::{EcdsaKeyStore, FilesystemEcdsaKeyStore, FilesystemKeyStore, KeyStore};
    use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey as FalconSecretKey;
    use miden_protocol::utils::serde::Serializable;
    use rand_chacha::ChaCha20Rng;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingProvider {
        falcon_secret: Option<FalconSecretKey>,
        ecdsa_secret: Option<EcdsaSecretKey>,
        falcon_calls: Arc<AtomicUsize>,
        ecdsa_calls: Arc<AtomicUsize>,
    }

    impl CountingProvider {
        fn new(
            falcon_secret: Option<FalconSecretKey>,
            ecdsa_secret: Option<EcdsaSecretKey>,
        ) -> Self {
            Self {
                falcon_secret,
                ecdsa_secret,
                falcon_calls: Arc::new(AtomicUsize::new(0)),
                ecdsa_calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl AckSecretProvider for CountingProvider {
        async fn falcon_secret_key(&self) -> Result<FalconSecretKey> {
            self.falcon_calls.fetch_add(1, Ordering::SeqCst);
            self.falcon_secret.clone().ok_or_else(|| {
                GuardianError::ConfigurationError("falcon ack secret not found".to_string())
            })
        }

        async fn ecdsa_secret_key(&self) -> Result<EcdsaSecretKey> {
            self.ecdsa_calls.fetch_add(1, Ordering::SeqCst);
            self.ecdsa_secret.clone().ok_or_else(|| {
                GuardianError::ConfigurationError("ecdsa ack secret not found".to_string())
            })
        }
    }

    fn temp_keystore(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "guardian_ack_registry_{tag}_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn in_memory_backend_imports_keys_into_filesystem_keystore() {
        let dir = temp_keystore("import");
        let falcon_secret = FalconSecretKey::new();
        let ecdsa_secret = EcdsaSecretKey::new();
        let provider =
            CountingProvider::new(Some(falcon_secret.clone()), Some(ecdsa_secret.clone()));

        let registry =
            AckRegistry::from_provider(dir.clone(), EcdsaBackendKind::InMemory, Some(&provider))
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

        let falcon_keystore = FilesystemKeyStore::<ChaCha20Rng>::new(dir.clone()).unwrap();
        let ecdsa_keystore = FilesystemEcdsaKeyStore::new(dir.clone()).unwrap();
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
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn in_memory_backend_requires_ecdsa_secret() {
        let dir = temp_keystore("require");
        let provider = CountingProvider::new(Some(FalconSecretKey::new()), None);

        let result =
            AckRegistry::from_provider(dir.clone(), EcdsaBackendKind::InMemory, Some(&provider))
                .await;

        assert!(
            matches!(result, Err(GuardianError::ConfigurationError(message)) if message.contains("ecdsa"))
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn aws_kms_backend_skips_ecdsa_secret_fetch() {
        let provider =
            CountingProvider::new(Some(FalconSecretKey::new()), Some(EcdsaSecretKey::new()));

        let secret = acquire_ecdsa_secret(EcdsaBackendKind::AwsKms, Some(&provider))
            .await
            .unwrap();

        assert!(secret.is_none());
        assert_eq!(provider.ecdsa_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn in_memory_backend_fetches_ecdsa_secret() {
        let provider =
            CountingProvider::new(Some(FalconSecretKey::new()), Some(EcdsaSecretKey::new()));

        let secret = acquire_ecdsa_secret(EcdsaBackendKind::InMemory, Some(&provider))
            .await
            .unwrap();

        assert!(secret.is_some());
        assert_eq!(provider.ecdsa_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn falcon_secret_is_fetched_regardless_of_ecdsa_backend() {
        let dir = temp_keystore("falcon");
        let provider =
            CountingProvider::new(Some(FalconSecretKey::new()), Some(EcdsaSecretKey::new()));

        build_falcon_signer(&dir, Some(&provider)).await.unwrap();

        assert_eq!(provider.falcon_calls.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(dir).ok();
    }
}
