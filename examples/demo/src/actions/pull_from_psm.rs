use base64::Engine;
use miden_client::account::Account;
use miden_client::Deserializable;
use miden_objects::account::AccountId;
use rustyline::DefaultEditor;

use crate::account_inspector::AccountInspector;
use crate::display::{print_full_hex, print_section, print_success, print_waiting};
use crate::menu::prompt_input;
use crate::state::SessionState;

pub async fn action_pull_from_psm(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    print_section("Pull Account from PSM");

    let account_id_hex = prompt_input(editor, "Enter account ID: ")?;
    let account_id =
        AccountId::from_hex(&account_id_hex).map_err(|e| format!("Invalid account ID: {}", e))?;

    print_waiting("Configuring PSM authentication");
    state.configure_psm_auth()?;

    print_waiting("Fetching account from PSM");

    let psm_client = state.get_psm_client_mut()?;
    let account_state_response = psm_client
        .get_state(&account_id)
        .await
        .map_err(|e| format!("Failed to get account state: {}", e))?;

    print_waiting("Deserializing account data");

    let state_json = account_state_response
        .state
        .ok_or_else(|| "No state returned from PSM".to_string())?
        .state_json;

    let state_value: serde_json::Value = serde_json::from_str(&state_json)
        .map_err(|e| format!("Failed to parse state JSON: {}", e))?;

    let account_base64 = state_value["data"]
        .as_str()
        .ok_or_else(|| "Missing 'data' field in state".to_string())?;

    let account_bytes = base64::engine::general_purpose::STANDARD
        .decode(account_base64)
        .map_err(|e| format!("Failed to decode account data: {}", e))?;

    let account = Account::read_from_bytes(&account_bytes)
        .map_err(|e| format!("Failed to deserialize account: {}", e))?;

    print_waiting("Adding account to Miden client");

    let miden_client = state.get_miden_client_mut()?;
    miden_client
        .add_account(&account, false)
        .await
        .map_err(|e| e.to_string())?;

    let inspector = AccountInspector::new(&account);
    let commitments = inspector.extract_cosigner_commitments();
    state.cosigner_commitments = commitments;

    state.set_account(account);

    print_success("Account pulled successfully and added to local client");
    print_full_hex("  Account ID", &account_id.to_string());

    Ok(())
}
