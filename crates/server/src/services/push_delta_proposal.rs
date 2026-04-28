use crate::builder::state::AppState;
use crate::delta_object::{CosignerSignature, DeltaObject, DeltaStatus};
use crate::error::{GuardianError, Result};
use crate::metadata::auth::Credentials;
use crate::services::{normalize_payload, resolve_account};
use guardian_shared::DeltaSignature;
use tracing::info;

const DEFAULT_MAX_PENDING_PROPOSALS_PER_ACCOUNT: usize = 20;
const MAX_PENDING_PROPOSALS_ENV_VAR: &str = "GUARDIAN_MAX_PENDING_PROPOSALS_PER_ACCOUNT";

fn max_pending_proposals_per_account() -> usize {
    std::env::var(MAX_PENDING_PROPOSALS_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_PENDING_PROPOSALS_PER_ACCOUNT)
}

#[derive(Debug, Clone)]
pub struct PushDeltaProposalParams {
    pub account_id: String,
    pub nonce: u64,
    pub delta_payload: serde_json::Value,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct PushDeltaProposalResult {
    pub delta: DeltaObject,
    pub commitment: String,
}

pub async fn push_delta_proposal(
    state: &AppState,
    params: PushDeltaProposalParams,
) -> Result<PushDeltaProposalResult> {
    #[cfg(not(feature = "evm"))]
    if crate::metadata::network::is_evm_account_id(&params.account_id)
        || params
            .delta_payload
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|kind| kind == "evm")
    {
        return Err(GuardianError::EvmSupportDisabled);
    }

    #[cfg(feature = "evm")]
    if crate::evm::is_evm_payload(&params.delta_payload) {
        return push_evm_delta_proposal(state, params).await;
    }

    let PushDeltaProposalParams {
        account_id,
        nonce,
        delta_payload,
        credentials,
    } = params;

    let delta_payload = normalize_payload(delta_payload)?;

    let resolved = resolve_account(state, &account_id, &credentials).await?;
    if resolved.metadata.network_config.is_evm() {
        return Err(GuardianError::InvalidEvmProposal(
            "EVM accounts require EVM proposal payloads".to_string(),
        ));
    }

    // Fetch current state to validate delta
    let current_state = resolved
        .storage
        .pull_state(&account_id)
        .await
        .map_err(|_| GuardianError::StateNotFound(account_id.clone()))?;

    // Check for pending candidates before accepting new proposal
    let has_pending = resolved
        .storage
        .has_pending_candidate(&account_id)
        .await
        .map_err(|e| {
            tracing::error!(
                account_id = %account_id,
                error = %e,
                "Failed to check pending candidate in push_delta_proposal"
            );
            GuardianError::StorageError(format!("Failed to check pending candidate: {e}"))
        })?;

    if has_pending {
        return Err(GuardianError::ConflictPendingDelta);
    }

    let pending_proposals = resolved
        .storage
        .pull_pending_proposals(&account_id)
        .await
        .map_err(|e| {
            tracing::error!(
                account_id = %account_id,
                error = %e,
                "Failed to load pending proposals in push_delta_proposal"
            );
            GuardianError::StorageError(format!("Failed to load pending proposals: {e}"))
        })?;

    let max_pending_proposals = max_pending_proposals_per_account();
    if pending_proposals.len() >= max_pending_proposals {
        return Err(GuardianError::PendingProposalsLimit {
            limit: max_pending_proposals,
        });
    }

    // Extract tx_summary and signatures from delta_payload
    let tx_summary = delta_payload
        .get("tx_summary")
        .ok_or_else(|| GuardianError::InvalidDelta("Missing 'tx_summary' field".to_string()))?;

    let signatures = delta_payload
        .get("signatures")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();

    // Validate delta using network client (check validity but don't apply)
    // and compute the delta commitment
    let commitment = {
        let client = state.network_client.lock().await;
        client
            .verify_delta(
                &current_state.commitment,
                &current_state.state_json,
                tx_summary,
            )
            .map_err(GuardianError::InvalidDelta)?;

        // Compute the delta proposal ID from the tx_summary
        client
            .delta_proposal_id(&account_id, nonce, tx_summary)
            .map_err(GuardianError::InvalidDelta)?
    };

    // Extract proposer ID from credentials
    let proposer_id = match &credentials {
        Credentials::Signature { pubkey, .. } => resolved
            .metadata
            .auth
            .compute_signer_commitment(pubkey)
            .map_err(|e| {
                GuardianError::AuthenticationFailed(format!(
                    "invalid proposer public key for {}: {}",
                    account_id, e
                ))
            })?,
    };

    // Parse cosigner signatures from the payload and add timestamp
    let signature_timestamp = state.clock.now_rfc3339();
    let mut cosigner_sigs = Vec::new();
    for sig_value in signatures {
        let parsed: DeltaSignature = serde_json::from_value(sig_value).map_err(|e| {
            GuardianError::InvalidDelta(format!("Invalid signature entry in payload: {e}"))
        })?;

        cosigner_sigs.push(CosignerSignature {
            signature: parsed.signature,
            timestamp: signature_timestamp.clone(),
            signer_id: parsed.signer_id,
        });
    }
    let cosigner_ids: Vec<String> = cosigner_sigs
        .iter()
        .map(|sig| sig.signer_id.clone())
        .collect();
    info!(
        account_id = %account_id,
        nonce,
        proposer_id = %proposer_id,
        signer_ids = ?cosigner_ids,
        "push_delta_proposal received"
    );

    // Create delta object with Pending status including any provided signatures
    let timestamp = state.clock.now_rfc3339();
    let delta_proposal = DeltaObject {
        account_id: account_id.clone(),
        nonce,
        prev_commitment: current_state.commitment.clone(),
        new_commitment: None,
        delta_payload,
        ack_sig: String::new(),
        ack_pubkey: String::new(),
        ack_scheme: String::new(),
        status: DeltaStatus::Pending {
            timestamp,
            proposer_id,
            cosigner_sigs,
        },
    };

    // Store the delta proposal in the proposals directory using the commitment as ID
    resolved
        .storage
        .submit_delta_proposal(&commitment, &delta_proposal)
        .await
        .map_err(GuardianError::StorageError)?;
    let stored_signer_count = match &delta_proposal.status {
        DeltaStatus::Pending { cosigner_sigs, .. } => cosigner_sigs.len(),
        _ => 0,
    };
    info!(
        account_id = %account_id,
        nonce,
        commitment = %commitment,
        signer_count = stored_signer_count,
        "push_delta_proposal stored"
    );

    Ok(PushDeltaProposalResult {
        delta: delta_proposal.clone(),
        commitment: commitment.clone(),
    })
}

#[cfg(feature = "evm")]
async fn push_evm_delta_proposal(
    state: &AppState,
    params: PushDeltaProposalParams,
) -> Result<PushDeltaProposalResult> {
    let PushDeltaProposalParams {
        account_id,
        nonce,
        delta_payload,
        credentials,
    } = params;

    let normalized = crate::evm::normalize_evm_proposal_payload(delta_payload)?;
    let resolved = resolve_account(state, &account_id, &credentials).await?;
    if !resolved.metadata.network_config.is_evm() {
        return Err(GuardianError::UnsupportedForNetwork {
            network: "miden".to_string(),
            operation: "evm_proposal".to_string(),
        });
    }

    let commitment =
        crate::evm::compute_proposal_id(&resolved.metadata.network_config, &normalized)?;
    let existing = resolved
        .storage
        .pull_delta_proposal(&account_id, &commitment)
        .await;
    if let Ok(existing) = existing
        && existing.status.is_pending()
    {
        return Ok(PushDeltaProposalResult {
            delta: existing,
            commitment,
        });
    }

    let pending_proposals = resolved
        .storage
        .pull_pending_proposals(&account_id)
        .await
        .map_err(|e| {
            tracing::error!(
                account_id = %account_id,
                error = %e,
                "Failed to load pending EVM proposals in push_delta_proposal"
            );
            GuardianError::StorageError(format!("Failed to load pending proposals: {e}"))
        })?;

    let max_pending_proposals = max_pending_proposals_per_account();
    if pending_proposals.len() >= max_pending_proposals {
        return Err(GuardianError::PendingProposalsLimit {
            limit: max_pending_proposals,
        });
    }

    let proposer_id = match &credentials {
        Credentials::Signature { pubkey, .. } => resolved
            .metadata
            .auth
            .compute_signer_commitment(pubkey)
            .map_err(GuardianError::AuthenticationFailed)?,
    };

    let timestamp = state.clock.now_rfc3339();
    let delta_proposal = DeltaObject {
        account_id: account_id.clone(),
        nonce,
        prev_commitment: String::new(),
        new_commitment: None,
        delta_payload: normalized.payload,
        ack_sig: String::new(),
        ack_pubkey: String::new(),
        ack_scheme: String::new(),
        status: DeltaStatus::Pending {
            timestamp,
            proposer_id,
            cosigner_sigs: Vec::new(),
        },
    };

    resolved
        .storage
        .submit_delta_proposal(&commitment, &delta_proposal)
        .await
        .map_err(GuardianError::StorageError)?;

    Ok(PushDeltaProposalResult {
        delta: delta_proposal,
        commitment,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta_object::DeltaStatus;
    use crate::metadata::AccountMetadata;
    use crate::metadata::auth::Auth;
    use crate::state_object::StateObject;
    use crate::testing::fixtures;
    use crate::testing::helpers::create_test_app_state_with_mocks;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use guardian_shared::ProposalSignature;
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

    fn create_account_metadata(account_id: String, auth: Auth) -> AccountMetadata {
        AccountMetadata {
            account_id,
            auth,
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-11-14T12:00:00Z".to_string(),
            updated_at: "2024-11-14T12:00:00Z".to_string(),
            has_pending_candidate: false,
            last_auth_timestamp: None,
        }
    }

    fn create_state_object(
        account_id: String,
        commitment: String,
        state_json: serde_json::Value,
    ) -> StateObject {
        StateObject {
            account_id,
            commitment,
            state_json,
            created_at: "2024-11-14T12:00:00Z".to_string(),
            updated_at: "2024-11-14T12:00:00Z".to_string(),
            auth_scheme: String::new(),
        }
    }

    fn create_pending_proposal(account_id: &str, nonce: u64) -> DeltaObject {
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();

        DeltaObject {
            account_id: account_id.to_string(),
            nonce,
            prev_commitment: "0x123".to_string(),
            new_commitment: None,
            delta_payload: serde_json::json!({
                "tx_summary": delta_fixture["delta_payload"].clone(),
                "signatures": []
            }),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::Pending {
                timestamp: "2024-11-14T12:00:00Z".to_string(),
                proposer_id: "0xproposer".to_string(),
                cosigner_sigs: vec![],
            },
        }
    }

    #[tokio::test]
    async fn test_push_delta_proposal_success() {
        let (state, storage, network, metadata) = create_test_state();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let test_commitment = "0x780aa2edb983c1baab3c81edcfe400bc54b516d5cb51f2a7cec4690667329392";

        // Generate valid Falcon signature
        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        ))));

        let storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            test_commitment.to_string(),
            account_json.clone(),
        )));

        let network = network.with_verify_delta(Ok(()));
        let _network = network.with_validate_credential(Ok(()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [test_commitment_hex.clone()]
            }
        });

        let params = PushDeltaProposalParams {
            account_id: account_id.clone(),
            nonce: 1,
            delta_payload,
            credentials: Credentials::signature(
                test_pubkey.clone(),
                test_signature.clone(),
                test_timestamp,
            ),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_ok(), "Expected success, got: {:?}", result);
        let result = result.unwrap();
        assert_eq!(result.commitment, "mock_proposal_id");
        assert_eq!(result.delta.account_id, account_id);
        assert_eq!(result.delta.nonce, 1);

        match &result.delta.status {
            DeltaStatus::Pending {
                proposer_id,
                cosigner_sigs,
                ..
            } => {
                assert_eq!(*proposer_id, test_commitment_hex);
                assert_eq!(cosigner_sigs.len(), 0);
            }
            _ => panic!("Expected Pending status"),
        }

        let submit_calls = storage.get_submit_delta_proposal_calls();
        assert_eq!(submit_calls.len(), 1);
        assert_eq!(submit_calls[0].0, "mock_proposal_id");
    }

    #[tokio::test]
    async fn test_push_delta_proposal_success_for_ecdsa() {
        use crate::testing::helpers::TestEcdsaSigner;
        use guardian_shared::auth_request_payload::AuthRequestPayload;

        let (state, storage, network, metadata) = create_test_state();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let test_commitment = "0x780aa2edb983c1baab3c81edcfe400bc54b516d5cb51f2a7cec4690667329392";
        let signer = TestEcdsaSigner::new();

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenEcdsa {
                cosigner_commitments: vec![signer.commitment_hex.clone()],
            },
        ))));

        let storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            test_commitment.to_string(),
            account_json.clone(),
        )));

        let network = network.with_verify_delta(Ok(()));
        let _network = network.with_validate_credential(Ok(()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 2,
                "required_signatures": 2,
                "signer_commitments": [signer.commitment_hex.clone()]
            },
            "signatures": []
        });
        let request_body = serde_json::json!({
            "account_id": account_id.clone(),
            "nonce": 1,
            "delta_payload": delta_payload.clone(),
        });
        let request_payload = AuthRequestPayload::from_json_serializable(&request_body).unwrap();
        let (test_signature, test_timestamp) = signer.sign_request(&account_id, &request_payload);

        let params = PushDeltaProposalParams {
            account_id: account_id.clone(),
            nonce: 1,
            delta_payload,
            credentials: Credentials::signature(
                signer.pubkey_hex.clone(),
                test_signature,
                test_timestamp,
            )
            .with_request_payload(request_payload),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_ok(), "Expected success, got: {:?}", result);
        let result = result.unwrap();

        match &result.delta.status {
            DeltaStatus::Pending {
                proposer_id,
                cosigner_sigs,
                ..
            } => {
                assert_eq!(*proposer_id, signer.commitment_hex);
                assert_eq!(cosigner_sigs.len(), 0);
            }
            _ => panic!("Expected Pending status"),
        }

        let submit_calls = storage.get_submit_delta_proposal_calls();
        assert_eq!(submit_calls.len(), 1);
        assert_eq!(submit_calls[0].0, "mock_proposal_id");
    }

    #[tokio::test]
    async fn test_push_delta_proposal_with_signature() {
        let (state, storage, network, metadata) = create_test_state();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let test_commitment = "0x780aa2edb983c1baab3c81edcfe400bc54b516d5cb51f2a7cec4690667329392";

        // Generate valid Falcon signatures for two cosigners
        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let (_, cosigner_commitment, _, _) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![
                    test_commitment_hex.clone(),
                    cosigner_commitment.clone(),
                ],
            },
        ))));

        let _storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            test_commitment.to_string(),
            account_json.clone(),
        )));

        let network = network.with_verify_delta(Ok(()));
        let _network = network.with_validate_credential(Ok(()));

        let dummy_sig = format!("0x{}", "a".repeat(666));
        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": [
                {
                    "signer_id": cosigner_commitment.clone(),
                    "signature": {
                        "scheme": "falcon",
                        "signature": dummy_sig
                    }
                }
            ],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [test_commitment_hex.clone(), cosigner_commitment.clone()]
            }
        });

        let params = PushDeltaProposalParams {
            account_id,
            nonce: 1,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature, test_timestamp),
        };

        let result = push_delta_proposal(&state, params).await.unwrap();

        match &result.delta.status {
            DeltaStatus::Pending { cosigner_sigs, .. } => {
                assert_eq!(cosigner_sigs.len(), 1);
                assert_eq!(cosigner_sigs[0].signer_id, cosigner_commitment);
                match &cosigner_sigs[0].signature {
                    ProposalSignature::Falcon { signature } => {
                        assert_eq!(*signature, dummy_sig);
                    }
                    ProposalSignature::Ecdsa { signature, .. } => {
                        assert_eq!(*signature, dummy_sig);
                    }
                }
            }
            _ => panic!("Expected Pending status"),
        }
    }

    #[tokio::test]
    async fn test_push_delta_proposal_missing_tx_summary() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();

        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        ))));

        let _storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            "0x123".to_string(),
            account_json,
        )));

        let delta_payload = serde_json::json!({
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [test_commitment_hex]
            }
        });

        let params = PushDeltaProposalParams {
            account_id,
            nonce: 1,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature, test_timestamp),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::InvalidDelta(msg) => {
                assert!(msg.contains("tx_summary"));
            }
            e => panic!("Expected InvalidDelta error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_push_delta_proposal_invalid_delta() {
        let (state, storage, network, metadata) = create_test_state();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        ))));

        let _storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            "0x123".to_string(),
            account_json,
        )));

        let _network = network.with_verify_delta(Err("Invalid delta".to_string()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [test_commitment_hex]
            }
        });

        let params = PushDeltaProposalParams {
            account_id,
            nonce: 1,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature, test_timestamp),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::InvalidDelta(msg) => {
                assert_eq!(msg, "Invalid delta");
            }
            e => panic!("Expected InvalidDelta error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_push_delta_proposal_state_not_found() {
        let (state, storage, _network, metadata) = create_test_state();

        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        ))));

        let _storage = storage.with_pull_state(Err("State not found".to_string()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [test_commitment_hex]
            }
        });

        let params = PushDeltaProposalParams {
            account_id: account_id.clone(),
            nonce: 1,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature, test_timestamp),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::StateNotFound(id) => {
                assert_eq!(id, account_id);
            }
            e => panic!("Expected StateNotFound error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_push_delta_proposal_blocked_by_pending_candidate() {
        let (state, storage, network, metadata) = create_test_state();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let test_commitment = "0x780aa2edb983c1baab3c81edcfe400bc54b516d5cb51f2a7cec4690667329392";

        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        ))));

        let storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            test_commitment.to_string(),
            account_json.clone(),
        )));

        // Mock pull_deltas_after to return a candidate delta (this triggers has_pending_candidate)
        let candidate_delta = DeltaObject {
            account_id: account_id.clone(),
            nonce: 1,
            prev_commitment: test_commitment.to_string(),
            new_commitment: Some("0xnewcommitment".to_string()),
            delta_payload: serde_json::json!({}),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::Candidate {
                timestamp: "2024-11-14T12:00:00Z".to_string(),
                retry_count: 0,
            },
        };
        let _storage = storage.with_pull_deltas_after(Ok(vec![candidate_delta]));

        let _network = network.with_validate_credential(Ok(()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [test_commitment_hex]
            }
        });

        let params = PushDeltaProposalParams {
            account_id: account_id.clone(),
            nonce: 2,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature, test_timestamp),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::ConflictPendingDelta => {
                // Expected - proposal creation blocked because there's a pending candidate
            }
            e => panic!("Expected ConflictPendingDelta error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_push_delta_proposal_blocked_by_pending_proposal_limit() {
        let (state, storage, network, metadata) = create_test_state();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        ))));

        let mut pending = Vec::new();
        for nonce in 1..=20u64 {
            pending.push(create_pending_proposal(&account_id, nonce));
        }

        let _storage = storage
            .with_pull_state(Ok(create_state_object(
                account_id.clone(),
                "0x123".to_string(),
                account_json,
            )))
            .with_pull_all_delta_proposals(Ok(pending));

        let _network = network.with_validate_credential(Ok(()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [test_commitment_hex.clone()]
            }
        });

        let params = PushDeltaProposalParams {
            account_id,
            nonce: 21,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature, test_timestamp),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GuardianError::PendingProposalsLimit { limit } => {
                assert_eq!(limit, 20);
            }
            e => panic!("Expected PendingProposalsLimit error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_push_delta_proposal_allows_when_pending_proposals_below_limit() {
        let (state, storage, network, metadata) = create_test_state();

        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex.clone()],
            },
        ))));

        let mut pending = Vec::new();
        for nonce in 1..20u64 {
            pending.push(create_pending_proposal(&account_id, nonce));
        }

        let _storage = storage
            .with_pull_state(Ok(create_state_object(
                account_id.clone(),
                "0x123".to_string(),
                account_json,
            )))
            .with_pull_all_delta_proposals(Ok(pending));

        let network = network.with_verify_delta(Ok(()));
        let _network = network.with_validate_credential(Ok(()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": [],
            "metadata": {
                "proposal_type": "change_threshold",
                "target_threshold": 1,
                "signer_commitments": [test_commitment_hex.clone()]
            }
        });

        let params = PushDeltaProposalParams {
            account_id: account_id.clone(),
            nonce: 20,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature, test_timestamp),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_ok(), "Expected success, got: {:?}", result);
    }
}
