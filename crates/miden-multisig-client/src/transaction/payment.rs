//! Payment transaction utilities.
//!
//! Functions for building P2ID (pay-to-id) and other payment transactions.

use miden_client::transaction::{OutputNote, TransactionRequest, TransactionRequestBuilder};
use miden_standards::note::create_p2id_note;
use miden_protocol::account::AccountId;
use miden_protocol::asset::Asset;
use miden_protocol::crypto::rand::RpoRandomCoin;
use miden_protocol::note::NoteType;
use miden_protocol::{Felt, Word};

use crate::error::{MultisigError, Result};

/// Builds a P2ID transaction request.
///
/// Creates a pay-to-id note and builds a transaction request to send it.
pub fn build_p2id_transaction_request<I>(
    sender_id: AccountId,
    recipient: AccountId,
    assets: Vec<Asset>,
    salt: Word,
    signature_advice: I,
) -> Result<TransactionRequest>
where
    I: IntoIterator<Item = (Word, Vec<Felt>)>,
{
    let mut rng = RpoRandomCoin::new(salt);

    let note = create_p2id_note(
        sender_id,
        recipient,
        assets,
        NoteType::Public,
        Default::default(),
        &mut rng,
    )
    .map_err(|e| {
        MultisigError::TransactionExecution(format!("failed to create P2ID note: {}", e))
    })?;

    // Build the transaction request using own_output_notes
    let request = TransactionRequestBuilder::new()
        .own_output_notes(vec![OutputNote::Full(note)])
        .extend_advice_map(signature_advice)
        .auth_arg(salt)
        .build()?;

    Ok(request)
}
