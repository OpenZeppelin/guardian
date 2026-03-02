use crate::display::{print_full_hex, print_section, print_success, print_waiting};
use crate::state::SessionState;

pub async fn action_verify_state_commitment(state: &SessionState) -> Result<(), String> {
    print_section("Verify State Commitment");

    if !state.has_account() {
        return Err("No account loaded. Sync or create an account first.".to_string());
    }

    print_waiting("Verifying local account commitment against on-chain commitment");

    let client = state.get_client()?;
    let result = client
        .verify_state_commitment()
        .await
        .map_err(|e| format!("State commitment verification failed: {}", e))?;

    print_success("State commitment verified");
    print_full_hex("  Account ID", &result.account_id.to_hex());
    print_full_hex("  Local commitment", &result.local_commitment_hex);
    print_full_hex("  On-chain commitment", &result.on_chain_commitment_hex);

    Ok(())
}
