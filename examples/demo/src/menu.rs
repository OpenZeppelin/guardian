use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

use crate::display::{print_error, print_menu_header, print_menu_option};
use crate::state::SessionState;

pub enum MenuAction {
    GenerateKeypair,
    CreateAccount,
    ConfigurePsm,
    PullFromPsm,
    PullDeltasFromPsm,
    AddCosigner,
    SignTransaction,
    FinalizePendingTransaction,
    ShowAccount,
    ShowStatus,
    Quit,
}

pub fn print_menu(state: &SessionState) {
    print_menu_header();

    print_menu_option("1", "Generate keypair", !state.has_keypair());
    print_menu_option(
        "2",
        "Create multisig account",
        state.has_keypair() && !state.has_account(),
    );
    print_menu_option(
        "3",
        "Configure account in PSM",
        state.has_account() && state.is_psm_connected(),
    );
    print_menu_option(
        "4",
        "Pull account from PSM",
        !state.has_account() && state.is_psm_connected(),
    );
    print_menu_option(
        "5",
        "Pull deltas from PSM",
        state.has_account() && state.is_psm_connected(),
    );
    print_menu_option("6", "Add cosigner (update to N+1)", state.has_account());
    print_menu_option(
        "7",
        "Sign pending transaction",
        state.has_account() && state.pending_tx_store.has_pending(),
    );
    print_menu_option(
        "8",
        "Finalize pending transaction",
        state.has_account() && state.pending_tx_store.has_pending(),
    );
    print_menu_option("s", "Show account details", state.has_account());
    print_menu_option("c", "Show connection status", true);
    print_menu_option("q", "Quit", true);

    println!();
}

pub fn get_user_choice(editor: &mut DefaultEditor) -> Result<String, ReadlineError> {
    let input = editor.readline("Choice: ")?;
    editor
        .add_history_entry(&input)
        .map_err(|e| ReadlineError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

    Ok(input.trim().to_lowercase())
}

pub fn parse_menu_choice(choice: &str, state: &SessionState) -> Option<MenuAction> {
    match choice {
        "1" if !state.has_keypair() => Some(MenuAction::GenerateKeypair),
        "2" if state.has_keypair() && !state.has_account() => Some(MenuAction::CreateAccount),
        "3" if state.has_account() && state.is_psm_connected() => Some(MenuAction::ConfigurePsm),
        "4" if !state.has_account() && state.is_psm_connected() => Some(MenuAction::PullFromPsm),
        "5" if state.has_account() && state.is_psm_connected() => {
            Some(MenuAction::PullDeltasFromPsm)
        }
        "6" if state.has_account() => Some(MenuAction::AddCosigner),
        "7" if state.has_account() && state.pending_tx_store.has_pending() => {
            Some(MenuAction::SignTransaction)
        }
        "8" if state.has_account() && state.pending_tx_store.has_pending() => {
            Some(MenuAction::FinalizePendingTransaction)
        }
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

pub fn prompt_confirm(editor: &mut DefaultEditor, message: &str) -> Result<bool, String> {
    let input = prompt_input(editor, &format!("{} (y/n): ", message))?;
    Ok(input.to_lowercase() == "y" || input.to_lowercase() == "yes")
}

pub fn handle_invalid_choice() {
    print_error("Invalid choice or action not available");
}

pub fn run_menu_loop<F>(state: &SessionState, mut handler: F) -> Result<(), String>
where
    F: FnMut(&str, &SessionState) -> Result<bool, String>,
{
    let mut editor = DefaultEditor::new().map_err(|e| format!("Failed to create editor: {}", e))?;

    loop {
        print_menu(state);

        let choice = match get_user_choice(&mut editor) {
            Ok(c) => c,
            Err(ReadlineError::Interrupted) => {
                println!("\nInterrupted");
                continue;
            }
            Err(ReadlineError::Eof) => {
                println!("\nGoodbye!");
                break;
            }
            Err(e) => {
                print_error(&format!("Input error: {}", e));
                continue;
            }
        };

        let should_continue = handler(&choice, state)?;
        if !should_continue {
            break;
        }
    }

    Ok(())
}
