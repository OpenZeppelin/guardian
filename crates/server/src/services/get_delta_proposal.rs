use crate::builder::state::AppState;
use crate::delta_object::DeltaObject;
use crate::error::{GuardianError, Result};
use crate::metadata::auth::Credentials;
use crate::services::resolve_account;

#[derive(Debug, Clone)]
pub struct GetDeltaProposalParams {
    pub account_id: String,
    pub commitment: String,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct GetDeltaProposalResult {
    pub proposal: DeltaObject,
}

pub async fn get_delta_proposal(
    state: &AppState,
    params: GetDeltaProposalParams,
) -> Result<GetDeltaProposalResult> {
    let GetDeltaProposalParams {
        account_id,
        commitment,
        credentials,
    } = params;

    let resolved = resolve_account(state, &account_id, &credentials).await?;

    let proposal = resolved
        .storage
        .pull_delta_proposal(&account_id, &commitment)
        .await
        .map_err(|_| GuardianError::ProposalNotFound {
            account_id: account_id.clone(),
            commitment: commitment.clone(),
        })?;

    if !proposal.status.is_pending() {
        return Err(GuardianError::ProposalNotFound {
            account_id,
            commitment,
        });
    }

    Ok(GetDeltaProposalResult { proposal })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta_object::{DeltaObject, DeltaStatus};
    use crate::metadata::AccountMetadata;
    use crate::metadata::auth::Auth;
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
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-11-14T12:00:00Z".to_string(),
            updated_at: "2024-11-14T12:00:00Z".to_string(),
            has_pending_candidate: false,
            last_auth_timestamp: None,
        }
    }

    fn pending_proposal(account_id: String, commitment: &str) -> DeltaObject {
        DeltaObject {
            account_id,
            nonce: 1,
            prev_commitment: "0xprev".to_string(),
            new_commitment: None,
            delta_payload: serde_json::json!({
                "tx_summary": { "data": "dGVzdA==" },
                "signatures": [],
                "metadata": { "proposal_type": "change_threshold", "target_threshold": 2, "signer_commitments": [] }
            }),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::Pending {
                timestamp: "2024-11-14T12:00:00Z".to_string(),
                proposer_id: commitment.to_string(),
                cosigner_sigs: vec![],
            },
        }
    }

    #[tokio::test]
    async fn get_delta_proposal_returns_pending_proposal() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let commitment = "0xabc".to_string();

        let (signer_pubkey, signer_commitment, signer_signature, timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![signer_commitment],
        ))));

        let proposal = pending_proposal(account_id.clone(), &commitment);
        let _storage = storage.with_pull_delta_proposal(Ok(proposal.clone()));

        let params = GetDeltaProposalParams {
            account_id,
            commitment,
            credentials: Credentials::signature(signer_pubkey, signer_signature, timestamp),
        };

        let result = get_delta_proposal(&state, params).await.unwrap();
        assert_eq!(result.proposal.nonce, proposal.nonce);
    }

    #[tokio::test]
    async fn get_delta_proposal_rejects_non_pending_proposal() {
        let (state, storage, _network, metadata) = create_test_state();
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let commitment = "0xabc".to_string();

        let (signer_pubkey, signer_commitment, signer_signature, timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            vec![signer_commitment],
        ))));

        let mut proposal = pending_proposal(account_id.clone(), &commitment);
        proposal.status = DeltaStatus::Canonical {
            timestamp: "2024-11-14T12:01:00Z".to_string(),
        };
        let _storage = storage.with_pull_delta_proposal(Ok(proposal));

        let params = GetDeltaProposalParams {
            account_id: account_id.clone(),
            commitment: commitment.clone(),
            credentials: Credentials::signature(signer_pubkey, signer_signature, timestamp),
        };

        let error = get_delta_proposal(&state, params).await.unwrap_err();
        assert_eq!(
            error,
            GuardianError::ProposalNotFound {
                account_id,
                commitment
            }
        );
    }
}
