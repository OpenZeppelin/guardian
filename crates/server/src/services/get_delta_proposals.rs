use crate::builder::state::AppState;
use crate::delta_object::DeltaObject;
use crate::error::Result;
use crate::metadata::auth::Credentials;
use crate::services::resolve_account;

#[derive(Debug, Clone)]
pub struct GetDeltaProposalsParams {
    pub account_id: String,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct GetDeltaProposalsResult {
    pub proposals: Vec<DeltaObject>,
}

pub async fn get_delta_proposals(
    state: &AppState,
    params: GetDeltaProposalsParams,
) -> Result<GetDeltaProposalsResult> {
    let GetDeltaProposalsParams {
        account_id,
        credentials,
    } = params;

    // Resolve account and verify authentication
    let resolved = resolve_account(state, &account_id, &credentials).await?;

    // Get all proposals from the proposals directory
    let mut proposals = resolved
        .storage
        .pull_all_delta_proposals(&account_id)
        .await
        .unwrap_or_default();

    // Filter by status::Pending and sort by nonce
    proposals.retain(|p| p.status.is_pending());
    proposals.sort_by_key(|p| p.nonce);

    Ok(GetDeltaProposalsResult { proposals })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta_object::{DeltaObject, DeltaStatus};
    use crate::metadata::AccountMetadata;
    use crate::metadata::auth::Auth;
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
            created_at: "2024-11-14T12:00:00Z".to_string(),
            updated_at: "2024-11-14T12:00:00Z".to_string(),
            has_pending_candidate: false,
            last_auth_timestamp: None,
        }
    }

    fn create_pending_proposal(account_id: String, nonce: u64, proposer_id: String) -> DeltaObject {
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
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::Pending {
                timestamp: "2024-11-14T12:00:00Z".to_string(),
                proposer_id,
                cosigner_sigs: vec![],
            },
        }
    }

    fn create_canonical_proposal(account_id: String, nonce: u64) -> DeltaObject {
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();

        DeltaObject {
            account_id: account_id.clone(),
            nonce,
            prev_commitment: "0x123".to_string(),
            new_commitment: Some("0x456".to_string()),
            delta_payload: serde_json::json!({
                "tx_summary": delta_fixture["delta_payload"].clone(),
                "signatures": []
            }),
            ack_sig: "0xabc".to_string(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::Canonical {
                timestamp: "2024-11-14T12:00:00Z".to_string(),
            },
        }
    }

    #[tokio::test]
    async fn test_get_delta_proposals_empty() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();

        let (signer_pubkey, signer_commitment, signer_signature, timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![signer_commitment.clone()],
        ))));

        let _storage = storage.with_pull_all_delta_proposals(Ok(vec![]));

        let params = GetDeltaProposalsParams {
            account_id: account_id.clone(),
            credentials: Credentials::signature(signer_pubkey, signer_signature, timestamp),
        };

        let result = get_delta_proposals(&state, params).await.unwrap();

        assert_eq!(result.proposals.len(), 0);
    }

    #[tokio::test]
    async fn test_get_delta_proposals_single_proposal() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();

        let (signer_pubkey, signer_commitment, signer_signature, timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![signer_commitment.clone()],
        ))));

        let proposal = create_pending_proposal(account_id.clone(), 1, signer_commitment.clone());
        let _storage = storage.with_pull_all_delta_proposals(Ok(vec![proposal.clone()]));

        let params = GetDeltaProposalsParams {
            account_id: account_id.clone(),
            credentials: Credentials::signature(signer_pubkey, signer_signature, timestamp),
        };

        let result = get_delta_proposals(&state, params).await.unwrap();

        assert_eq!(result.proposals.len(), 1);
        assert_eq!(result.proposals[0].nonce, 1);
    }

    #[tokio::test]
    async fn test_get_delta_proposals_multiple_sorted() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();

        let (signer_pubkey, signer_commitment, signer_signature, timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![signer_commitment.clone()],
        ))));

        let proposal_3 = create_pending_proposal(account_id.clone(), 3, signer_commitment.clone());
        let proposal_1 = create_pending_proposal(account_id.clone(), 1, signer_commitment.clone());
        let proposal_2 = create_pending_proposal(account_id.clone(), 2, signer_commitment.clone());

        let _storage =
            storage.with_pull_all_delta_proposals(Ok(vec![proposal_3, proposal_1, proposal_2]));

        let params = GetDeltaProposalsParams {
            account_id: account_id.clone(),
            credentials: Credentials::signature(signer_pubkey, signer_signature, timestamp),
        };

        let result = get_delta_proposals(&state, params).await.unwrap();

        assert_eq!(result.proposals.len(), 3);
        assert_eq!(result.proposals[0].nonce, 1);
        assert_eq!(result.proposals[1].nonce, 2);
        assert_eq!(result.proposals[2].nonce, 3);
    }

    #[tokio::test]
    async fn test_get_delta_proposals_filters_canonical() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();

        let (signer_pubkey, signer_commitment, signer_signature, timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![signer_commitment.clone()],
        ))));

        let proposal_pending =
            create_pending_proposal(account_id.clone(), 1, signer_commitment.clone());
        let proposal_canonical = create_canonical_proposal(account_id.clone(), 2);

        let _storage =
            storage.with_pull_all_delta_proposals(Ok(vec![proposal_pending, proposal_canonical]));

        let params = GetDeltaProposalsParams {
            account_id: account_id.clone(),
            credentials: Credentials::signature(signer_pubkey, signer_signature, timestamp),
        };

        let result = get_delta_proposals(&state, params).await.unwrap();

        assert_eq!(result.proposals.len(), 1);
        assert_eq!(result.proposals[0].nonce, 1);
        assert!(result.proposals[0].status.is_pending());
    }

    #[tokio::test]
    async fn test_get_delta_proposals_storage_error_returns_empty() {
        let (state, storage, _network, metadata) = create_test_state();

        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();

        let (signer_pubkey, signer_commitment, signer_signature, timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![signer_commitment.clone()],
        ))));

        let _storage = storage.with_pull_all_delta_proposals(Err("Storage error".to_string()));

        let params = GetDeltaProposalsParams {
            account_id: account_id.clone(),
            credentials: Credentials::signature(signer_pubkey, signer_signature, timestamp),
        };

        let result = get_delta_proposals(&state, params).await.unwrap();

        assert_eq!(result.proposals.len(), 0);
    }

    #[tokio::test]
    async fn test_get_delta_proposals_unauthorized() {
        let (state, _storage, _network, metadata) = create_test_state();

        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();

        let (_authorized_pubkey, authorized_commitment, _, _) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let (
            unauthorized_pubkey,
            _unauthorized_commitment,
            unauthorized_signature,
            unauthorized_ts,
        ) = crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![authorized_commitment],
        ))));

        let params = GetDeltaProposalsParams {
            account_id: account_id.clone(),
            credentials: Credentials::signature(
                unauthorized_pubkey,
                unauthorized_signature,
                unauthorized_ts,
            ),
        };

        let result = get_delta_proposals(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            crate::error::PsmError::AuthenticationFailed(_) => {}
            e => panic!("Expected AuthenticationFailed error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_get_delta_proposals_account_not_found() {
        let (state, _storage, _network, metadata) = create_test_state();

        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();

        let (signer_pubkey, _signer_commitment, signer_signature, timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(None));

        let params = GetDeltaProposalsParams {
            account_id: account_id.clone(),
            credentials: Credentials::signature(signer_pubkey, signer_signature, timestamp),
        };

        let result = get_delta_proposals(&state, params).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            crate::error::PsmError::AccountNotFound(_) => {}
            e => panic!("Expected AccountNotFound error, got: {:?}", e),
        }
    }
}
