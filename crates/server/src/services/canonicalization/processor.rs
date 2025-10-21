use crate::error::{PsmError, Result};
use crate::state::AppState;
use crate::storage::{AccountState, DeltaObject, DeltaStatus, StorageBackend};
use std::sync::Arc;

use super::{CandidateDelta, VerificationResult};

pub async fn process_candidates(
    state: &AppState,
    storage_backend: &Arc<dyn StorageBackend>,
    candidates: Vec<DeltaObject>,
    account_id: &str,
) -> Result<()> {
    for delta in candidates {
        let nonce = delta.nonce;
        let candidate = CandidateDelta::new(delta);
        if let Err(e) = process_candidate(state, storage_backend, candidate).await {
            eprintln!(
                "Failed to canonicalize delta {} for account {}: {}",
                nonce, account_id, e
            );
        }
    }
    Ok(())
}

async fn process_candidate(
    state: &AppState,
    storage_backend: &Arc<dyn StorageBackend>,
    candidate: CandidateDelta,
) -> Result<()> {
    let on_chain_commitment = fetch_on_chain_commitment(state, &candidate.delta.account_id).await?;
    let verification_result = candidate.verify(on_chain_commitment);

    match verification_result {
        VerificationResult::Matched(verified) => {
            canonicalize_verified_delta(state, storage_backend, &verified).await
        }
        VerificationResult::Mismatched {
            delta,
            expected_commitment,
            actual_commitment,
        } => {
            discard_mismatched_delta(
                storage_backend,
                delta,
                &expected_commitment,
                &actual_commitment,
            )
            .await
        }
    }
}

async fn fetch_on_chain_commitment(state: &AppState, account_id: &str) -> Result<String> {
    let mut client = state.network_client.lock().await;
    client
        .verify_on_chain_state(account_id)
        .await
        .map_err(PsmError::NetworkError)
}

async fn canonicalize_verified_delta(
    state: &AppState,
    storage_backend: &Arc<dyn StorageBackend>,
    verified: &super::VerifiedDelta,
) -> Result<()> {
    let delta = verified.delta();

    println!(
        "✓ Canonicalizing delta {} for account {} (commitment matches on-chain)",
        delta.nonce, delta.account_id
    );

    let current_state = storage_backend
        .pull_state(&delta.account_id)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to get current state: {e}")))?;

    let (new_state_json, new_commitment) = {
        let client = state.network_client.lock().await;
        client
            .apply_delta(&current_state.state_json, &delta.delta_payload)
            .map_err(PsmError::InvalidDelta)?
    };

    let now = chrono::Utc::now().to_rfc3339();

    let updated_state = AccountState {
        account_id: delta.account_id.clone(),
        state_json: new_state_json,
        commitment: new_commitment,
        created_at: current_state.created_at.clone(),
        updated_at: now.clone(),
    };

    storage_backend
        .submit_state(&updated_state)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to update account state: {e}")))?;

    let mut canonical_delta = delta.clone();
    canonical_delta.status = DeltaStatus::canonical(now);

    storage_backend
        .submit_delta(&canonical_delta)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to update delta as canonical: {e}")))?;

    Ok(())
}

async fn discard_mismatched_delta(
    storage_backend: &Arc<dyn StorageBackend>,
    delta: DeltaObject,
    expected_commitment: &str,
    actual_commitment: &str,
) -> Result<()> {
    println!(
        "✗ Discarding delta {} for account {} (commitment mismatch: expected {}, got {})",
        delta.nonce, delta.account_id, expected_commitment, actual_commitment
    );

    let now = chrono::Utc::now().to_rfc3339();

    let mut discarded_delta = delta.clone();
    discarded_delta.status = DeltaStatus::discarded(now);

    storage_backend
        .submit_delta(&discarded_delta)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to update delta as discarded: {e}")))?;

    Err(PsmError::CommitmentMismatch {
        expected: expected_commitment.to_string(),
        actual: actual_commitment.to_string(),
    })
}
