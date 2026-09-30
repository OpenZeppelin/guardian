use anyhow::anyhow;
use miden_client::transaction::{PaymentNoteDescription, TransactionRequestBuilder};
use miden_protocol::account::AccountId;
use miden_protocol::asset::{Asset, FungibleAsset};
use miden_protocol::note::NoteType;

use crate::environment::error_chain;

use super::network::MidenClient;

/// Sends `amount` of the given faucet's asset from the treasury to `recipient`.
///
/// Public notes, deliberately: a private note is only deliverable out of band,
/// and a recipient that never receives it reads as an unfunded account with no
/// way to tell that from a transfer that never happened.
pub async fn send(
    client: &mut MidenClient,
    treasury: AccountId,
    recipient: AccountId,
    faucet: AccountId,
    amount: u64,
) -> anyhow::Result<()> {
    let asset = FungibleAsset::new(faucet, amount)
        .map_err(|error| anyhow!("{amount} of {faucet} is not a valid asset: {error}"))?;

    let payment = PaymentNoteDescription::new(vec![Asset::from(asset)], treasury, recipient);

    let request = TransactionRequestBuilder::new()
        .build_pay_to_id(payment, NoteType::Public, client.rng())
        .map_err(|error| anyhow!("building the funding transfer failed: {error}"))?;

    client
        .submit_new_transaction(treasury, request)
        .await
        .map_err(|error| anyhow!("the funding transfer failed: {}", error_chain(&error)))?;

    client.sync_state().await.map_err(|error| {
        anyhow!(
            "syncing after the funding transfer failed: {}",
            error_chain(&error)
        )
    })?;

    Ok(())
}
