use std::collections::BTreeMap;

use miden_multisig_client::{NoteImportOutcome, NoteRecoveryReport};

use crate::display::{print_info, print_section, print_success, print_waiting, print_warning};
use crate::state::SessionState;

/// Runs the wallet-facing note-recovery flow (`recover_notes`): transport
/// drain, proposal-embedded note import, historical public-note backfill,
/// and a final verifying sync.
pub async fn action_recover_notes(state: &mut SessionState) -> Result<(), String> {
    print_section("Recover Notes");
    print_waiting("Running the recovery strategies (transport drain, proposal import, backfill)");

    let client = state.get_client_mut()?;
    let report = client
        .recover_notes(None)
        .await
        .map_err(|e| format!("Note recovery failed to start: {}", e))?;

    print_report(&report);
    Ok(())
}

fn print_report(report: &NoteRecoveryReport) {
    match &report.transport {
        Some(transport) => print_info(&format!(
            "Transport drain: {:?} — {} note(s) imported{}",
            transport.status,
            transport.imported,
            transport
                .reason
                .as_deref()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default(),
        )),
        None => print_info("Transport drain: skipped"),
    }

    match &report.proposal_import {
        Some(outcomes) => print_info(&format!(
            "Proposal import: {} embedded note(s){}",
            outcomes.len(),
            outcome_summary(outcomes),
        )),
        None => print_info("Proposal import: skipped"),
    }

    match &report.backfill {
        Some(backfill) => {
            print_info(&format!(
                "Public backfill: scanned blocks [{}, {}] — {} discovered, {} private skipped, \
                 {} irrelevant skipped{}",
                backfill.scanned_from,
                backfill.scanned_to,
                backfill.discovered,
                backfill.skipped_private,
                backfill.skipped_irrelevant,
                outcome_summary(&backfill.outcomes),
            ));
            for range in &backfill.uncovered {
                print_warning(&format!(
                    "  Blocks [{}, {}] could not be scanned",
                    range.from, range.to
                ));
            }
        }
        None => print_info("Public backfill: skipped"),
    }

    for problem in &report.problems {
        print_warning(&format!(
            "Step {} did not run: {}",
            problem.step, problem.reason
        ));
    }
    if report.synced {
        print_info("Verifying sync completed");
    }

    print_success(&format!(
        "Recovered {} note record(s) in total",
        report.imported
    ));
    if report.retryable {
        print_info("Some steps are retryable — rerunning this action may recover more.");
    }
}

/// Renders per-status counts like ": 2 imported, 1 already-present".
fn outcome_summary(outcomes: &[NoteImportOutcome]) -> String {
    if outcomes.is_empty() {
        return String::new();
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for outcome in outcomes {
        *counts.entry(outcome.status.as_str()).or_default() += 1;
    }
    let rendered: Vec<String> = counts
        .iter()
        .map(|(status, count)| format!("{count} {status}"))
        .collect();
    format!(": {}", rendered.join(", "))
}
