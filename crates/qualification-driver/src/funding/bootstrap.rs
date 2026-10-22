use anyhow::anyhow;
use miden_client::note::NoteConsumptionStatus;
use miden_client::transaction::TransactionRequestBuilder;
use miden_protocol::account::AccountId;
use miden_protocol::note::Note;

use crate::environment::error_chain;

use super::network::MidenClient;

/// How many notes one consume transaction carries. Faucet top-ups arrive as
/// many small notes, and proving all of them in one transaction outlasts the
/// remote prover's connection: 84 notes timed out on every attempt, while
/// batches of ten each proved in seconds.
pub const NOTES_PER_TRANSACTION: usize = 10;

/// What one bootstrap did: the notes it consumed, and the notes it left for a
/// later run because the account cannot consume them yet.
pub struct Bootstrap {
    pub consumed: usize,
    pub deferred: usize,
}

/// Consumes the notes addressed to an account, which is what actually deploys
/// it and moves the funds into its vault. A note sent to an address is not a
/// balance: the account does not exist on chain until it transacts, and this
/// first transaction pays its own fee out of the note it consumes.
///
/// Each batch commits on its own, so a failure partway leaves the earlier
/// batches consumed and the rest waiting for the next attempt.
///
/// Only notes the account can consume now are batched. A note that becomes
/// consumable at a later block, such as a timelocked P2IDE, would fail its whole
/// batch, and since the note order is stable it would fail the same batch on
/// every run, so it is deferred instead.
pub async fn consume_pending_notes(
    client: &mut MidenClient,
    account_id: AccountId,
) -> anyhow::Result<Bootstrap> {
    client
        .sync_state()
        .await
        .map_err(|error| anyhow!("syncing before the bootstrap failed: {error}"))?;

    let consumable = client
        .get_consumable_notes(Some(account_id))
        .await
        .map_err(|error| anyhow!("listing consumable notes failed: {error}"))?;

    let mut notes: Vec<Note> = Vec::with_capacity(consumable.len());
    let mut deferred = 0;
    for (record, relevance) in &consumable {
        if !relevance
            .iter()
            .any(|(id, status)| *id == account_id && consumable_now(status))
        {
            deferred += 1;
            continue;
        }
        let note: Note = record
            .clone()
            .try_into()
            .map_err(|error| anyhow!("a consumable note could not be read as a note: {error:?}"))?;
        notes.push(note);
    }
    let waiting = notes.len();

    let mut consumed = 0;
    for batch in notes.chunks(NOTES_PER_TRANSACTION) {
        let request = TransactionRequestBuilder::new()
            .input_notes(batch.iter().cloned().map(|note| (note, None)))
            .build()
            .map_err(|error| anyhow!("building the consume request failed: {error}"))?;

        client
            .submit_new_transaction(account_id, request)
            .await
            .map_err(|error| {
                anyhow!(
                    "the bootstrap transaction failed after consuming {consumed} of {waiting} note(s): {}",
                    error_chain(&error)
                )
            })?;

        client.sync_state().await.map_err(|error| {
            anyhow!(
                "syncing after the bootstrap failed with {} of {waiting} note(s) submitted: {}",
                consumed + batch.len(),
                error_chain(&error)
            )
        })?;
        consumed += batch.len();
    }

    Ok(Bootstrap { consumed, deferred })
}

fn consumable_now(status: &NoteConsumptionStatus) -> bool {
    match status {
        NoteConsumptionStatus::Consumable | NoteConsumptionStatus::ConsumableWithAuthorization => {
            true
        }
        NoteConsumptionStatus::ConsumableAfter(_)
        | NoteConsumptionStatus::UnconsumableConditions
        | NoteConsumptionStatus::NeverConsumable(_) => false,
    }
}
