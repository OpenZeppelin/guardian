use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt;
use std::path::PathBuf;

use aws_config::BehaviorVersion;
use aws_sdk_secretsmanager::Client as SecretsManagerClient;
use guardian_shared::hex::{FromHex, IntoHex};
use miden_protocol::Word;
use miden_protocol::crypto::dsa::falcon512_poseidon2::PublicKey;

use super::types::AuthenticatedOperator;

pub(crate) const ENV_OPERATOR_PUBLIC_KEYS_FILE: &str = "GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE";
pub(crate) const ENV_OPERATOR_PUBLIC_KEYS_SECRET_ID: &str =
    "GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ID";
const ENV_AWS_REGION: &str = "AWS_REGION";

#[derive(Clone)]
pub(crate) enum AllowlistSource {
    Static,
    File(PathBuf),
    AwsSecret {
        secret_id: String,
        client: SecretsManagerClient,
    },
}

impl fmt::Debug for AllowlistSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Static => formatter.debug_tuple("Static").finish(),
            Self::File(path) => formatter.debug_tuple("File").field(path).finish(),
            Self::AwsSecret { secret_id, .. } => formatter
                .debug_struct("AwsSecret")
                .field("secret_id", secret_id)
                .finish_non_exhaustive(),
        }
    }
}

impl AllowlistSource {
    pub(crate) async fn from_env() -> std::result::Result<Self, String> {
        if let Ok(secret_id) = env::var(ENV_OPERATOR_PUBLIC_KEYS_SECRET_ID) {
            let secret_id = secret_id.trim();
            if !secret_id.is_empty() {
                ensure_aws_region()?;
                let config = aws_config::defaults(BehaviorVersion::latest()).load().await;
                return Ok(Self::AwsSecret {
                    secret_id: secret_id.to_string(),
                    client: SecretsManagerClient::new(&config),
                });
            }
        }

        match env::var(ENV_OPERATOR_PUBLIC_KEYS_FILE) {
            Ok(path) if !path.trim().is_empty() => Ok(Self::File(PathBuf::from(path.trim()))),
            _ => Ok(Self::Static),
        }
    }

    pub(crate) async fn load(&self) -> std::result::Result<OperatorAllowlist, String> {
        match self {
            Self::Static => Ok(OperatorAllowlist::default()),
            Self::File(path) => {
                let json = tokio::fs::read_to_string(path).await.map_err(|error| {
                    format!(
                        "Failed to read {ENV_OPERATOR_PUBLIC_KEYS_FILE} file {}: {error}",
                        path.display()
                    )
                })?;
                OperatorAllowlist::from_json(
                    &format!("{}={}", ENV_OPERATOR_PUBLIC_KEYS_FILE, path.display()),
                    &json,
                )
            }
            Self::AwsSecret { secret_id, client } => {
                let response = client
                    .get_secret_value()
                    .secret_id(secret_id)
                    .send()
                    .await
                    .map_err(|error| {
                        format!(
                            "Failed to load {ENV_OPERATOR_PUBLIC_KEYS_SECRET_ID} {secret_id} from Secrets Manager: {error}"
                        )
                    })?;
                let json = response.secret_string().ok_or_else(|| {
                    format!("Secret {secret_id} does not contain a secret string value")
                })?;
                OperatorAllowlist::from_json(
                    &format!("{ENV_OPERATOR_PUBLIC_KEYS_SECRET_ID}={secret_id}"),
                    json,
                )
            }
        }
    }

    pub(crate) async fn load_dynamic(
        &self,
    ) -> std::result::Result<Option<OperatorAllowlist>, String> {
        match self {
            Self::Static => Ok(None),
            Self::File(_) | Self::AwsSecret { .. } => self.load().await.map(Some),
        }
    }

    pub(crate) fn label(&self) -> String {
        match self {
            Self::Static => "static".to_string(),
            Self::File(path) => format!("{ENV_OPERATOR_PUBLIC_KEYS_FILE}={}", path.display()),
            Self::AwsSecret { secret_id, .. } => {
                format!("{ENV_OPERATOR_PUBLIC_KEYS_SECRET_ID}={secret_id}")
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct OperatorAllowlist {
    by_commitment: HashMap<String, AuthenticatedOperator>,
}

impl OperatorAllowlist {
    pub(crate) fn from_json(source_label: &str, json: &str) -> std::result::Result<Self, String> {
        let public_keys: Vec<String> = serde_json::from_str(json)
            .map_err(|error| format!("Failed to parse {source_label}: {error}"))?;
        Self::from_public_keys(source_label, public_keys)
    }

    fn from_public_keys(
        source_label: &str,
        public_keys: Vec<String>,
    ) -> std::result::Result<Self, String> {
        let mut by_commitment = HashMap::new();
        let mut commitments = HashSet::new();

        for (index, public_key_hex) in public_keys.iter().enumerate() {
            let public_key_hex = public_key_hex.trim();
            if public_key_hex.is_empty() {
                return Err(format!(
                    "{source_label} entry {} must not be empty",
                    index + 1
                ));
            }

            let public_key = PublicKey::from_hex(public_key_hex).map_err(|error| {
                format!(
                    "Failed to parse {source_label} entry {}: {error}",
                    index + 1
                )
            })?;
            let commitment = public_key.to_commitment().into_hex();
            if !commitments.insert(commitment.clone()) {
                return Err(format!(
                    "Duplicate operator public key commitment in {source_label}: {commitment}"
                ));
            }

            by_commitment.insert(
                commitment.clone(),
                AuthenticatedOperator {
                    operator_id: commitment.clone(),
                    commitment,
                },
            );
        }

        Ok(Self { by_commitment })
    }

    pub(crate) fn from_entries(
        entries: Vec<OperatorAllowlistEntryInput>,
    ) -> std::result::Result<Self, String> {
        let mut by_commitment = HashMap::with_capacity(entries.len());
        let mut operator_ids = HashSet::with_capacity(entries.len());
        let mut commitments = HashSet::with_capacity(entries.len());

        for entry in entries {
            if entry.operator_id.trim().is_empty() {
                return Err(
                    "Operator allowlist entries must have a non-empty operator_id".to_string(),
                );
            }

            let normalized_commitment = normalize_commitment(&entry.commitment)?;
            if !operator_ids.insert(entry.operator_id.clone()) {
                return Err(format!(
                    "Duplicate operator identifier in allowlist: {}",
                    entry.operator_id
                ));
            }
            if !commitments.insert(normalized_commitment.clone()) {
                return Err(format!(
                    "Duplicate operator commitment in allowlist: {}",
                    normalized_commitment
                ));
            }

            by_commitment.insert(
                normalized_commitment.clone(),
                AuthenticatedOperator {
                    operator_id: entry.operator_id,
                    commitment: normalized_commitment,
                },
            );
        }

        Ok(Self { by_commitment })
    }

    pub(crate) fn lookup(&self, commitment: &str) -> Option<&AuthenticatedOperator> {
        self.by_commitment.get(commitment)
    }

    pub(crate) fn len(&self) -> usize {
        self.by_commitment.len()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct OperatorAllowlistEntryInput {
    pub(crate) operator_id: String,
    pub(crate) commitment: String,
}

pub(crate) fn normalize_commitment(commitment: &str) -> std::result::Result<String, String> {
    Word::from_hex(commitment).map(|parsed| parsed.into_hex())
}

fn ensure_aws_region() -> std::result::Result<(), String> {
    match env::var(ENV_AWS_REGION) {
        Ok(value) if !value.trim().is_empty() => Ok(()),
        Ok(_) => Err(format!(
            "{ENV_AWS_REGION} must not be empty when {ENV_OPERATOR_PUBLIC_KEYS_SECRET_ID} is set"
        )),
        Err(env::VarError::NotPresent) => Err(format!(
            "{ENV_AWS_REGION} is required when {ENV_OPERATOR_PUBLIC_KEYS_SECRET_ID} is set"
        )),
        Err(env::VarError::NotUnicode(_)) => {
            Err(format!("{ENV_AWS_REGION} must contain valid UTF-8"))
        }
    }
}
