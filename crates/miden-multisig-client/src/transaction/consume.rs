//! Note consumption transaction utilities.

use miden_client::transaction::{NoteArgs, TransactionRequest, TransactionRequestBuilder};
use miden_protocol::note::{Note, NoteId};
use miden_protocol::{Felt, Word};

use crate::MidenSdkClient;
use crate::error::{MultisigError, Result};

/// Fetches a slice of notes by ID from the client's local Miden store
/// and converts each `InputNoteRecord` to a `Note`. Returns
/// `LegacyConsumeNotesNoteMissing` for the first missing ID. Used by
/// both proposal creation (the proposer-local fetch in
/// `ProposalBuilder::build_consume_notes`) and the v1 verification
/// adapter below.
pub(crate) async fn fetch_notes_from_store(
    client: &MidenSdkClient,
    note_ids: &[NoteId],
) -> Result<Vec<Note>> {
    let mut notes: Vec<Note> = Vec::with_capacity(note_ids.len());
    for note_id in note_ids {
        let input_note_record = client
            .get_input_note(*note_id)
            .await
            .map_err(|e| MultisigError::MidenClient(format!("failed to fetch note: {}", e)))?
            .ok_or(MultisigError::LegacyConsumeNotesNoteMissing { note_id: *note_id })?;
        let note: Note = input_note_record.try_into().map_err(|e| {
            MultisigError::InvalidConfig(format!("failed to convert note record to note: {:?}", e))
        })?;
        notes.push(note);
    }
    Ok(notes)
}

/// Builds a consume-notes transaction request directly from a slice of
/// already-loaded `Note` objects. No local-store read is performed.
///
/// This is the v2 (issue #229) rebuild path: cosigners use it to verify
/// and execute a `consume_notes` proposal whose metadata carries the
/// serialized notes inline, eliminating the per-device IndexedDB
/// dependency of the legacy path.
///
/// Spec FR-005 / FR-013 / FR-014.
pub fn build_consume_notes_transaction_request_from_notes<I>(
    notes: Vec<Note>,
    salt: Word,
    signature_advice: I,
) -> Result<TransactionRequest>
where
    I: IntoIterator<Item = (Word, Vec<Felt>)>,
{
    if notes.is_empty() {
        return Err(MultisigError::InvalidConfig(
            "no notes specified for consumption".to_string(),
        ));
    }

    let note_and_args: Vec<(Note, Option<NoteArgs>)> =
        notes.into_iter().map(|n| (n, None)).collect();

    let mut builder = TransactionRequestBuilder::new()
        .input_notes(note_and_args)
        .auth_arg(salt);

    for (key, values) in signature_advice {
        builder = builder.extend_advice_map([(key, values)]);
    }

    builder.build().map_err(|e| {
        MultisigError::TransactionExecution(format!("failed to build transaction request: {}", e))
    })
}

/// Builds a consume-notes transaction request by fetching notes from
/// the client's local store. This is the legacy (v1) path used during
/// proposal creation (where the proposer is expected to hold the notes
/// locally — spec FR-012) and during v1 verification on transitional
/// builds.
///
/// On v2 proposals, callers should use
/// `build_consume_notes_transaction_request_from_notes` instead with
/// notes decoded from the signed metadata.
pub async fn build_consume_notes_transaction_request<I>(
    client: &MidenSdkClient,
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

    let notes = fetch_notes_from_store(client, &note_ids).await?;
    build_consume_notes_transaction_request_from_notes(notes, salt, signature_advice)
}
