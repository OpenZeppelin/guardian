use crate::display::{print_account_info, print_storage_overview};
use crate::state::SessionState;

pub async fn action_show_account(state: &SessionState) -> Result<(), String> {
    let account = state.get_account()?;

    print_account_info(account);
    print_storage_overview(account);

    Ok(())
}
