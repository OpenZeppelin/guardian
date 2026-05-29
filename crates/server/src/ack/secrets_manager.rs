use crate::error::{GuardianError, Result};
use crate::secret::{SecretBytes, SecretString};
use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_sdk_secretsmanager::Client;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey as FalconSecretKey;
use miden_protocol::utils::serde::Deserializable;

const ENV_AWS_REGION: &str = "AWS_REGION";
const ENV_ACK_FALCON_SECRET_ID: &str = "GUARDIAN_ACK_FALCON_SECRET_ID";
const ENV_ACK_ECDSA_SECRET_ID: &str = "GUARDIAN_ACK_ECDSA_SECRET_ID";
pub const DEFAULT_FALCON_SECRET_ID: &str = "guardian-prod/server/ack-falcon-secret-key";
pub const DEFAULT_ECDSA_SECRET_ID: &str = "guardian-prod/server/ack-ecdsa-secret-key";

#[async_trait]
pub trait AckSecretProvider: Send + Sync {
    async fn falcon_secret_key(&self) -> Result<FalconSecretKey>;
    async fn ecdsa_secret_key(&self) -> Result<EcdsaSecretKey>;
}

pub struct AwsSecretsManagerProvider {
    client: Client,
    falcon_secret_id: String,
    ecdsa_secret_id: String,
}

impl AwsSecretsManagerProvider {
    pub async fn from_env() -> Result<Self> {
        ensure_aws_region()?;
        let falcon_secret_id =
            resolve_secret_id(ENV_ACK_FALCON_SECRET_ID, DEFAULT_FALCON_SECRET_ID)?;
        let ecdsa_secret_id = resolve_secret_id(ENV_ACK_ECDSA_SECRET_ID, DEFAULT_ECDSA_SECRET_ID)?;
        let config = aws_config::defaults(BehaviorVersion::latest()).load().await;

        Ok(Self {
            client: Client::new(&config),
            falcon_secret_id,
            ecdsa_secret_id,
        })
    }

    async fn secret_string(&self, secret_id: &str) -> Result<SecretString> {
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

        response
            .secret_string()
            .map(|s| SecretString::new(s.to_owned()))
            .ok_or_else(|| {
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
        let secret_bytes = SecretBytes::new(
            hex::decode(secret_hex.expose_secret().trim()).map_err(|error| {
                GuardianError::ConfigurationError(format!(
                    "Secret {secret_id} must contain valid hex-encoded key bytes: {error}"
                ))
            })?,
        );

        parser(secret_bytes.expose_secret()).map_err(|error| {
            GuardianError::ConfigurationError(format!(
                "Secret {secret_id} does not contain a valid key: {error}"
            ))
        })
    }
}

#[async_trait]
impl AckSecretProvider for AwsSecretsManagerProvider {
    async fn falcon_secret_key(&self) -> Result<FalconSecretKey> {
        self.parsed_secret_key(&self.falcon_secret_id, |secret_bytes| {
            FalconSecretKey::read_from_bytes(secret_bytes).map_err(|error| error.to_string())
        })
        .await
    }

    async fn ecdsa_secret_key(&self) -> Result<EcdsaSecretKey> {
        self.parsed_secret_key(&self.ecdsa_secret_id, |secret_bytes| {
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

fn resolve_secret_id(env_var: &str, default: &str) -> Result<String> {
    match std::env::var(env_var) {
        Ok(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Err(GuardianError::ConfigurationError(format!(
                    "{env_var} must not be blank when set"
                )))
            } else {
                Ok(trimmed.to_string())
            }
        }
        Err(std::env::VarError::NotPresent) => Ok(default.to_string()),
        Err(std::env::VarError::NotUnicode(_)) => Err(GuardianError::ConfigurationError(format!(
            "{env_var} must contain valid UTF-8"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            // SAFETY: callers hold ENV_LOCK to serialize env mutations
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            // SAFETY: callers hold ENV_LOCK to serialize env mutations
            unsafe { std::env::remove_var(key) };
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: callers hold ENV_LOCK to serialize env mutations
            match &self.previous {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    #[test]
    fn resolve_secret_id_returns_default_when_unset() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvVarGuard::remove(ENV_ACK_FALCON_SECRET_ID);

        let resolved =
            resolve_secret_id(ENV_ACK_FALCON_SECRET_ID, DEFAULT_FALCON_SECRET_ID).unwrap();
        assert_eq!(resolved, DEFAULT_FALCON_SECRET_ID);
    }

    #[test]
    fn resolve_secret_id_uses_override_when_set() {
        let _lock = ENV_LOCK.lock().unwrap();
        let override_value = "custom/path/ack-falcon";
        let _guard = EnvVarGuard::set(ENV_ACK_FALCON_SECRET_ID, override_value);

        let resolved =
            resolve_secret_id(ENV_ACK_FALCON_SECRET_ID, DEFAULT_FALCON_SECRET_ID).unwrap();
        assert_eq!(resolved, override_value);
    }

    #[test]
    fn resolve_secret_id_trims_surrounding_whitespace() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvVarGuard::set(ENV_ACK_FALCON_SECRET_ID, "  custom/path/ack-falcon  ");

        let resolved =
            resolve_secret_id(ENV_ACK_FALCON_SECRET_ID, DEFAULT_FALCON_SECRET_ID).unwrap();
        assert_eq!(resolved, "custom/path/ack-falcon");
    }

    #[test]
    fn resolve_secret_id_rejects_empty_override() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvVarGuard::set(ENV_ACK_ECDSA_SECRET_ID, "");

        let result = resolve_secret_id(ENV_ACK_ECDSA_SECRET_ID, DEFAULT_ECDSA_SECRET_ID);
        assert!(matches!(
            result,
            Err(GuardianError::ConfigurationError(message))
                if message.contains(ENV_ACK_ECDSA_SECRET_ID)
        ));
    }

    #[test]
    fn resolve_secret_id_rejects_whitespace_only_override() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvVarGuard::set(ENV_ACK_ECDSA_SECRET_ID, "   ");

        let result = resolve_secret_id(ENV_ACK_ECDSA_SECRET_ID, DEFAULT_ECDSA_SECRET_ID);
        assert!(matches!(
            result,
            Err(GuardianError::ConfigurationError(message))
                if message.contains(ENV_ACK_ECDSA_SECRET_ID)
        ));
    }
}
