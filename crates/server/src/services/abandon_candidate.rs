use crate::builder::state::AppState;
use crate::delta_object::DeltaStatus;
use crate::error::{GuardianError, Result};
use crate::metadata::auth::Credentials;
use crate::network::StateVerification;
use crate::services::account_status::ensure_account_active_metadata;
use crate::services::resolve_account;
use crate::storage::AbandonIntent;
use tracing::info;

#[derive(Debug, Clone)]
pub struct AbandonCandidateParams {
    pub account_id: String,
    pub nonce: u64,
    pub credentials: Credentials,
}

/// Where the abandon stands from the client's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbandonState {
    /// The intent is recorded; the delta remains a candidate (the account
    /// stays locked) until the canonicalization worker resolves it after
    /// the abandon quarantine.
    Pending,
    /// The worker already resolved the abandon: the delta is
    /// `Discarded { reason: ClientAbandoned }` and the account released.
    Abandoned,
}

impl AbandonState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Abandoned => "abandoned",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AbandonCandidateResult {
    pub account_id: String,
    pub nonce: u64,
    pub state: AbandonState,
    /// RFC 3339 UTC timestamp of the recorded abandon request. Retries
    /// return the original timestamp — the quarantine never restarts.
    /// `None` once the abandon is already resolved.
    pub abandon_requested_at: Option<String>,
}

/// Read the delta at `nonce` while distinguishing "absent" from "backend
/// failure": a storage outage must surface as a 5xx `StorageError`, never
/// as a spurious 404.
async fn pull_delta_at_nonce(
    storage: &std::sync::Arc<dyn crate::storage::StorageBackend>,
    account_id: &str,
    nonce: u64,
) -> Result<crate::delta_object::DeltaObject> {
    storage.pull_delta(account_id, nonce).await.map_err(|e| {
        if crate::storage::is_storage_not_found(&e) {
            GuardianError::DeltaNotFound {
                account_id: account_id.to_string(),
                nonce,
            }
        } else {
            GuardianError::StorageError(format!("Failed to read delta: {e}"))
        }
    })
}

/// Client-initiated abandon of a pending canonicalization candidate
/// (issue #319), as an intent: the request records
/// `abandon_requested_at` on the candidate and returns immediately; the
/// canonicalization worker — the sole owner of candidate lifecycles under
/// the lease model — resolves the intent after the abandon quarantine
/// (consecutive at-base observations plus a minimum age), transitioning
/// the delta to `Discarded { reason: ClientAbandoned }` and releasing the
/// account.
///
/// A candidate whose transaction died client-side after approval (RPC
/// submit failure, prover timeout, crash) looks identical to one that is
/// slowly proving, so without this the account stays locked for the full
/// submission grace period plus retry budget. Only the client knows its
/// transaction will never land.
///
/// The request is refused with [`GuardianError::CandidateLanded`] when
/// the on-chain state already matches the candidate's expected state. An
/// on-chain read failure does NOT block the request — recording intent is
/// non-destructive, and the worker independently re-verifies against
/// chain before finalizing. Retries are idempotent: the original request
/// timestamp is preserved so the quarantine never restarts.
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

    let delta = pull_delta_at_nonce(&resolved.storage, &account_id, nonce).await?;

    match &delta.status {
        DeltaStatus::Candidate { .. } => {}
        DeltaStatus::Canonical { .. } => {
            return Err(GuardianError::CandidateLanded { account_id, nonce });
        }
        status if status.is_client_abandoned() => {
            // Already resolved: a retried abandon reports success.
            return Ok(AbandonCandidateResult {
                account_id,
                nonce,
                state: AbandonState::Abandoned,
                abandon_requested_at: None,
            });
        }
        DeltaStatus::Retained { .. } => {
            // The worker already gave up on this candidate and released
            // the account (issue #345): there is nothing left to unlock,
            // so the abandon is reported resolved immediately. The row
            // stays in the recovery net — if the transaction actually
            // landed it is promoted later, the same landed-always-wins
            // rule every abandon carries — and a resubmission at this
            // nonce supersedes it either way.
            return Ok(AbandonCandidateResult {
                account_id,
                nonce,
                state: AbandonState::Abandoned,
                abandon_requested_at: None,
            });
        }
        _ => {
            return Err(GuardianError::DeltaNotFound { account_id, nonce });
        }
    }

    // Landed guard, best-effort: refuse when the transaction demonstrably
    // landed (the worker will canonicalize it shortly; abandoning would
    // put guardian behind chain). An RPC failure only skips the guard —
    // the intent write is non-destructive and the worker re-verifies
    // against chain before finalizing.
    let current_state = resolved
        .storage
        .pull_state(&account_id)
        .await
        .map_err(|_| GuardianError::StateNotFound(account_id.clone()))?;

    let (_, expected_commitment) = {
        let client = state.network_client.clone();
        let prev_state_json = current_state.state_json;
        let delta_payload = std::sync::Arc::new(delta.delta_payload.clone());
        crate::network::reconstructor()
            .run_background(move || client.apply_delta(&prev_state_json, &delta_payload))
            .await?
    };

    let verify_result = state
        .network_client
        .verify_commitment(&account_id, &expected_commitment)
        .await;
    match verify_result {
        Ok(StateVerification::Match) => {
            return Err(GuardianError::CandidateLanded { account_id, nonce });
        }
        // Mismatch: on-chain differs from the candidate's expected state
        // (tx not landed, or the account diverged). Absent: the account
        // has no on-chain state at all, so the tx certainly did not land.
        // Abandoning is safe in both cases.
        Ok(StateVerification::Mismatch { .. }) | Ok(StateVerification::Absent) => {}
        Err(e) => {
            tracing::warn!(
                account_id = %account_id,
                nonce,
                error = %e,
                "Could not verify on-chain state before abandon; recording \
                 intent anyway (worker re-verifies before finalizing)"
            );
        }
    }

    let now = state.clock.now_rfc3339();
    let intent = resolved
        .storage
        .request_candidate_abandon(&account_id, nonce, &now)
        .await
        .map_err(|e| {
            GuardianError::StorageError(format!("Failed to record abandon request: {e}"))
        })?;

    match intent {
        AbandonIntent::Recorded => {
            info!(
                account_id = %account_id,
                nonce,
                "Abandon requested; worker resolves after the quarantine"
            );
            Ok(AbandonCandidateResult {
                account_id,
                nonce,
                state: AbandonState::Pending,
                abandon_requested_at: Some(now),
            })
        }
        AbandonIntent::AlreadyRequested { requested_at } => Ok(AbandonCandidateResult {
            account_id,
            nonce,
            state: AbandonState::Pending,
            abandon_requested_at: Some(requested_at),
        }),
        AbandonIntent::NotCandidate => {
            // Resolved between our read and the write: classify precisely.
            // A row the worker retained meanwhile (issue #345) is treated
            // like the direct retained arm above — the account is already
            // released, nothing landed.
            let delta = pull_delta_at_nonce(&resolved.storage, &account_id, nonce).await?;
            if delta.status.is_client_abandoned() || delta.status.is_retained() {
                Ok(AbandonCandidateResult {
                    account_id,
                    nonce,
                    state: AbandonState::Abandoned,
                    abandon_requested_at: None,
                })
            } else {
                Err(GuardianError::CandidateLanded { account_id, nonce })
            }
        }
    }
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
        params: AbandonCandidateParams,
    }

    /// Common scaffolding: authenticated account with a candidate delta at
    /// nonce 1. Individual tests override mock responses before calling
    /// the service.
    fn setup(network: MockNetworkClient) -> TestSetup {
        let storage = MockStorageBackend::new();
        let metadata = MockMetadataStore::new();
        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(network.clone()),
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
            params,
        }
    }

    #[tokio::test]
    async fn test_abandon_records_intent_and_deletes_nothing() {
        let network =
            MockNetworkClient::new().with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: "0x123".to_string(),
            }));
        let t = setup(network);

        let result = abandon_candidate(&t.state, t.params.clone()).await;
        assert!(result.is_ok(), "Expected success, got: {:?}", result);
        let result = result.unwrap();
        assert_eq!(result.account_id, t.params.account_id);
        assert_eq!(result.nonce, 1);
        assert_eq!(result.state, AbandonState::Pending);
        assert!(result.abandon_requested_at.is_some());

        // Intent only: the endpoint never deletes or discards anything —
        // resolution belongs to the canonicalization worker.
        assert!(t.storage.get_delete_delta_calls().is_empty());
        assert!(t.storage.get_delete_delta_proposal_calls().is_empty());
        assert!(t.storage.get_update_delta_status_calls().is_empty());
    }

    #[tokio::test]
    async fn test_abandon_retry_preserves_original_request_timestamp() {
        let network =
            MockNetworkClient::new().with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: "0x123".to_string(),
            }));
        let t = setup(network);
        let _ = t.storage.clone().with_request_candidate_abandon(Ok(
            crate::storage::AbandonIntent::AlreadyRequested {
                requested_at: "2024-11-14T12:01:00Z".to_string(),
            },
        ));

        let result = abandon_candidate(&t.state, t.params).await.unwrap();
        assert_eq!(result.state, AbandonState::Pending);
        assert_eq!(
            result.abandon_requested_at.as_deref(),
            Some("2024-11-14T12:01:00Z"),
            "retries must return the original request timestamp"
        );
    }

    #[tokio::test]
    async fn test_abandon_refused_when_candidate_landed_on_chain() {
        let network = MockNetworkClient::new().with_verify_commitment(Ok(StateVerification::Match));
        let t = setup(network);

        let result = abandon_candidate(&t.state, t.params).await;
        match result.unwrap_err() {
            GuardianError::CandidateLanded { nonce, .. } => assert_eq!(nonce, 1),
            e => panic!("Expected CandidateLanded, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_abandon_records_intent_despite_rpc_failure() {
        // The intent write is non-destructive and the worker re-verifies
        // against chain before finalizing, so an RPC failure must not
        // block the request.
        let network =
            MockNetworkClient::new().with_verify_commitment(Err("rpc timeout".to_string()));
        let t = setup(network);

        let result = abandon_candidate(&t.state, t.params).await;
        assert!(result.is_ok(), "Expected success, got: {:?}", result);
        assert_eq!(result.unwrap().state, AbandonState::Pending);
    }

    #[tokio::test]
    async fn test_abandon_missing_delta_maps_to_delta_not_found() {
        // No pull_delta response is canned, so the mock returns its
        // "delta not found" default.
        let network = MockNetworkClient::new();
        let storage = MockStorageBackend::new();
        let metadata = MockMetadataStore::new();
        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(network.clone()),
            Arc::new(metadata.clone()),
        );

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
    }

    #[tokio::test]
    async fn test_abandon_storage_read_failure_is_not_delta_not_found() {
        // A backend outage must surface as a 5xx StorageError, never as
        // DeltaNotFound.
        let network = MockNetworkClient::new();
        let t = setup(network);
        let _ = t
            .storage
            .clone()
            .with_pull_delta(Err("connection refused".to_string()));

        let result = abandon_candidate(&t.state, t.params).await;
        match result.unwrap_err() {
            GuardianError::StorageError(msg) => assert!(msg.contains("connection refused")),
            e => panic!("Expected StorageError, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_abandon_already_canonical_maps_to_candidate_landed() {
        let network = MockNetworkClient::new();
        let t = setup(network);
        let mut canonical = create_candidate_delta(&t.params.account_id, 1);
        canonical.status = DeltaStatus::canonical("2024-11-14T12:05:00Z".to_string());
        let _ = t.storage.clone().with_pull_delta(Ok(canonical));

        let result = abandon_candidate(&t.state, t.params).await;
        match result.unwrap_err() {
            GuardianError::CandidateLanded { .. } => {}
            e => panic!("Expected CandidateLanded, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_abandon_already_resolved_reports_abandoned() {
        let network = MockNetworkClient::new();
        let t = setup(network);
        let mut resolved = create_candidate_delta(&t.params.account_id, 1);
        resolved.status =
            DeltaStatus::discarded_client_abandoned("2024-11-14T12:05:00Z".to_string());
        let _ = t.storage.clone().with_pull_delta(Ok(resolved));

        let result = abandon_candidate(&t.state, t.params).await.unwrap();
        assert_eq!(result.state, AbandonState::Abandoned);
        assert!(result.abandon_requested_at.is_none());
    }

    #[tokio::test]
    async fn test_abandon_retained_candidate_reports_abandoned() {
        // A retained row (issue #345) means the worker already gave up
        // and released the account: the abandon resolves immediately
        // instead of 404ing on a row the client can plainly see.
        let network = MockNetworkClient::new();
        let t = setup(network);
        let mut retained = create_candidate_delta(&t.params.account_id, 1);
        retained.status = DeltaStatus::retained(
            "2024-11-14T12:05:00Z".to_string(),
            crate::delta_object::RetainReason::RetryExhausted,
        );
        let _ = t.storage.clone().with_pull_delta(Ok(retained));

        let result = abandon_candidate(&t.state, t.params).await.unwrap();
        assert_eq!(result.state, AbandonState::Abandoned);
        assert!(result.abandon_requested_at.is_none());
    }

    #[tokio::test]
    async fn test_abandon_paused_account_rejected() {
        let network = MockNetworkClient::new();
        let storage = MockStorageBackend::new();
        let metadata = MockMetadataStore::new();
        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(network.clone()),
            Arc::new(metadata.clone()),
        );

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
    }

    #[tokio::test]
    async fn test_abandon_race_resolved_as_abandoned_between_read_and_write() {
        // The worker resolves the candidate between the service's read and
        // the intent write: classify via re-read.
        let network =
            MockNetworkClient::new().with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: "0x123".to_string(),
            }));
        let t = setup(network);
        let mut resolved = create_candidate_delta(&t.params.account_id, 1);
        resolved.status =
            DeltaStatus::discarded_client_abandoned("2024-11-14T12:05:00Z".to_string());
        // LIFO: the initial read pops the candidate canned by setup() only
        // after this re-read response, so push the re-read FIRST.
        let _ = t
            .storage
            .clone()
            .with_request_candidate_abandon(Ok(crate::storage::AbandonIntent::NotCandidate));
        // Re-read response must be popped AFTER setup's candidate: push
        // order is [setup candidate, resolved] -> pops resolved first.
        // Re-can both reads explicitly to control order.
        let _ = t
            .storage
            .clone()
            .with_pull_delta(Ok(resolved))
            .with_pull_delta(Ok(create_candidate_delta(&t.params.account_id, 1)));

        let result = abandon_candidate(&t.state, t.params).await.unwrap();
        assert_eq!(result.state, AbandonState::Abandoned);
    }

    #[tokio::test]
    async fn test_abandon_race_canonicalized_between_read_and_write() {
        let network =
            MockNetworkClient::new().with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: "0x123".to_string(),
            }));
        let t = setup(network);
        let mut canonical = create_candidate_delta(&t.params.account_id, 1);
        canonical.status = DeltaStatus::canonical("2024-11-14T12:05:00Z".to_string());
        let _ = t
            .storage
            .clone()
            .with_request_candidate_abandon(Ok(crate::storage::AbandonIntent::NotCandidate))
            .with_pull_delta(Ok(canonical))
            .with_pull_delta(Ok(create_candidate_delta(&t.params.account_id, 1)));

        let result = abandon_candidate(&t.state, t.params).await;
        match result.unwrap_err() {
            GuardianError::CandidateLanded { .. } => {}
            e => panic!("Expected CandidateLanded, got: {:?}", e),
        }
    }
}
