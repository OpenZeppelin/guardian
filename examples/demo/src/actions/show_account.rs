use crate::display::{print_account_info, print_storage_overview, print_vault};
use crate::state::SessionState;

pub async fn action_show_account(state: &SessionState) -> Result<(), String> {
    let client = state.get_client()?;
    let account = client
        .account()
        .ok_or_else(|| "No account loaded".to_string())?;

    print_account_info(account);
    print_vault(account);
    print_storage_overview(account, state.is_ecdsa(), client.guardian_endpoint());

    Ok(())
}
