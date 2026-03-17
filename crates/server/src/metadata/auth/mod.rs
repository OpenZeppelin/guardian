use miden_protocol::crypto::dsa::ecdsa_k256_keccak::PublicKey as EcdsaPublicKey;
use miden_protocol::crypto::dsa::falcon512_rpo::PublicKey as FalconPublicKey;
use miden_protocol::utils::{Deserializable, Serializable};
use private_state_manager_shared::SignatureScheme;
use private_state_manager_shared::hex::FromHex;

use crate::api::grpc::state_manager::auth_config;
use crate::error::PsmError;
use crate::metadata::MetadataStore;

mod credentials;
mod miden_ecdsa;
mod miden_falcon_rpo;

pub use credentials::{AuthHeader, Credentials, ExtractCredentials, MAX_TIMESTAMP_SKEW_MS};

/// Authentication and authorization handler
/// Defines which signature scheme to use and handles verification
/// Each variant contains auth-specific authorization data
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum Auth {
    /// Miden Falcon RPO signature scheme
    MidenFalconRpo { cosigner_commitments: Vec<String> },
    /// Miden ECDSA secp256k1 signature scheme
    MidenEcdsa { cosigner_commitments: Vec<String> },
}

impl Auth {
    pub fn scheme(&self) -> SignatureScheme {
        match self {
            Auth::MidenFalconRpo { .. } => SignatureScheme::Falcon,
            Auth::MidenEcdsa { .. } => SignatureScheme::Ecdsa,
        }
    }

    pub fn compute_signer_commitment(&self, pubkey_hex: &str) -> Result<String, String> {
        match self {
            Auth::MidenFalconRpo { .. } => {
                let clean = pubkey_hex.trim_start_matches("0x");
                if clean.len() == 64 && hex::decode(clean).is_ok() {
                    return Ok(format!("0x{}", clean));
                }
                let public_key = FalconPublicKey::from_hex(pubkey_hex)
                    .map_err(|e| format!("invalid Falcon public key: {}", e))?;
                let commitment = public_key.to_commitment();
                Ok(format!("0x{}", hex::encode(commitment.to_bytes())))
            }
            Auth::MidenEcdsa { .. } => {
                let clean = pubkey_hex.trim_start_matches("0x");
                if clean.len() == 64 && hex::decode(clean).is_ok() {
                    return Ok(format!("0x{}", clean));
                }
                let public_key_bytes = hex::decode(clean)
                    .map_err(|e| format!("invalid ECDSA public key hex: {}", e))?;
                let public_key = EcdsaPublicKey::read_from_bytes(&public_key_bytes)
                    .map_err(|e| format!("invalid ECDSA public key: {}", e))?;
                let commitment = public_key.to_commitment();
                Ok(format!("0x{}", hex::encode(commitment.to_bytes())))
            }
        }
    }

    pub fn with_updated_commitments(&self, cosigner_commitments: Vec<String>) -> Self {
        match self {
            Auth::MidenFalconRpo { .. } => Auth::MidenFalconRpo {
                cosigner_commitments,
            },
            Auth::MidenEcdsa { .. } => Auth::MidenEcdsa {
                cosigner_commitments,
            },
        }
    }

    /// Verify credentials are authorized for account
    ///
    /// This verifies:
    /// 1. The signature is valid for the account_id + timestamp payload,
    ///    optionally bound to request bytes when provided by the transport layer
    /// 2. The signer's commitment is in the authorized list
    ///
    /// # Arguments
    /// * `account_id` - The account ID
    /// * `credentials` - The credentials to verify (includes timestamp)
    pub fn verify(&self, account_id: &str, credentials: &Credentials) -> Result<(), String> {
        match self {
            Auth::MidenFalconRpo {
                cosigner_commitments,
            } => {
                let (_pubkey, signature, timestamp) =
                    credentials.as_signature().ok_or_else(|| {
                        tracing::error!(
                            account_id = %account_id,
                            "MidenFalconRpo requires signature credentials but got different type"
                        );
                        "MidenFalconRpo requires signature credentials".to_string()
                    })?;

                miden_falcon_rpo::verify_request_signature(
                    account_id,
                    timestamp,
                    cosigner_commitments,
                    signature,
                    credentials.request_payload(),
                )
            }
            Auth::MidenEcdsa {
                cosigner_commitments,
            } => {
                let (pubkey, signature, timestamp) =
                    credentials.as_signature().ok_or_else(|| {
                        tracing::error!(
                            account_id = %account_id,
                            "MidenEcdsa requires signature credentials but got different type"
                        );
                        "MidenEcdsa requires signature credentials".to_string()
                    })?;

                miden_ecdsa::verify_request_signature(
                    account_id,
                    timestamp,
                    cosigner_commitments,
                    signature,
                    pubkey,
                    credentials.request_payload(),
                )
            }
        }
    }
}

impl TryFrom<crate::api::grpc::state_manager::AuthConfig> for Auth {
    type Error = String;

    fn try_from(
        auth_config: crate::api::grpc::state_manager::AuthConfig,
    ) -> Result<Self, Self::Error> {
        match auth_config.auth_type {
            Some(auth_config::AuthType::MidenFalconRpo(miden_auth)) => Ok(Auth::MidenFalconRpo {
                cosigner_commitments: miden_auth.cosigner_commitments,
            }),
            Some(auth_config::AuthType::MidenEcdsa(miden_auth)) => Ok(Auth::MidenEcdsa {
                cosigner_commitments: miden_auth.cosigner_commitments,
            }),
            None => {
                tracing::error!("Auth type not specified in AuthConfig");
                Err("Auth type not specified".to_string())
            }
        }
    }
}

pub async fn update_credentials(
    store: &dyn MetadataStore,
    account_id: &str,
    new_auth: Auth,
    now: &str,
) -> Result<(), PsmError> {
    let mut metadata = store
        .get(account_id)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to get metadata: {e}")))?
        .ok_or_else(|| PsmError::AccountNotFound(account_id.to_string()))?;

    if metadata.auth == new_auth {
        return Ok(());
    }

    metadata.auth = new_auth;
    metadata.updated_at = now.to_string();

    store
        .set(metadata)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to update metadata: {e}")))?;

    Ok(())
}
