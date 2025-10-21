use crate::canonicalization::{CanonicalizationConfig, CanonicalizationMode};
use crate::error::{PsmError, Result};
use crate::state::AppState;
use crate::storage::{AccountState, DeltaObject, DeltaStatus, StorageBackend};
use std::sync::Arc;
use tokio::time::interval;

pub fn start_canonicalization_worker(state: AppState) {
    tokio::spawn(async move {
        canonicalization_worker(state).await;
    });
}

async fn canonicalization_worker(state: AppState) {
    let config = match &state.canonicalization_mode {
        CanonicalizationMode::Enabled(config) => config.clone(),
        CanonicalizationMode::Optimistic => {
            eprintln!(
                "Warning: Canonicalization worker started in Optimistic mode - this should not happen"
            );
            return;
        }
    };

    let mut interval_timer = interval(config.check_interval());

    loop {
        interval_timer.tick().await;

        if let Err(e) = process_pending_canonicalizations(&state, &config).await {
            eprintln!("Canonicalization worker error: {e}");
        }
    }
}

async fn process_pending_canonicalizations(
    state: &AppState,
    config: &CanonicalizationConfig,
) -> Result<()> {
    let account_ids = state
        .metadata
        .list()
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to list accounts: {e}")))?;

    for account_id in account_ids {
        if let Err(e) = process_account_canonicalizations(state, &account_id, config).await {
            eprintln!("Failed to process canonicalizations for account {account_id}: {e}");
        }
    }

    Ok(())
}

pub async fn process_canonicalizations_now(state: &AppState) -> Result<()> {
    let account_ids = state
        .metadata
        .list()
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to list accounts: {e}")))?;

    for account_id in account_ids {
        if let Err(e) = process_account_canonicalizations_now(state, &account_id).await {
            eprintln!("Failed to process canonicalizations for account {account_id}: {e}");
        }
    }

    Ok(())
}

async fn process_account_canonicalizations_now(
    state: &AppState,
    account_id: &str,
) -> Result<()> {
    let account_metadata = state
        .metadata
        .get(account_id)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to get metadata: {e}")))?
        .ok_or_else(|| PsmError::InvalidInput("Account metadata not found".to_string()))?;

    let storage_backend = state
        .storage
        .get(&account_metadata.storage_type)
        .map_err(PsmError::ConfigurationError)?;

    let all_deltas = storage_backend
        .pull_deltas_after(account_id, 0)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to pull deltas: {e}")))?;

    let ready_candidates = filter_pending_candidates(&all_deltas);
    process_candidates(state, &storage_backend, ready_candidates, account_id).await?;

    Ok(())
}

async fn process_account_canonicalizations(
    state: &AppState,
    account_id: &str,
    config: &CanonicalizationConfig,
) -> Result<()> {
    let account_metadata = state
        .metadata
        .get(account_id)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to get metadata: {e}")))?
        .ok_or_else(|| PsmError::InvalidInput("Account metadata not found".to_string()))?;

    let storage_backend = state
        .storage
        .get(&account_metadata.storage_type)
        .map_err(PsmError::ConfigurationError)?;

    let all_deltas = storage_backend
        .pull_deltas_after(account_id, 0)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to pull deltas: {e}")))?;

    let ready_candidates = filter_ready_candidates(&all_deltas, config);
    process_candidates(state, &storage_backend, ready_candidates, account_id).await?;

    Ok(())
}

fn filter_pending_candidates(deltas: &[DeltaObject]) -> Vec<DeltaObject> {
    let mut candidates: Vec<DeltaObject> = deltas
        .iter()
        .filter(|delta| is_pending_candidate(delta))
        .cloned()
        .collect();

    candidates.sort_by_key(|d| d.nonce);
    candidates
}

fn filter_ready_candidates(
    deltas: &[DeltaObject],
    config: &CanonicalizationConfig,
) -> Vec<DeltaObject> {
    let now = chrono::Utc::now();
    let mut candidates: Vec<DeltaObject> = deltas
        .iter()
        .filter(|delta| is_ready_candidate(delta, &now, config))
        .cloned()
        .collect();

    candidates.sort_by_key(|d| d.nonce);
    candidates
}

fn is_pending_candidate(delta: &DeltaObject) -> bool {
    delta.status.is_candidate()
}

fn is_ready_candidate(
    delta: &DeltaObject,
    now: &chrono::DateTime<chrono::Utc>,
    config: &CanonicalizationConfig,
) -> bool {
    if !delta.status.is_candidate() {
        return false;
    }

    let candidate_at_str = delta.status.timestamp();
    if let Ok(candidate_at) = chrono::DateTime::parse_from_rfc3339(candidate_at_str) {
        let elapsed = now.signed_duration_since(candidate_at);
        return elapsed.num_seconds() >= config.delay_seconds as i64;
    }

    false
}

async fn process_candidates(
    state: &AppState,
    storage_backend: &Arc<dyn StorageBackend>,
    candidates: Vec<DeltaObject>,
    account_id: &str,
) -> Result<()> {
    for delta in candidates {
        if let Err(e) = verify_and_canonicalize_delta(state, storage_backend, &delta).await {
            eprintln!(
                "Failed to canonicalize delta {} for account {}: {}",
                delta.nonce, account_id, e
            );
        }
    }
    Ok(())
}

async fn verify_and_canonicalize_delta(
    state: &AppState,
    storage_backend: &Arc<dyn StorageBackend>,
    delta: &DeltaObject,
) -> Result<()> {
    let on_chain_commitment = fetch_on_chain_commitment(state, &delta.account_id).await?;

    if on_chain_commitment == delta.new_commitment {
        canonicalize_delta(state, storage_backend, delta).await
    } else {
        discard_delta(
            storage_backend,
            delta,
            &delta.new_commitment,
            &on_chain_commitment,
        )
        .await
    }
}

async fn fetch_on_chain_commitment(state: &AppState, account_id: &str) -> Result<String> {
    let mut client = state.network_client.lock().await;
    client
        .verify_on_chain_state(account_id)
        .await
        .map_err(PsmError::NetworkError)
}

async fn canonicalize_delta(
    state: &AppState,
    storage_backend: &Arc<dyn StorageBackend>,
    delta: &DeltaObject,
) -> Result<()> {
    println!(
        "✓ Canonicalizing delta {} for account {} (commitment matches on-chain)",
        delta.nonce, delta.account_id
    );

    let current_state = storage_backend
        .pull_state(&delta.account_id)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to get current state: {e}")))?;

    let (new_state_json, new_commitment) =
        apply_delta_to_state(state, &current_state.state_json, &delta.delta_payload).await?;

    let now = chrono::Utc::now().to_rfc3339();

    update_account_state(
        storage_backend,
        &delta.account_id,
        new_state_json,
        new_commitment,
        &current_state.created_at,
        &now,
    )
    .await?;

    mark_delta_canonical(storage_backend, delta, &now).await?;

    Ok(())
}

async fn discard_delta(
    storage_backend: &Arc<dyn StorageBackend>,
    delta: &DeltaObject,
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

async fn apply_delta_to_state(
    state: &AppState,
    current_state_json: &serde_json::Value,
    delta_payload: &serde_json::Value,
) -> Result<(serde_json::Value, String)> {
    let client = state.network_client.lock().await;
    client
        .apply_delta(current_state_json, delta_payload)
        .map_err(PsmError::InvalidDelta)
}

async fn update_account_state(
    storage_backend: &Arc<dyn StorageBackend>,
    account_id: &str,
    state_json: serde_json::Value,
    commitment: String,
    created_at: &str,
    updated_at: &str,
) -> Result<()> {
    let updated_state = AccountState {
        account_id: account_id.to_string(),
        state_json,
        commitment,
        created_at: created_at.to_string(),
        updated_at: updated_at.to_string(),
    };

    storage_backend
        .submit_state(&updated_state)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to update account state: {e}")))
}

async fn mark_delta_canonical(
    storage_backend: &Arc<dyn StorageBackend>,
    delta: &DeltaObject,
    timestamp: &str,
) -> Result<()> {
    let mut canonical_delta = delta.clone();
    canonical_delta.status = DeltaStatus::canonical(timestamp.to_string());

    storage_backend
        .submit_delta(&canonical_delta)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to update delta as canonical: {e}")))
}
