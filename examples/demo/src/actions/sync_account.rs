use miden_multisig_client::AccountId;
use rustyline::DefaultEditor;

use crate::display::{print_info, print_section, print_success, print_waiting, shorten_hex};
use crate::menu::prompt_input;
use crate::state::SessionState;

pub async fn action_sync_account(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    print_section("Sync Account");

    let client = state.get_client_mut()?;

    if client.has_account() {
        // Account exists locally, sync deltas from PSM
        print_waiting("Syncing account state from PSM");

        client
            .sync_account()
            .await
            .map_err(|e| format!("Failed to sync account: {}", e))?;

        let account = client
            .account()
            .ok_or_else(|| "Account not found after sync".to_string())?;

        print_success("Account synced successfully");
        print_info(&format!("  Current nonce: {}", account.nonce()));
    } else {
        // No local account, pull from PSM
        let account_id_hex = prompt_input(editor, "Enter account ID: ")?;
        let account_id = AccountId::from_hex(&account_id_hex)
            .map_err(|e| format!("Invalid account ID: {}", e))?;

        print_waiting("Fetching account from PSM");

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
