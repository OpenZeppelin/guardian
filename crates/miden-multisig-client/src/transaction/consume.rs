//! Note consumption transaction utilities.

use std::collections::BTreeMap;
use std::num::NonZeroU16;
use std::sync::Arc;

use miden_client::note::NoteFile;
use miden_client::rpc::NodeRpcClient;
use miden_client::store::{InputNoteRecord, NoteFilter as StoreNoteFilter};
use miden_client::transaction::{NoteArgs, TransactionRequest, TransactionRequestBuilder};
use miden_protocol::note::{Note, NoteId, NoteInclusionProof};
use miden_protocol::transaction::InputNote;
use miden_protocol::{Felt, Word};
use miden_standards::account::auth::MultisigAuthArgs;

use super::TransactionRequestBuilderExt;
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
            .map_err(|e| MultisigError::miden_client_with_context("failed to fetch note", e))?
            .ok_or(MultisigError::LegacyConsumeNotesNoteMissing { note_id: *note_id })?;
        let note: Note = input_note_record.try_into().map_err(|e| {
            MultisigError::InvalidConfig(format!("failed to convert note record to note: {:?}", e))
        })?;
        notes.push(note);
    }
    Ok(notes)
}

/// Builds a consume-notes transaction request directly from already-loaded notes, applying
/// `expiration_delta`. No local-store read is performed; cosigners rebuild a v2 `consume_notes`
/// proposal this way from the notes its metadata carries.
pub fn build_consume_notes_transaction_request_from_notes<I>(
    notes: Vec<Note>,
    auth_args: &MultisigAuthArgs,
    signature_advice: I,
    expiration_delta: Option<NonZeroU16>,
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

    let mut builder = TransactionRequestBuilder::new().input_notes(note_and_args);

    for (key, values) in signature_advice {
        builder = builder.extend_advice_map([(key, values)]);
    }
    if let Some(delta) = expiration_delta {
        builder = builder.expiration_delta(delta.get());
    }

    builder.multisig_auth_args(auth_args).build().map_err(|e| {
        MultisigError::TransactionExecution(format!("failed to build transaction request: {}", e))
    })
}

/// A consume-notes request whose notes are pinned as authenticated, with their inclusion proofs,
/// so any party executes it in the same mode without consulting its own store. This is the
/// request a Guardian-executable proposal stores; call [`ensure_notes_authenticated`] first.
pub(crate) async fn build_pinned_consume_notes_transaction_request(
    client: &MidenSdkClient,
    notes: &[Note],
    auth_args: &MultisigAuthArgs,
    expiration_delta: Option<NonZeroU16>,
) -> Result<TransactionRequest> {
    let records: BTreeMap<NoteId, InputNoteRecord> = client
        .get_input_notes(StoreNoteFilter::List(notes.iter().map(Note::id).collect()))
        .await
        .map_err(|e| MultisigError::miden_client_with_context("failed to read input notes", e))?
        .into_iter()
        .filter_map(|record| record.id().map(|id| (id, record)))
        .collect();
    let mut pinned = Vec::with_capacity(notes.len());
    for note in notes {
        let proof = records
            .get(&note.id())
            .and_then(InputNoteRecord::inclusion_proof)
            .ok_or_else(|| MultisigError::ConsumeNoteNotAuthenticated {
                note_id: note.id(),
                reason: "the local store holds no inclusion proof to pin it with".to_string(),
            })?;
        pinned.push((InputNote::authenticated(note.clone(), proof.clone()), None));
    }
    let mut builder = TransactionRequestBuilder::new().explicit_input_notes(pinned);
    if let Some(delta) = expiration_delta {
        builder = builder.expiration_delta(delta.get());
    }
    builder.multisig_auth_args(auth_args).build().map_err(|e| {
        MultisigError::TransactionExecution(format!("failed to build transaction request: {}", e))
    })
}

/// Makes every note in `notes` an *authenticated* input note in the client's
/// local store: present, with its on-chain inclusion proof and block header.
///
/// miden-client decides per input note, at execution time and from the local
/// store alone, whether it is consumed as authenticated (store holds a proof)
/// or unauthenticated (anything else). The two modes commit differently into
/// the transaction summary — `hash(nullifier || note_id_or_ZERO)` — so the
/// proposer and every verifier must be in the same mode or the summary
/// commitment, and with it the proposal id, differs (issue #409: a fresh
/// cosigner whose store had never seen the notes failed with "metadata does
/// not match tx_summary"). Authenticated is the canonical mode: proposal
/// creation and every rebuild call this first, so the executed transaction is
/// the one the cosigners signed regardless of what each store held before.
///
/// Notes already authenticated locally are left alone. For the rest the
/// inclusion proofs come from the node in one round trip (the node serves
/// proofs for private notes too) and each note is imported as committed,
/// which also upgrades a proof-less record the store already tracked.
///
/// # Errors
///
/// `ConsumeNoteNotAuthenticated` when a note is not committed on chain yet,
/// the node does not serve its proof, the import fails, or the record still
/// lacks authentication after a sync.
pub(crate) async fn ensure_notes_authenticated(
    client: &mut MidenSdkClient,
    node_rpc: &Arc<dyn NodeRpcClient>,
    notes: &[Note],
) -> Result<()> {
    let pending = unauthenticated_notes(client, notes).await?;
    if pending.is_empty() {
        return Ok(());
    }

    let ids: Vec<NoteId> = pending.iter().map(Note::id).collect();
    let fetched = node_rpc.get_notes_by_id(&ids).await.map_err(|e| {
        MultisigError::ConsumeNoteNotAuthenticated {
            note_id: ids[0],
            reason: format!("failed to fetch inclusion proofs from the node: {e}"),
        }
    })?;
    let mut proofs: BTreeMap<NoteId, NoteInclusionProof> = fetched
        .iter()
        .map(|f| (f.id(), f.inclusion_proof().clone()))
        .collect();

    for note in pending {
        let note_id = note.id();
        let Some(proof) = proofs.remove(&note_id) else {
            return Err(MultisigError::ConsumeNoteNotAuthenticated {
                note_id,
                reason: "the node has no inclusion proof for it (not committed on chain yet)"
                    .to_string(),
            });
        };
        client
            .import_notes(&[NoteFile::Committed { note, proof }])
            .await
            .map_err(|e| MultisigError::ConsumeNoteNotAuthenticated {
                note_id,
                reason: format!("failed to import it with its inclusion proof: {e}"),
            })?;
    }

    // The import authenticates a note committed at or below the client's
    // sync height; a newer one lands unverified until a sync fetches its
    // block header. A cosigner that just pulled the account is typically
    // behind the note's block, so sync once and re-check before failing.
    if !unauthenticated_notes(client, notes).await?.is_empty() {
        client.sync_state().await.map_err(|e| {
            MultisigError::miden_client_with_context(
                "failed to sync while authenticating consume-notes input notes",
                e,
            )
        })?;
    }
    if let Some(still) = unauthenticated_notes(client, notes).await?.first() {
        return Err(MultisigError::ConsumeNoteNotAuthenticated {
            note_id: still.id(),
            reason: "its inclusion proof was imported but the local store could not \
                     verify it against the chain even after a sync"
                .to_string(),
        });
    }
    Ok(())
}

/// The subset of `notes` whose local record is missing or carries no
/// inclusion proof, in the order given.
async fn unauthenticated_notes(client: &MidenSdkClient, notes: &[Note]) -> Result<Vec<Note>> {
    let records: BTreeMap<NoteId, InputNoteRecord> = client
        .get_input_notes(StoreNoteFilter::List(notes.iter().map(Note::id).collect()))
        .await
        .map_err(|e| MultisigError::miden_client_with_context("failed to read local notes", e))?
        .into_iter()
        .filter_map(|record| record.id().map(|id| (id, record)))
        .collect();
    Ok(notes
        .iter()
        .filter(|note| {
            !records
                .get(&note.id())
                .is_some_and(InputNoteRecord::is_authenticated)
        })
        .cloned()
        .collect())
}
