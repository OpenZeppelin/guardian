//! Payment transaction utilities.
//!
//! Functions for building P2ID (pay-to-id) and other payment transactions.

use miden_client::transaction::{TransactionRequest, TransactionRequestBuilder};
use miden_lib::account::interface::AccountInterface;
use miden_lib::note::create_p2id_note;
use miden_objects::account::{Account, AccountId};
use miden_objects::asset::Asset;
use miden_objects::crypto::rand::RpoRandomCoin;
use miden_objects::note::NoteType;
use miden_objects::{Felt, Word};

use crate::error::{MultisigError, Result};

/// Builds a P2ID transaction request.
///
/// Creates a pay-to-id note and builds a transaction request to send it.
/// This uses the low-level `create_p2id_note` and `build_send_notes_script`
pub fn build_p2id_transaction_request<I>(
    sender_account: &Account,
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
        sender_account.id(),
        recipient,
        assets,
        NoteType::Public,
        Default::default(),
        &mut rng,
    )
    .map_err(|e| {
        MultisigError::TransactionExecution(format!("failed to create P2ID note: {}", e))
    })?;

    // Build the send notes script using AccountInterface
    let account_interface = AccountInterface::from(sender_account);
    let send_script = account_interface
        .build_send_notes_script(&[note.clone().into()], None, false)
        .map_err(|e| {
            MultisigError::TransactionExecution(format!("failed to build send script: {}", e))
        })?;

    // Build the transaction request with signature advice
    let request = TransactionRequestBuilder::new()
        .custom_script(send_script)
        .expected_output_recipients(vec![note.recipient().clone()])
        .extend_advice_map(signature_advice)
        .auth_arg(salt)
        .build()?;

    Ok(request)
}
