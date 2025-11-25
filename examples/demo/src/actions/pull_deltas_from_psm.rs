use miden_objects::transaction::TransactionSummary;
use private_state_manager_client::FromJson;

use crate::display::{print_info, print_section, print_success, print_waiting};
use crate::state::SessionState;

pub async fn action_pull_deltas_from_psm(state: &mut SessionState) -> Result<(), String> {
    print_section("Pull Deltas from PSM");

    let account = state.get_account()?;
    let account_id = account.id();
    let current_nonce = account.nonce().as_int();

    print_waiting("Configuring PSM authentication");
    state.configure_psm_auth()?;

    print_waiting(&format!("Fetching deltas since nonce {}", current_nonce));

    let psm_client = state.get_psm_client_mut()?;
    let delta_response = psm_client
        .get_delta_since(&account_id, current_nonce)
        .await
        .map_err(|e| format!("Failed to get deltas: {}", e))?;

    if let Some(merged_delta) = delta_response.merged_delta {
        println!("\nReceived merged delta:");
        println!(
            "  Delta payload: {} bytes",
            merged_delta.delta_payload.len()
        );
        println!("  New commitment: {}", merged_delta.new_commitment);

        print_waiting("Applying delta to local account");

        let delta_payload: serde_json::Value = serde_json::from_str(&merged_delta.delta_payload)
            .map_err(|e| format!("Failed to parse delta payload: {}", e))?;

        let tx_summary = TransactionSummary::from_json(&delta_payload)
            .map_err(|e| format!("Failed to deserialize transaction summary: {}", e))?;

        let account = state.get_account_mut()?;
        account
            .apply_delta(tx_summary.account_delta())
            .map_err(|e| format!("Failed to apply delta to account: {}", e))?;

        print_success("Delta applied successfully");
        println!("  New nonce: {}", account.nonce().as_int());
    } else {
        print_info("No new deltas found");
    }

    Ok(())
}
