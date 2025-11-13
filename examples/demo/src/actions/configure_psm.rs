use base64::Engine;
use miden_client::Serializable;
use private_state_manager_client::{AuthConfig, MidenFalconRpoAuth};

use crate::display::{print_full_hex, print_section, print_success, print_waiting};
use crate::state::SessionState;

pub async fn action_configure_psm(state: &mut SessionState) -> Result<(), String> {
    print_section("Configure Account in PSM");

    let account = state.get_account()?;
    let account_id = account.id();

    let cosigner_commitments = state.cosigner_commitments.clone();
    if cosigner_commitments.is_empty() {
        return Err("No cosigner commitments found. Create account first.".to_string());
    }

    let account_bytes = account.to_bytes();
    let account_base64 = base64::engine::general_purpose::STANDARD.encode(&account_bytes);

    let auth_config = AuthConfig {
        auth_type: Some(
            private_state_manager_client::auth_config::AuthType::MidenFalconRpo(
                MidenFalconRpoAuth {
                    cosigner_commitments,
                },
            ),
        ),
    };

    let initial_state = serde_json::json!({
        "data": account_base64,
        "account_id": account_id.to_string(),
    });

    state.configure_psm_auth()?;

    print_waiting("Configuring account in PSM");

    let psm_client = state.get_psm_client_mut()?;

    let response = psm_client
        .configure(&account_id, auth_config, initial_state, "Filesystem")
        .await
        .map_err(|e| format!("PSM configuration failed: {}", e))?;

    print_success(&format!("Account configured in PSM: {}", response.message));
    print_full_hex("  Account ID", &account_id.to_string());

    Ok(())
}
