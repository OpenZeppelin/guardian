use crate::builder::state::AppState;
use crate::delta_object::{CosignerSignature, DeltaObject, DeltaStatus};
use crate::error::{PsmError, Result};
use crate::metadata::auth::Credentials;
use crate::services::{normalize_payload, resolve_account};
use private_state_manager_shared::DeltaSignature;
use tracing::info;

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
    let PushDeltaProposalParams {
        account_id,
        nonce,
        delta_payload,
        credentials,
    } = params;

    let delta_payload = normalize_payload(delta_payload)?;

    let resolved = resolve_account(state, &account_id, &credentials).await?;

    // Fetch current state to validate delta
    let current_state = resolved
        .backend
        .pull_state(&account_id)
        .await
        .map_err(|_| PsmError::StateNotFound(account_id.clone()))?;

    // Extract tx_summary and signatures from delta_payload
    let tx_summary = delta_payload
        .get("tx_summary")
        .ok_or_else(|| PsmError::InvalidDelta("Missing 'tx_summary' field".to_string()))?;

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
            .map_err(PsmError::InvalidDelta)?;

        // Compute the delta proposal ID from the tx_summary
        client
            .delta_proposal_id(&account_id, nonce, tx_summary)
            .map_err(PsmError::InvalidDelta)?
    };

    // Extract proposer ID from credentials
    let proposer_id = match &credentials {
        Credentials::Signature { pubkey, .. } => pubkey.clone(),
    };

    // Parse cosigner signatures from the payload and add timestamp
    let signature_timestamp = state.clock.now_rfc3339();
    let mut cosigner_sigs = Vec::new();
    for sig_value in signatures {
        let parsed: DeltaSignature = serde_json::from_value(sig_value).map_err(|e| {
            PsmError::InvalidDelta(format!("Invalid signature entry in payload: {e}"))
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
        ack_sig: None,
        status: DeltaStatus::Pending {
            timestamp,
            proposer_id,
            cosigner_sigs,
        },
    };

    // Store the delta proposal in the proposals directory using the commitment as ID
    resolved
        .backend
        .submit_delta_proposal(&commitment, &delta_proposal)
        .await
        .map_err(PsmError::StorageError)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta_object::DeltaStatus;
    use crate::metadata::AccountMetadata;
    use crate::metadata::auth::Auth;
    use crate::state_object::StateObject;
    use crate::storage::StorageType;
    use crate::testing::fixtures;
    use crate::testing::helpers::create_test_app_state_with_mocks;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use private_state_manager_shared::ProposalSignature;
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
        let (test_pubkey, test_commitment_hex, test_signature) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![test_commitment_hex.clone()],
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
            "signatures": []
        });

        let params = PushDeltaProposalParams {
            account_id: account_id.clone(),
            nonce: 1,
            delta_payload,
            credentials: Credentials::signature(test_pubkey.clone(), test_signature.clone()),
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
                assert_eq!(*proposer_id, test_pubkey);
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
        let (test_pubkey, test_commitment_hex, test_signature) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let (_, cosigner_commitment, _) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![test_commitment_hex.clone(), cosigner_commitment.clone()],
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
            ]
        });

        let params = PushDeltaProposalParams {
            account_id,
            nonce: 1,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature),
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

        let (test_pubkey, test_commitment_hex, test_signature) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![test_commitment_hex],
        ))));

        let _storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            "0x123".to_string(),
            account_json,
        )));

        let delta_payload = serde_json::json!({
            "signatures": []
        });

        let params = PushDeltaProposalParams {
            account_id,
            nonce: 1,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            PsmError::InvalidDelta(msg) => {
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

        let (test_pubkey, test_commitment_hex, test_signature) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![test_commitment_hex],
        ))));

        let _storage = storage.with_pull_state(Ok(create_state_object(
            account_id.clone(),
            "0x123".to_string(),
            account_json,
        )));

        let _network = network.with_verify_delta(Err("Invalid delta".to_string()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": []
        });

        let params = PushDeltaProposalParams {
            account_id,
            nonce: 1,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            PsmError::InvalidDelta(msg) => {
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

        let (test_pubkey, test_commitment_hex, test_signature) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![test_commitment_hex],
        ))));

        let _storage = storage.with_pull_state(Err("State not found".to_string()));

        let delta_payload = serde_json::json!({
            "tx_summary": delta_fixture["delta_payload"].clone(),
            "signatures": []
        });

        let params = PushDeltaProposalParams {
            account_id: account_id.clone(),
            nonce: 1,
            delta_payload,
            credentials: Credentials::signature(test_pubkey, test_signature),
        };

        let result = push_delta_proposal(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            PsmError::StateNotFound(id) => {
                assert_eq!(id, account_id);
            }
            e => panic!("Expected StateNotFound error, got: {:?}", e),
        }
    }
}
