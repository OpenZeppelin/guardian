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
    if let Some(ref id) = proposal_id
        && let Ok(_existing_proposal) = storage_backend
            .pull_delta_proposal(&delta.account_id, id)
            .await
    {
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

    // Clear the pending candidate flag after discard
    if let Err(e) = state
        .metadata
        .set_has_pending_candidate(&delta.account_id, false, now)
        .await
    {
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
