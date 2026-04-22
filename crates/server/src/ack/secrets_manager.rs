use crate::error::{GuardianError, Result};
use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_sdk_secretsmanager::Client;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey as FalconSecretKey;
use miden_protocol::utils::serde::Deserializable;

const ENV_AWS_REGION: &str = "AWS_REGION";
pub const PROD_FALCON_SECRET_ID: &str = "guardian-prod/server/ack-falcon-secret-key";
pub const PROD_ECDSA_SECRET_ID: &str = "guardian-prod/server/ack-ecdsa-secret-key";

#[async_trait]
pub trait AckSecretProvider: Send + Sync {
    async fn falcon_secret_key(&self) -> Result<FalconSecretKey>;
    async fn ecdsa_secret_key(&self) -> Result<EcdsaSecretKey>;
}

pub struct AwsSecretsManagerProvider {
    client: Client,
}

impl AwsSecretsManagerProvider {
    pub async fn from_env() -> Result<Self> {
        ensure_aws_region()?;
        let config = aws_config::defaults(BehaviorVersion::latest()).load().await;

        Ok(Self {
            client: Client::new(&config),
        })
    }

    async fn secret_string(&self, secret_id: &str) -> Result<String> {
        let response = self
            .client
            .get_secret_value()
            .secret_id(secret_id)
            .send()
            .await
            .map_err(|error| {
                GuardianError::ConfigurationError(format!(
                    "Failed to load ack secret {secret_id} from Secrets Manager: {error}"
                ))
            })?;

        response.secret_string().map(str::to_owned).ok_or_else(|| {
            GuardianError::ConfigurationError(format!(
                "Secret {secret_id} does not contain a secret string value"
            ))
        })
    }

    async fn parsed_secret_key<T, F>(&self, secret_id: &str, parser: F) -> Result<T>
    where
        F: FnOnce(&[u8]) -> std::result::Result<T, String>,
    {
        let secret_hex = self.secret_string(secret_id).await?;
        let secret_bytes = hex::decode(secret_hex.trim()).map_err(|error| {
            GuardianError::ConfigurationError(format!(
                "Secret {secret_id} must contain valid hex-encoded key bytes: {error}"
            ))
        })?;

        parser(&secret_bytes).map_err(|error| {
            GuardianError::ConfigurationError(format!(
                "Secret {secret_id} does not contain a valid key: {error}"
            ))
        })
    }
}

#[async_trait]
impl AckSecretProvider for AwsSecretsManagerProvider {
    async fn falcon_secret_key(&self) -> Result<FalconSecretKey> {
        self.parsed_secret_key(PROD_FALCON_SECRET_ID, |secret_bytes| {
            FalconSecretKey::read_from_bytes(secret_bytes).map_err(|error| error.to_string())
        })
        .await
    }

    async fn ecdsa_secret_key(&self) -> Result<EcdsaSecretKey> {
        self.parsed_secret_key(PROD_ECDSA_SECRET_ID, |secret_bytes| {
            EcdsaSecretKey::read_from_bytes(secret_bytes).map_err(|error| error.to_string())
        })
        .await
    }
}

fn ensure_aws_region() -> Result<()> {
    match std::env::var(ENV_AWS_REGION) {
        Ok(value) if !value.is_empty() => Ok(()),
        Ok(_) => Err(GuardianError::ConfigurationError(format!(
            "{ENV_AWS_REGION} must not be empty when GUARDIAN_ENV=prod"
        ))),
        Err(std::env::VarError::NotPresent) => Err(GuardianError::ConfigurationError(format!(
            "{ENV_AWS_REGION} is required when GUARDIAN_ENV=prod"
        ))),
        Err(std::env::VarError::NotUnicode(_)) => Err(GuardianError::ConfigurationError(format!(
            "{ENV_AWS_REGION} must contain valid UTF-8"
        ))),
    }
}
