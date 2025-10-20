use crate::canonicalization::{CanonicalizationConfig, CanonicalizationMode};
use crate::state::AppState;
use crate::storage::{AccountState, DeltaObject, StorageBackend};
use std::sync::Arc;
use tokio::time::interval;

/// Start the canonicalization worker that runs periodically
pub fn start_canonicalization_worker(state: AppState) {
    tokio::spawn(async move {
        canonicalization_worker(state).await;
    });
}

/// Background worker that periodically checks for candidate deltas
/// and canonicalizes them if they're older than the configured delay
async fn canonicalization_worker(state: AppState) {
    // Get config from canonicalization mode
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

/// Process all pending canonicalizations across all accounts
async fn process_pending_canonicalizations(
    state: &AppState,
    config: &CanonicalizationConfig,
) -> Result<(), String> {
    // Get all accounts
    let account_ids = state
        .metadata
        .list()
        .await
        .map_err(|e| format!("Failed to list accounts: {e}"))?;

    for account_id in account_ids {
        if let Err(e) = process_account_canonicalizations(state, &account_id, config).await {
            eprintln!("Failed to process canonicalizations for account {account_id}: {e}");
        }
    }

    Ok(())
}

/// Process all pending canonicalizations immediately (test helper)
/// This bypasses the time delay check and processes all pending candidates
pub async fn process_canonicalizations_now(state: &AppState) -> Result<(), String> {
    let account_ids = state
        .metadata
        .list()
        .await
        .map_err(|e| format!("Failed to list accounts: {e}"))?;

    for account_id in account_ids {
        if let Err(e) = process_account_canonicalizations_now(state, &account_id).await {
            eprintln!("Failed to process canonicalizations for account {account_id}: {e}");
        }
    }

    Ok(())
}

/// Process canonicalizations for a single account (test helper - bypasses time delay)
async fn process_account_canonicalizations_now(
    state: &AppState,
    account_id: &str,
) -> Result<(), String> {
    let account_metadata = state
        .metadata
        .get(account_id)
        .await
        .map_err(|e| format!("Failed to get metadata: {e}"))?
        .ok_or_else(|| "Account metadata not found".to_string())?;

    let storage_backend = state
        .storage
        .get(&account_metadata.storage_type)
        .map_err(|e| format!("Failed to get storage backend: {e}"))?;

    let all_deltas = storage_backend
        .pull_deltas_after(account_id, 0)
        .await
        .map_err(|e| format!("Failed to pull deltas: {e}"))?;

    // Get all pending candidates (bypass time check for tests)
    let ready_candidates: Vec<DeltaObject> = all_deltas
        .iter()
        .filter(|delta| {
            delta.candidate_at.is_some()
                && delta.canonical_at.is_none()
                && delta.discarded_at.is_none()
        })
        .cloned()
        .collect();

    let mut sorted_candidates = ready_candidates;
    sorted_candidates.sort_by_key(|d| d.nonce);

    for delta in sorted_candidates {
        if let Err(e) = verify_and_canonicalize_delta(state, &storage_backend, &delta).await {
            eprintln!(
                "Failed to canonicalize delta {} for account {}: {}",
                delta.nonce, account_id, e
            );
        }
    }

    Ok(())
}

/// Process canonicalizations for a single account
async fn process_account_canonicalizations(
    state: &AppState,
    account_id: &str,
    config: &crate::canonicalization::CanonicalizationConfig,
) -> Result<(), String> {
    // Get account metadata to determine storage backend
    let account_metadata = state
        .metadata
        .get(account_id)
        .await
        .map_err(|e| format!("Failed to get metadata: {e}"))?
        .ok_or_else(|| "Account metadata not found".to_string())?;

    let storage_backend = state
        .storage
        .get(&account_metadata.storage_type)
        .map_err(|e| format!("Failed to get storage backend: {e}"))?;

    // Get all deltas for the account
    let all_deltas = storage_backend
        .pull_deltas_after(account_id, 0)
        .await
        .map_err(|e| format!("Failed to pull deltas: {e}"))?;

    // Filter for candidate deltas that are ready for canonicalization
    let now = chrono::Utc::now();
    let ready_candidates: Vec<DeltaObject> = all_deltas
        .iter()
        .filter(|delta| {
            // Must be a candidate (has candidate_at)
            if let Some(candidate_at_str) = &delta.candidate_at {
                // Must not already be canonical or discarded
                if delta.canonical_at.is_some() || delta.discarded_at.is_some() {
                    return false;
                }

                // Must be older than the configured delay
                if let Ok(candidate_at) = chrono::DateTime::parse_from_rfc3339(candidate_at_str) {
                    let elapsed = now.signed_duration_since(candidate_at);
                    return elapsed.num_seconds() >= config.delay_seconds as i64;
                }
            }
            false
        })
        .cloned()
        .collect();

    // Sort candidates by nonce (process in order)
    let mut sorted_candidates = ready_candidates;
    sorted_candidates.sort_by_key(|d| d.nonce);

    // Process each ready candidate in order
    for delta in sorted_candidates {
        if let Err(e) = verify_and_canonicalize_delta(state, &storage_backend, &delta).await {
            eprintln!(
                "Failed to canonicalize delta {} for account {}: {}",
                delta.nonce, account_id, e
            );
            // Continue processing other deltas even if one fails
        }
    }

    Ok(())
}

/// Verify on-chain commitment and canonicalize delta
///
/// This function will:
/// 1. Fetch the on-chain commitment from the Miden network
/// 2. Compare it with the delta's new_commitment
/// 3. If they match:
///    - Apply the delta to current state
///    - Update account state
///    - Mark the delta as canonical
/// 4. If they don't match:
///    - Mark the delta as discarded
async fn verify_and_canonicalize_delta(
    state: &AppState,
    storage_backend: &Arc<dyn StorageBackend>,
    delta: &DeltaObject,
) -> Result<(), String> {
    let on_chain_commitment = {
        let mut client = state.network_client.lock().await;
        client
            .verify_on_chain_state(&delta.account_id)
            .await
            .map_err(|e| format!("Failed to fetch on-chain commitment: {e}"))?
    };

    // Check if commitments match
    if on_chain_commitment == delta.new_commitment {
        // Commitments match - canonicalize the delta and apply state update
        println!(
            "✓ Canonicalizing delta {} for account {} (commitment matches on-chain)",
            delta.nonce, delta.account_id
        );

        // Get current state and apply this delta directly
        let current_state = storage_backend
            .pull_state(&delta.account_id)
            .await
            .map_err(|e| format!("Failed to get current state: {e}"))?;

        let (new_state_json, new_commitment) = {
            let client = state.network_client.lock().await;
            client
                .apply_delta(&current_state.state_json, &delta.delta_payload)
                .map_err(|e| format!("Failed to apply delta during canonicalization: {e}"))?
        };

        // Update account state
        let now = chrono::Utc::now().to_rfc3339();
        let updated_state = AccountState {
            account_id: delta.account_id.clone(),
            state_json: new_state_json.clone(),
            commitment: new_commitment,
            created_at: current_state.created_at,
            updated_at: now.clone(),
        };

        storage_backend
            .submit_state(&updated_state)
            .await
            .map_err(|e| format!("Failed to update account state: {e}"))?;

        // Mark delta as canonical
        let mut canonical_delta = delta.clone();
        canonical_delta.canonical_at = Some(now);

        storage_backend
            .submit_delta(&canonical_delta)
            .await
            .map_err(|e| format!("Failed to update delta as canonical: {e}"))?;

        Ok(())
    } else {
        // Commitments don't match - discard this delta
        println!(
            "✗ Discarding delta {} for account {} (commitment mismatch: expected {}, got {})",
            delta.nonce, delta.account_id, delta.new_commitment, on_chain_commitment
        );

        let now = chrono::Utc::now().to_rfc3339();

        // Mark this delta as discarded
        let mut discarded_delta = delta.clone();
        discarded_delta.discarded_at = Some(now);
        storage_backend
            .submit_delta(&discarded_delta)
            .await
            .map_err(|e| format!("Failed to update delta as discarded: {e}"))?;

        Err(format!(
            "On-chain commitment mismatch: expected {}, got {}. Delta discarded.",
            delta.new_commitment, on_chain_commitment
        ))
    }
}
