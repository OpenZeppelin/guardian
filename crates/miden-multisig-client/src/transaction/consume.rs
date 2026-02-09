//! Note consumption transaction utilities.

use miden_client::Client;
use miden_client::transaction::{NoteArgs, TransactionRequest, TransactionRequestBuilder};
use miden_protocol::note::{Note, NoteId};
use miden_protocol::{Felt, Word};

use crate::error::{MultisigError, Result};

/// Builds a transaction request to consume notes.
///
/// Creates a transaction that will consume the specified notes, transferring their
/// assets to the multisig account.
///
/// # Arguments
///
/// * `client` - Miden client used to fetch full note objects from local store
/// * `note_ids` - IDs of the notes to consume
/// * `salt` - Salt for replay protection
/// * `signature_advice` - Iterator of (key, values) pairs for signature advice map
///
/// # Errors
///
/// Returns an error if:
/// - No note IDs are provided
/// - Any note is not found in the local store
/// - Any note cannot be converted to a full Note object (missing metadata)
pub async fn build_consume_notes_transaction_request<I>(
    client: &Client<()>,
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

    let mut notes: Vec<(Note, Option<NoteArgs>)> = Vec::new();
    for note_id in &note_ids {
        let input_note_record = client
            .get_input_note(*note_id)
            .await
            .map_err(|e| MultisigError::MidenClient(format!("failed to fetch note: {}", e)))?
            .ok_or_else(|| {
                MultisigError::InvalidConfig(format!(
                    "note not found in local store: {}",
                    note_id.to_hex()
                ))
            })?;

        let note: Note = input_note_record.try_into().map_err(|e| {
            MultisigError::InvalidConfig(format!("failed to convert note record to note: {:?}", e))
        })?;

        notes.push((note, None));
    }

    let mut builder = TransactionRequestBuilder::new()
        .input_notes(notes)
        .auth_arg(salt);

    for (key, values) in signature_advice {
        builder = builder.extend_advice_map([(key, values)]);
    }

    builder.build().map_err(|e| {
        MultisigError::TransactionExecution(format!("failed to build transaction request: {}", e))
    })
}
