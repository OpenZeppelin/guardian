//! Note consumption transaction utilities.

use miden_client::transaction::{TransactionRequest, TransactionRequestBuilder};
use miden_objects::note::NoteId;
use miden_objects::{Felt, Word};

use crate::error::{MultisigError, Result};

/// Builds a transaction request to consume notes.
///
/// Creates a transaction that will consume the specified notes, transferring their
/// assets to the multisig account.
///
/// # Arguments
///
/// * `note_ids` - IDs of the notes to consume
/// * `salt` - Salt for replay protection
/// * `signature_advice` - Iterator of (key, values) pairs for signature advice map
pub fn build_consume_notes_transaction_request<I>(
    note_ids: Vec<NoteId>,
    salt: Word,
    signature_advice: I,
) -> Result<TransactionRequest>
where
    I: IntoIterator<Item = (Word, Vec<Felt>)>,
{
    if note_ids.is_empty() {
        return Err(MultisigError::InvalidConfig(
            "no notes specified for consumption".to_string(),
        ));
    }

    let request = TransactionRequestBuilder::new()
        .authenticated_input_notes(note_ids.iter().map(|id| (*id, None)).collect::<Vec<_>>())
        .extend_advice_map(signature_advice)
        .auth_arg(salt)
        .build()?;

    Ok(request)
}
