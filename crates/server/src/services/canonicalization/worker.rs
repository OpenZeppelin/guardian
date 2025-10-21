use crate::canonicalization::{CanonicalizationConfig, CanonicalizationMode};
use crate::error::{PsmError, Result};
use crate::state::AppState;
use tokio::time::interval;

use super::filter::{filter_pending_candidates, filter_ready_candidates};
use super::processor::process_candidates;

pub fn start_worker(state: AppState) {
    tokio::spawn(async move {
        run_worker(state).await;
    });
}

async fn run_worker(state: AppState) {
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

        if let Err(e) = process_all_accounts(&state, &config).await {
            eprintln!("Canonicalization worker error: {e}");
        }
    }
}

async fn process_all_accounts(state: &AppState, config: &CanonicalizationConfig) -> Result<()> {
    let account_ids = state
        .metadata
        .list()
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to list accounts: {e}")))?;

    for account_id in account_ids {
        if let Err(e) = process_account(state, &account_id, config).await {
            eprintln!("Failed to process canonicalizations for account {account_id}: {e}");
        }
    }

    Ok(())
}

async fn process_account(
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

pub async fn process_all_accounts_now(state: &AppState) -> Result<()> {
    let account_ids = state
        .metadata
        .list()
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to list accounts: {e}")))?;

    for account_id in account_ids {
        if let Err(e) = process_account_now(state, &account_id).await {
            eprintln!("Failed to process canonicalizations for account {account_id}: {e}");
        }
    }

    Ok(())
}

async fn process_account_now(state: &AppState, account_id: &str) -> Result<()> {
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

    let pending_candidates = filter_pending_candidates(&all_deltas);
    process_candidates(state, &storage_backend, pending_candidates, account_id).await?;

    Ok(())
}
