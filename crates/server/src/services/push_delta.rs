use guardian_shared::SignatureScheme;
use serde_json::Value;
use std::sync::Arc;

use crate::delta_object::DeltaObject;
use crate::error::{GuardianError, Result};
use crate::metadata::auth::Credentials;
use crate::services::account_status::ensure_account_active_metadata;
use crate::services::candidate_chain::{self, CandidateChain};
use crate::services::delta_commit::{CommitContext, DeltaCommitStrategy};
use crate::services::multisig_admission::MultisigAccount;
use crate::services::resolve_account;
use crate::state::AppState;
use crate::storage::ChainPosition;

#[derive(Debug, Clone)]
pub struct PushDeltaParams {
    pub delta: DeltaObject,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct PushDeltaResult {
    pub delta: DeltaObject,
}

#[tracing::instrument(
    level = "info",
    skip(state, params),
    fields(account_id = %params.delta.account_id)
)]
pub async fn push_delta(state: &AppState, params: PushDeltaParams) -> Result<PushDeltaResult> {
    tracing::debug!("Pushing delta");

    let resolved = resolve_account(state, &params.delta.account_id, &params.credentials).await?;
    ensure_account_active_metadata(&resolved.metadata)?;
    if resolved.metadata.network_config.is_evm() {
        return Err(GuardianError::UnsupportedForNetwork {
            network: "evm".to_string(),
            operation: "push_delta".to_string(),
        });
    }

    let mut current_state = resolved
        .storage
        .pull_state(&params.delta.account_id)
        .await
        .map_err(|e| {
            tracing::error!(
                account_id = %params.delta.account_id,
                error = %e,
                "Failed to fetch account state in push_delta"
            );
            GuardianError::StorageError(format!("Failed to fetch account state: {e}"))
        })?;

    // Queue admission (issue #17), in the order the storage gate applies
    // under the account lock (`storage::gate_candidate_submission`), so
    // two racing submissions cannot both extend the tail:
    // - a full queue refuses every delta (409, wait for it to drain);
    //   checked first, so depth 1 is exactly the historical one
    //   in-flight candidate gate, whatever the delta's base;
    // - the delta must extend the chain at its tail: building on the
    //   canonical state or on a non-tail candidate while a later
    //   candidate already occupies that slot is a competing submission
    //   (409); building on a state this server does not know is a
    //   commitment mismatch against the canonical commitment the client
    //   can resync to (400);
    // - its nonce must exceed the tail's: a lower one collides with a
    //   queued candidate's slot (409, like the in-lock gate);
    // - the tail must not change who may act on the account: nothing
    //   chains behind a candidate that changes the signer set or the
    //   guardian key until it promotes (409; `ensure_tail_keeps_auth`,
    //   judged on the replayed tail, so it runs after the replay and in
    //   the request path only; its docs say why that is race-safe);
    // - behind a queued candidate, its nonce must be the nonce the delta
    //   leaves the account at (409): the queue orders, gates and looks up
    //   candidates by that label, so a timestamp label would sort past
    //   every real nonce and refuse each correctly labelled successor
    //   until it promoted. Request path only, after `apply_delta`, like
    //   the auth binding: the storage gate does not repeat it, and a
    //   delta that fails it never reaches the lock. A candidate admitted
    //   after the queue was read moves the tail, so the lock-side
    //   position check refuses this delta as competing. With nothing
    //   queued the label is not checked, so older clients that label
    //   with a timestamp keep working at the head.
    let chain = CandidateChain::load_for_admission(
        resolved.storage.as_ref(),
        &params.delta.account_id,
        &mut current_state,
        Some(&params.delta.prev_commitment),
    )
    .await?;
    let max_pending_candidates = candidate_chain::max_pending_candidates(state);
    if chain.len() >= max_pending_candidates {
        tracing::info!(
            account_id = %params.delta.account_id,
            nonce = params.delta.nonce,
            queued = chain.len(),
            max_pending_candidates,
            "Candidate queue is full; rejecting as pending-delta conflict"
        );
        return Err(GuardianError::ConflictPendingDelta);
    }
    match chain.position(&current_state.commitment, &params.delta.prev_commitment) {
        ChainPosition::Tail => {}
        ChainPosition::Competing => {
            tracing::debug!(
                account_id = %params.delta.account_id,
                nonce = params.delta.nonce,
                queued = chain.len(),
                "Delta competes with a queued candidate for its base state"
            );
            return Err(GuardianError::ConflictPendingDelta);
        }
        ChainPosition::Unrelated => {
            return Err(GuardianError::CommitmentMismatch {
                expected: current_state.commitment.clone(),
                actual: params.delta.prev_commitment.clone(),
            });
        }
    }
    if let Some(tail_nonce) = chain.tail_nonce()
        && params.delta.nonce <= tail_nonce
    {
        tracing::info!(
            account_id = %params.delta.account_id,
            nonce = params.delta.nonce,
            tail_nonce,
            "Delta nonce does not extend the candidate queue; rejecting as pending-delta conflict"
        );
        return Err(GuardianError::ConflictPendingDelta);
    }
    let tail = chain.reconstruct_tail(state, &current_state).await?;
    candidate_chain::ensure_tail_keeps_auth(state, &current_state, &tail).await?;

    // One lookup serves both the multisig authorization gate and
    // metadata. A multisig tail is acknowledged only when the matching
    // proposal meets the threshold of the procedures the delta invokes;
    // a miss or a storage fault refuses the push here, before the delta
    // is verified or applied, so an under-signed push never consumes
    // the CPU-heavy work below. A single-key tail keeps the previous
    // behavior: a miss is metadata-only, and a storage fault does not
    // block the push. Signature verification is CPU work, so it runs
    // through the shared reconstruction gate, off the async threads.
    let proposal_match = lookup_matching_proposal(
        state,
        &params.delta.account_id,
        params.delta.nonce,
        &params.delta.delta_payload,
    )
    .await;
    if let Some(multisig) = MultisigAccount::from_state(&tail.state_json) {
        let proposal = match &proposal_match {
            ProposalMatch::Found(proposal) => Some(proposal.clone()),
            ProposalMatch::Missing => None,
            ProposalMatch::Unavailable(error) => {
                return Err(GuardianError::StorageError(format!(
                    "Failed to load the proposal authorizing this delta: {error}"
                )));
            }
        };
        let delta_payload = params.delta.delta_payload.clone();
        crate::network::reconstructor()
            .run(move || Ok(multisig.authorize(&delta_payload, proposal.as_deref())))
            .await??;
    }

    let applied = {
        let client = state.network_client.clone();
        let prev_commitment = tail.commitment.clone();
        let prev_state_json = tail.state_json.clone();
        let delta_payload = Arc::new(params.delta.delta_payload.clone());
        crate::network::reconstructor()
            .run(move || {
                client.verify_delta(&prev_commitment, &prev_state_json, &delta_payload)?;
                client.apply_delta(&prev_state_json, &delta_payload)
            })
            .await?
    };
    if !chain.is_empty()
        && let Some(applied_nonce) = applied.nonce
        && applied_nonce != params.delta.nonce
    {
        tracing::info!(
            account_id = %params.delta.account_id,
            nonce = params.delta.nonce,
            applied_nonce,
            "Chained delta is not labelled with the nonce it produces; rejecting as pending-delta conflict"
        );
        return Err(GuardianError::ConflictPendingDelta);
    }

    let matching_proposal_payload = match &proposal_match {
        ProposalMatch::Found(proposal) => Some(proposal.delta_payload.clone()),
        ProposalMatch::Missing | ProposalMatch::Unavailable(_) => None,
    };

    let derived_metadata = crate::delta_summary::build_metadata(
        &params.delta.delta_payload,
        matching_proposal_payload.as_ref(),
    );

    let mut result_delta = params.delta.clone();
    result_delta.new_commitment = Some(applied.commitment.clone());
    result_delta.metadata = derived_metadata;
    let scheme = resolved.metadata.auth.scheme();
    result_delta = state.ack.ack_delta(result_delta, &scheme).await?;
    result_delta.ack_pubkey = state.ack.pubkey(&scheme);
    result_delta.ack_scheme = match scheme {
        SignatureScheme::Falcon => "falcon",
        SignatureScheme::Ecdsa => "ecdsa",
    }
    .to_string();

    let now = state.clock.now_rfc3339();
    let commit_strategy = DeltaCommitStrategy::from_app_state(state);
    commit_strategy
        .commit(
            CommitContext {
                state,
                resolved: &resolved,
                current_state: &current_state,
                now,
            },
            &mut result_delta,
            applied,
        )
        .await?;
    // Caveat: on a single-key account a storage fault during the
    // proposal lookup is non-fatal, so that push can be labeled
    // `direct`. A multisig account never reaches this point on that
    // fault. The underlying fault is visible via
    // storage_operations_total{outcome="error"}.
    let kind = if matching_proposal_payload.is_some() {
        crate::metrics::labels::DeltaKind::ProposalCommit
    } else {
        crate::metrics::labels::DeltaKind::Direct
    };
    metrics::counter!(
        crate::metrics::names::DELTAS_SUBMITTED_TOTAL,
        crate::metrics::names::LABEL_KIND => kind.as_str()
    )
    .increment(1);

    Ok(PushDeltaResult {
        delta: result_delta,
    })
}

enum ProposalMatch {
    Found(Box<DeltaObject>),
    Missing,
    Unavailable(String),
}

/// Look up the `delta_proposals` row for the delta being pushed.
/// A missing row is [`ProposalMatch::Missing`]. A storage fault is
/// [`ProposalMatch::Unavailable`] and is logged at `warn`; the caller
/// decides whether that fault refuses the push.
async fn lookup_matching_proposal(
    state: &AppState,
    account_id: &str,
    nonce: u64,
    delta_payload: &Value,
) -> ProposalMatch {
    let proposal_id = {
        let client = &state.network_client;
        match client.delta_proposal_id(account_id, nonce, delta_payload) {
            Ok(id) => id,
            Err(err) => {
                tracing::debug!(
                    account_id = %account_id,
                    nonce,
                    error = %err,
                    "delta_proposal_id could not compute an id for this payload; \
                     persisting metadata without proposal block (EVM / malformed payload)"
                );
                return ProposalMatch::Missing;
            }
        }
    };
    match state
        .storage
        .pull_delta_proposal(account_id, &proposal_id)
        .await
    {
        Ok(proposal) => ProposalMatch::Found(Box::new(proposal)),
        Err(err) => {
            if crate::storage::is_storage_not_found(&err) {
                tracing::debug!(
                    account_id = %account_id,
                    nonce,
                    proposal_id = %proposal_id,
                    "no matching delta_proposal row (single-key push or unrelated payload)"
                );
                ProposalMatch::Missing
            } else {
                tracing::warn!(
                    account_id = %account_id,
                    nonce,
                    proposal_id = %proposal_id,
                    error = %err,
                    "delta_proposals lookup errored during push_delta; a multisig push is \
                     refused and a single-key push continues without proposal metadata"
                );
                ProposalMatch::Unavailable(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::AccountMetadata;
    use crate::metadata::auth::Auth;
    use crate::testing::helpers::create_test_app_state_with_mocks;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use chrono::TimeZone;
    use std::sync::Arc;

    fn paused_metadata(account_id: &str, cosigner_commitment: String) -> AccountMetadata {
        AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![cosigner_commitment],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2026-05-01T00:00:00Z".into(),
            updated_at: "2026-05-01T00:00:00Z".into(),
            has_pending_candidate: false,
            paused_at: Some(
                chrono::Utc
                    .with_ymd_and_hms(2026, 5, 19, 14, 30, 0)
                    .unwrap(),
            ),
            paused_reason: Some("compliance".to_string()),
            released_at: None,
        }
    }

    /// Pause-gate guard: `push_delta` MUST reject before touching
    /// storage or the network — but only AFTER authentication
    /// succeeds, so unauthenticated probes cannot leak pause state.
    #[tokio::test]
    async fn paused_account_rejected_before_side_effects() {
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let (signer_pubkey, signer_commitment, signer_signature, signer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let storage = MockStorageBackend::new();
        let network = MockNetworkClient::new();
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(paused_metadata(&account_id, signer_commitment))));

        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(network.clone()),
            Arc::new(metadata.clone()),
        );

        let params = PushDeltaParams {
            delta: DeltaObject {
                account_id: account_id.clone(),
                ..Default::default()
            },
            credentials: Credentials::signature(signer_pubkey, signer_signature, signer_timestamp),
        };

        let err = push_delta(&state, params)
            .await
            .expect_err("paused account must be rejected");
        assert!(
            matches!(err, GuardianError::AccountPaused { ref paused_reason, .. }
                if paused_reason.as_deref() == Some("compliance")),
            "unexpected error: {err:?}"
        );

        assert!(
            storage.get_submit_delta_calls().is_empty(),
            "no delta should be submitted when the account is paused"
        );
    }

    /// Push-time metadata pipeline: decode TransactionSummary, look up
    /// the matching proposal, lift its `proposal_type` into the typed
    /// `DeltaMetadata`, persist on the candidate row, and verify the
    /// dashboard projection surfaces it.
    #[tokio::test]
    async fn push_delta_persists_metadata_with_proposal_from_matching_proposal_lookup() {
        use crate::delta_object::DeltaStatus;
        use crate::delta_summary::DashboardDeltaCategory;
        use crate::state_object::StateObject;
        use crate::testing::helpers::create_test_delta_payload;

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let (signer_pubkey, signer_commitment, signer_signature, signer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let candidate_payload = create_test_delta_payload(&account_id);

        let proposal_wrapper = serde_json::json!({
            "tx_summary": create_test_delta_payload(&account_id),
            "metadata": {
                "proposal_type": "consume_notes",
                "note_ids": ["0xnote0000000000000000000000000001"],
                "consume_notes_metadata_version": 2,
                "consume_notes_notes": ["c29tZWJhc2U2NA=="],
                "required_signatures": 2,
            },
            "signatures": [],
        });
        let prev_commitment = "0xprev".to_string();
        let stored_state = StateObject {
            account_id: account_id.clone(),
            state_json: serde_json::json!({}),
            commitment: prev_commitment.clone(),
            nonce: None,
            created_at: "2026-05-25T08:00:00Z".into(),
            updated_at: "2026-05-25T08:00:00Z".into(),
            auth_scheme: String::new(),
        };
        let storage = MockStorageBackend::new()
            .with_pull_state(Ok(stored_state))
            .with_pull_deltas_after(Ok(Vec::new()))
            .with_pull_delta_proposal(Ok(DeltaObject {
                account_id: account_id.clone(),
                nonce: 1,
                prev_commitment: prev_commitment.clone(),
                new_commitment: None,
                delta_payload: proposal_wrapper,
                ack_sig: String::new(),
                ack_pubkey: String::new(),
                ack_scheme: String::new(),
                status: DeltaStatus::Pending {
                    timestamp: "2026-05-25T07:59:00Z".to_string(),
                    proposer_id: "0xproposer".to_string(),
                    cosigner_sigs: vec![],
                },
                metadata: None,
            }))
            .with_submit_state(Ok(()))
            .with_submit_delta(Ok(()));

        let network = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_verify_delta(Ok(()))
            .with_apply_delta(Ok((
                serde_json::json!({"new_state": true}),
                "0xnew_commitment".to_string(),
            )));

        let metadata = MockMetadataStore::new().with_get(Ok(Some(AccountMetadata {
            account_id: account_id.clone(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![signer_commitment],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2026-05-01T00:00:00Z".into(),
            updated_at: "2026-05-01T00:00:00Z".into(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        })));

        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(network.clone()),
            Arc::new(metadata.clone()),
        );

        let params = PushDeltaParams {
            delta: DeltaObject {
                account_id: account_id.clone(),
                nonce: 1,
                prev_commitment: prev_commitment.clone(),
                new_commitment: None,
                delta_payload: candidate_payload,
                ack_sig: String::new(),
                ack_pubkey: String::new(),
                ack_scheme: String::new(),
                status: DeltaStatus::default(),
                metadata: None,
            },
            credentials: Credentials::signature(signer_pubkey, signer_signature, signer_timestamp),
        };

        let result = push_delta(&state, params)
            .await
            .expect("push succeeds with valid inputs");

        let persisted = storage
            .get_submit_delta_calls()
            .into_iter()
            .last()
            .expect("submit_delta was called");
        let lifted = persisted
            .metadata
            .as_ref()
            .expect("metadata persisted on candidate row");
        assert_eq!(lifted.category, DashboardDeltaCategory::NoteConsumption);
        let proposal = lifted
            .proposal
            .as_ref()
            .expect("proposal block lifted from matching delta_proposals row");
        assert_eq!(proposal.proposal_type, "consume_notes");
        assert_eq!(proposal.required_signatures, Some(2));
        assert_eq!(proposal.note_ids.len(), 1);
        assert_eq!(proposal.consume_notes_metadata_version, Some(2));

        assert!(result.delta.metadata.is_some());
        assert_eq!(result.delta.proposal_type(), Some("consume_notes"));
    }

    /// Direct push path: no matching proposal in storage. Candidate is
    /// still persisted with derived metadata; `proposal` absent. This
    /// is the regression guard for "push_delta does not require
    /// push_delta_proposal".
    #[tokio::test]
    async fn direct_push_delta_succeeds_without_existing_proposal() {
        use crate::delta_object::DeltaStatus;
        use crate::delta_summary::DashboardDeltaCategory;
        use crate::state_object::StateObject;
        use crate::testing::helpers::create_test_delta_payload;

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let (signer_pubkey, signer_commitment, signer_signature, signer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let candidate_payload = create_test_delta_payload(&account_id);
        let prev_commitment = "0xprev".to_string();
        let stored_state = StateObject {
            account_id: account_id.clone(),
            state_json: serde_json::json!({}),
            commitment: prev_commitment.clone(),
            nonce: None,
            created_at: "2026-05-25T08:00:00Z".into(),
            updated_at: "2026-05-25T08:00:00Z".into(),
            auth_scheme: String::new(),
        };
        let storage = MockStorageBackend::new()
            .with_pull_state(Ok(stored_state))
            .with_pull_deltas_after(Ok(Vec::new()))
            .with_pull_delta_proposal(Err("no matching proposal".to_string()))
            .with_submit_state(Ok(()))
            .with_submit_delta(Ok(()));

        let network = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_verify_delta(Ok(()))
            .with_apply_delta(Ok((
                serde_json::json!({"new_state": true}),
                "0xnew_commitment".to_string(),
            )));

        let metadata = MockMetadataStore::new().with_get(Ok(Some(AccountMetadata {
            account_id: account_id.clone(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![signer_commitment],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2026-05-01T00:00:00Z".into(),
            updated_at: "2026-05-01T00:00:00Z".into(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        })));

        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(network.clone()),
            Arc::new(metadata.clone()),
        );

        let params = PushDeltaParams {
            delta: DeltaObject {
                account_id: account_id.clone(),
                nonce: 1,
                prev_commitment: prev_commitment.clone(),
                new_commitment: None,
                delta_payload: candidate_payload,
                ack_sig: String::new(),
                ack_pubkey: String::new(),
                ack_scheme: String::new(),
                status: DeltaStatus::default(),
                metadata: None,
            },
            credentials: Credentials::signature(signer_pubkey, signer_signature, signer_timestamp),
        };

        push_delta(&state, params)
            .await
            .expect("push succeeds with valid inputs");

        let persisted = storage
            .get_submit_delta_calls()
            .into_iter()
            .last()
            .expect("submit_delta was called");
        let lifted = persisted
            .metadata
            .as_ref()
            .expect("metadata persisted from on-chain summary alone");
        assert_eq!(
            lifted.category,
            DashboardDeltaCategory::AccountStorageChange
        );
        assert!(
            lifted.proposal.is_none(),
            "no matching proposal → no proposal block"
        );
        assert!(persisted.proposal_type().is_none());
        assert!(
            storage.get_submit_delta_proposal_calls().is_empty(),
            "direct push must not create a delta proposal"
        );
        assert!(
            storage.get_update_delta_proposal_calls().is_empty(),
            "direct push must not update a delta proposal"
        );
        assert!(
            storage.get_delete_delta_proposal_calls().is_empty(),
            "direct push must not delete a delta proposal"
        );
        assert!(
            !storage.get_pull_delta_proposal_calls().is_empty(),
            "direct push may probe for matching proposal metadata, but misses are non-fatal"
        );
    }

    #[tokio::test]
    async fn paused_account_returns_auth_error_for_unauthenticated_caller() {
        let storage = MockStorageBackend::new();
        let network = MockNetworkClient::new();
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(paused_metadata("acc-paused", "0xc1".into()))));

        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(network.clone()),
            Arc::new(metadata.clone()),
        );

        let params = PushDeltaParams {
            delta: DeltaObject {
                account_id: "acc-paused".to_string(),
                ..Default::default()
            },
            credentials: Credentials::signature(String::new(), String::new(), 0),
        };

        let err = push_delta(&state, params)
            .await
            .expect_err("unauthenticated paused account must be rejected with auth error");
        assert!(
            matches!(err, GuardianError::AuthenticationFailed(_)),
            "unauthenticated caller must not learn pause state; got: {err:?}"
        );
    }

    // ------------------------------------------------------------------
    // Issue #17: per-account candidate queue admission.
    // ------------------------------------------------------------------

    fn candidate_mode_state(
        storage: MockStorageBackend,
        network: MockNetworkClient,
        metadata: MockMetadataStore,
        max_pending_candidates: usize,
    ) -> AppState {
        let mut state = create_test_app_state_with_mocks(
            Arc::new(storage),
            Arc::new(network),
            Arc::new(metadata),
        );
        state.canonicalization = Some(
            crate::canonicalization::CanonicalizationConfig::default()
                .with_max_pending_candidates_per_account(max_pending_candidates),
        );
        state
    }

    fn active_metadata(account_id: &str, signer_commitment: &str) -> AccountMetadata {
        AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![signer_commitment.to_string()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2026-05-01T00:00:00Z".into(),
            updated_at: "2026-05-01T00:00:00Z".into(),
            has_pending_candidate: true,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        }
    }

    fn stored_state(account_id: &str, commitment: &str) -> crate::state_object::StateObject {
        crate::state_object::StateObject {
            account_id: account_id.to_string(),
            state_json: serde_json::json!({"step": 0}),
            commitment: commitment.to_string(),
            nonce: None,
            created_at: "2026-05-25T08:00:00Z".into(),
            updated_at: "2026-05-25T08:00:00Z".into(),
            auth_scheme: String::new(),
        }
    }

    fn queued(account_id: &str, nonce: u64, prev: &str, new: &str) -> DeltaObject {
        DeltaObject {
            account_id: account_id.to_string(),
            nonce,
            prev_commitment: prev.to_string(),
            new_commitment: Some(new.to_string()),
            delta_payload: crate::testing::helpers::create_test_delta_payload(account_id),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: crate::delta_object::DeltaStatus::candidate("2026-05-25T08:00:00Z".into()),
            metadata: None,
        }
    }

    fn request(account_id: &str, nonce: u64, prev: &str) -> DeltaObject {
        DeltaObject {
            account_id: account_id.to_string(),
            nonce,
            prev_commitment: prev.to_string(),
            new_commitment: None,
            delta_payload: crate::testing::helpers::create_test_delta_payload(account_id),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: Default::default(),
            metadata: None,
        }
    }

    /// One queued candidate (nonce 1, base → 0xc1) on a canonical state
    /// at 0xbase; the request under test is applied against the mocks.
    async fn push_against_queue(
        max_pending_candidates: usize,
        queue: Vec<DeltaObject>,
        delta: DeltaObject,
    ) -> (
        Result<PushDeltaResult>,
        MockStorageBackend,
        MockNetworkClient,
    ) {
        push_against_queue_with(max_pending_candidates, queue, delta, |network| network).await
    }

    /// [`push_against_queue`] with extra canned network answers, for the
    /// checks that read the replayed tail.
    async fn push_against_queue_with(
        max_pending_candidates: usize,
        queue: Vec<DeltaObject>,
        delta: DeltaObject,
        configure_network: impl FnOnce(MockNetworkClient) -> MockNetworkClient,
    ) -> (
        Result<PushDeltaResult>,
        MockStorageBackend,
        MockNetworkClient,
    ) {
        let account_id = delta.account_id.clone();
        let (signer_pubkey, signer_commitment, signer_signature, signer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);
        // Two canned state reads: the admission path re-reads the state
        // once when the queue does not chain, to tolerate a promotion
        // racing the request.
        let storage = MockStorageBackend::new()
            .with_pull_state(Ok(stored_state(&account_id, "0xbase")))
            .with_pull_state(Ok(stored_state(&account_id, "0xbase")))
            .with_pull_candidate_deltas(Ok(queue.clone()))
            .with_pull_candidate_deltas(Ok(queue))
            .with_pull_delta_proposal(Err("no matching proposal".to_string()))
            .with_submit_delta(Ok(()));
        // Responses pop LIFO: the new delta's application is queued
        // first, the queue replay (candidate 1) last.
        let network = configure_network(
            MockNetworkClient::new()
                .with_validate_credential(Ok(()))
                .with_verify_delta(Ok(()))
                .with_apply_delta(Ok((serde_json::json!({"step": 2}), "0xc2".to_string())))
                .with_apply_delta(Ok((serde_json::json!({"step": 1}), "0xc1".to_string()))),
        );
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(active_metadata(&account_id, &signer_commitment))))
            .with_get(Ok(Some(active_metadata(&account_id, &signer_commitment))));
        let state = candidate_mode_state(
            storage.clone(),
            network.clone(),
            metadata,
            max_pending_candidates,
        );
        let result = push_delta(
            &state,
            PushDeltaParams {
                delta,
                credentials: Credentials::signature(
                    signer_pubkey,
                    signer_signature,
                    signer_timestamp,
                ),
            },
        )
        .await;
        (result, storage, network)
    }

    #[tokio::test]
    async fn chained_delta_extends_the_candidate_queue_tail() {
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let (result, storage, _) = push_against_queue(
            4,
            vec![queued(account_id, 1, "0xbase", "0xc1")],
            request(account_id, 2, "0xc1"),
        )
        .await;
        let result = result.expect("a delta chained from the tail is admitted");
        assert!(result.delta.status.is_candidate());
        assert_eq!(result.delta.prev_commitment, "0xc1");
        assert_eq!(
            result.delta.new_commitment.as_deref(),
            Some("0xc2"),
            "applied on the replayed tail state, not the canonical one"
        );
        let persisted = storage
            .get_submit_delta_calls()
            .pop()
            .expect("candidate persisted");
        assert_eq!(persisted.nonce, 2);
        assert!(persisted.status.is_candidate());
    }

    #[tokio::test]
    async fn competing_delta_is_refused_while_a_candidate_holds_its_base() {
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let (result, storage, network) = push_against_queue(
            4,
            vec![queued(account_id, 1, "0xbase", "0xc1")],
            request(account_id, 2, "0xbase"),
        )
        .await;
        assert!(
            matches!(result, Err(GuardianError::ConflictPendingDelta)),
            "{result:?}"
        );
        assert!(storage.get_submit_delta_calls().is_empty());
        assert_eq!(
            network.apply_delta_responses.lock().unwrap().len(),
            2,
            "refused before any reconstruction"
        );
    }

    #[tokio::test]
    async fn full_queue_refuses_a_correctly_chained_delta() {
        // Depth 1 is the historical single-candidate gate.
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let (result, storage, _) = push_against_queue(
            1,
            vec![queued(account_id, 1, "0xbase", "0xc1")],
            request(account_id, 2, "0xc1"),
        )
        .await;
        assert!(
            matches!(result, Err(GuardianError::ConflictPendingDelta)),
            "{result:?}"
        );
        assert!(storage.get_submit_delta_calls().is_empty());
    }

    #[tokio::test]
    async fn chained_delta_must_extend_the_queue_in_nonce_order() {
        // A nonce at or below the tail's collides with a queued
        // candidate's slot: the same conflict the in-lock gate returns.
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        for nonce in [4, 5] {
            let (result, storage, network) = push_against_queue(
                4,
                vec![queued(account_id, 5, "0xbase", "0xc1")],
                request(account_id, nonce, "0xc1"),
            )
            .await;
            assert!(
                matches!(result, Err(GuardianError::ConflictPendingDelta)),
                "nonce {nonce}: {result:?}"
            );
            assert!(storage.get_submit_delta_calls().is_empty());
            assert_eq!(
                network.apply_delta_responses.lock().unwrap().len(),
                2,
                "refused before any reconstruction"
            );
        }
    }

    fn applied(
        state_json: serde_json::Value,
        commitment: &str,
        nonce: u64,
    ) -> crate::network::AppliedState {
        crate::network::AppliedState {
            state_json,
            commitment: commitment.to_string(),
            nonce: Some(nonce),
        }
    }

    /// Network answers where the delta under test leaves the account at
    /// `nonce`, replacing the canned ones. Answers pop LIFO: the new
    /// delta's application first, the queued candidate's replay last.
    fn producing_nonce(nonce: u64) -> impl FnOnce(MockNetworkClient) -> MockNetworkClient {
        move |network| {
            network.apply_delta_responses.lock().unwrap().clear();
            network
                .with_applied_state(Ok(applied(serde_json::json!({"step": 2}), "0xc2", nonce)))
                .with_apply_delta(Ok((serde_json::json!({"step": 1}), "0xc1".to_string())))
        }
    }

    #[tokio::test]
    async fn chained_delta_labelled_with_another_nonce_is_refused() {
        // A timestamp label behind a queued candidate would sort past
        // every real nonce and become the tail.
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let (result, storage, _) = push_against_queue_with(
            4,
            vec![queued(account_id, 1, "0xbase", "0xc1")],
            request(account_id, 1_791_279_079_000, "0xc1"),
            producing_nonce(2),
        )
        .await;
        assert!(
            matches!(result, Err(GuardianError::ConflictPendingDelta)),
            "{result:?}"
        );
        assert!(storage.get_submit_delta_calls().is_empty());
    }

    #[tokio::test]
    async fn chained_delta_labelled_with_the_nonce_it_produces_is_admitted() {
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let (result, storage, _) = push_against_queue_with(
            4,
            vec![queued(account_id, 1, "0xbase", "0xc1")],
            request(account_id, 2, "0xc1"),
            producing_nonce(2),
        )
        .await;
        result.expect("a correctly labelled chained delta is admitted");
        assert_eq!(storage.get_submit_delta_calls().len(), 1);
    }

    #[tokio::test]
    async fn head_delta_label_is_not_checked_against_the_nonce_it_produces() {
        // Nothing queued: an older client labelling with a timestamp keeps
        // working, as it did before the queue existed.
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let (result, storage, _) = push_against_queue_with(
            4,
            Vec::new(),
            request(account_id, 1_791_279_079_000, "0xbase"),
            |network| {
                network.apply_delta_responses.lock().unwrap().clear();
                network.with_applied_state(Ok(applied(serde_json::json!({"step": 1}), "0xc1", 1)))
            },
        )
        .await;
        result.expect("a head delta is admitted whatever its label");
        assert_eq!(storage.get_submit_delta_calls().len(), 1);
    }

    fn binding(signers: &[&str]) -> crate::network::AuthBinding {
        crate::network::AuthBinding {
            signers: signers.iter().map(|s| s.to_string()).collect(),
            guardian: Some("0xguardian".to_string()),
        }
    }

    /// Nothing chains behind a candidate that changes who may act on the
    /// account: the gate runs after the tail replay and before the delta
    /// itself is applied or stored. The decision matrix lives in
    /// `candidate_chain`; this proves the wiring.
    #[tokio::test]
    async fn delta_behind_a_signer_changing_candidate_is_refused() {
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let (result, storage, network) = push_against_queue_with(
            4,
            vec![queued(account_id, 1, "0xbase", "0xc1")],
            request(account_id, 2, "0xc1"),
            // Bindings pop LIFO: the canonical state is read first.
            |network| {
                network
                    .with_account_auth_binding(Ok(Some(binding(&["0xaa", "0xbb"]))))
                    .with_account_auth_binding(Ok(Some(binding(&["0xaa"]))))
            },
        )
        .await;
        assert!(
            matches!(result, Err(GuardianError::ConflictPendingDelta)),
            "{result:?}"
        );
        assert!(storage.get_submit_delta_calls().is_empty());
        assert_eq!(
            network.apply_delta_responses.lock().unwrap().len(),
            1,
            "refused on the replayed tail, before the delta itself is applied"
        );
    }

    #[tokio::test]
    async fn delta_behind_a_candidate_that_keeps_the_binding_is_admitted() {
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let (result, _, _) = push_against_queue_with(
            4,
            vec![queued(account_id, 1, "0xbase", "0xc1")],
            request(account_id, 2, "0xc1"),
            |network| {
                network
                    .with_account_auth_binding(Ok(Some(binding(&["0xaa"]))))
                    .with_account_auth_binding(Ok(Some(binding(&["0xaa"]))))
            },
        )
        .await;
        let result = result.expect("an unchanged binding does not block the queue");
        assert_eq!(result.delta.new_commitment.as_deref(), Some("0xc2"));
    }

    #[tokio::test]
    async fn full_queue_is_a_conflict_whatever_the_base() {
        // Depth 1 with a candidate in flight behaves as before the queue
        // existed: the pending check came first, so an unknown base was a
        // conflict, never a commitment mismatch.
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        for prev in ["0xzzz", "0xbase", "0xc1"] {
            let (result, storage, _) = push_against_queue(
                1,
                vec![queued(account_id, 1, "0xbase", "0xc1")],
                request(account_id, 2, prev),
            )
            .await;
            assert!(
                matches!(result, Err(GuardianError::ConflictPendingDelta)),
                "{prev}: {result:?}"
            );
            assert!(storage.get_submit_delta_calls().is_empty());
        }
    }

    #[tokio::test]
    async fn delta_on_the_tail_admitted_after_the_last_candidate_promotes_mid_request() {
        // The request read the state at 0xbase; the worker then promoted
        // the only queued candidate (state → 0xc1) before the queue read,
        // which came back empty. The delta builds on 0xc1, the real tail:
        // it must be admitted against the fresh state, not refused as
        // unrelated with the stale 0xbase as the commitment to resync to.
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let (signer_pubkey, signer_commitment, signer_signature, signer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(account_id);
        let mut promoted = stored_state(account_id, "0xc1");
        promoted.state_json = serde_json::json!({"step": 1});
        // Reads pop LIFO: the request's first read sees 0xbase; the
        // commitment re-read peeks at, and the full re-read takes, 0xc1.
        let storage = MockStorageBackend::new()
            .with_pull_state(Ok(promoted))
            .with_pull_state(Ok(stored_state(account_id, "0xbase")))
            .with_pull_candidate_deltas(Ok(vec![]))
            .with_pull_candidate_deltas(Ok(vec![]))
            .with_pull_delta_proposal(Err("no matching proposal".to_string()))
            .with_submit_delta(Ok(()));
        let network = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_verify_delta(Ok(()))
            .with_apply_delta(Ok((serde_json::json!({"step": 2}), "0xc2".to_string())));
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(active_metadata(account_id, &signer_commitment))))
            .with_get(Ok(Some(active_metadata(account_id, &signer_commitment))));
        let state = candidate_mode_state(storage.clone(), network, metadata, 4);
        let result = push_delta(
            &state,
            PushDeltaParams {
                delta: request(account_id, 2, "0xc1"),
                credentials: Credentials::signature(
                    signer_pubkey,
                    signer_signature,
                    signer_timestamp,
                ),
            },
        )
        .await
        .expect("the delta on the promoted tail is admitted");
        assert_eq!(result.delta.prev_commitment, "0xc1");
        assert_eq!(result.delta.new_commitment.as_deref(), Some("0xc2"));
        assert_eq!(storage.get_submit_delta_calls().len(), 1);
    }

    #[tokio::test]
    async fn unknown_base_is_a_commitment_mismatch_against_the_canonical_state() {
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let (result, storage, _) = push_against_queue(
            4,
            vec![queued(account_id, 1, "0xbase", "0xc1")],
            request(account_id, 2, "0xzzz"),
        )
        .await;
        assert!(
            matches!(
                result,
                Err(GuardianError::CommitmentMismatch { ref expected, ref actual })
                    if expected == "0xbase" && actual == "0xzzz"
            ),
            "{result:?}"
        );
        assert!(storage.get_submit_delta_calls().is_empty());
    }

    #[tokio::test]
    async fn broken_queue_refuses_admission_until_the_worker_sweeps_it() {
        // The queued candidate does not chain from the canonical state
        // (its predecessor was parked): nothing can be admitted, not
        // even on the canonical base, until the orphan is swept.
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let (result, storage, _) = push_against_queue(
            4,
            vec![queued(account_id, 2, "0xgone", "0xc2")],
            request(account_id, 3, "0xbase"),
        )
        .await;
        assert!(
            matches!(result, Err(GuardianError::ConflictPendingDelta)),
            "{result:?}"
        );
        assert!(storage.get_submit_delta_calls().is_empty());
    }

    #[tokio::test]
    async fn empty_queue_admits_the_canonical_base_only() {
        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let (result, _, _) = push_against_queue(4, vec![], request(account_id, 1, "0xc1")).await;
        assert!(
            matches!(
                result,
                Err(GuardianError::CommitmentMismatch { ref expected, .. }) if expected == "0xbase"
            ),
            "{result:?}"
        );
    }

    fn word_from_hex(value: &str) -> miden_protocol::Word {
        use miden_protocol::utils::serde::Deserializable;
        let bytes = hex::decode(value.trim_start_matches("0x")).expect("commitment hex");
        miden_protocol::Word::read_from_bytes(&bytes).expect("commitment word")
    }

    fn multisig_state(
        approver_commitments: &[String],
        threshold: u32,
        procedure_overrides: &[(miden_protocol::Word, u32)],
    ) -> serde_json::Value {
        use guardian_shared::ToJson;
        use miden_protocol::account::{
            Account, AccountCode, AccountId, AccountIdVersion, AccountStorage, AccountType,
            StorageMap, StorageMapKey, StorageSlot, StorageSlotName,
        };
        use miden_protocol::asset::AssetVault;
        use miden_standards::account::auth::AuthGuardedMultisig;

        fn slot_name(name: &str) -> StorageSlotName {
            StorageSlotName::new(name).expect("slot name")
        }

        let approvers: Vec<miden_protocol::Word> = approver_commitments
            .iter()
            .map(|commitment| word_from_hex(commitment))
            .collect();
        let signer_entries = approvers.iter().enumerate().map(|(index, pubkey)| {
            (
                StorageMapKey::new(miden_protocol::Word::from([index as u32, 0, 0, 0])),
                *pubkey,
            )
        });
        let mut slots = vec![
            StorageSlot::with_value(
                slot_name(AuthGuardedMultisig::threshold_config_slot().as_str()),
                miden_protocol::Word::from([threshold, approvers.len() as u32, 0, 0]),
            ),
            StorageSlot::with_map(
                slot_name(AuthGuardedMultisig::approver_public_keys_slot().as_str()),
                StorageMap::with_entries(signer_entries).expect("signer map"),
            ),
        ];
        if !procedure_overrides.is_empty() {
            let entries = procedure_overrides.iter().map(|(root, threshold)| {
                (
                    StorageMapKey::new(*root),
                    miden_protocol::Word::from([*threshold, 0, 0, 0]),
                )
            });
            slots.push(StorageSlot::with_map(
                slot_name(AuthGuardedMultisig::procedure_thresholds_slot().as_str()),
                StorageMap::with_entries(entries).expect("procedure thresholds"),
            ));
        }
        let storage = AccountStorage::new(slots).expect("storage");
        let account_id = AccountId::dummy(
            [3u8; 15],
            AccountIdVersion::Version1,
            AccountType::Private,
            miden_protocol::account::AssetCallbackFlag::Disabled,
        );
        let account = Account::new_existing(
            account_id,
            AssetVault::new(&[]).expect("vault"),
            storage,
            AccountCode::mock(),
            miden_protocol::Felt::new_unchecked(1),
        );
        account.to_json()
    }

    fn falcon_approval(
        signer: &crate::testing::helpers::TestSigner,
        summary: &miden_protocol::transaction::TransactionSummary,
    ) -> crate::delta_object::CosignerSignature {
        use crate::delta_object::ProposalSignature;
        crate::delta_object::CosignerSignature {
            signature: ProposalSignature::Falcon {
                signature: signer.sign_word(summary.to_commitment()),
            },
            timestamp: "2026-05-25T07:59:00Z".into(),
            signer_id: signer.commitment_hex.clone(),
        }
    }

    fn ecdsa_approval(
        signer: &crate::testing::helpers::TestEcdsaSigner,
        summary: &miden_protocol::transaction::TransactionSummary,
    ) -> crate::delta_object::CosignerSignature {
        use crate::delta_object::ProposalSignature;
        use guardian_shared::EcdsaMessageFormat;
        crate::delta_object::CosignerSignature {
            signature: ProposalSignature::Ecdsa {
                signature: signer.sign_word(summary.to_commitment()),
                public_key: Some(signer.pubkey_hex.clone()),
                message_format: EcdsaMessageFormat::Raw,
            },
            timestamp: "2026-05-25T07:59:00Z".into(),
            signer_id: signer.commitment_hex.clone(),
        }
    }

    enum ProposalLookup {
        Missing,
        Signatures(Vec<crate::delta_object::CosignerSignature>),
        NonPending,
        OtherSummary {
            tx_summary: serde_json::Value,
            signatures: Vec<crate::delta_object::CosignerSignature>,
        },
        Error(String),
    }

    fn pending_status(
        proposer_id: &str,
        cosigner_sigs: Vec<crate::delta_object::CosignerSignature>,
    ) -> crate::delta_object::DeltaStatus {
        crate::delta_object::DeltaStatus::Pending {
            timestamp: "2026-05-25T07:59:00Z".into(),
            proposer_id: proposer_id.to_string(),
            cosigner_sigs,
        }
    }

    fn stored_proposal(
        storage: MockStorageBackend,
        account_id: &str,
        prev_commitment: &str,
        tx_summary: &serde_json::Value,
        proposal_type: &str,
        status: crate::delta_object::DeltaStatus,
    ) -> MockStorageBackend {
        storage.with_pull_delta_proposal(Ok(DeltaObject {
            account_id: account_id.to_string(),
            nonce: 1,
            prev_commitment: prev_commitment.to_string(),
            new_commitment: None,
            delta_payload: serde_json::json!({
                "tx_summary": tx_summary,
                "metadata": { "proposal_type": proposal_type },
                "signatures": [],
            }),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status,
            metadata: None,
        }))
    }

    async fn push_on_multisig(
        approvers: &[String],
        threshold: u32,
        procedure_overrides: &[(miden_protocol::Word, u32)],
        proposal_type: &str,
        lookup: ProposalLookup,
    ) -> (Result<PushDeltaResult>, MockStorageBackend) {
        let tx_summary =
            crate::testing::helpers::create_test_delta_payload("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b");
        push_payload_on_multisig(
            tx_summary,
            approvers,
            threshold,
            procedure_overrides,
            proposal_type,
            lookup,
        )
        .await
    }

    async fn push_payload_on_multisig(
        tx_summary: serde_json::Value,
        approvers: &[String],
        threshold: u32,
        procedure_overrides: &[(miden_protocol::Word, u32)],
        proposal_type: &str,
        lookup: ProposalLookup,
    ) -> (Result<PushDeltaResult>, MockStorageBackend) {
        use crate::delta_object::DeltaStatus;
        use crate::state_object::StateObject;
        use crate::testing::helpers::TestSigner;

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".to_string();
        let caller = TestSigner::new();
        let (signature, timestamp) = caller.sign(&account_id);
        let prev_commitment = "0xprev".to_string();
        let storage = MockStorageBackend::new().with_pull_state(Ok(StateObject {
            account_id: account_id.clone(),
            state_json: multisig_state(approvers, threshold, procedure_overrides),
            commitment: prev_commitment.clone(),
            nonce: None,
            created_at: "2026-05-25T08:00:00Z".into(),
            updated_at: "2026-05-25T08:00:00Z".into(),
            auth_scheme: String::new(),
        }));
        let storage = match lookup {
            ProposalLookup::Missing => storage,
            ProposalLookup::Error(error) => storage.with_pull_delta_proposal(Err(error)),
            ProposalLookup::Signatures(cosigner_sigs) => stored_proposal(
                storage,
                &account_id,
                &prev_commitment,
                &tx_summary,
                proposal_type,
                pending_status(&caller.commitment_hex, cosigner_sigs),
            ),
            ProposalLookup::NonPending => stored_proposal(
                storage,
                &account_id,
                &prev_commitment,
                &tx_summary,
                proposal_type,
                DeltaStatus::Candidate {
                    timestamp: "2026-05-25T07:59:00Z".into(),
                    retry_count: 0,
                    divergence_count: 0,
                    abandon_requested_at: None,
                    abandon_confirm_count: 0,
                },
            ),
            ProposalLookup::OtherSummary {
                tx_summary: proposal_summary,
                signatures,
            } => stored_proposal(
                storage,
                &account_id,
                &prev_commitment,
                &proposal_summary,
                proposal_type,
                pending_status(&caller.commitment_hex, signatures),
            ),
        };
        let storage = storage
            .with_pull_deltas_after(Ok(Vec::new()))
            .with_submit_state(Ok(()))
            .with_submit_delta(Ok(()));
        let network = MockNetworkClient::new()
            .with_validate_credential(Ok(()))
            .with_verify_delta(Ok(()))
            .with_apply_delta(Ok((
                serde_json::json!({"new_state": true}),
                "0xnew_commitment".to_string(),
            )));
        let metadata = MockMetadataStore::new().with_get(Ok(Some(AccountMetadata {
            account_id: account_id.clone(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![caller.commitment_hex.clone()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2026-05-01T00:00:00Z".into(),
            updated_at: "2026-05-01T00:00:00Z".into(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        })));
        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(network),
            Arc::new(metadata),
        );
        let result = push_delta(
            &state,
            PushDeltaParams {
                delta: DeltaObject {
                    account_id,
                    nonce: 1,
                    prev_commitment,
                    new_commitment: None,
                    delta_payload: tx_summary,
                    ack_sig: String::new(),
                    ack_pubkey: String::new(),
                    ack_scheme: String::new(),
                    status: DeltaStatus::default(),
                    metadata: None,
                },
                credentials: Credentials::signature(caller.pubkey_hex, signature, timestamp),
            },
        )
        .await;
        (result, storage)
    }

    fn assert_not_committed(storage: &MockStorageBackend) {
        assert!(
            storage.get_submit_delta_calls().is_empty(),
            "a refused multisig push must not be stored"
        );
        assert!(
            storage.get_submit_state_calls().is_empty(),
            "a refused multisig push must not move account state"
        );
    }

    #[tokio::test]
    async fn multisig_push_without_a_proposal_is_rejected() {
        let approver = crate::testing::helpers::TestSigner::new();
        let (result, storage) = push_on_multisig(
            &[approver.commitment_hex],
            1,
            &[],
            "p2id",
            ProposalLookup::Missing,
        )
        .await;
        assert!(
            matches!(
                result,
                Err(GuardianError::InsufficientSignatures {
                    required: 1,
                    got: 0
                })
            ),
            "{result:?}"
        );
        assert_not_committed(&storage);
    }

    #[tokio::test]
    async fn multisig_push_below_the_threshold_is_rejected() {
        use crate::testing::helpers::TestSigner;
        use guardian_shared::FromJson;

        let first = TestSigner::new();
        let second = TestSigner::new();
        let summary = miden_protocol::transaction::TransactionSummary::from_json(
            &crate::testing::helpers::create_test_delta_payload("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b"),
        )
        .expect("summary");
        let (result, storage) = push_on_multisig(
            &[first.commitment_hex.clone(), second.commitment_hex.clone()],
            2,
            &[],
            "p2id",
            ProposalLookup::Signatures(vec![falcon_approval(&first, &summary)]),
        )
        .await;
        assert!(
            matches!(
                result,
                Err(GuardianError::InsufficientSignatures {
                    required: 2,
                    got: 1
                })
            ),
            "{result:?}"
        );
        assert_not_committed(&storage);
    }

    #[tokio::test]
    async fn multisig_push_at_the_falcon_threshold_is_acknowledged() {
        use crate::testing::helpers::TestSigner;
        use guardian_shared::FromJson;

        let first = TestSigner::new();
        let second = TestSigner::new();
        let summary = miden_protocol::transaction::TransactionSummary::from_json(
            &crate::testing::helpers::create_test_delta_payload("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b"),
        )
        .expect("summary");
        let (result, storage) = push_on_multisig(
            &[first.commitment_hex.clone(), second.commitment_hex.clone()],
            2,
            &[],
            "p2id",
            ProposalLookup::Signatures(vec![
                falcon_approval(&first, &summary),
                falcon_approval(&second, &summary),
            ]),
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(storage.get_submit_delta_calls().len(), 1);
    }

    #[tokio::test]
    async fn multisig_push_counts_a_verified_ecdsa_approver() {
        use crate::testing::helpers::TestEcdsaSigner;
        use guardian_shared::FromJson;

        let approver = TestEcdsaSigner::new();
        let summary = miden_protocol::transaction::TransactionSummary::from_json(
            &crate::testing::helpers::create_test_delta_payload("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b"),
        )
        .expect("summary");
        let (result, storage) = push_on_multisig(
            std::slice::from_ref(&approver.commitment_hex),
            1,
            &[],
            "p2id",
            ProposalLookup::Signatures(vec![ecdsa_approval(&approver, &summary)]),
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(storage.get_submit_delta_calls().len(), 1);
    }

    #[tokio::test]
    async fn multisig_push_does_not_count_a_non_approver() {
        use crate::testing::helpers::TestSigner;
        use guardian_shared::FromJson;

        let approver = TestSigner::new();
        let outsider = TestSigner::new();
        let summary = miden_protocol::transaction::TransactionSummary::from_json(
            &crate::testing::helpers::create_test_delta_payload("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b"),
        )
        .expect("summary");
        let (result, storage) = push_on_multisig(
            &[approver.commitment_hex],
            1,
            &[],
            "p2id",
            ProposalLookup::Signatures(vec![falcon_approval(&outsider, &summary)]),
        )
        .await;
        assert!(
            matches!(
                result,
                Err(GuardianError::InsufficientSignatures {
                    required: 1,
                    got: 0
                })
            ),
            "{result:?}"
        );
        assert_not_committed(&storage);
    }

    #[tokio::test]
    async fn multisig_push_fails_closed_when_the_proposal_lookup_fails() {
        let approver = crate::testing::helpers::TestSigner::new();
        let (result, storage) = push_on_multisig(
            &[approver.commitment_hex],
            1,
            &[],
            "p2id",
            ProposalLookup::Error("database unavailable".to_string()),
        )
        .await;
        assert!(
            matches!(result, Err(GuardianError::StorageError(_))),
            "{result:?}"
        );
        assert_not_committed(&storage);
    }

    #[tokio::test]
    async fn multisig_push_uses_the_procedure_threshold_override() {
        use crate::testing::helpers::TestSigner;
        use guardian_shared::FromJson;
        use miden_standards::account::wallets::BasicWallet;

        let approver = TestSigner::new();
        let summary = miden_protocol::transaction::TransactionSummary::from_json(
            &crate::testing::helpers::create_test_delta_payload("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b"),
        )
        .expect("summary");
        let receive = BasicWallet::receive_asset_root().into();
        let (admitted, _) = push_on_multisig(
            std::slice::from_ref(&approver.commitment_hex),
            2,
            &[(receive, 1)],
            "consume_notes",
            ProposalLookup::Signatures(vec![falcon_approval(&approver, &summary)]),
        )
        .await;
        assert!(admitted.is_ok(), "{admitted:?}");

        let (refused, storage) = push_on_multisig(
            std::slice::from_ref(&approver.commitment_hex),
            2,
            &[(receive, 1)],
            "p2id",
            ProposalLookup::Signatures(vec![falcon_approval(&approver, &summary)]),
        )
        .await;
        assert!(
            matches!(
                refused,
                Err(GuardianError::InsufficientSignatures {
                    required: 2,
                    got: 1
                })
            ),
            "{refused:?}"
        );
        assert_not_committed(&storage);
    }

    #[tokio::test]
    async fn multisig_push_rejects_a_proposal_for_a_different_summary() {
        use crate::testing::helpers::TestSigner;
        use guardian_shared::{FromJson, ToJson};
        use miden_protocol::account::{
            AccountCodePatch, AccountDelta, AccountId, AccountVaultDelta,
        };
        use miden_protocol::transaction::{
            InputNotes, RawOutputNotes, TransactionSummary, TransactionSummaryUserParams,
        };
        use miden_protocol::{Felt, Word, ZERO};

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let approver = TestSigner::new();
        let delta = AccountDelta::new(
            AccountId::from_hex(account_id).expect("account id"),
            miden_protocol::account::AccountStoragePatch::default(),
            AccountVaultDelta::default(),
            AccountCodePatch::default(),
            Felt::ZERO,
        )
        .expect("delta");
        let other = TransactionSummary::new(
            delta,
            InputNotes::new(Vec::new()).unwrap(),
            RawOutputNotes::new(Vec::new()).unwrap(),
            miden_protocol::block::BlockNumber::from(1),
            Word::from([ZERO; 4]),
            0,
            TransactionSummaryUserParams::new([ZERO; 6]),
        );
        let (result, storage) = push_on_multisig(
            std::slice::from_ref(&approver.commitment_hex),
            1,
            &[],
            "p2id",
            ProposalLookup::OtherSummary {
                tx_summary: other.to_json(),
                signatures: vec![falcon_approval(&approver, &other)],
            },
        )
        .await;
        let pushed = miden_protocol::transaction::TransactionSummary::from_json(
            &crate::testing::helpers::create_test_delta_payload(account_id),
        )
        .expect("pushed summary");
        assert_ne!(other.to_commitment(), pushed.to_commitment());
        assert!(
            matches!(
                result,
                Err(GuardianError::InsufficientSignatures {
                    required: 1,
                    got: 0
                })
            ),
            "{result:?}"
        );
        assert_not_committed(&storage);
    }

    fn update_signers_shaped_summary(account_id: &str) -> serde_json::Value {
        use guardian_shared::ToJson;
        use miden_protocol::account::{
            AccountCodePatch, AccountDelta, AccountId, AccountStoragePatch, AccountVaultDelta,
            StorageSlotPatch, StorageValuePatch,
        };
        use miden_protocol::transaction::{
            InputNotes, RawOutputNotes, TransactionSummary, TransactionSummaryUserParams,
        };
        use miden_protocol::{Felt, Word, ZERO};
        use miden_standards::account::auth::AuthGuardedMultisig;

        let storage_patch = AccountStoragePatch::from_entries([(
            AuthGuardedMultisig::threshold_config_slot().clone(),
            StorageSlotPatch::Value(StorageValuePatch::Update {
                value: Word::from([2u32, 2, 0, 0]),
            }),
        )])
        .expect("storage patch");
        let delta = AccountDelta::new(
            AccountId::from_hex(account_id).expect("account id"),
            storage_patch,
            AccountVaultDelta::default(),
            AccountCodePatch::default(),
            Felt::new_unchecked(1),
        )
        .expect("delta");
        TransactionSummary::new(
            delta,
            InputNotes::new(Vec::new()).unwrap(),
            RawOutputNotes::new(Vec::new()).unwrap(),
            miden_protocol::block::BlockNumber::from(0),
            Word::from([ZERO; 4]),
            0,
            TransactionSummaryUserParams::new([ZERO; 6]),
        )
        .to_json()
    }

    /// A proposal's claimed type cannot buy a lower threshold than the
    /// procedures its summary shows were invoked: a signer-set change
    /// labelled `p2id` is held to the update-signers threshold, not the
    /// send override.
    #[tokio::test]
    async fn multisig_push_ignores_a_cheaper_mislabelled_proposal_type() {
        use crate::testing::helpers::TestSigner;
        use guardian_shared::FromJson;
        use miden_standards::account::wallets::BasicWallet;

        let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
        let approver = TestSigner::new();
        let payload = update_signers_shaped_summary(account_id);
        let summary =
            miden_protocol::transaction::TransactionSummary::from_json(&payload).expect("summary");
        let send = BasicWallet::move_asset_to_note_root().into();
        let (result, storage) = push_payload_on_multisig(
            payload,
            std::slice::from_ref(&approver.commitment_hex),
            2,
            &[(send, 1)],
            "p2id",
            ProposalLookup::Signatures(vec![falcon_approval(&approver, &summary)]),
        )
        .await;
        assert!(
            matches!(
                result,
                Err(GuardianError::InsufficientSignatures {
                    required: 2,
                    got: 1
                })
            ),
            "{result:?}"
        );
        assert_not_committed(&storage);
    }

    #[tokio::test]
    async fn multisig_push_counts_no_signatures_on_a_non_pending_proposal() {
        let approver = crate::testing::helpers::TestSigner::new();
        let (result, storage) = push_on_multisig(
            std::slice::from_ref(&approver.commitment_hex),
            1,
            &[],
            "p2id",
            ProposalLookup::NonPending,
        )
        .await;
        assert!(
            matches!(
                result,
                Err(GuardianError::InsufficientSignatures {
                    required: 1,
                    got: 0
                })
            ),
            "{result:?}"
        );
        assert_not_committed(&storage);
    }

    #[tokio::test]
    async fn multisig_push_rejects_a_zero_threshold() {
        use crate::testing::helpers::TestSigner;
        use guardian_shared::FromJson;

        let approver = TestSigner::new();
        let summary = miden_protocol::transaction::TransactionSummary::from_json(
            &crate::testing::helpers::create_test_delta_payload("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b"),
        )
        .expect("summary");
        let (result, storage) = push_on_multisig(
            std::slice::from_ref(&approver.commitment_hex),
            0,
            &[],
            "p2id",
            ProposalLookup::Signatures(vec![falcon_approval(&approver, &summary)]),
        )
        .await;
        assert!(
            matches!(result, Err(GuardianError::InvalidDelta(_))),
            "{result:?}"
        );
        assert_not_committed(&storage);
    }
}
