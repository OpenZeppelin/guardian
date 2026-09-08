use rustyline::DefaultEditor;

use crate::display::{print_info, print_section, print_success, print_waiting, shorten_hex};
use crate::state::SessionState;

const PAGE_SIZE: u32 = 10;

/// Walk the account's canonical delta history from Guardian
/// (issue #413), one page at a time, newest-first by nonce.
pub async fn action_delta_history(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    print_section("Delta History");

    let mut cursor: Option<String> = None;
    let mut page_number = 1u32;

    loop {
        print_waiting(&format!("Fetching page {page_number}..."));
        let page = {
            let client = state.get_client_mut()?;
            client
                .delta_history(Some(PAGE_SIZE), cursor.take())
                .await
                .map_err(|e| format!("Failed to fetch delta history: {}", e))?
        };
        println!();

        if page.entries.is_empty() {
            print_info("No confirmed deltas yet.");
            print_info("Tip: Only canonical (confirmed) transactions appear here;");
            print_info("     pending proposals live under 'Proposal management'.");
            return Ok(());
        }

        print_success(&format!(
            "Page {page_number}: {} entr{}",
            page.entries.len(),
            if page.entries.len() == 1 { "y" } else { "ies" }
        ));
        println!();

        for entry in &page.entries {
            println!(
                "  Nonce {} — {} ({})",
                entry.nonce,
                entry.timestamp,
                entry.status.as_str()
            );
            if let Some(commitment) = &entry.new_commitment {
                println!("      Commitment: {}", shorten_hex(commitment));
            }
            for (label, notes) in [("In", &entry.input_notes), ("Out", &entry.output_notes)] {
                for note in notes {
                    let mut line = format!(
                        "      {label}: {} note {} ({})",
                        note.tag.as_str(),
                        shorten_hex(&note.note_id),
                        note.note_type.as_str()
                    );
                    if let Some(recipient) = &note.recipient {
                        line.push_str(&format!(" -> {}", shorten_hex(recipient)));
                    }
                    println!("{line}");
                }
            }
            if !entry.decode_warnings.is_empty() {
                println!(
                    "      (note details unavailable: {})",
                    entry.decode_warnings[0].reason
                );
            }
            println!();
        }

        match page.next_cursor {
            Some(next) => {
                let answer = editor
                    .readline("Load next page? (y/N): ")
                    .map_err(|e| format!("Input error: {}", e))?;
                if !answer.trim().eq_ignore_ascii_case("y") {
                    return Ok(());
                }
                cursor = Some(next);
                page_number += 1;
            }
            None => {
                print_info("End of history.");
                return Ok(());
            }
        }
    }
}
