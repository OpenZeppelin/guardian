use crate::builder::state::AppState;
use crate::delta_object::DeltaStatus;
use crate::error::{GuardianError, Result};
use crate::jobs::canonicalization::{RemovalMode, record_candidate_outcome, remove_candidate};
use crate::metadata::auth::Credentials;
use crate::network::StateVerification;
use crate::services::account_status::ensure_account_active_metadata;
use crate::services::resolve_account;
use tracing::info;

#[derive(Debug, Clone)]
pub struct AbandonCandidateParams {
    pub account_id: String,
    pub nonce: u64,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct AbandonCandidateResult {
    pub account_id: String,
    pub nonce: u64,
    pub abandoned_at: String,
}

/// Client-initiated release of a pending canonicalization candidate
/// (issue #319).
///
/// A candidate whose transaction died client-side after approval (RPC
/// submit failure, prover timeout, crash) looks identical to one that is
/// slowly proving, so the worker holds the account for the full
/// submission grace period plus retry budget. Only the client knows its
/// transaction will never land; this service lets it free the account
/// immediately instead of waiting that window out.
///
/// The abandon is the client's assertion that the transaction is dead.
/// As a guard, the service refuses with [`GuardianError::CandidateLanded`]
/// when the on-chain state already matches the candidate's expected state
/// (the transaction landed; the worker will canonicalize it shortly) —
/// abandoning then would leave guardian state behind chain. An on-chain
/// read failure fails the request rather than proceeding blind.
///
/// The `delete_delta` inside [`remove_candidate`] is the linearization
/// point: a retry after a 5xx that returns `delta_not_found` means the
/// abandon already succeeded.
pub async fn abandon_candidate(
    state: &AppState,
    params: AbandonCandidateParams,
) -> Result<AbandonCandidateResult> {
    let AbandonCandidateParams {
        account_id,
        nonce,
        credentials,
    } = params;

    let resolved = resolve_account(state, &account_id, &credentials).await?;
    ensure_account_active_metadata(&resolved.metadata)?;

    let delta = resolved
        .storage
        .pull_delta(&account_id, nonce)
        .await
        .map_err(|_| GuardianError::DeltaNotFound {
            account_id: account_id.clone(),
            nonce,
        })?;

    match &delta.status {
        DeltaStatus::Candidate { .. } => {}
        DeltaStatus::Canonical { .. } => {
            return Err(GuardianError::CandidateLanded { account_id, nonce });
        }
        _ => {
            return Err(GuardianError::DeltaNotFound { account_id, nonce });
        }
    }

    let current_state = resolved
        .storage
        .pull_state(&account_id)
        .await
        .map_err(|_| GuardianError::StateNotFound(account_id.clone()))?;

    let (new_state_json, _) = {
        let client = state.network_client.lock().await;
        client
            .apply_delta(&current_state.state_json, &delta.delta_payload)
            .map_err(GuardianError::InvalidDelta)?
    };

    let verify_result = {
        let mut client = state.network_client.lock().await;
        client.verify_state(&account_id, &new_state_json).await
    };

    match verify_result {
        Ok(StateVerification::Match) => {
            return Err(GuardianError::CandidateLanded { account_id, nonce });
        }
        // Covers both "on-chain still at the candidate's base (tx never
        // landed)" and "account diverged past the base": in either case the
        // candidate is not observed on-chain and abandoning is safe.
        Ok(StateVerification::Mismatch { .. }) => {}
        Err(e) => {
            return Err(GuardianError::NetworkError(format!(
                "Cannot verify on-chain state before abandon: {e}"
            )));
        }
    }

    // Re-read immediately before removal: the worker may have canonicalized
    // (or discarded) the candidate between the on-chain read above and now.
    let delta = resolved
        .storage
        .pull_delta(&account_id, nonce)
        .await
        .map_err(|_| GuardianError::DeltaNotFound {
            account_id: account_id.clone(),
            nonce,
        })?;
    if !delta.status.is_candidate() {
        return Err(GuardianError::CandidateLanded { account_id, nonce });
    }

    let now = state.clock.now_rfc3339();
    remove_candidate(state, &delta, &now, RemovalMode::Strict).await?;
    record_candidate_outcome(crate::metrics::labels::CandidateOutcome::Abandoned);

    info!(
        account_id = %account_id,
        nonce,
        "Candidate abandoned on client request; account released"
    );

    Ok(AbandonCandidateResult {
        account_id,
        nonce,
        abandoned_at: now,
    })
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::delta_object::DeltaObject;
    use crate::metadata::AccountMetadata;
    use crate::metadata::auth::Auth;
    use crate::state_object::StateObject;
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

    fn create_account_metadata(account_id: String, auth: Auth) -> AccountMetadata {
        AccountMetadata {
            account_id,
            auth,
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2024-11-14T12:00:00Z".to_string(),
            updated_at: "2024-11-14T12:00:00Z".to_string(),
            has_pending_candidate: true,
            last_auth_timestamp: None,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        }
    }

    fn create_state_object(account_id: String, commitment: String) -> StateObject {
        let account_json: serde_json::Value = serde_json::from_str(fixtures::ACCOUNT_JSON).unwrap();
        StateObject {
            account_id,
            commitment,
            state_json: account_json,
            created_at: "2024-11-14T12:00:00Z".to_string(),
            updated_at: "2024-11-14T12:00:00Z".to_string(),
            auth_scheme: String::new(),
        }
    }

    fn create_candidate_delta(account_id: &str, nonce: u64) -> DeltaObject {
        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        DeltaObject {
            account_id: account_id.to_string(),
            nonce,
            prev_commitment: "0x123".to_string(),
            new_commitment: Some("0x456".to_string()),
            delta_payload: delta_fixture["delta_payload"].clone(),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: DeltaStatus::candidate("2024-11-14T12:00:00Z".to_string()),
            metadata: None,
        }
    }

    struct TestSetup {
        state: AppState,
        storage: MockStorageBackend,
        metadata: MockMetadataStore,
        params: AbandonCandidateParams,
    }

    /// Common happy-path scaffolding: authenticated account with a
    /// candidate delta at nonce 1. Individual tests override mock
    /// responses before calling the service.
    fn setup(network: MockNetworkClient) -> TestSetup {
        let storage = MockStorageBackend::new();
        let metadata = MockMetadataStore::new();
        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(Mutex::new(network.clone())),
            Arc::new(metadata.clone()),
        );

        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();

        let (test_pubkey, test_commitment_hex, test_signature, test_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let _metadata = metadata.clone().with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![test_commitment_hex],
            },
        ))));

        let _storage = storage
            .clone()
            .with_pull_state(Ok(create_state_object(
                account_id.clone(),
                "0x123".to_string(),
            )))
            .with_pull_delta(Ok(create_candidate_delta(&account_id, 1)))
            .with_pull_delta(Ok(create_candidate_delta(&account_id, 1)));

        let _network =
            network.with_apply_delta(Ok((serde_json::json!({"new": true}), "0x456".to_string())));

        let params = AbandonCandidateParams {
            account_id,
            nonce: 1,
            credentials: Credentials::signature(test_pubkey, test_signature, test_timestamp),
        };

        TestSetup {
            state,
            storage,
            metadata,
            params,
        }
    }

    #[tokio::test]
    async fn test_abandon_success_deletes_delta_proposal_and_clears_flag() {
        let network = MockNetworkClient::new().with_verify_state(Ok(StateVerification::Mismatch {
            on_chain: "0x123".to_string(),
        }));
        let t = setup(network);
        // The matching proposal exists and its deletion succeeds.
        let _ = t
            .storage
            .clone()
            .with_pull_delta_proposal(Ok(create_candidate_delta(&t.params.account_id, 1)))
            .with_delete_delta_proposal(Ok(()));

        let result = abandon_candidate(&t.state, t.params.clone()).await;
        assert!(result.is_ok(), "Expected success, got: {:?}", result);
        let result = result.unwrap();
        assert_eq!(result.account_id, t.params.account_id);
        assert_eq!(result.nonce, 1);
        assert!(!result.abandoned_at.is_empty());

        let delete_calls = t.storage.get_delete_delta_calls();
        assert_eq!(delete_calls, vec![(t.params.account_id.clone(), 1)]);
        assert_eq!(t.storage.get_delete_delta_proposal_calls().len(), 1);

        // The flag-clear goes through the default get→set path; the last
        // written metadata must have the flag cleared.
        let set_calls = t.metadata.get_set_calls();
        assert!(!set_calls.is_empty(), "expected metadata set call");
        assert!(!set_calls.last().unwrap().has_pending_candidate);
    }

    #[tokio::test]
    async fn test_abandon_refused_when_candidate_landed_on_chain() {
        let network = MockNetworkClient::new().with_verify_state(Ok(StateVerification::Match));
        let t = setup(network);

        let result = abandon_candidate(&t.state, t.params).await;
        match result.unwrap_err() {
            GuardianError::CandidateLanded { nonce, .. } => assert_eq!(nonce, 1),
            e => panic!("Expected CandidateLanded, got: {:?}", e),
        }
        assert!(t.storage.get_delete_delta_calls().is_empty());
    }

    #[tokio::test]
    async fn test_abandon_refused_on_chain_read_failure_fails_closed() {
        let network = MockNetworkClient::new().with_verify_state(Err("rpc timeout".to_string()));
        let t = setup(network);

        let result = abandon_candidate(&t.state, t.params).await;
        match result.unwrap_err() {
            GuardianError::NetworkError(msg) => assert!(msg.contains("rpc timeout")),
            e => panic!("Expected NetworkError, got: {:?}", e),
        }
        assert!(t.storage.get_delete_delta_calls().is_empty());
    }

    #[tokio::test]
    async fn test_abandon_missing_delta_maps_to_delta_not_found() {
        // No pull_delta response is canned, so the mock returns its
        // "No delta found" error.
        let (state, storage, _, metadata) = create_test_state();

        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();
        let (pubkey, commitment, signature, timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let _ = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment],
            },
        ))));

        let params = AbandonCandidateParams {
            account_id,
            nonce: 9,
            credentials: Credentials::signature(pubkey, signature, timestamp),
        };
        let result = abandon_candidate(&state, params).await;
        match result.unwrap_err() {
            GuardianError::DeltaNotFound { nonce, .. } => assert_eq!(nonce, 9),
            e => panic!("Expected DeltaNotFound, got: {:?}", e),
        }
        assert!(storage.get_delete_delta_calls().is_empty());
    }

    #[tokio::test]
    async fn test_abandon_already_canonical_maps_to_candidate_landed() {
        let network = MockNetworkClient::new();
        let (state, storage, _, metadata) = create_test_state();

        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();
        let (pubkey, commitment, signature, timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let _ = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment],
            },
        ))));

        let mut canonical = create_candidate_delta(&account_id, 1);
        canonical.status = DeltaStatus::canonical("2024-11-14T12:05:00Z".to_string());
        let _ = storage.clone().with_pull_delta(Ok(canonical));

        let params = AbandonCandidateParams {
            account_id,
            nonce: 1,
            credentials: Credentials::signature(pubkey, signature, timestamp),
        };
        let result = abandon_candidate(&state, params).await;
        match result.unwrap_err() {
            GuardianError::CandidateLanded { .. } => {}
            e => panic!("Expected CandidateLanded, got: {:?}", e),
        }
        assert!(storage.get_delete_delta_calls().is_empty());
        let _ = network;
    }

    #[tokio::test]
    async fn test_abandon_race_recheck_bails_when_no_longer_candidate() {
        // First pull_delta returns the candidate; second (pre-delete
        // re-check) returns it already canonicalized by the worker.
        let network = MockNetworkClient::new().with_verify_state(Ok(StateVerification::Mismatch {
            on_chain: "0x123".to_string(),
        }));
        let (state, storage, _, metadata) = create_test_state();

        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();
        let (pubkey, commitment, signature, timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        let _ = metadata.with_get(Ok(Some(create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment],
            },
        ))));

        let mut canonical = create_candidate_delta(&account_id, 1);
        canonical.status = DeltaStatus::canonical("2024-11-14T12:05:00Z".to_string());
        // Mock responses are a stack (LIFO): push the re-check response
        // first so the candidate is returned on the first pull.
        let _ = storage
            .clone()
            .with_pull_delta(Ok(canonical))
            .with_pull_delta(Ok(create_candidate_delta(&account_id, 1)))
            .with_pull_state(Ok(create_state_object(
                account_id.clone(),
                "0x123".to_string(),
            )));
        let _ = network
            .clone()
            .with_apply_delta(Ok((serde_json::json!({"new": true}), "0x456".to_string())));

        let params = AbandonCandidateParams {
            account_id,
            nonce: 1,
            credentials: Credentials::signature(pubkey, signature, timestamp),
        };
        let result = abandon_candidate(&state, params).await;
        match result.unwrap_err() {
            GuardianError::CandidateLanded { .. } => {}
            e => panic!("Expected CandidateLanded, got: {:?}", e),
        }
        assert!(storage.get_delete_delta_calls().is_empty());
    }

    #[tokio::test]
    async fn test_abandon_paused_account_rejected() {
        let network = MockNetworkClient::new();
        let (state, storage, _, metadata) = create_test_state();

        let delta_fixture: serde_json::Value =
            serde_json::from_str(fixtures::DELTA_1_JSON).unwrap();
        let account_id = delta_fixture["account_id"].as_str().unwrap().to_string();
        let (pubkey, commitment, signature, timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let mut account_metadata = create_account_metadata(
            account_id.clone(),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment],
            },
        );
        account_metadata.paused_at = Some(
            chrono::DateTime::parse_from_rfc3339("2026-05-19T14:23:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let _ = metadata.with_get(Ok(Some(account_metadata)));

        let params = AbandonCandidateParams {
            account_id,
            nonce: 1,
            credentials: Credentials::signature(pubkey, signature, timestamp),
        };
        let result = abandon_candidate(&state, params).await;
        match result.unwrap_err() {
            GuardianError::AccountPaused { .. } => {}
            e => panic!("Expected AccountPaused, got: {:?}", e),
        }
        assert!(storage.get_delete_delta_calls().is_empty());
        let _ = network;
    }

    #[tokio::test]
    async fn test_abandon_strict_flag_clear_failure_is_hard_error() {
        let network = MockNetworkClient::new().with_verify_state(Ok(StateVerification::Mismatch {
            on_chain: "0x123".to_string(),
        }));
        let t = setup(network);
        // Fail the metadata write used by the default set_has_pending_candidate
        // (get → set) implementation.
        let _ = t.metadata.clone().with_set(Err("disk full".to_string()));

        let result = abandon_candidate(&t.state, t.params).await;
        match result.unwrap_err() {
            GuardianError::StorageError(msg) => {
                assert!(msg.contains("has_pending_candidate"), "got: {msg}")
            }
            e => panic!("Expected StorageError, got: {:?}", e),
        }
        // The delta itself was deleted before the failure — the account is
        // released (linearization point) even though the request errored.
        assert_eq!(t.storage.get_delete_delta_calls().len(), 1);
    }
}
