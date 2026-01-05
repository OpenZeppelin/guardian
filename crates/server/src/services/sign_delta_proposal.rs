use crate::builder::state::AppState;
use crate::delta_object::{CosignerSignature, DeltaObject, DeltaStatus, ProposalSignature};
use crate::error::{PsmError, Result};
use crate::metadata::auth::Credentials;
use crate::services::resolve_account;
use miden_objects::crypto::dsa::rpo_falcon512::PublicKey;
use miden_objects::utils::Serializable;
use private_state_manager_shared::DeltaSignature;
use private_state_manager_shared::hex::FromHex;
use tracing::info;

#[derive(Debug, Clone)]
pub struct SignDeltaProposalParams {
    pub account_id: String,
    pub commitment: String,
    pub signature: ProposalSignature,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct SignDeltaProposalResult {
    pub delta: DeltaObject,
}

pub async fn sign_delta_proposal(
    state: &AppState,
    params: SignDeltaProposalParams,
) -> Result<SignDeltaProposalResult> {
    let SignDeltaProposalParams {
        account_id,
        commitment,
        signature,
        credentials,
    } = params;

    // Resolve account and verify authentication
    let resolved = resolve_account(state, &account_id, &credentials).await?;

    // Fetch the proposal by commitment
    let mut delta_proposal = resolved
        .backend
        .pull_delta_proposal(&account_id, &commitment)
        .await
        .map_err(|_| PsmError::ProposalNotFound {
            account_id: account_id.clone(),
            commitment: commitment.clone(),
        })?;

    // Verify is a pending proposal
    let (timestamp, proposer_id, mut cosigner_sigs) = match &delta_proposal.status {
        DeltaStatus::Pending {
            timestamp,
            proposer_id,
            cosigner_sigs,
        } => (
            timestamp.clone(),
            proposer_id.clone(),
            cosigner_sigs.clone(),
        ),
        _ => {
            return Err(PsmError::ProposalNotFound {
                account_id: account_id.clone(),
                commitment: commitment.clone(),
            });
        }
    };

    // Extract signer ID from credentials
    let signer_commitment_hex = match &credentials {
        Credentials::Signature { pubkey, .. } => {
            let public_key = PublicKey::from_hex(pubkey).map_err(|e| {
                PsmError::AuthenticationFailed(format!(
                    "invalid signer public key for {}: {}",
                    account_id, e
                ))
            })?;
            let commitment = public_key.to_commitment();
            format!("0x{}", hex::encode(commitment.to_bytes()))
        }
    };

    // Check if already signed by this signer
    if cosigner_sigs
        .iter()
        .any(|sig| sig.signer_id.eq_ignore_ascii_case(&signer_commitment_hex))
    {
        return Err(PsmError::ProposalAlreadySigned {
            signer_id: signer_commitment_hex.clone(),
        });
    }

    // Create the proposal signature based on scheme
    // Add the new signature
    let new_signature = CosignerSignature {
        signature,
        timestamp: state.clock.now_rfc3339(),
        signer_id: signer_commitment_hex.clone(),
    };
    cosigner_sigs.push(new_signature);

    let new_sig = DeltaSignature {
        signer_id: signer_commitment_hex.clone(),
        signature: cosigner_sigs.last().expect("just pushed").signature.clone(),
    };
    if let Some(signatures) = delta_proposal
        .delta_payload
        .get_mut("signatures")
        .and_then(|v| v.as_array_mut())
    {
        signatures.push(
            serde_json::to_value(new_sig).map_err(|e| {
                PsmError::InvalidDelta(format!("Failed to serialize signature: {e}"))
            })?,
        );
    } else {
        delta_proposal
            .delta_payload
            .as_object_mut()
            .ok_or_else(|| PsmError::InvalidDelta("delta_payload must be an object".to_string()))?
            .insert(
                "signatures".to_string(),
                serde_json::to_value(vec![new_sig]).map_err(|e| {
                    PsmError::InvalidDelta(format!("Failed to serialize signature: {e}"))
                })?,
            );
    }

    info!(
        account_id = %account_id,
        signer_commitment = %signer_commitment_hex,
        total_signatures = cosigner_sigs.len(),
        "sign_delta_proposal appended signature"
    );

    // Update the delta proposal with the new signature
    delta_proposal.status = DeltaStatus::Pending {
        timestamp,
        proposer_id,
        cosigner_sigs,
    };

    // Store the updated proposal
    resolved
        .backend
        .update_delta_proposal(&commitment, &delta_proposal)
        .await
        .map_err(PsmError::StorageError)?;

    Ok(SignDeltaProposalResult {
        delta: delta_proposal.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta_object::DeltaStatus;
    use crate::metadata::AccountMetadata;
    use crate::metadata::auth::Auth;
    use crate::storage::StorageType;
    use crate::testing::fixtures;
    use crate::testing::helpers::create_test_app_state_with_mocks;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn create_test_state() -> (
        AppState,
        MockStorageBackend,
        MockNetworkClient,
        MockMetadataStore,
    ) {
        let storage = MockStorageBackend::new();
        let network = MockNetworkClient::new();
        let metadata = MockMetadataStore::new();

        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(Mutex::new(network.clone())),
            Arc::new(metadata.clone()),
        );

        (state, storage, network, metadata)
    }

    fn create_account_metadata(
        account_id: String,
        cosigner_commitments: Vec<String>,
    ) -> AccountMetadata {
        AccountMetadata {
            account_id,
            auth: Auth::MidenFalconRpo {
                cosigner_commitments,
            },
            storage_type: StorageType::Filesystem,
            created_at: "2024-11-14T12:00:00Z".to_string(),
            updated_at: "2024-11-14T12:00:00Z".to_string(),
        }
    }

    fn create_pending_proposal(
        account_id: String,
        nonce: u64,
        proposer_id: String,
        cosigner_sigs: Vec<CosignerSignature>,
    ) -> DeltaObject {
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();

        DeltaObject {
            account_id: account_id.clone(),
            nonce,
            prev_commitment: "0x123".to_string(),
            new_commitment: None,
            delta_payload: serde_json::json!({
                "tx_summary": delta_fixture["delta_payload"].clone(),
                "signatures": []
            }),
            ack_sig: None,
            status: DeltaStatus::Pending {
                timestamp: "2024-11-14T12:00:00Z".to_string(),
                proposer_id,
                cosigner_sigs,
            },
        }
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_success() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let commitment = "mock_proposal_id".to_string();

        let (_proposer_pubkey, proposer_commitment, _proposer_signature) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let (signer_pubkey, signer_commitment, signer_signature) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![proposer_commitment.clone(), signer_commitment.clone()],
        ))));

        let pending_proposal =
            create_pending_proposal(account_id.clone(), 1, proposer_commitment.clone(), vec![]);

        let storage = storage
            .with_pull_delta_proposal(Ok(pending_proposal.clone()))
            .with_update_delta_proposal(Ok(()));

        let dummy_sig = format!("0x{}", "a".repeat(666));
        let params = SignDeltaProposalParams {
            account_id: account_id.clone(),
            commitment: commitment.clone(),
            signature: ProposalSignature::Falcon {
                signature: dummy_sig.clone(),
            },
            credentials: Credentials::signature(signer_pubkey.clone(), signer_signature.clone()),
        };

        let result = sign_delta_proposal(&state, params).await;

        assert!(result.is_ok(), "Expected success, got: {:?}", result);
        let result = result.unwrap();

        match &result.delta.status {
            DeltaStatus::Pending { cosigner_sigs, .. } => {
                assert_eq!(cosigner_sigs.len(), 1);
                assert_eq!(cosigner_sigs[0].signer_id, signer_commitment);
                match &cosigner_sigs[0].signature {
                    ProposalSignature::Falcon { signature } => {
                        assert_eq!(*signature, dummy_sig);
                    }
                }
            }
            _ => panic!("Expected Pending status"),
        }

        let payload_sigs = result
            .delta
            .delta_payload
            .get("signatures")
            .and_then(|v| v.as_array())
            .expect("signatures must exist in delta_payload");
        assert_eq!(payload_sigs.len(), 1);

        let update_calls = storage.get_update_delta_proposal_calls();
        assert_eq!(update_calls.len(), 1);
        assert_eq!(update_calls[0].0, commitment);
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_second_signature() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let commitment = "mock_proposal_id".to_string();

        let (_proposer_pubkey, proposer_commitment, _proposer_signature) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let (_first_signer_pubkey, first_signer_commitment, _) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let (second_signer_pubkey, second_signer_commitment, second_signer_signature) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![
                proposer_commitment.clone(),
                first_signer_commitment.clone(),
                second_signer_commitment.clone(),
            ],
        ))));

        let first_sig = format!("0x{}", "a".repeat(666));
        let pending_proposal = create_pending_proposal(
            account_id.clone(),
            1,
            proposer_commitment.clone(),
            vec![CosignerSignature {
                signature: ProposalSignature::Falcon {
                    signature: first_sig,
                },
                timestamp: "2024-11-14T12:00:00Z".to_string(),
                signer_id: first_signer_commitment.clone(),
            }],
        );

        let _storage = storage
            .with_pull_delta_proposal(Ok(pending_proposal.clone()))
            .with_update_delta_proposal(Ok(()));

        let second_sig = format!("0x{}", "b".repeat(666));
        let params = SignDeltaProposalParams {
            account_id: account_id.clone(),
            commitment: commitment.clone(),
            signature: ProposalSignature::Falcon {
                signature: second_sig.clone(),
            },
            credentials: Credentials::signature(
                second_signer_pubkey.clone(),
                second_signer_signature.clone(),
            ),
        };

        let result = sign_delta_proposal(&state, params).await.unwrap();

        match &result.delta.status {
            DeltaStatus::Pending { cosigner_sigs, .. } => {
                assert_eq!(cosigner_sigs.len(), 2);
                assert_eq!(cosigner_sigs[0].signer_id, first_signer_commitment);
                assert_eq!(cosigner_sigs[1].signer_id, second_signer_commitment);
            }
            _ => panic!("Expected Pending status"),
        }
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_not_found() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let commitment = "nonexistent_proposal".to_string();

        let (signer_pubkey, signer_commitment, signer_signature) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![signer_commitment.clone()],
        ))));

        let _storage = storage.with_pull_delta_proposal(Err("Proposal not found".to_string()));

        let dummy_sig = format!("0x{}", "a".repeat(666));
        let params = SignDeltaProposalParams {
            account_id: account_id.clone(),
            commitment: commitment.clone(),
            signature: ProposalSignature::Falcon {
                signature: dummy_sig,
            },
            credentials: Credentials::signature(signer_pubkey, signer_signature),
        };

        let result = sign_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            PsmError::ProposalNotFound {
                account_id: err_account_id,
                commitment: err_commitment,
            } => {
                assert_eq!(err_account_id, account_id);
                assert_eq!(err_commitment, commitment);
            }
            e => panic!("Expected ProposalNotFound error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_duplicate_signature() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let commitment = "mock_proposal_id".to_string();

        let (_proposer_pubkey, proposer_commitment, _proposer_signature) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let (signer_pubkey, signer_commitment, signer_signature) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![proposer_commitment.clone(), signer_commitment.clone()],
        ))));

        let existing_sig = format!("0x{}", "a".repeat(666));
        let pending_proposal = create_pending_proposal(
            account_id.clone(),
            1,
            proposer_commitment.clone(),
            vec![CosignerSignature {
                signature: ProposalSignature::Falcon {
                    signature: existing_sig,
                },
                timestamp: "2024-11-14T12:00:00Z".to_string(),
                signer_id: signer_commitment.clone(),
            }],
        );

        let _storage = storage.with_pull_delta_proposal(Ok(pending_proposal.clone()));

        let new_sig = format!("0x{}", "b".repeat(666));
        let params = SignDeltaProposalParams {
            account_id: account_id.clone(),
            commitment: commitment.clone(),
            signature: ProposalSignature::Falcon { signature: new_sig },
            credentials: Credentials::signature(signer_pubkey, signer_signature),
        };

        let result = sign_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            PsmError::ProposalAlreadySigned { signer_id } => {
                assert_eq!(signer_id, signer_commitment);
            }
            e => panic!("Expected ProposalAlreadySigned error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_unauthorized_signer() {
        let (state, _storage, _network, metadata) = create_test_state();

        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let commitment = "mock_proposal_id".to_string();

        let (_proposer_pubkey, proposer_commitment, _proposer_signature) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let (unauthorized_pubkey, _unauthorized_commitment, unauthorized_signature) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![proposer_commitment.clone()],
        ))));

        let dummy_sig = format!("0x{}", "a".repeat(666));
        let params = SignDeltaProposalParams {
            account_id: account_id.clone(),
            commitment: commitment.clone(),
            signature: ProposalSignature::Falcon {
                signature: dummy_sig,
            },
            credentials: Credentials::signature(unauthorized_pubkey, unauthorized_signature),
        };

        let result = sign_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            PsmError::AuthenticationFailed(_) => {}
            e => panic!("Expected AuthenticationFailed error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_sign_delta_proposal_storage_error() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let commitment = "mock_proposal_id".to_string();

        let (_proposer_pubkey, proposer_commitment, _proposer_signature) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let (signer_pubkey, signer_commitment, signer_signature) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![proposer_commitment.clone(), signer_commitment.clone()],
        ))));

        let pending_proposal =
            create_pending_proposal(account_id.clone(), 1, proposer_commitment.clone(), vec![]);

        let _storage = storage
            .with_pull_delta_proposal(Ok(pending_proposal.clone()))
            .with_update_delta_proposal(Err("Storage write failed".to_string()));

        let dummy_sig = format!("0x{}", "a".repeat(666));
        let params = SignDeltaProposalParams {
            account_id: account_id.clone(),
            commitment: commitment.clone(),
            signature: ProposalSignature::Falcon {
                signature: dummy_sig,
            },
            credentials: Credentials::signature(signer_pubkey, signer_signature),
        };

        let result = sign_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            PsmError::StorageError(_) => {}
            e => panic!("Expected StorageError, got: {:?}", e),
        }
    }
}
