use guardian_shared::SignatureScheme;
use serde_json::Value;

use crate::delta_object::DeltaObject;
use crate::error::{GuardianError, Result};
use crate::metadata::auth::Credentials;
use crate::services::account_status::ensure_account_active_metadata;
use crate::services::delta_commit::{CommitContext, DeltaCommitStrategy};
use crate::services::resolve_account;
use crate::state::AppState;

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
    skip(state, params),
    fields(account_id = %params.delta.account_id)
)]
pub async fn push_delta(state: &AppState, params: PushDeltaParams) -> Result<PushDeltaResult> {
    tracing::info!(account_id = %params.delta.account_id, "Pushing delta");

    let resolved = resolve_account(state, &params.delta.account_id, &params.credentials).await?;
    ensure_account_active_metadata(&resolved.metadata)?;
    if resolved.metadata.network_config.is_evm() {
        return Err(GuardianError::UnsupportedForNetwork {
            network: "evm".to_string(),
            operation: "push_delta".to_string(),
        });
    }

    let current_state = resolved
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

    // Check for pending candidates before accepting new delta
    let has_pending = resolved
        .storage
        .has_pending_candidate(&params.delta.account_id)
        .await
        .map_err(|e| {
            tracing::error!(
                account_id = %params.delta.account_id,
                error = %e,
                "Failed to check deltas in push_delta"
            );
            GuardianError::StorageError(format!("Failed to check deltas: {e}"))
        })?;

    if has_pending {
        return Err(GuardianError::ConflictPendingDelta);
    }

    if params.delta.prev_commitment != current_state.commitment {
        return Err(GuardianError::CommitmentMismatch {
            expected: current_state.commitment.clone(),
            actual: params.delta.prev_commitment.clone(),
        });
    }

    let (new_state_json, new_commitment) = {
        let client = state.network_client.lock().await;
        client
            .verify_delta(
                &current_state.commitment,
                &current_state.state_json,
                &params.delta.delta_payload,
            )
            .map_err(GuardianError::InvalidDelta)?;
        client
            .apply_delta(&current_state.state_json, &params.delta.delta_payload)
            .map_err(GuardianError::InvalidDelta)?
    };

    // Look up the matching proposal in `delta_proposals` (if any) so
    // `build_metadata` can lift its operator-stated intent into the
    // persisted `metadata.proposal` block. The TS multisig client
    // calls `pushDelta` with only the unwrapped `tx_summary`, so this
    // is the one place metadata can be recovered from the
    // pre-execution proposal storage. Single-key pushes have no
    // matching proposal — `Ok(None)` is the common path.
    let matching_proposal_payload = lookup_matching_proposal_payload(
        state,
        &params.delta.account_id,
        params.delta.nonce,
        &params.delta.delta_payload,
    )
    .await;

    let derived_metadata = crate::delta_summary::build_metadata(
        &params.delta.delta_payload,
        matching_proposal_payload.as_ref(),
    );

    let mut result_delta = params.delta.clone();
    result_delta.new_commitment = Some(new_commitment.clone());
    result_delta.metadata = derived_metadata;
    let scheme = resolved.metadata.auth.scheme();
    result_delta = state.ack.ack_delta(result_delta, &scheme)?;
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
            new_state_json,
            &new_commitment,
        )
        .await?;

    Ok(PushDeltaResult {
        delta: result_delta,
    })
}

/// Look up the matching `delta_proposals` row's `delta_payload` (the
/// wrapper that carries `metadata`) for the given delta about to be
/// pushed. Returns `None` if no matching proposal exists — the common
/// case for single-key push.
///
/// Failures at any layer are non-fatal — the push proceeds with the
/// `proposal` block absent — but they are logged so silent metadata
/// loss is detectable in production:
///
///   - `delta_proposal_id` errors: the persisted `delta_payload` is
///     not a decodable `TransactionSummary` (EVM, malformed). Logged
///     at `debug`; this is the expected path for non-Miden payloads.
///   - `pull_delta_proposal` "not found": no proposal row matched —
///     the expected path for single-key `push_delta`. Logged at
///     `debug`.
///   - `pull_delta_proposal` other errors: a real storage failure
///     (connection lost, query error). Logged at `warn` with the
///     full error string so on-call operators can detect "we kept
///     accepting deltas but lost the operator-stated intent block."
async fn lookup_matching_proposal_payload(
    state: &AppState,
    account_id: &str,
    nonce: u64,
    delta_payload: &Value,
) -> Option<Value> {
    let proposal_id = {
        let client = state.network_client.lock().await;
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
                return None;
            }
        }
    };
    match state
        .storage
        .pull_delta_proposal(account_id, &proposal_id)
        .await
    {
        Ok(proposal) => Some(proposal.delta_payload),
        Err(err) => {
            // Best-effort "not found" detection. Both backends format
            // the underlying error into a String, so we sniff for the
            // common shapes Diesel and `tokio::fs` produce. False
            // positives (a real error containing "not found") would
            // demote to debug, which is acceptable — the alternative
            // is silently swallowing the failure.
            let lower = err.to_lowercase();
            let looks_like_not_found = lower.contains("not found")
                || lower.contains("notfound")
                || lower.contains("no such file");
            if looks_like_not_found {
                tracing::debug!(
                    account_id = %account_id,
                    nonce,
                    proposal_id = %proposal_id,
                    "no matching delta_proposal row (single-key push or unrelated payload)"
                );
            } else {
                tracing::warn!(
                    account_id = %account_id,
                    nonce,
                    proposal_id = %proposal_id,
                    error = %err,
                    "delta_proposals lookup errored during push_delta metadata derivation; \
                     persisting metadata without proposal block (operator-stated intent lost \
                     until storage recovers — investigate storage backend)"
                );
            }
            None
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
    use tokio::sync::Mutex;

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
            last_auth_timestamp: None,
            paused_at: Some(
                chrono::Utc
                    .with_ymd_and_hms(2026, 5, 19, 14, 30, 0)
                    .unwrap(),
            ),
            paused_reason: Some("compliance".to_string()),
        }
    }

    /// Pause-gate guard: `push_delta` MUST reject before touching
    /// storage or the network — but only AFTER authentication
    /// succeeds, so unauthenticated probes cannot leak pause state.
    #[tokio::test]
    async fn paused_account_rejected_before_side_effects() {
        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let (signer_pubkey, signer_commitment, signer_signature, signer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let storage = MockStorageBackend::new();
        let network = MockNetworkClient::new();
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(paused_metadata(&account_id, signer_commitment))));

        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(Mutex::new(network.clone())),
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

    /// Pause-gate ordering: unauthenticated callers MUST get an auth
    /// error, NOT `AccountPaused`. Defends against reintroducing the
    /// pre-auth chokepoint that leaked pause state to probes.
    /// End-to-end check that the push-time metadata pipeline:
    ///   1. Decodes the candidate `TransactionSummary` (from the
    ///      already-running `verify_delta` / `apply_delta` path).
    ///   2. Looks up the matching proposal in `delta_proposals`.
    ///   3. Lifts `proposal.proposal_type` into the typed
    ///      `DeltaMetadata` blob and persists it on the candidate row.
    ///   4. Dashboard projection then surfaces it unchanged.
    ///
    /// Highest-value test for feature 007's main pivot — locks in the
    /// "metadata derived at push time, not at canonicalization or
    /// listing" architecture.
    #[tokio::test]
    async fn push_delta_persists_metadata_with_proposal_from_matching_proposal_lookup() {
        use crate::delta_object::DeltaStatus;
        use crate::delta_summary::DashboardDeltaCategory;
        use crate::state_object::StateObject;
        use crate::testing::helpers::create_test_delta_payload;

        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let (signer_pubkey, signer_commitment, signer_signature, signer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        // The candidate delta carries the unwrapped TransactionSummary
        // shape that the TS multisig client forwards via pushDelta.
        let candidate_payload = create_test_delta_payload(&account_id);

        // Storage returns: an active state on `pull_state`, no pending
        // candidates on `pull_deltas_after`, and a matching proposal
        // (wrapper shape with metadata) on `pull_delta_proposal`. The
        // mock's `delta_proposal_id` returns a stable id, so the
        // proposal lookup hits this seeded row.
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
            last_auth_timestamp: None,
            paused_at: None,
            paused_reason: None,
        })));

        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(Mutex::new(network.clone())),
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

        // The persisted candidate must carry the typed metadata blob
        // with proposal block lifted from the matching proposal.
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

        // The returned result also carries metadata (handy for callers
        // that surface the new candidate in the response body).
        assert!(result.delta.metadata.is_some());

        // Proposal-type accessor still works (regression guard for
        // the fallback path).
        assert_eq!(result.delta.proposal_type(), Some("consume_notes"));
    }

    /// When no matching proposal exists in storage (single-key
    /// `push_delta` path), the candidate is still persisted with a
    /// typed `metadata` blob — derived from the `TransactionSummary`
    /// topology — and the `proposal` block is absent.
    #[tokio::test]
    async fn push_delta_persists_metadata_without_proposal_when_lookup_misses() {
        use crate::delta_object::DeltaStatus;
        use crate::delta_summary::DashboardDeltaCategory;
        use crate::state_object::StateObject;
        use crate::testing::helpers::create_test_delta_payload;

        let account_id = "0x7bfb0f38b0fafa103f86a805594170".to_string();
        let (signer_pubkey, signer_commitment, signer_signature, signer_timestamp) =
            crate::testing::helpers::generate_falcon_signature(&account_id);

        let candidate_payload = create_test_delta_payload(&account_id);
        let prev_commitment = "0xprev".to_string();
        let stored_state = StateObject {
            account_id: account_id.clone(),
            state_json: serde_json::json!({}),
            commitment: prev_commitment.clone(),
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
            last_auth_timestamp: None,
            paused_at: None,
            paused_reason: None,
        })));

        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(Mutex::new(network.clone())),
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
        // Empty test summary has no notes → falls through to
        // account_storage_change via topology inference.
        assert_eq!(
            lifted.category,
            DashboardDeltaCategory::AccountStorageChange
        );
        assert!(
            lifted.proposal.is_none(),
            "no matching proposal → no proposal block"
        );
        assert!(persisted.proposal_type().is_none());
    }

    #[tokio::test]
    async fn paused_account_returns_auth_error_for_unauthenticated_caller() {
        let storage = MockStorageBackend::new();
        let network = MockNetworkClient::new();
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(paused_metadata("acc-paused", "0xc1".into()))));

        let state = create_test_app_state_with_mocks(
            Arc::new(storage.clone()),
            Arc::new(Mutex::new(network.clone())),
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
}
