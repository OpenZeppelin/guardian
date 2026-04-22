use miden_multisig_client::AccountId;
use rustyline::DefaultEditor;

use crate::display::{
    print_error, print_info, print_section, print_success, print_waiting, shorten_hex,
};
use crate::menu::prompt_input;
use crate::state::SessionState;

/// Maximum number of sync retry attempts.
const MAX_SYNC_RETRIES: u32 = 3;
/// Delay between sync retries in milliseconds.
const SYNC_RETRY_DELAY_MS: u64 = 1000;

/// Check if an error is related to store/SMT issues that require a client reset.
fn is_store_error(error: &str) -> bool {
    error.contains("subtract with overflow")
        || error.contains("SMT")
        || error.contains("forest")
        || error.contains("store error")
        || error.contains("PoisonError")
        || error.contains("Poison")
}

/// Check if an error is a fatal poison error that requires process restart.
fn is_fatal_poison_error(error: &str) -> bool {
    error.contains("PoisonError") || error.contains("Poison")
}

/// Check if an error is a commitment mismatch (account was updated on-chain by another client).
fn is_commitment_mismatch(error: &str) -> bool {
    error.contains("doesn't match the imported account commitment")
        || error.contains("commitment mismatch")
}

/// Sync with retry logic to handle transient errors (e.g., SMT store issues).
///
/// When a store error is detected, we reinitialize the entire client to get
/// a fresh database connection, avoiding poisoned connection pools. We also
/// re-pull the account from GUARDIAN to ensure we have the latest state.
pub async fn sync_with_retry(state: &mut SessionState) -> Result<(), String> {
    let mut last_error = String::new();

    for attempt in 1..=MAX_SYNC_RETRIES {
        // If we had an error on the previous attempt that requires re-pulling from GUARDIAN
        let needs_repull = is_store_error(&last_error) || is_commitment_mismatch(&last_error);

        if attempt > 1 && needs_repull {
            // Check if this is a fatal poison error - if so, no point in retrying
            if is_fatal_poison_error(&last_error) {
                print_error("Fatal: Database connection pool is poisoned.");
                print_info("  This is a known issue in miden-crypto v0.19.4.");
                print_info("  Please restart the demo to recover.");
                return Err("Database connection pool poisoned - restart required".to_string());
            }

            // Get the account ID before we reinitialize
            let account_id = state
                .get_client()?
                .account_id()
                .ok_or_else(|| "No account loaded".to_string())?;

            if is_commitment_mismatch(&last_error) {
                print_info("  Account was updated on-chain by another client.");
                print_info("  Reinitializing and re-pulling latest state from GUARDIAN...");
            } else {
                print_info(&format!(
                    "  Reinitializing client before attempt {}...",
                    attempt
                ));
            }

            // Reinitialize the client completely (creates fresh SQLite DB)
            if let Err(e) = state.reinitialize_client().await {
                if is_fatal_poison_error(&e) {
                    print_error("Fatal: Cannot reinitialize - connection pool is poisoned.");
                    print_info("  Please restart the demo to recover.");
                    return Err("Database connection pool poisoned - restart required".to_string());
                }
                return Err(format!("Failed to reinitialize client: {}", e));
            }

            // Re-pull the account from GUARDIAN with fresh state
            print_info("  Re-pulling account from GUARDIAN...");
            let client = state.get_client_mut()?;
            if let Err(e) = client.pull_account(account_id).await {
                print_info(&format!("  Warning: Failed to re-pull account: {}", e));
            }

            // Small delay after reinit
            tokio::time::sleep(tokio::time::Duration::from_millis(SYNC_RETRY_DELAY_MS)).await;
        }

        let client = state.get_client_mut()?;

        match client.sync().await {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_error = e.to_string();

                // Check for fatal poison error immediately
                if is_fatal_poison_error(&last_error) {
                    print_error("Fatal: Database connection pool is poisoned.");
                    print_info("  This is a known issue in miden-crypto v0.19.4.");
                    print_info("  Please restart the demo to recover.");
                    return Err("Database connection pool poisoned - restart required".to_string());
                }

                if attempt < MAX_SYNC_RETRIES {
                    if is_commitment_mismatch(&last_error) {
                        print_info(&format!(
                            "  Sync attempt {} failed (commitment mismatch), will re-pull from GUARDIAN...",
                            attempt
                        ));
                    } else if is_store_error(&last_error) {
                        print_info(&format!(
                            "  Sync attempt {} failed (store error), will reinitialize and retry...",
                            attempt
                        ));
                    } else {
                        print_info(&format!(
                            "  Sync attempt {} failed: {}, retrying...",
                            attempt, last_error
                        ));
                        tokio::time::sleep(tokio::time::Duration::from_millis(SYNC_RETRY_DELAY_MS))
                            .await;
                    }
                }
            }
        }
    }

    Err(format!(
        "Sync failed after {} attempts: {}",
        MAX_SYNC_RETRIES, last_error
    ))
}

pub async fn action_sync_account(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    print_section("Sync Account");

    let has_account = state.get_client()?.has_account();

    if has_account {
        // Get account ID in case we need to re-pull
        let account_id = state
            .get_client()?
            .account_id()
            .ok_or_else(|| "No account loaded".to_string())?;

        // Account exists locally, sync deltas from GUARDIAN first
        print_waiting("Syncing account state from GUARDIAN");

        // Get deltas from GUARDIAN
        let client = state.get_client_mut()?;
        if let Err(e) = client.get_deltas().await {
            let error_str = e.to_string();

            // Check for fatal poison error first
            if is_fatal_poison_error(&error_str) {
                print_error("Fatal: Database connection pool is poisoned.");
                print_info("  This is a known issue in miden-crypto v0.19.4.");
                print_info("  Please restart the demo to recover.");
                return Err("Database connection pool poisoned - restart required".to_string());
            }

            // Check if this is a store error from a previous panic
            if is_store_error(&error_str) {
                print_info("  Store error detected, reinitializing client...");
                state.reinitialize_client().await?;

                // Re-pull account from GUARDIAN
                print_info("  Re-pulling account from GUARDIAN...");
                let client = state.get_client_mut()?;
                client
                    .pull_account(account_id)
                    .await
                    .map_err(|e| format!("Failed to re-pull account: {}", e))?;
            } else {
                // Log but don't fail - we still do full state sync below.
                print_info(&format!(
                    "  Note: Delta sync failed ({}). Continuing with full state sync.",
                    e
                ));
            }
        }

        // Sync with the Miden network using retry logic
        print_waiting("Syncing with Miden network");

        match sync_with_retry(state).await {
            Ok(()) => {
                let client = state.get_client()?;
                let account = client
                    .account()
                    .ok_or_else(|| "Account not found after sync".to_string())?;

                print_success("Account synced successfully");
                print_info(&format!("  Current nonce: {}", account.nonce()));
            }
            Err(e) => {
                print_error(&format!("Sync failed: {}", e));
                if !e.contains("restart required") {
                    print_info("  Tip: If the error persists, try restarting the demo.");
                }
                return Err(e);
            }
        }
    } else {
        // No local account, pull from GUARDIAN
        let account_id_hex = prompt_input(editor, "Enter account ID: ")?;
        let account_id = AccountId::from_hex(&account_id_hex)
            .map_err(|e| format!("Invalid account ID: {}", e))?;

        print_waiting("Fetching account from GUARDIAN");

        let client = state.get_client_mut()?;
        let account = client
            .pull_account(account_id)
            .await
            .map_err(|e| format!("Failed to pull account: {}", e))?;

        print_success("Account pulled successfully");
        print_info(&format!(
            "  Account ID: {}",
            shorten_hex(&account.id().to_string())
        ));
        print_info(&format!("  Current nonce: {}", account.nonce()));
    }

    Ok(())
}
