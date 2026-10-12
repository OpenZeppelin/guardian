use anyhow::anyhow;
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

/// Consumes the notes addressed to an account, which is what actually deploys
/// it and moves the funds into its vault. A note sent to an address is not a
/// balance: the account does not exist on chain until it transacts, and this
/// first transaction pays its own fee out of the note it consumes.
///
/// Each batch commits on its own, so a failure partway leaves the earlier
/// batches consumed and the rest waiting for the next attempt.
pub async fn consume_pending_notes(
    client: &mut MidenClient,
    account_id: AccountId,
) -> anyhow::Result<usize> {
    client
        .sync_state()
        .await
        .map_err(|error| anyhow!("syncing before the bootstrap failed: {error}"))?;

    let consumable = client
        .get_consumable_notes(Some(account_id))
        .await
        .map_err(|error| anyhow!("listing consumable notes failed: {error}"))?;

    let mut notes: Vec<Note> = Vec::with_capacity(consumable.len());
    for (record, _relevance) in &consumable {
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
                "syncing after the bootstrap failed: {}",
                error_chain(&error)
            )
        })?;
        consumed += batch.len();
    }

    Ok(consumed)
}
