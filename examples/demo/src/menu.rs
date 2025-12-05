use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

use crate::display::{print_error, print_menu_header, print_menu_option};
use crate::state::SessionState;

pub enum MenuAction {
    CreateAccount,
    SyncAccount,
    ListNotes,
    ProposalManagement,
    ShowAccount,
    ShowStatus,
    Quit,
}

pub fn print_menu(state: &SessionState) {
    print_menu_header();
    print_menu_option("1", "Create multisig account", !state.has_account());
    print_menu_option("2", "Sync account", true);
    print_menu_option("3", "List consumable notes", state.has_account());
    print_menu_option("4", "Proposal management", state.has_account());
    print_menu_option("s", "Show account details", state.has_account());
    print_menu_option("c", "Show connection status", true);
    print_menu_option("q", "Quit", true);

    println!();
}

pub fn get_user_choice(editor: &mut DefaultEditor) -> Result<String, ReadlineError> {
    let input = editor.readline("Choice: ")?;
    editor
        .add_history_entry(&input)
        .map_err(|e| ReadlineError::Io(std::io::Error::other(e)))?;

    Ok(input.trim().to_lowercase())
}

pub fn parse_menu_choice(choice: &str, state: &SessionState) -> Option<MenuAction> {
    match choice {
        "1" if !state.has_account() => Some(MenuAction::CreateAccount),
        "2" => Some(MenuAction::SyncAccount),
        "3" if state.has_account() => Some(MenuAction::ListNotes),
        "4" if state.has_account() => Some(MenuAction::ProposalManagement),
        "s" if state.has_account() => Some(MenuAction::ShowAccount),
        "c" => Some(MenuAction::ShowStatus),
        "q" => Some(MenuAction::Quit),
        _ => None,
    }
}

pub fn prompt_input(editor: &mut DefaultEditor, prompt: &str) -> Result<String, String> {
    editor
        .readline(prompt)
        .map(|s| s.trim().to_string())
        .map_err(|e| format!("Input error: {}", e))
}

pub fn handle_invalid_choice() {
    print_error("Invalid choice or action not available");
}
