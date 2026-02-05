use miden_objects::crypto::dsa::ecdsa_k256_keccak::PublicKey as EcdsaPublicKey;
use miden_objects::crypto::dsa::rpo_falcon512::PublicKey as FalconPublicKey;
use miden_objects::utils::Serializable;
use private_state_manager_shared::hex::FromHex;

use crate::api::grpc::state_manager::auth_config;
use crate::error::PsmError;
use crate::metadata::MetadataStore;
use private_state_manager_shared::SignatureScheme;

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

    /// Verify credentials are authorized for account.
    ///
    /// Tries the stored scheme first. If signature deserialization fails (e.g.
    /// the account was configured with Falcon but the client now sends ECDSA),
    /// falls back to the other scheme using the same cosigner commitments.
    ///
    /// This handles the migration case where an account's auth scheme changed
    /// but the metadata hasn't been updated via `configure` yet.
    pub fn verify(&self, account_id: &str, credentials: &Credentials) -> Result<(), String> {
        let primary_result = self.verify_scheme(account_id, credentials);
        if primary_result.is_ok() {
            return primary_result;
        }

        // If the primary scheme failed with a deserialization error, try the alternate scheme.
        let primary_err = primary_result.unwrap_err();
        if !primary_err.contains("Failed to deserialize") {
            return Err(primary_err);
        }

        let alternate = self.with_alternate_scheme();
        tracing::warn!(
            account_id = %account_id,
            stored_scheme = ?self.scheme(),
            fallback_scheme = ?alternate.scheme(),
            "Primary auth scheme failed to deserialize signature, trying alternate scheme"
        );

        alternate.verify_scheme(account_id, credentials).map_err(|fallback_err| {
            // Both schemes failed — return the original error
            tracing::error!(
                account_id = %account_id,
                primary_error = %primary_err,
                fallback_error = %fallback_err,
                "Both auth schemes failed verification"
            );
            primary_err
        })
    }

    pub fn compute_signer_commitment(&self, pubkey_hex: &str) -> Result<String, String> {
        match self {
            Auth::MidenFalconRpo { .. } => {
                let public_key = FalconPublicKey::from_hex(pubkey_hex)
                    .map_err(|e| format!("invalid Falcon public key: {}", e))?;
                let commitment = public_key.to_commitment();
                Ok(format!("0x{}", hex::encode(commitment.to_bytes())))
            }
            Auth::MidenEcdsa { .. } => {
                let public_key = EcdsaPublicKey::from_hex(pubkey_hex)
                    .map_err(|e| format!("invalid ECDSA public key: {}", e))?;
                let commitment = public_key.to_commitment();
                Ok(format!("0x{}", hex::encode(commitment.to_bytes())))
            }
        }
    }

    pub fn with_updated_commitments(&self, cosigner_commitments: Vec<String>) -> Self {
        match self {
            Auth::MidenFalconRpo { .. } => Auth::MidenFalconRpo { cosigner_commitments },
            Auth::MidenEcdsa { .. } => Auth::MidenEcdsa { cosigner_commitments },
        }
    }

    fn with_alternate_scheme(&self) -> Auth {
        match self {
            Auth::MidenFalconRpo { cosigner_commitments } => Auth::MidenEcdsa {
                cosigner_commitments: cosigner_commitments.clone(),
            },
            Auth::MidenEcdsa { cosigner_commitments } => Auth::MidenFalconRpo {
                cosigner_commitments: cosigner_commitments.clone(),
            },
        }
    }

    fn verify_scheme(&self, account_id: &str, credentials: &Credentials) -> Result<(), String> {
        match self {
            Auth::MidenFalconRpo {
                cosigner_commitments,
            } => {
                let (_pubkey, signature, timestamp) =
                    credentials.as_signature().ok_or_else(|| {
                        "MidenFalconRpo requires signature credentials".to_string()
                    })?;

                miden_falcon_rpo::verify_request_signature(
                    account_id,
                    timestamp,
                    cosigner_commitments,
                    signature,
                )
            }
            Auth::MidenEcdsa {
                cosigner_commitments,
            } => {
                let (pubkey, signature, timestamp) =
                    credentials.as_signature().ok_or_else(|| {
                        "MidenEcdsa requires signature credentials".to_string()
                    })?;

                miden_ecdsa::verify_request_signature(
                    account_id,
                    timestamp,
                    cosigner_commitments,
                    signature,
                    pubkey,
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
