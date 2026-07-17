use crate::delta_object::DeltaObject;
use crate::error::{GuardianError, Result};
use crate::state::AppState;

/// How strictly [`remove_candidate`] treats failures in its cleanup tail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemovalMode {
    /// Worker discard loop: proposal-id derivation, proposal deletion, and
    /// flag-clear failures are logged and swallowed so one account cannot
    /// wedge the whole canonicalization sweep.
    BestEffort,
    /// Client-initiated abandon (issue #319): a success response must mean
    /// the account is actually released, so every step is a hard error.
    Strict,
}

/// Record one candidate-processing outcome.
pub(crate) fn record_candidate_outcome(outcome: crate::metrics::labels::CandidateOutcome) {
    metrics::counter!(
        crate::metrics::names::CANONICALIZATION_CANDIDATES_TOTAL,
        crate::metrics::names::LABEL_OUTCOME => outcome.as_str()
    )
    .increment(1);
}

/// Clear the account's pending-candidate flag, then re-read the delta
/// store and restore the flag if a candidate delta is (still) present.
///
/// The re-read closes a race with `delta_commit`: once the account has no
/// candidate delta the 409 gate opens, so a new candidate can be committed
/// concurrently with this clear, and a plain clear would leave that
/// candidate invisible to `list_with_pending_candidates` forever.
/// `delta_commit` writes the candidate delta *before* setting the flag,
/// so any commit whose flag-set raced this clear is visible to the
/// re-read here.
pub(crate) async fn clear_pending_candidate_flag(
    state: &AppState,
    account_id: &str,
    now: &str,
) -> std::result::Result<(), String> {
    state
        .metadata
        .set_has_pending_candidate(account_id, false, now)
        .await?;

    let deltas = state.storage.pull_deltas_after(account_id, 0).await?;
    if deltas.iter().any(|d| d.status.is_candidate()) {
        tracing::warn!(
            account_id = %account_id,
            "Candidate committed concurrently with flag clear; restoring flag"
        );
        state
            .metadata
            .set_has_pending_candidate(account_id, true, now)
            .await?;
    }

    Ok(())
}

/// Delete a candidate that can never be canonicalized, delete its
/// matching proposal, and release the account's pending-candidate lock.
///
/// The `delete_delta` call is the linearization point: the 409
/// `conflict_pending_delta` gate scans for a candidate delta row, so the
/// account is released the moment that delete lands, even if a later
/// cleanup step fails.
pub(crate) async fn remove_candidate(
    state: &AppState,
    delta: &DeltaObject,
    now: &str,
    mode: RemovalMode,
) -> Result<()> {
    let storage_backend = state.storage.clone();

    storage_backend
        .delete_delta(&delta.account_id, delta.nonce)
        .await
        .map_err(|e| GuardianError::StorageError(format!("Failed to delete delta: {e}")))?;

    // A discarded candidate can never be canonicalized, so delete its proposal:
    // leaving it would strand it as `pending` forever and let clients re-submit a
    // stale intent.
    let proposal_id = {
        let client = state.network_client.lock().await;
        match client.delta_proposal_id(&delta.account_id, delta.nonce, &delta.delta_payload) {
            Ok(id) => Some(id),
            Err(e) => {
                if mode == RemovalMode::Strict {
                    return Err(GuardianError::NetworkError(format!(
                        "Failed to derive proposal id for abandoned delta: {e}"
                    )));
                }
                tracing::warn!(
                    account_id = %delta.account_id,
                    nonce = delta.nonce,
                    error = %e,
                    "Could not derive proposal id for discarded delta; \
                     its proposal may remain stranded as pending"
                );
                None
            }
        }
    };
    if let Some(ref id) = proposal_id {
        // A candidate pushed via the direct delta path legitimately has no
        // proposal, so absence must not fail the removal — but a backend
        // read failure must not be mistaken for absence either. The
        // single-record read reports both as an opaque error, so use the
        // list read, which errors only on backend failure.
        let proposal_exists = match storage_backend
            .pull_all_delta_proposals(&delta.account_id)
            .await
        {
            Ok(records) => records.iter().any(|record| &record.commitment == id),
            Err(e) => {
                if mode == RemovalMode::Strict {
                    return Err(GuardianError::StorageError(format!(
                        "Failed to check for matching proposal after abandon: {e}"
                    )));
                }
                tracing::warn!(
                    account_id = %delta.account_id,
                    proposal_id = %id,
                    error = %e,
                    "Failed to check for matching proposal after discard; \
                     it may remain stranded as pending"
                );
                false
            }
        };
        if proposal_exists {
            tracing::warn!(
                account_id = %delta.account_id,
                proposal_id = %id,
                "Deleting matching proposal as its delta was discarded"
            );
            if let Err(e) = storage_backend
                .delete_delta_proposal(&delta.account_id, id)
                .await
            {
                if mode == RemovalMode::Strict {
                    return Err(GuardianError::StorageError(format!(
                        "Failed to delete proposal after abandon: {e}"
                    )));
                }
                tracing::warn!(
                    account_id = %delta.account_id,
                    proposal_id = %id,
                    error = %e,
                    "Failed to delete proposal after discard, but continuing"
                );
            }
        }
    }

    // Clear the pending candidate flag after discard
    if let Err(e) = clear_pending_candidate_flag(state, &delta.account_id, now).await {
        if mode == RemovalMode::Strict {
            return Err(GuardianError::StorageError(format!(
                "Failed to clear has_pending_candidate flag after abandon: {e}"
            )));
        }
        tracing::warn!(
            account_id = %delta.account_id,
            error = %e,
            "Failed to clear has_pending_candidate flag after discard"
        );
    }

    Ok(())
}
