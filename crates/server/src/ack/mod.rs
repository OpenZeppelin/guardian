//! ACK signing: Guardian's own response signers over delta commitments.
//!
//! [`AckRegistry`] holds both schemes — Falcon and ECDSA — and signs a delta
//! with the scheme the request selects. The ECDSA signer is abstracted over a
//! pluggable backend so its key can live in a hosted service; Falcon stays
//! concrete. The two are built independently at startup: a hosted ECDSA backend
//! must not require an ECDSA secret in Secrets Manager that does not exist.

mod file_provider;
pub mod miden_ecdsa;
pub mod miden_falcon_rpo;
mod secrets_manager;

use crate::config::account_schemes::AllowedAccountSchemes;
use crate::delta_object::DeltaObject;
use crate::error::{GuardianError, Result};
use guardian_shared::SignatureScheme;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey as EcdsaSecretKey;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use self::file_provider::FileSecretProvider;
use self::secrets_manager::{AckSecretProvider, AwsSecretsManagerProvider};

pub(crate) use miden_ecdsa::{
    AwsKmsEcdsaBackend, EcdsaBackendKind, EcdsaSignerBackend, InMemoryEcdsaBackend,
    MidenEcdsaSigner,
};
pub use miden_falcon_rpo::MidenFalconRpoSigner;

const ENV_GUARDIAN_ENV: &str = "GUARDIAN_ENV";
const PROD_ENV: &str = "prod";
const ENV_ACK_SECRET_PROVIDER: &str = "GUARDIAN_ACK_SECRET_PROVIDER";
const PROVIDER_AWS: &str = "aws";
const PROVIDER_FILE: &str = "file";
const PROVIDER_NONE: &str = "none";

/// The ECDSA signer is abstracted over [`EcdsaSignerBackend`] so its key can live
/// in a hosted backend (e.g. AWS KMS); Falcon stays concrete because hosted
/// backends only support the secp256k1 ECDSA scheme.
#[derive(Clone)]
pub struct AckRegistry {
    falcon: MidenFalconRpoSigner,
    ecdsa: MidenEcdsaSigner,
    account_schemes: AllowedAccountSchemes,
}

impl AckRegistry {
    pub async fn new(keystore_path: PathBuf) -> Result<Self> {
        let ecdsa_backend = EcdsaBackendKind::from_env()?;
        let account_schemes = AllowedAccountSchemes::from_env()?;
        let provider = AckSecretProviderKind::from_env()?.build().await?;
        Self::from_provider(keystore_path, ecdsa_backend, provider.as_deref())
            .await
            .map(|registry| registry.with_account_schemes(account_schemes))
    }

    /// Restrict which signature schemes new accounts may register with. Both
    /// ACK signers stay loaded regardless, because accounts registered before
    /// a restriction keep their scheme for life.
    pub fn with_account_schemes(mut self, account_schemes: AllowedAccountSchemes) -> Self {
        self.account_schemes = account_schemes;
        self
    }

    pub fn account_schemes(&self) -> AllowedAccountSchemes {
        self.account_schemes
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

    /// Whether `delta` carries an ack this server signed with the key it
    /// holds now for `scheme` (checked against the signature itself, which
    /// every storage backend keeps). Tells a delta this server acknowledged
    /// apart from a change of this server's own key.
    pub fn acked_with_current_key(&self, delta: &DeltaObject, scheme: &SignatureScheme) -> bool {
        match scheme {
            SignatureScheme::Falcon => self.falcon.signed_ack(delta),
            SignatureScheme::Ecdsa => self.ecdsa.signed_ack(delta),
        }
    }

    /// A registry whose Falcon signer holds `falcon_secret` (an ephemeral
    /// ECDSA signer beside it), for tests that drive pre-generated
    /// fixture accounts: their guardian key must be this server's, or
    /// every promotion looks like a switch away from it.
    #[cfg(all(test, feature = "e2e"))]
    pub(crate) async fn with_falcon_secret_for_tests(
        keystore_path: PathBuf,
        falcon_secret: &miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey,
    ) -> Result<Self> {
        let falcon = MidenFalconRpoSigner::new(keystore_path.clone(), Some(falcon_secret))?;
        let ecdsa = build_ecdsa_signer(
            keystore_path,
            EcdsaBackendKind::InMemory,
            None::<&FileSecretProvider>,
        )
        .await?;
        Ok(Self {
            falcon,
            ecdsa,
            account_schemes: AllowedAccountSchemes::ALL,
        })
    }

    async fn from_provider<P: AckSecretProvider + ?Sized>(
        keystore_path: PathBuf,
        ecdsa_backend: EcdsaBackendKind,
        provider: Option<&P>,
    ) -> Result<Self> {
        let falcon = build_falcon_signer(&keystore_path, provider).await?;
        let ecdsa = build_ecdsa_signer(keystore_path, ecdsa_backend, provider).await?;
        Ok(Self {
            falcon,
            ecdsa,
            account_schemes: AllowedAccountSchemes::ALL,
        })
    }
}

/// The message a delta's ack signs: its transaction summary's commitment.
pub(crate) fn ack_message(delta: &DeltaObject) -> Option<miden_protocol::Word> {
    use guardian_shared::FromJson;
    miden_protocol::transaction::TransactionSummary::from_json(&delta.delta_payload)
        .ok()
        .map(|summary| summary.to_commitment())
}

/// A hex-encoded ack signature (as stored in `ack_sig`), if it decodes.
pub(crate) fn decode_ack_signature<S: miden_protocol::utils::serde::Deserializable>(
    ack_sig: &str,
) -> Option<S> {
    let bytes = hex::decode(ack_sig.trim_start_matches("0x")).ok()?;
    S::read_from_bytes(&bytes).ok()
}

async fn build_falcon_signer<P: AckSecretProvider + ?Sized>(
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

async fn acquire_ecdsa_secret<P: AckSecretProvider + ?Sized>(
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

async fn build_ecdsa_signer<P: AckSecretProvider + ?Sized>(
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

/// Selects where the ACK signing secrets come from at startup, and thus whether
/// Guardian keeps a stable identity. [`Aws`](Self::Aws) and [`File`](Self::File)
/// both supply fixed key material; [`None`](Self::None) generates an ephemeral
/// keypair on every boot, so the on-chain ack-key commitment changes each
/// restart and freezes any account that pinned the previous one. Selected by
/// `GUARDIAN_ACK_SECRET_PROVIDER`; when unset it defaults to AWS in prod and
/// ephemeral elsewhere, preserving the historical behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AckSecretProviderKind {
    Aws,
    File,
    None,
}

impl AckSecretProviderKind {
    fn from_env() -> Result<Self> {
        let raw = match std::env::var(ENV_ACK_SECRET_PROVIDER) {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(GuardianError::ConfigurationError(format!(
                    "{ENV_ACK_SECRET_PROVIDER} must contain valid UTF-8"
                )));
            }
        };
        Self::resolve(raw.as_deref(), crate::config::stage::is_prod()?)
    }

    /// Pure resolution of the provider kind, split out from [`from_env`] so it is
    /// testable without mutating process-global env vars (which `AckRegistry::new`
    /// reads concurrently). `raw` is the `GUARDIAN_ACK_SECRET_PROVIDER` value
    /// (`None` when unset); `is_prod` is whether `GUARDIAN_ENV=prod`. Unset or
    /// blank falls back to the backward-compatible default — AWS in prod,
    /// ephemeral elsewhere — while an explicit value wins, except `none` is
    /// refused in prod so a stable-identity deployment can't boot ephemeral.
    fn resolve(raw: Option<&str>, is_prod: bool) -> Result<Self> {
        let default = if is_prod { Self::Aws } else { Self::None };
        match raw.map(|value| value.trim().to_ascii_lowercase()) {
            None => Ok(default),
            Some(value) => match value.as_str() {
                "" => Ok(default),
                PROVIDER_AWS => Ok(Self::Aws),
                PROVIDER_FILE => Ok(Self::File),
                // Explicit `none` in prod would boot ephemeral keys and silently
                // invalidate every account that pinned the on-chain commitment;
                // refuse it rather than start with a throwaway identity.
                PROVIDER_NONE if is_prod => Err(GuardianError::ConfigurationError(format!(
                    "{ENV_ACK_SECRET_PROVIDER}=`{PROVIDER_NONE}` is not allowed when {ENV_GUARDIAN_ENV}=`{PROD_ENV}`"
                ))),
                PROVIDER_NONE => Ok(Self::None),
                other => Err(GuardianError::ConfigurationError(format!(
                    "{ENV_ACK_SECRET_PROVIDER} `{other}` is not supported (expected `{PROVIDER_AWS}`, `{PROVIDER_FILE}`, or `{PROVIDER_NONE}`)"
                ))),
            },
        }
    }

    /// Constructs the selected provider. `None` means no provider — the signers
    /// fall back to ephemeral keys generated in the keystore.
    async fn build(self) -> Result<Option<Box<dyn AckSecretProvider>>> {
        match self {
            Self::Aws => Ok(Some(Box::new(AwsSecretsManagerProvider::from_env().await?))),
            Self::File => Ok(Some(Box::new(FileSecretProvider::from_env()?))),
            Self::None => Ok(None),
        }
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::error::GuardianError;
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
    async fn an_ack_is_recognised_only_under_the_key_that_signed_it() {
        use crate::delta_object::{DeltaObject, DeltaStatus};
        use crate::testing::helpers::create_test_delta_payload;

        async fn registry(tag: &str) -> (AckRegistry, PathBuf) {
            let dir = temp_keystore(tag);
            let provider =
                CountingProvider::new(Some(FalconSecretKey::new()), Some(EcdsaSecretKey::new()));
            let registry = AckRegistry::from_provider(
                dir.clone(),
                EcdsaBackendKind::InMemory,
                Some(&provider),
            )
            .await
            .unwrap();
            (registry, dir)
        }
        fn delta(account_id: &str) -> DeltaObject {
            DeltaObject {
                account_id: account_id.to_string(),
                nonce: 1,
                prev_commitment: "0xprev".to_string(),
                new_commitment: None,
                delta_payload: create_test_delta_payload(account_id),
                ack_sig: String::new(),
                ack_pubkey: String::new(),
                ack_scheme: String::new(),
                status: DeltaStatus::canonical("2026-09-28T00:00:00Z".to_string()),
                metadata: None,
            }
        }
        const ACCOUNT: &str = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";

        let (current, current_dir) = registry("current").await;
        let (previous, previous_dir) = registry("previous").await;
        for scheme in [SignatureScheme::Falcon, SignatureScheme::Ecdsa] {
            let acked = current.ack_delta(delta(ACCOUNT), &scheme).await.unwrap();
            assert!(current.acked_with_current_key(&acked, &scheme));

            let by_previous = previous.ack_delta(delta(ACCOUNT), &scheme).await.unwrap();
            assert!(
                !current.acked_with_current_key(&by_previous, &scheme),
                "an ack made with a key this server no longer holds"
            );
            assert!(
                !current.acked_with_current_key(&delta(ACCOUNT), &scheme),
                "an unsigned delta"
            );
            // (Every empty delta has the same commitment, so the other
            // summary is a real one.)
            let mut other_summary = acked.clone();
            other_summary.delta_payload =
                crate::testing::helpers::load_fixture_delta(1)["delta_payload"].clone();
            assert!(
                !current.acked_with_current_key(&other_summary, &scheme),
                "a signature over another summary"
            );
        }
        let falcon_ack = current
            .ack_delta(delta(ACCOUNT), &SignatureScheme::Falcon)
            .await
            .unwrap();
        assert!(!current.acked_with_current_key(&falcon_ack, &SignatureScheme::Ecdsa));
        std::fs::remove_dir_all(current_dir).ok();
        std::fs::remove_dir_all(previous_dir).ok();
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

    // `AckSecretProviderKind::resolve` is the pure core of provider selection;
    // it is tested directly so these cases never mutate the process-global env
    // vars that `AckRegistry::new` reads concurrently in other tests.
    #[test]
    fn provider_kind_unset_defaults_to_none_outside_prod() {
        assert_eq!(
            AckSecretProviderKind::resolve(None, false).unwrap(),
            AckSecretProviderKind::None
        );
    }

    #[test]
    fn provider_kind_unset_defaults_to_aws_in_prod() {
        assert_eq!(
            AckSecretProviderKind::resolve(None, true).unwrap(),
            AckSecretProviderKind::Aws
        );
    }

    #[test]
    fn provider_kind_blank_falls_back_to_default() {
        assert_eq!(
            AckSecretProviderKind::resolve(Some("   "), true).unwrap(),
            AckSecretProviderKind::Aws
        );
        assert_eq!(
            AckSecretProviderKind::resolve(Some(""), false).unwrap(),
            AckSecretProviderKind::None
        );
    }

    #[test]
    fn provider_kind_explicit_file_overrides_prod_default() {
        assert_eq!(
            AckSecretProviderKind::resolve(Some("FILE"), true).unwrap(),
            AckSecretProviderKind::File
        );
    }

    #[test]
    fn provider_kind_rejects_explicit_none_in_prod() {
        let error = AckSecretProviderKind::resolve(Some("none"), true).unwrap_err();
        assert!(
            matches!(error, GuardianError::ConfigurationError(message) if message.contains("not allowed"))
        );
    }

    #[test]
    fn provider_kind_explicit_none_allowed_outside_prod() {
        assert_eq!(
            AckSecretProviderKind::resolve(Some("none"), false).unwrap(),
            AckSecretProviderKind::None
        );
    }

    #[test]
    fn provider_kind_explicit_aws_selected_outside_prod() {
        assert_eq!(
            AckSecretProviderKind::resolve(Some("aws"), false).unwrap(),
            AckSecretProviderKind::Aws
        );
    }

    #[test]
    fn provider_kind_rejects_unknown_value() {
        let error = AckSecretProviderKind::resolve(Some("vault"), false).unwrap_err();
        assert!(
            matches!(error, GuardianError::ConfigurationError(message) if message.contains("vault"))
        );
    }
}
