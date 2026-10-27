use crate::builder::state::AppState;
use crate::delta_object::{DeltaObject, DeltaStatus};
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
    /// The transaction to abandon: its summary's commitment, which is the
    /// proposal id of a multisig proposal. `None` from a client up to
    /// 0.18.3, which names only the nonce.
    pub proposal_id: Option<String>,
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
    /// The worker already stopped actively verifying this candidate and
    /// released the account slot (issue #345). Unlike `Abandoned`, the
    /// on-chain outcome remains UNCERTAIN: background reconciliation may
    /// still promote the delta to `canonical` until its retention TTL
    /// expires. Nothing is locked, so there is nothing left to abandon —
    /// but callers must not read this as "the transaction did not land".
    Retained,
}

impl AbandonState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Abandoned => "abandoned",
            Self::Retained => "retained",
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
    /// The abandoned delta's `new_commitment`, which tells it apart from a
    /// delta that later takes the same nonce.
    pub commitment: String,
}

/// Read the delta at `nonce`, `None` when there is none: a storage outage
/// must surface as a 5xx `StorageError`, never as a spurious 404.
async fn pull_delta_at_nonce(
    storage: &std::sync::Arc<dyn crate::storage::StorageBackend>,
    account_id: &str,
    nonce: u64,
) -> Result<Option<DeltaObject>> {
    match storage.pull_delta(account_id, nonce).await {
        Ok(delta) => Ok(Some(delta)),
        Err(e) if crate::storage::is_storage_not_found(&e) => Ok(None),
        Err(e) => Err(GuardianError::StorageError(format!(
            "Failed to read delta: {e}"
        ))),
    }
}

/// Whether `delta` carries the transaction `proposal_id` names.
fn carries_transaction(delta: &DeltaObject, proposal_id: &str) -> bool {
    use guardian_shared::hex::IntoHex;
    crate::ack::ack_message(delta)
        .is_some_and(|commitment| commitment.into_hex().eq_ignore_ascii_case(proposal_id))
}

/// The delta an abandon names. A Guardian execution can archive a parked
/// delta and take its nonce, so a delta is found either at its nonce or in
/// the archive, where reconciliation can still restore it.
enum Target {
    Live(DeltaObject),
    Archived(DeltaObject),
}

/// The delta at `nonce` carrying `proposal_id`, live or archived, or
/// without a proposal id (a client up to 0.18.3) whatever delta is at the
/// nonce.
async fn find_target(
    storage: &std::sync::Arc<dyn crate::storage::StorageBackend>,
    account_id: &str,
    nonce: u64,
    proposal_id: Option<&str>,
) -> Result<Target> {
    let not_found = || GuardianError::DeltaNotFound {
        account_id: account_id.to_string(),
        nonce,
    };
    let live = pull_delta_at_nonce(storage, account_id, nonce).await?;
    let Some(proposal_id) = proposal_id else {
        return live.map(Target::Live).ok_or_else(not_found);
    };
    if let Some(live) = live.filter(|live| carries_transaction(live, proposal_id)) {
        return Ok(Target::Live(live));
    }
    let archived = storage
        .pull_superseded_deltas(account_id, crate::storage::EVERY_ARCHIVED_DELTA)
        .await
        .map_err(|e| GuardianError::StorageError(format!("Failed to read archived deltas: {e}")))?;
    if let Some(archived) = archived
        .into_iter()
        .filter(|archived| {
            archived.delta.nonce == nonce && carries_transaction(&archived.delta, proposal_id)
        })
        .max_by_key(|archived| archived.superseded_at)
    {
        return Ok(Target::Archived(archived.delta));
    }
    // A restore moves an archived delta back to its nonce in between the
    // two reads, so a delta missed by both is read once more where it went.
    pull_delta_at_nonce(storage, account_id, nonce)
        .await?
        .filter(|live| carries_transaction(live, proposal_id))
        .map(Target::Live)
        .ok_or_else(not_found)
}

/// Where the named delta stands: settled, so the abandon answers at once,
/// or a candidate the intent can be recorded on.
enum Resolution {
    Settled(AbandonCandidateResult),
    Candidate(Box<DeltaObject>, String),
}

fn resolve(account_id: &str, nonce: u64, target: Target) -> Result<Resolution> {
    let delta = match target {
        Target::Live(delta) | Target::Archived(delta) => delta,
    };
    let commitment = delta.new_commitment.clone().ok_or_else(|| {
        GuardianError::StorageError(format!(
            "Delta at nonce {nonce} of account {account_id} has no new_commitment"
        ))
    })?;
    let settled = |state| {
        Ok(Resolution::Settled(AbandonCandidateResult {
            account_id: account_id.to_string(),
            nonce,
            state,
            abandon_requested_at: None,
            commitment: commitment.clone(),
        }))
    };
    match &delta.status {
        DeltaStatus::Candidate { .. } => Ok(Resolution::Candidate(Box::new(delta), commitment)),
        DeltaStatus::Canonical { .. } => Err(GuardianError::CandidateLanded {
            account_id: account_id.to_string(),
            nonce,
        }),
        // Already resolved: a retried abandon reports success.
        status if status.is_client_abandoned() => settled(AbandonState::Abandoned),
        // The worker already gave up on this candidate and released the
        // account (issue #345): there is nothing left to unlock. Reported
        // as `retained`, NOT `abandoned`: the on-chain outcome is still
        // uncertain, and the row stays in the recovery net, so if the
        // transaction landed it is promoted later (the landed-always-wins
        // rule every abandon carries).
        DeltaStatus::Retained { .. } => settled(AbandonState::Retained),
        DeltaStatus::Pending { .. } | DeltaStatus::Discarded { .. } => {
            Err(GuardianError::DeltaNotFound {
                account_id: account_id.to_string(),
                nonce,
            })
        }
    }
}

/// Client-initiated abandon of a pending canonicalization candidate
/// (issue #319), as an intent: the request records
/// `abandon_requested_at` on the candidate and returns immediately; the
/// canonicalization worker, the sole owner of candidate lifecycles under
/// the lease model, resolves the intent after the abandon quarantine
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
/// The request names the transaction as well as the nonce: a Guardian
/// execution can archive a parked delta and take its nonce, and a retry
/// then answers for the archived delta, never for Guardian's. The intent
/// is recorded only while the candidate at the nonce is still the named
/// one, and never on an execution's candidate
/// ([`GuardianError::ExecutionConflict`]).
///
/// The request is refused with [`GuardianError::CandidateLanded`] when
/// the on-chain state already matches the candidate's expected state. An
/// on-chain read failure does NOT block the request: recording intent is
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
        proposal_id,
        credentials,
    } = params;

    let proposal_id = proposal_id
        .as_deref()
        .map(crate::utils::normalize_commitment_hex)
        .transpose()?;
    let resolved = resolve_account(state, &account_id, &credentials).await?;
    ensure_account_active_metadata(&resolved.metadata)?;

    let target = find_target(
        &resolved.storage,
        &account_id,
        nonce,
        proposal_id.as_deref(),
    )
    .await?;
    let (delta, commitment) = match resolve(&account_id, nonce, target)? {
        Resolution::Settled(answer) => return Ok(answer),
        Resolution::Candidate(delta, commitment) => (delta, commitment),
    };

    let execution = resolved
        .storage
        .load_active_execution(&account_id)
        .await
        .map_err(|e| GuardianError::StorageError(format!("Failed to load the execution: {e}")))?;
    if let Some(execution) = execution.filter(|execution| execution.owns_candidate(nonce)) {
        return Err(GuardianError::ExecutionConflict {
            blocking_proposal_id: execution.reservation.proposal_id,
        });
    }

    // Landed guard, best-effort: refuse when the transaction demonstrably
    // landed (the worker will canonicalize it shortly; abandoning would
    // put guardian behind chain). An RPC failure only skips the guard:
    // the intent write is non-destructive and the worker re-verifies
    // against chain before finalizing.
    let current_state = resolved
        .storage
        .pull_state(&account_id)
        .await
        .map_err(|_| GuardianError::StateNotFound(account_id.clone()))?;

    let expected_commitment = {
        let client = state.network_client.clone();
        let prev_state_json = current_state.state_json;
        let delta_payload = std::sync::Arc::new(delta.delta_payload.clone());
        crate::network::reconstructor()
            .run_background(move || client.apply_delta(&prev_state_json, &delta_payload))
            .await?
            .commitment
    };

    let verify_result = state
        .network_client
        .verify_commitment(
            &account_id,
            &expected_commitment,
            crate::network::RpcReadMode::Configured,
        )
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
        .request_candidate_abandon(&account_id, nonce, &commitment, &now)
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
                commitment,
            })
        }
        AbandonIntent::AlreadyRequested { requested_at } => Ok(AbandonCandidateResult {
            account_id,
            nonce,
            state: AbandonState::Pending,
            abandon_requested_at: Some(requested_at),
            commitment,
        }),
        AbandonIntent::OwnedByExecution { proposal_id } => Err(GuardianError::ExecutionConflict {
            blocking_proposal_id: proposal_id,
        }),
        AbandonIntent::NotCandidate => {
            // The candidate settled, or left the nonce, between the read
            // and the write: answer for the named delta as it now stands.
            // Without a proposal id the nonce names only the delta read
            // first, so its commitment pins the re-read.
            let named = proposal_id.as_deref();
            let target = find_target(&resolved.storage, &account_id, nonce, named).await?;
            let (Target::Live(delta) | Target::Archived(delta)) = &target;
            if delta.new_commitment.as_deref() != Some(commitment.as_str()) {
                return Err(GuardianError::DeltaNotFound { account_id, nonce });
            }
            match resolve(&account_id, nonce, target)? {
                Resolution::Settled(answer) => Ok(answer),
                Resolution::Candidate(..) => {
                    Err(GuardianError::DeltaNotFound { account_id, nonce })
                }
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
            nonce: None,
            state_json: account_json,
            created_at: "2024-11-14T12:00:00Z".to_string(),
            updated_at: "2024-11-14T12:00:00Z".to_string(),
            auth_scheme: String::new(),
        }
    }

    fn proposal_id_of(delta: &DeltaObject) -> String {
        use guardian_shared::hex::IntoHex;
        crate::ack::ack_message(delta)
            .expect("the fixture payload is a transaction summary")
            .into_hex()
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
            proposal_id: Some(proposal_id_of(&create_candidate_delta(&account_id, 1))),
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
        assert_eq!(result.commitment, "0x456");

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
            proposal_id: None,
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

    const GUARDIAN_COMMITMENT: &str = "0x457c";
    const ABANDONED_COMMITMENT: &str = "0x2085";

    fn execution_owning(account_id: &str, nonce: u64) -> crate::storage::ExecutionRecord {
        let now = chrono::Utc::now();
        crate::storage::ExecutionRecord {
            reservation: crate::storage::ExecutionReservation {
                account_id: account_id.to_string(),
                proposal_id: "0xproposal".to_string(),
                attempt: 1,
                fence: crate::storage::LeaseFence {
                    lease_name: format!("execution:{account_id}"),
                    holder_id: "replica:worker".to_string(),
                    fence_token: 1,
                },
                lease_expires_at: now,
                phase: crate::storage::ExecutionPhase::Sent,
                ignored_signatures: 0,
                released_at: None,
                created_at: now,
                updated_at: now,
            },
            evidence: Some(crate::storage::SubmissionEvidence {
                account_id: account_id.to_string(),
                proposal_id: "0xproposal".to_string(),
                attempt: 1,
                candidate_nonce: nonce,
                transaction_id: "0xtx".to_string(),
                expected_commitment: GUARDIAN_COMMITMENT.to_string(),
                reference_block: 100,
                expiration_block: 356,
                base_commitment: "0x123".to_string(),
                committed_at: now,
            }),
            outcome: None,
        }
    }

    /// The client's abandoned delta, archived when a Guardian execution took
    /// its nonce.
    fn archived_abandoned(delta: DeltaObject) -> crate::storage::SupersededDelta {
        crate::storage::SupersededDelta {
            delta: DeltaObject {
                new_commitment: Some(ABANDONED_COMMITMENT.to_string()),
                status: DeltaStatus::discarded_client_abandoned("2024-11-14T12:05:00Z".to_string()),
                ..delta
            },
            superseded_at: chrono::Utc::now(),
        }
    }

    /// A delta at `nonce` carrying another transaction than the fixture's,
    /// as a Guardian execution's does.
    fn other_transaction(account_id: &str, nonce: u64, status: DeltaStatus) -> DeltaObject {
        let fixture: serde_json::Value = serde_json::from_str(fixtures::DELTA_2_JSON).unwrap();
        DeltaObject {
            new_commitment: Some(GUARDIAN_COMMITMENT.to_string()),
            delta_payload: fixture["delta_payload"].clone(),
            status,
            ..create_candidate_delta(account_id, nonce)
        }
    }

    #[tokio::test]
    async fn test_abandon_retry_after_an_execution_took_the_nonce_answers_from_the_archive() {
        // Guardian's candidate sits at the nonce: the retry reports the
        // client's archived delta and records no intent on Guardian's.
        let t = setup(MockNetworkClient::new());
        let account_id = t.params.account_id.clone();
        let guardian = other_transaction(
            &account_id,
            1,
            DeltaStatus::candidate("2024-11-14T12:06:00Z".to_string()),
        );
        let _ = t
            .storage
            .clone()
            .with_pull_delta(Ok(guardian))
            .with_pull_superseded_deltas(Ok(vec![archived_abandoned(create_candidate_delta(
                &account_id,
                1,
            ))]))
            .with_request_candidate_abandon(Err("intent recorded on Guardian's candidate".into()));

        let result = abandon_candidate(&t.state, t.params).await.unwrap();
        assert_eq!(result.state, AbandonState::Abandoned);
        assert_eq!(result.commitment, ABANDONED_COMMITMENT);
        assert!(result.abandon_requested_at.is_none());
    }

    #[tokio::test]
    async fn test_abandon_of_an_execution_candidate_is_refused() {
        // A client up to 0.18.3 names only the nonce, which Guardian's
        // candidate holds: no intent is recorded on it.
        let t = setup(MockNetworkClient::new());
        let account_id = t.params.account_id.clone();
        let _ = t
            .storage
            .clone()
            .with_load_active_execution(Ok(Some(execution_owning(&account_id, 1))))
            .with_request_candidate_abandon(Err("intent recorded on Guardian's candidate".into()));
        let params = AbandonCandidateParams {
            proposal_id: None,
            ..t.params
        };

        match abandon_candidate(&t.state, params).await.unwrap_err() {
            GuardianError::ExecutionConflict {
                blocking_proposal_id,
            } => assert_eq!(blocking_proposal_id, "0xproposal"),
            e => panic!("Expected ExecutionConflict, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_abandon_retry_after_an_execution_landed_at_the_nonce_answers_from_the_archive() {
        // Guardian's transaction landed at the nonce: the abandoned one did
        // not, so the retry reports the archived delta, not CandidateLanded.
        let t = setup(MockNetworkClient::new());
        let account_id = t.params.account_id.clone();
        let guardian = other_transaction(
            &account_id,
            1,
            DeltaStatus::canonical("2024-11-14T12:06:00Z".to_string()),
        );
        let _ = t
            .storage
            .clone()
            .with_pull_delta(Ok(guardian))
            .with_pull_superseded_deltas(Ok(vec![archived_abandoned(create_candidate_delta(
                &account_id,
                1,
            ))]));

        let result = abandon_candidate(&t.state, t.params).await.unwrap();
        assert_eq!(result.state, AbandonState::Abandoned);
        assert_eq!(result.commitment, ABANDONED_COMMITMENT);
    }

    #[tokio::test]
    async fn test_abandon_of_a_landed_transaction_ignores_an_older_archived_one() {
        // The client's own transaction was restored and landed; an older
        // archived transaction at the nonce says nothing about it.
        let t = setup(MockNetworkClient::new());
        let account_id = t.params.account_id.clone();
        let mut landed = create_candidate_delta(&account_id, 1);
        landed.status = DeltaStatus::canonical("2024-11-14T12:06:00Z".to_string());
        let older = other_transaction(
            &account_id,
            1,
            DeltaStatus::discarded_client_abandoned("2024-11-14T12:01:00Z".to_string()),
        );
        let _ = t
            .storage
            .clone()
            .with_pull_delta(Ok(landed))
            .with_pull_superseded_deltas(Ok(vec![crate::storage::SupersededDelta {
                delta: older,
                superseded_at: chrono::Utc::now(),
            }]));

        match abandon_candidate(&t.state, t.params).await.unwrap_err() {
            GuardianError::CandidateLanded { nonce, .. } => assert_eq!(nonce, 1),
            e => panic!("Expected CandidateLanded, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_abandon_retry_after_an_execution_failed_answers_from_the_archive() {
        // Guardian's failed execution deleted its candidate, leaving the
        // nonce empty; the archived delta still answers.
        let t = setup(MockNetworkClient::new());
        let account_id = t.params.account_id.clone();
        let _ = t
            .storage
            .clone()
            .with_pull_delta(Err("Mock: delta not found".to_string()))
            .with_pull_superseded_deltas(Ok(vec![archived_abandoned(create_candidate_delta(
                &account_id,
                1,
            ))]));

        let result = abandon_candidate(&t.state, t.params).await.unwrap();
        assert_eq!(result.state, AbandonState::Abandoned);
        assert_eq!(result.commitment, ABANDONED_COMMITMENT);
    }

    #[tokio::test]
    async fn test_abandon_of_another_transaction_at_the_nonce_is_not_found() {
        let t = setup(MockNetworkClient::new());
        let account_id = t.params.account_id.clone();
        let guardian = other_transaction(
            &account_id,
            1,
            DeltaStatus::candidate("2024-11-14T12:06:00Z".to_string()),
        );
        let _ = t
            .storage
            .clone()
            .with_pull_delta(Ok(guardian.clone()))
            .with_pull_delta(Ok(guardian))
            .with_request_candidate_abandon(Err("intent recorded on Guardian's candidate".into()));

        match abandon_candidate(&t.state, t.params).await.unwrap_err() {
            GuardianError::DeltaNotFound { nonce, .. } => assert_eq!(nonce, 1),
            e => panic!("Expected DeltaNotFound, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_abandon_finds_a_delta_restored_between_its_reads() {
        // Reconciliation restored the archived delta to its nonce after the
        // first read and before the archive read: the delta is read once
        // more where it went, and it landed.
        let t = setup(MockNetworkClient::new());
        let account_id = t.params.account_id.clone();
        let mut restored = create_candidate_delta(&account_id, 1);
        restored.status = DeltaStatus::canonical("2024-11-14T12:06:00Z".to_string());
        let guardian = other_transaction(
            &account_id,
            1,
            DeltaStatus::retained(
                "2024-11-14T12:06:00Z".to_string(),
                crate::delta_object::RetainReason::RetryExhausted,
            ),
        );
        let _ = t
            .storage
            .clone()
            .with_pull_delta(Ok(restored))
            .with_pull_delta(Ok(guardian));

        match abandon_candidate(&t.state, t.params).await.unwrap_err() {
            GuardianError::CandidateLanded { nonce, .. } => assert_eq!(nonce, 1),
            e => panic!("Expected CandidateLanded, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_abandon_refuses_a_malformed_proposal_id() {
        let t = setup(MockNetworkClient::new());
        let params = AbandonCandidateParams {
            proposal_id: Some("not hex".to_string()),
            ..t.params
        };

        match abandon_candidate(&t.state, params).await.unwrap_err() {
            GuardianError::InvalidCommitment(_) => {}
            e => panic!("Expected InvalidCommitment, got: {:?}", e),
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
    async fn test_abandon_retained_candidate_reports_retained() {
        // A retained row (issue #345) means the worker already gave up
        // and released the account: the abandon resolves immediately
        // instead of 404ing on a row the client can plainly see — but as
        // `retained`, never `abandoned`: the on-chain outcome is still
        // uncertain and reconciliation may yet promote the delta.
        let network = MockNetworkClient::new();
        let t = setup(network);
        let mut retained = create_candidate_delta(&t.params.account_id, 1);
        retained.status = DeltaStatus::retained(
            "2024-11-14T12:05:00Z".to_string(),
            crate::delta_object::RetainReason::RetryExhausted,
        );
        let _ = t.storage.clone().with_pull_delta(Ok(retained));

        let result = abandon_candidate(&t.state, t.params).await.unwrap();
        assert_eq!(result.state, AbandonState::Retained);
        assert_eq!(result.state.as_str(), "retained");
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
            proposal_id: None,
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
        let _ = t
            .storage
            .clone()
            .with_request_candidate_abandon(Ok(crate::storage::AbandonIntent::NotCandidate));
        // Canned reads pop last-in first: the first read gets the candidate
        // pushed last, the re-read after the lost race gets `resolved`.
        let _ = t
            .storage
            .clone()
            .with_pull_delta(Ok(resolved))
            .with_pull_delta(Ok(create_candidate_delta(&t.params.account_id, 1)));

        let result = abandon_candidate(&t.state, t.params).await.unwrap();
        assert_eq!(result.state, AbandonState::Abandoned);
    }

    #[tokio::test]
    async fn test_abandon_race_answers_only_for_the_delta_it_read() {
        // A client up to 0.18.3 names only the nonce. Its candidate left the
        // nonce between the read and the write, and Guardian's delta landed
        // there: that says nothing about the client's transaction.
        let network =
            MockNetworkClient::new().with_verify_commitment(Ok(StateVerification::Mismatch {
                on_chain: "0x123".to_string(),
            }));
        let t = setup(network);
        let account_id = t.params.account_id.clone();
        let guardian = other_transaction(
            &account_id,
            1,
            DeltaStatus::canonical("2024-11-14T12:06:00Z".to_string()),
        );
        let _ = t
            .storage
            .clone()
            .with_request_candidate_abandon(Ok(crate::storage::AbandonIntent::NotCandidate))
            .with_pull_delta(Ok(guardian))
            .with_pull_delta(Ok(create_candidate_delta(&account_id, 1)));
        let params = AbandonCandidateParams {
            proposal_id: None,
            ..t.params
        };

        match abandon_candidate(&t.state, params).await.unwrap_err() {
            GuardianError::DeltaNotFound { nonce, .. } => assert_eq!(nonce, 1),
            e => panic!("Expected DeltaNotFound, got: {:?}", e),
        }
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
