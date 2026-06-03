use super::EcdsaSignerBackend;
use crate::error::{GuardianError, Result};
use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_sdk_kms::Client;
use aws_sdk_kms::config::timeout::TimeoutConfig;
use aws_sdk_kms::primitives::Blob;
use aws_sdk_kms::types::{KeySpec, KeyUsageType, MessageType, SigningAlgorithmSpec};
use miden_protocol::Word;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{PublicKey, Signature};
use sha3::{Digest, Keccak256};
use std::sync::Arc;
use std::time::Duration;

const ENV_KMS_KEY_ID: &str = "GUARDIAN_ACK_ECDSA_KMS_KEY_ID";
const KMS_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);

pub struct KmsPublicKeyInfo {
    pub spki_der: Vec<u8>,
    pub key_spec: Option<KeySpec>,
    pub key_usage: Option<KeyUsageType>,
}

#[async_trait]
pub trait KmsEcdsaClient: Send + Sync {
    async fn get_public_key(&self) -> Result<KmsPublicKeyInfo>;
    async fn sign(&self, digest: [u8; 32]) -> Result<Vec<u8>>;
}

struct AwsKmsClient {
    client: Client,
    key_id: String,
}

#[async_trait]
impl KmsEcdsaClient for AwsKmsClient {
    async fn get_public_key(&self) -> Result<KmsPublicKeyInfo> {
        let response = self
            .client
            .get_public_key()
            .key_id(&self.key_id)
            .send()
            .await
            .map_err(|error| {
                GuardianError::ConfigurationError(format!(
                    "KMS GetPublicKey failed for key {}: {error}",
                    self.key_id
                ))
            })?;

        let spki_der = response
            .public_key()
            .map(|blob| blob.as_ref().to_vec())
            .ok_or_else(|| {
                GuardianError::ConfigurationError(
                    "KMS GetPublicKey returned no public key bytes".to_string(),
                )
            })?;

        Ok(KmsPublicKeyInfo {
            spki_der,
            key_spec: response.key_spec().cloned(),
            key_usage: response.key_usage().cloned(),
        })
    }

    async fn sign(&self, digest: [u8; 32]) -> Result<Vec<u8>> {
        let response = self
            .client
            .sign()
            .key_id(&self.key_id)
            .message(Blob::new(digest.to_vec()))
            .message_type(MessageType::Digest)
            .signing_algorithm(SigningAlgorithmSpec::EcdsaSha256)
            .send()
            .await
            .map_err(|error| GuardianError::SigningError(format!("KMS Sign failed: {error}")))?;

        response
            .signature()
            .map(|blob| blob.as_ref().to_vec())
            .ok_or_else(|| {
                GuardianError::SigningError("KMS Sign returned no signature bytes".to_string())
            })
    }
}

pub struct AwsKmsEcdsaBackend {
    client: Arc<dyn KmsEcdsaClient>,
    public_key: PublicKey,
    timeout: Duration,
}

impl std::fmt::Debug for AwsKmsEcdsaBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsKmsEcdsaBackend").finish_non_exhaustive()
    }
}

impl AwsKmsEcdsaBackend {
    pub async fn connect_from_env() -> Result<Self> {
        let key_id = resolve_key_id()?;
        let shared = aws_config::defaults(BehaviorVersion::latest()).load().await;
        let config = aws_sdk_kms::config::Builder::from(&shared)
            .timeout_config(
                TimeoutConfig::builder()
                    .operation_timeout(KMS_OPERATION_TIMEOUT)
                    .build(),
            )
            .build();
        let client = AwsKmsClient {
            client: Client::from_conf(config),
            key_id,
        };
        Self::connect(Arc::new(client)).await
    }

    async fn connect(client: Arc<dyn KmsEcdsaClient>) -> Result<Self> {
        Self::connect_with(client, KMS_OPERATION_TIMEOUT).await
    }

    async fn connect_with(client: Arc<dyn KmsEcdsaClient>, timeout: Duration) -> Result<Self> {
        let info = match tokio::time::timeout(timeout, client.get_public_key()).await {
            Ok(result) => result?,
            Err(_) => {
                return Err(GuardianError::ConfigurationError(format!(
                    "KMS GetPublicKey timed out after {timeout:?}"
                )));
            }
        };
        let public_key = validate_public_key(&info)?;
        let backend = Self {
            client,
            public_key,
            timeout,
        };
        backend.run_sign_probe().await?;
        Ok(backend)
    }

    async fn run_sign_probe(&self) -> Result<()> {
        let probe = Word::default();
        let signature = self.sign_digest(probe).await.map_err(|error| {
            GuardianError::ConfigurationError(format!("KMS sign probe failed: {error}"))
        })?;
        if self.public_key.verify(probe, &signature) {
            Ok(())
        } else {
            Err(GuardianError::ConfigurationError(
                "KMS sign probe produced a signature that does not verify against the configured public key".to_string(),
            ))
        }
    }

    async fn sign_digest(&self, message: Word) -> Result<Signature> {
        let der = match tokio::time::timeout(self.timeout, self.client.sign(keccak_digest(message)))
            .await
        {
            Ok(result) => result?,
            Err(_) => {
                return Err(GuardianError::SigningError(format!(
                    "KMS Sign timed out after {:?}",
                    self.timeout
                )));
            }
        };
        der_to_signature(&der, message, &self.public_key)
    }
}

#[async_trait]
impl EcdsaSignerBackend for AwsKmsEcdsaBackend {
    fn public_key(&self) -> &PublicKey {
        &self.public_key
    }

    async fn sign(&self, message: Word) -> Result<Signature> {
        self.sign_digest(message).await
    }

    fn backend_id(&self) -> &'static str {
        "aws-kms"
    }
}

fn resolve_key_id() -> Result<String> {
    match std::env::var(ENV_KMS_KEY_ID) {
        Ok(value) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        Ok(_) => Err(GuardianError::ConfigurationError(format!(
            "{ENV_KMS_KEY_ID} must not be blank when GUARDIAN_ACK_ECDSA_BACKEND=aws-kms"
        ))),
        Err(std::env::VarError::NotPresent) => Err(GuardianError::ConfigurationError(format!(
            "{ENV_KMS_KEY_ID} is required when GUARDIAN_ACK_ECDSA_BACKEND=aws-kms"
        ))),
        Err(std::env::VarError::NotUnicode(_)) => Err(GuardianError::ConfigurationError(format!(
            "{ENV_KMS_KEY_ID} must contain valid UTF-8"
        ))),
    }
}

fn validate_public_key(info: &KmsPublicKeyInfo) -> Result<PublicKey> {
    match info.key_spec {
        Some(KeySpec::EccSecgP256K1) => {}
        ref other => {
            return Err(GuardianError::ConfigurationError(format!(
                "KMS key spec {other:?} is not ECC_SECG_P256K1 (secp256k1)"
            )));
        }
    }
    match info.key_usage {
        Some(KeyUsageType::SignVerify) => {}
        ref other => {
            return Err(GuardianError::ConfigurationError(format!(
                "KMS key usage {other:?} is not SIGN_VERIFY"
            )));
        }
    }
    PublicKey::from_der(&info.spki_der).map_err(|error| {
        GuardianError::ConfigurationError(format!("KMS public key could not be parsed: {error}"))
    })
}

fn keccak_digest(message: Word) -> [u8; 32] {
    let bytes: [u8; 32] = message.into();
    let mut hasher = Keccak256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

fn der_to_signature(der: &[u8], message: Word, public_key: &PublicKey) -> Result<Signature> {
    for recovery_id in [0u8, 1u8] {
        let Ok(candidate) = Signature::from_der(der, recovery_id) else {
            continue;
        };
        if let Ok(recovered) = PublicKey::recover_from(message, &candidate)
            && &recovered == public_key
        {
            return Ok(candidate);
        }
    }
    Err(GuardianError::SigningError(
        "Could not derive a recovery id for the KMS signature matching the configured public key"
            .to_string(),
    ))
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use k256::SecretKey;
    use k256::ecdsa::SigningKey;
    use k256::ecdsa::signature::hazmat::PrehashSigner;
    use k256::pkcs8::EncodePublicKey;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_secret() -> SecretKey {
        SecretKey::from_slice(&[7u8; 32]).unwrap()
    }

    fn spki_der(secret: &SecretKey) -> Vec<u8> {
        secret
            .public_key()
            .to_public_key_der()
            .unwrap()
            .as_bytes()
            .to_vec()
    }

    fn der_low_s(secret: &SecretKey, digest: [u8; 32]) -> Vec<u8> {
        let signing_key = SigningKey::from(secret);
        let signature: k256::ecdsa::Signature = signing_key.sign_prehash(&digest).unwrap();
        signature.to_der().as_bytes().to_vec()
    }

    fn der_high_s(secret: &SecretKey, digest: [u8; 32]) -> Vec<u8> {
        let signing_key = SigningKey::from(secret);
        let signature: k256::ecdsa::Signature = signing_key.sign_prehash(&digest).unwrap();
        let r = signature.r();
        let s_high = -*signature.s();
        let high = k256::ecdsa::Signature::from_scalars(r.to_bytes(), s_high.to_bytes()).unwrap();
        high.to_der().as_bytes().to_vec()
    }

    struct FakeKmsClient {
        secret: SecretKey,
        key_spec: Option<KeySpec>,
        key_usage: Option<KeyUsageType>,
        fail_get_public_key: bool,
        fail_sign: bool,
        delay: Option<Duration>,
        high_s: bool,
        sign_calls: Arc<AtomicUsize>,
    }

    impl FakeKmsClient {
        fn valid() -> Self {
            Self {
                secret: test_secret(),
                key_spec: Some(KeySpec::EccSecgP256K1),
                key_usage: Some(KeyUsageType::SignVerify),
                fail_get_public_key: false,
                fail_sign: false,
                delay: None,
                high_s: false,
                sign_calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl KmsEcdsaClient for FakeKmsClient {
        async fn get_public_key(&self) -> Result<KmsPublicKeyInfo> {
            if self.fail_get_public_key {
                return Err(GuardianError::ConfigurationError("denied".to_string()));
            }
            Ok(KmsPublicKeyInfo {
                spki_der: spki_der(&self.secret),
                key_spec: self.key_spec.clone(),
                key_usage: self.key_usage.clone(),
            })
        }

        async fn sign(&self, digest: [u8; 32]) -> Result<Vec<u8>> {
            self.sign_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(delay) = self.delay {
                tokio::time::sleep(delay).await;
            }
            if self.fail_sign {
                return Err(GuardianError::SigningError("denied".to_string()));
            }
            let der = if self.high_s {
                der_high_s(&self.secret, digest)
            } else {
                der_low_s(&self.secret, digest)
            };
            Ok(der)
        }
    }

    #[tokio::test]
    async fn connect_validates_and_signs_verifiable_signatures() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut fake = FakeKmsClient::valid();
        fake.sign_calls = calls.clone();
        let backend = AwsKmsEcdsaBackend::connect(Arc::new(fake)).await.unwrap();

        assert!(calls.load(Ordering::SeqCst) >= 1);

        let message = Word::default();
        let signature = backend.sign(message).await.unwrap();
        assert!(backend.public_key().verify(message, &signature));
    }

    #[tokio::test]
    async fn converts_high_s_signatures() {
        let mut fake = FakeKmsClient::valid();
        fake.high_s = true;
        let backend = AwsKmsEcdsaBackend::connect(Arc::new(fake)).await.unwrap();
        let message = Word::default();
        let signature = backend.sign(message).await.unwrap();
        assert!(backend.public_key().verify(message, &signature));
    }

    #[tokio::test]
    async fn signature_serializes_to_sixty_five_bytes() {
        use miden_protocol::utils::serde::Serializable;
        let backend = AwsKmsEcdsaBackend::connect(Arc::new(FakeKmsClient::valid()))
            .await
            .unwrap();
        let signature = backend.sign(Word::default()).await.unwrap();
        assert_eq!(signature.to_bytes().len(), 65);
    }

    #[tokio::test]
    async fn rejects_wrong_key_spec() {
        let mut fake = FakeKmsClient::valid();
        fake.key_spec = Some(KeySpec::EccNistP256);
        let error = AwsKmsEcdsaBackend::connect(Arc::new(fake))
            .await
            .unwrap_err();
        assert!(matches!(error, GuardianError::ConfigurationError(_)));
    }

    #[tokio::test]
    async fn rejects_non_signing_key_usage() {
        let mut fake = FakeKmsClient::valid();
        fake.key_usage = Some(KeyUsageType::EncryptDecrypt);
        let error = AwsKmsEcdsaBackend::connect(Arc::new(fake))
            .await
            .unwrap_err();
        assert!(matches!(error, GuardianError::ConfigurationError(_)));
    }

    #[tokio::test]
    async fn fails_fast_when_get_public_key_denied() {
        let mut fake = FakeKmsClient::valid();
        fake.fail_get_public_key = true;
        let error = AwsKmsEcdsaBackend::connect(Arc::new(fake))
            .await
            .unwrap_err();
        assert!(matches!(error, GuardianError::ConfigurationError(_)));
    }

    #[tokio::test]
    async fn sign_times_out_instead_of_hanging() {
        let mut fake = FakeKmsClient::valid();
        fake.delay = Some(Duration::from_secs(30));
        let error = AwsKmsEcdsaBackend::connect_with(Arc::new(fake), Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(matches!(error, GuardianError::ConfigurationError(_)));
    }

    #[tokio::test]
    async fn sign_probe_fails_fast_when_sign_denied() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut fake = FakeKmsClient::valid();
        fake.fail_sign = true;
        fake.sign_calls = calls.clone();
        let error = AwsKmsEcdsaBackend::connect(Arc::new(fake))
            .await
            .unwrap_err();
        assert!(calls.load(Ordering::SeqCst) >= 1);
        assert!(matches!(error, GuardianError::ConfigurationError(_)));
    }
}
