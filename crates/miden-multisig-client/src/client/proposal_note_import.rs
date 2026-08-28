//! Recovery primitives for MultisigClient.
//!
//! After key-based recovery the local Miden store starts empty, so notes the
//! account was in the middle of consuming are gone. v2 `consume_notes`
//! proposals embed the serialized notes they consume, which makes
//! pending proposals opportunistic recovery material: this module rebuilds
//! importable notes from those embedded bytes plus a node-fetched inclusion
//! proof, without needing the node to hold the note body — so it works for
//! private notes too.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use miden_client::ClientError;
use miden_client::note::{NoteFile, NoteSyncHint};
use miden_client::store::{InputNoteRecord, NoteFilter as StoreNoteFilter};
use miden_protocol::block::BlockNumber;
use miden_protocol::note::{Note, NoteDetailsCommitment, NoteId, NoteInclusionProof};

use super::MultisigClient;
use crate::proposal::{Proposal, ProposalMetadata};

/// Where a recovered note's bytes came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteImportSource {
    /// Embedded in a v2 `consume_notes` proposal (`consume_notes_notes`).
    Proposal,
    /// Discovered on chain by a tag-scoped historical scan
    /// ([`MultisigClient::backfill_public_notes_by_tag`]).
    Backfill,
}

impl NoteImportSource {
    /// Stable string form, shared with the TS SDK's `source` union.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Proposal => "proposal",
            Self::Backfill => "backfill",
        }
    }
}

impl std::fmt::Display for NoteImportSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-note result of a recovery import attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteImportStatus {
    /// Note imported with its on-chain inclusion proof; it lands in the
    /// store's `Unverified` state and the next sync verifies it.
    Imported,
    /// The local store already tracks this note (not yet consumed).
    AlreadyPresent,
    /// The note is already consumed — either the local store tracked it as
    /// consumed, or the chain had nullified it and the import recorded it as
    /// consumption history rather than a consumable note.
    AlreadyConsumed,
    /// The chain does not know the note yet. Its details were recorded in
    /// `Expected` state with its tag tracked, so a later sync picks it up
    /// once it commits.
    NotCommitted,
    /// The embedded bytes could not be decoded into a note.
    Invalid,
    /// The import attempt failed (store or RPC error).
    Failed,
}

impl NoteImportStatus {
    /// Stable string form, shared with the TS SDK's `status` union.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Imported => "imported",
            Self::AlreadyPresent => "already-present",
            Self::AlreadyConsumed => "already-consumed",
            Self::NotCommitted => "not-committed",
            Self::Invalid => "invalid",
            Self::Failed => "failed",
        }
    }
}

impl std::fmt::Display for NoteImportStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Outcome of one unique embedded note's recovery import. A batch of outcomes
/// is the full report of [`MultisigClient::import_notes_from_proposals`]; no
/// per-note problem aborts the batch. A note embedded by several proposals is
/// deduplicated into a single outcome (its first occurrence).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteImportOutcome {
    /// The note ID hex when the bytes decoded, otherwise a positional
    /// reference into the proposal (`proposal <id> notes[<i>]`).
    pub identifier: String,
    /// Where the note bytes came from.
    pub source: NoteImportSource,
    /// What happened to this note.
    pub status: NoteImportStatus,
    /// Whether retrying the import later can change the status (transient
    /// RPC failures, notes not yet committed).
    pub retryable: bool,
    /// Human-readable detail for non-success statuses — or, on an `Imported`
    /// outcome, a warning that the post-import consumed-state check failed
    /// and a sync should confirm the note's status.
    pub reason: Option<String>,
}

/// Decodes, validates, and deduplicates the embedded notes of v2
/// `consume_notes` proposals. Undecodable entries, embeddings past the
/// declared `note_ids` list, and embeddings whose decoded ID disagrees with
/// the declared one are reported as `Invalid` outcomes with a positional
/// identifier; a note embedded by several proposals folds into its first
/// occurrence.
///
/// The per-index ID binding is enforced here even though the verify/execute
/// path checks it again before signing: recovery runs automatically over
/// synced proposals, so without the binding a malformed or adversarial
/// proposal could smuggle arbitrary notes (and, for uncommitted ones,
/// persistent expected records and tag subscriptions) into the local store.
/// Only notes the signed proposal actually declares are recovery material.
fn collect_notes<'a>(
    proposals: impl IntoIterator<Item = (&'a str, &'a ProposalMetadata)>,
    outcomes: &mut Vec<NoteImportOutcome>,
) -> Vec<Note> {
    let mut seen: HashSet<NoteId> = HashSet::new();
    let mut decoded = Vec::new();
    for (proposal_id, metadata) in proposals {
        if !metadata.is_consume_notes_v2() {
            continue;
        }
        for (index, serialized) in metadata.consume_notes_notes.iter().enumerate() {
            let identifier = format!("proposal {} notes[{}]", proposal_id, index);
            let Some(declared_id) = metadata.note_ids_hex.get(index) else {
                outcomes.push(NoteImportOutcome {
                    identifier,
                    source: NoteImportSource::Proposal,
                    status: NoteImportStatus::Invalid,
                    retryable: false,
                    reason: Some(
                        "embedded note has no matching entry in the proposal's declared \
                         note ids"
                            .to_string(),
                    ),
                });
                continue;
            };
            match serialized.to_note() {
                Ok(note) => {
                    if !crate::utils::hex_body_eq(&note.id().to_hex(), declared_id) {
                        outcomes.push(NoteImportOutcome {
                            identifier,
                            source: NoteImportSource::Proposal,
                            status: NoteImportStatus::Invalid,
                            retryable: false,
                            reason: Some(format!(
                                "embedded note decodes to {} but the proposal declares {}",
                                note.id().to_hex(),
                                declared_id
                            )),
                        });
                        continue;
                    }
                    if seen.insert(note.id()) {
                        decoded.push(note);
                    }
                }
                Err(e) => outcomes.push(NoteImportOutcome {
                    identifier,
                    source: NoteImportSource::Proposal,
                    status: NoteImportStatus::Invalid,
                    retryable: false,
                    reason: Some(format!("failed to decode embedded note: {}", e)),
                }),
            }
        }
    }
    decoded
}

/// Import errors are usually local store problems, but upstream
/// `import_notes` performs node RPC internally (nullifier checks, block
/// header fetches), so a transient RPC failure mid-import is retryable.
fn import_error_retryable(error: &ClientError) -> bool {
    matches!(error, ClientError::RpcError(rpc) if crate::rpc::is_transient_rpc_error(rpc))
}

impl MultisigClient {
    /// Looks up existing store records for the given notes, keyed by details
    /// commitment rather than note ID: records the store keeps without
    /// metadata (a note details import in `Expected` state, or a note
    /// observed as consumed on chain) have no note ID, and an ID lookup
    /// would keep re-importing them forever. Shared by the recovery
    /// primitives (proposal import and public backfill).
    pub(crate) async fn existing_records_by_commitment(
        &mut self,
        commitments: Vec<NoteDetailsCommitment>,
    ) -> std::result::Result<BTreeMap<NoteDetailsCommitment, InputNoteRecord>, ClientError> {
        Ok(self
            .miden_client
            .get_input_notes(StoreNoteFilter::DetailsCommitments(commitments))
            .await?
            .into_iter()
            .map(|record| (record.details_commitment(), record))
            .collect())
    }

    /// Imports one note with its inclusion proof and classifies the result.
    /// Upstream `import_notes` batches are atomic, which is why callers
    /// import individually — one bad note must not sink the rest. Returns
    /// the outcome and whether the import succeeded (input for the batched
    /// consumed-state re-check).
    pub(crate) async fn import_note_with_proof(
        &mut self,
        source: NoteImportSource,
        note: Note,
        proof: NoteInclusionProof,
    ) -> (NoteImportOutcome, bool) {
        let identifier = note.id().to_hex();
        let file = NoteFile::Committed { note, proof };
        match self
            .miden_client
            .import_notes(std::slice::from_ref(&file))
            .await
        {
            Ok(_) => (
                NoteImportOutcome {
                    identifier,
                    source,
                    status: NoteImportStatus::Imported,
                    retryable: false,
                    reason: None,
                },
                true,
            ),
            Err(e) => (
                NoteImportOutcome {
                    identifier,
                    source,
                    status: NoteImportStatus::Failed,
                    retryable: import_error_retryable(&e),
                    reason: Some(format!("failed to import note: {}", e)),
                },
                false,
            ),
        }
    }

    /// Re-classifies provisionally `Imported` outcomes from the records the
    /// import actually left behind. A note the chain had already nullified
    /// is stored as consumption history, not as a consumable note — reported
    /// as `AlreadyConsumed`. A note whose inclusion proof failed
    /// verification against the authenticated block header is stored in
    /// `Invalid` state by upstream while the import still returns `Ok` —
    /// reported as `Failed`, because "recovered" notes that can never be
    /// consumed must not count as recovered. One batched store read covers
    /// every imported note. A failed check downgrades nothing; it flags the
    /// outcome's classification as unconfirmed instead.
    pub(crate) async fn reclassify_consumed_imports(
        &mut self,
        imported: &[(usize, NoteDetailsCommitment)],
        outcomes: &mut [NoteImportOutcome],
    ) {
        use miden_client::store::InputNoteState;

        if imported.is_empty() {
            return;
        }
        let check = self
            .miden_client
            .get_input_notes(StoreNoteFilter::DetailsCommitments(
                imported.iter().map(|(_, commitment)| *commitment).collect(),
            ))
            .await;
        match check {
            Ok(records) => {
                let mut consumed: BTreeSet<NoteDetailsCommitment> = BTreeSet::new();
                let mut invalid: BTreeSet<NoteDetailsCommitment> = BTreeSet::new();
                for record in &records {
                    if record.is_consumed() {
                        consumed.insert(record.details_commitment());
                    } else if matches!(record.state(), InputNoteState::Invalid(_)) {
                        invalid.insert(record.details_commitment());
                    }
                }
                for (index, commitment) in imported {
                    if consumed.contains(commitment) {
                        outcomes[*index].status = NoteImportStatus::AlreadyConsumed;
                        outcomes[*index].reason = Some(
                            "note was already consumed on chain; recorded as consumption \
                             history"
                                .to_string(),
                        );
                    } else if invalid.contains(commitment) {
                        outcomes[*index].status = NoteImportStatus::Failed;
                        outcomes[*index].retryable = false;
                        outcomes[*index].reason = Some(
                            "the note's inclusion proof failed verification against the \
                             authenticated block header; the record is stored as invalid \
                             and the note is not consumable"
                                .to_string(),
                        );
                    }
                }
            }
            Err(e) => {
                // The imports themselves succeeded; stay `Imported` but
                // flag that the post-import state check is unknown.
                for (index, _) in imported {
                    outcomes[*index].reason = Some(format!(
                        "imported, but the post-import state check failed ({}); run sync \
                         to confirm the note's status",
                        e
                    ));
                }
            }
        }
    }
}

impl MultisigClient {
    /// Imports the notes embedded in v2 `consume_notes` proposals into the
    /// local Miden store, typically after key-based recovery
    /// rebuilt the proposal list but left the note store empty.
    ///
    /// Proposals are opportunistic recovery material, not a backup: v1
    /// proposals carry no note bytes, and proposals disappear once
    /// canonicalized, so only notes still mid-consumption are recoverable
    /// this way.
    ///
    /// Per note: decode the embedded bytes, skip notes the store already
    /// tracks, fetch the on-chain inclusion proof, and import the note
    /// individually (upstream `import_notes` batches are atomic, so one bad
    /// note must not sink the rest). A note the chain does not know yet is
    /// recorded in `Expected` state with its tag tracked so a later sync
    /// picks it up, and is reported as `NotCommitted`/retryable. A note the
    /// chain has already nullified is recorded as consumption history and
    /// reported `AlreadyConsumed`.
    ///
    /// The returned outcomes cover every unique embedded note — a note
    /// embedded by several proposals yields one outcome, not one per
    /// embedding — and no per-note problem aborts the batch, which is why
    /// this returns a plain `Vec` instead of `Result`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let proposals = client.list_proposals().await?;
    /// for outcome in client.import_notes_from_proposals(&proposals).await {
    ///     println!("{}: {}", outcome.identifier, outcome.status);
    /// }
    /// client.sync().await?;
    /// ```
    pub(crate) async fn import_notes_from_proposals(
        &mut self,
        proposals: &[Proposal],
    ) -> Vec<NoteImportOutcome> {
        let mut outcomes = Vec::new();
        let decoded = collect_notes(
            proposals.iter().map(|p| (p.id.as_str(), &p.metadata)),
            &mut outcomes,
        );
        if decoded.is_empty() {
            return outcomes;
        }

        let commitments: Vec<NoteDetailsCommitment> =
            decoded.iter().map(Note::details_commitment).collect();
        let existing = match self.existing_records_by_commitment(commitments).await {
            Ok(existing) => existing,
            Err(e) => {
                let reason = format!("failed to read local store: {}", e);
                for note in &decoded {
                    outcomes.push(NoteImportOutcome {
                        identifier: note.id().to_hex(),
                        source: NoteImportSource::Proposal,
                        status: NoteImportStatus::Failed,
                        retryable: false,
                        reason: Some(reason.clone()),
                    });
                }
                return outcomes;
            }
        };

        let mut pending: Vec<Note> = Vec::new();
        for note in decoded {
            match existing.get(&note.details_commitment()) {
                Some(record) => {
                    let status = if record.is_consumed() {
                        NoteImportStatus::AlreadyConsumed
                    } else {
                        NoteImportStatus::AlreadyPresent
                    };
                    outcomes.push(NoteImportOutcome {
                        identifier: note.id().to_hex(),
                        source: NoteImportSource::Proposal,
                        status,
                        retryable: false,
                        reason: None,
                    });
                }
                None => pending.push(note),
            }
        }

        if pending.is_empty() {
            return outcomes;
        }

        // One round trip for all missing notes; only the import itself is
        // per-note. The node returns proofs for private notes too, so the
        // locally-held bytes are the only body this path ever needs.
        let ids: Vec<NoteId> = pending.iter().map(Note::id).collect();
        let fetched = match self.node_rpc_client().get_notes_by_id(&ids).await {
            Ok(fetched) => fetched,
            Err(e) => {
                let retryable = crate::rpc::is_transient_rpc_error(&e);
                let reason = format!("failed to fetch inclusion proofs: {}", e);
                for note in &pending {
                    outcomes.push(NoteImportOutcome {
                        identifier: note.id().to_hex(),
                        source: NoteImportSource::Proposal,
                        status: NoteImportStatus::Failed,
                        retryable,
                        reason: Some(reason.clone()),
                    });
                }
                return outcomes;
            }
        };
        let mut proofs: BTreeMap<NoteId, NoteInclusionProof> = fetched
            .iter()
            .map(|f| (f.id(), f.inclusion_proof().clone()))
            .collect();

        // Provisionally `Imported` outcomes, re-classified in one batched
        // consumed-state check below.
        let mut imported: Vec<(usize, NoteDetailsCommitment)> = Vec::new();

        for note in pending {
            let identifier = note.id().to_hex();
            let outcome = match proofs.remove(&note.id()) {
                Some(proof) => {
                    let details_commitment = note.details_commitment();
                    let (outcome, was_imported) = self
                        .import_note_with_proof(NoteImportSource::Proposal, note, proof)
                        .await;
                    if was_imported {
                        imported.push((outcomes.len(), details_commitment));
                    }
                    outcome
                }
                None => {
                    // The sync hint's tag makes the resulting `Expected`
                    // record reachable by sync (upstream registers a
                    // note-source tag record from it).
                    let tag = note.metadata().tag();
                    let file = NoteFile::ExpectedNote {
                        details: note.into(),
                        sync_hint: NoteSyncHint::new(BlockNumber::from(0u32), tag),
                    };
                    match self
                        .miden_client
                        .import_notes(std::slice::from_ref(&file))
                        .await
                    {
                        Ok(_) => NoteImportOutcome {
                            identifier,
                            source: NoteImportSource::Proposal,
                            status: NoteImportStatus::NotCommitted,
                            retryable: true,
                            reason: Some(
                                "note not yet committed on chain; recorded as expected so a \
                                 later sync picks it up"
                                    .to_string(),
                            ),
                        },
                        Err(e) => NoteImportOutcome {
                            identifier,
                            source: NoteImportSource::Proposal,
                            status: NoteImportStatus::Failed,
                            retryable: import_error_retryable(&e),
                            reason: Some(format!("failed to record expected note: {}", e)),
                        },
                    }
                }
            };
            outcomes.push(outcome);
        }

        self.reclassify_consumed_imports(&imported, &mut outcomes)
            .await;

        outcomes
    }
}

#[cfg(test)]
mod tests {
    use miden_protocol::account::delta::{AccountDelta, AccountVaultDelta};
    use miden_protocol::account::{AccountId, AccountStoragePatch};
    use miden_protocol::asset::FungibleAsset;
    use miden_protocol::crypto::rand::RandomCoin;
    use miden_protocol::note::NoteType;
    use miden_protocol::{Felt, Word};
    use miden_standards::note::P2idNote;

    use super::*;
    use crate::proposal::SerializedNote;

    fn build_test_note(seed: u64) -> Note {
        let sender = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();
        let target = AccountId::from_hex("0x1b1b1b1a1b1b1b011b1b1b1b1b1b1b").unwrap();
        let mut rng = RandomCoin::new(Word::from([Felt::new_unchecked(seed); 4]));
        P2idNote::builder()
            .sender(sender)
            .target(target)
            .asset(FungibleAsset::mock(1))
            .note_type(NoteType::Private)
            .generate_serial_number(&mut rng)
            .build()
            .unwrap()
            .into()
    }

    /// v2 metadata with explicit `(embedded bytes, declared note id)` pairs,
    /// for driving the binding check with mismatches and undecodable bytes.
    fn v2_metadata_entries(entries: Vec<(SerializedNote, String)>) -> ProposalMetadata {
        ProposalMetadata {
            consume_notes_metadata_version: Some(
                crate::proposal::CONSUME_NOTES_METADATA_VERSION_V2,
            ),
            note_ids_hex: entries.iter().map(|(_, id)| id.clone()).collect(),
            consume_notes_notes: entries.into_iter().map(|(note, _)| note).collect(),
            ..Default::default()
        }
    }

    /// Well-formed v2 metadata: embedded bytes with their real note ids
    /// declared, the shape proposal creation emits.
    fn v2_metadata(notes: &[&Note]) -> ProposalMetadata {
        v2_metadata_entries(
            notes
                .iter()
                .map(|note| (SerializedNote::from_note(note), note.id().to_hex()))
                .collect(),
        )
    }

    #[test]
    fn collects_decoded_notes_from_v2_metadata() {
        let note = build_test_note(1);
        let expected_id = note.id();
        let metadata = v2_metadata(&[&note]);

        let mut outcomes = Vec::new();
        let decoded = collect_notes([("p-1", &metadata)], &mut outcomes);
        assert!(outcomes.is_empty());
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].id(), expected_id);
    }

    /// An embedded note whose decoded ID disagrees with the declared one is
    /// not recovery material: recovery runs automatically over synced
    /// proposals, so the binding is what keeps a malformed or adversarial
    /// proposal from smuggling arbitrary notes into the store.
    #[test]
    fn embedded_note_mismatching_the_declared_id_is_invalid() {
        let declared = build_test_note(1);
        let smuggled = build_test_note(2);
        let metadata = v2_metadata_entries(vec![(
            SerializedNote::from_note(&smuggled),
            declared.id().to_hex(),
        )]);

        let mut outcomes = Vec::new();
        let decoded = collect_notes([("p-1", &metadata)], &mut outcomes);
        assert!(decoded.is_empty(), "mismatched note must not be collected");
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].status, NoteImportStatus::Invalid);
        assert!(!outcomes[0].retryable);
        assert!(outcomes[0].reason.as_deref().unwrap().contains("declares"));
    }

    /// An embedding past the declared note-id list has no binding to check
    /// against, so it is rejected rather than trusted.
    #[test]
    fn embedded_note_beyond_the_declared_ids_is_invalid() {
        let declared = build_test_note(1);
        let extra = build_test_note(2);
        let mut metadata = v2_metadata(&[&declared]);
        metadata
            .consume_notes_notes
            .push(SerializedNote::from_note(&extra));

        let mut outcomes = Vec::new();
        let decoded = collect_notes([("p-1", &metadata)], &mut outcomes);
        assert_eq!(decoded.len(), 1, "the bound note is still collected");
        assert_eq!(decoded[0].id(), declared.id());
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].identifier, "proposal p-1 notes[1]");
        assert_eq!(outcomes[0].status, NoteImportStatus::Invalid);
        assert!(
            outcomes[0]
                .reason
                .as_deref()
                .unwrap()
                .contains("no matching entry")
        );
    }

    /// The binding compares hex bodies, so prefix/case differences in the
    /// wire form must not reject a genuinely matching note.
    #[test]
    fn declared_id_matching_ignores_hex_prefix_and_case() {
        let note = build_test_note(1);
        let metadata = v2_metadata_entries(vec![(
            SerializedNote::from_note(&note),
            note.id().to_hex().trim_start_matches("0x").to_uppercase(),
        )]);

        let mut outcomes = Vec::new();
        let decoded = collect_notes([("p-1", &metadata)], &mut outcomes);
        assert!(outcomes.is_empty(), "{outcomes:?}");
        assert_eq!(decoded.len(), 1);
    }

    #[test]
    fn skips_v1_metadata() {
        let note = build_test_note(1);
        // v1 wire shape never carries notes, but even a malformed hybrid must
        // not be treated as recovery material.
        let metadata = ProposalMetadata {
            consume_notes_metadata_version: None,
            consume_notes_notes: vec![SerializedNote::from_note(&note)],
            ..Default::default()
        };

        let mut outcomes = Vec::new();
        assert!(collect_notes([("p-1", &metadata)], &mut outcomes).is_empty());
        assert!(outcomes.is_empty());
    }

    #[test]
    fn malformed_note_is_isolated_with_positional_identifier() {
        let good = build_test_note(1);
        let metadata = v2_metadata_entries(vec![
            (
                SerializedNote::from_base64("!!! not base64 !!!".to_string()),
                "0xdead".to_string(),
            ),
            (SerializedNote::from_note(&good), good.id().to_hex()),
        ]);

        let mut outcomes = Vec::new();
        let decoded = collect_notes([("p-9", &metadata)], &mut outcomes);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].id(), good.id());
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].identifier, "proposal p-9 notes[0]");
        assert_eq!(outcomes[0].status, NoteImportStatus::Invalid);
        assert!(!outcomes[0].retryable);
        assert!(
            outcomes[0]
                .reason
                .as_deref()
                .unwrap()
                .contains("failed to decode")
        );
    }

    #[test]
    fn duplicate_notes_across_proposals_are_deduplicated() {
        let shared = build_test_note(1);
        let other = build_test_note(2);
        let first = v2_metadata(&[&shared]);
        let second = v2_metadata(&[&shared, &other]);

        let mut outcomes = Vec::new();
        let decoded = collect_notes([("p-1", &first), ("p-2", &second)], &mut outcomes);
        assert!(outcomes.is_empty());
        let ids: Vec<_> = decoded.iter().map(Note::id).collect();
        assert_eq!(ids, vec![shared.id(), other.id()]);
    }

    fn v2_proposal(id: &str, entries: Vec<(SerializedNote, String)>) -> Proposal {
        let notes: Vec<SerializedNote> = entries.iter().map(|(note, _)| note.clone()).collect();
        let account_id = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();
        let delta = AccountDelta::new(
            account_id,
            AccountStoragePatch::default(),
            AccountVaultDelta::default(),
            None,
            miden_protocol::Felt::ZERO,
        )
        .unwrap();
        let tx_summary = miden_protocol::transaction::TransactionSummary::new(
            delta,
            miden_protocol::transaction::InputNotes::new(Vec::new()).unwrap(),
            miden_protocol::transaction::RawOutputNotes::new(Vec::new()).unwrap(),
            Word::default(),
            0,
            miden_protocol::transaction::TransactionSummaryUserParams::new(
                [miden_protocol::Felt::ZERO; 7],
            ),
        );
        Proposal {
            id: id.to_string(),
            nonce: 1,
            transaction_type: crate::proposal::TransactionType::ConsumeNotes {
                note_ids: vec![],
                metadata_version: Some(crate::proposal::CONSUME_NOTES_METADATA_VERSION_V2),
                notes: notes.clone(),
            },
            status: crate::proposal::ProposalStatus::Pending,
            tx_summary,
            signatures: vec![],
            metadata: v2_metadata_entries(entries),
        }
    }

    /// Exercises the method end to end against an unreachable node:
    /// decoding, deduplication, and invalid isolation must all happen before
    /// any network access, and the proof-fetch failure must surface as
    /// per-note retryable outcomes instead of aborting the batch. (The
    /// success-path branches are covered by the mock-chain behavioral tests
    /// below.)
    #[tokio::test]
    async fn unreachable_node_reports_per_note_failures_without_aborting() {
        let dir = tempfile::tempdir().unwrap();
        let mut client = crate::MultisigClient::builder()
            .miden_endpoint(miden_client::rpc::Endpoint::try_from("http://127.0.0.1:1").unwrap())
            .guardian_endpoint("http://127.0.0.1:1")
            .account_dir(dir.path())
            .generate_key()
            .build()
            .await
            .unwrap();

        let note = build_test_note(1);
        let proposals = vec![
            v2_proposal(
                "p-1",
                vec![
                    (SerializedNote::from_note(&note), note.id().to_hex()),
                    (
                        SerializedNote::from_base64("!!! corrupt !!!".to_string()),
                        "0xdead".to_string(),
                    ),
                ],
            ),
            // Duplicate embedding of the same note -> deduplicated.
            v2_proposal(
                "p-2",
                vec![(SerializedNote::from_note(&note), note.id().to_hex())],
            ),
        ];

        let outcomes = client.import_notes_from_proposals(&proposals).await;

        assert_eq!(outcomes.len(), 2, "dedup folds the duplicate embedding");
        let invalid = outcomes
            .iter()
            .find(|o| o.status == NoteImportStatus::Invalid)
            .expect("malformed note reported");
        assert_eq!(invalid.identifier, "proposal p-1 notes[1]");
        assert!(!invalid.retryable);

        let failed = outcomes
            .iter()
            .find(|o| o.identifier == note.id().to_hex())
            .expect("decodable note reported");
        assert_eq!(failed.status, NoteImportStatus::Failed);
        assert!(
            failed.retryable,
            "connection failure must classify retryable"
        );
        assert!(
            failed
                .reason
                .as_deref()
                .unwrap()
                .contains("failed to fetch inclusion proofs"),
            "failure must point at the proof fetch"
        );
    }

    // ---------------------------------------------------------------------
    // behavioral tests against the mock chain: the success-path branches the
    // public method routes through (committed public/private import,
    // uncommitted expected recording, store-tracked dedup).
    // ---------------------------------------------------------------------

    /// Mock-chain client with a synced store, ready for proof imports.
    async fn behavioral_client(
        dir: &std::path::Path,
        api: std::sync::Arc<miden_client::testing::mock::MockRpcApi>,
    ) -> crate::MultisigClient {
        let mut client = crate::client::test_support::offline_client_with_node(dir, api).await;
        client.miden_client.sync_state().await.unwrap();
        client
    }

    fn proposal_for(note: &Note) -> Proposal {
        v2_proposal(
            "p-live",
            vec![(SerializedNote::from_note(note), note.id().to_hex())],
        )
    }

    /// A committed PUBLIC note imports with its fetched proof and lands as a
    /// proof-backed consumable record; re-running reports it already
    /// present (the store-tracked dedup branch).
    #[tokio::test]
    async fn imports_a_committed_public_note_and_dedups_on_rerun() {
        use miden_protocol::note::NoteType as ProtoNoteType;
        use miden_protocol::transaction::RawOutputNote;

        let dir = tempfile::tempdir().unwrap();
        let target = crate::client::test_support::test_wallet(41);
        let note = crate::client::test_support::p2id_note_for(&target, 1, ProtoNoteType::Public);
        let api =
            crate::client::test_support::chain_with_notes(vec![RawOutputNote::Full(note.clone())]);
        let mut client = behavioral_client(dir.path(), api).await;

        let outcomes = client
            .import_notes_from_proposals(&[proposal_for(&note)])
            .await;
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        assert_eq!(outcomes[0].status, NoteImportStatus::Imported);
        assert_eq!(outcomes[0].identifier, note.id().to_hex());

        let records = client
            .miden_client
            .get_input_notes(StoreNoteFilter::All)
            .await
            .unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            records[0].inclusion_proof().is_some(),
            "the record must be proof-backed"
        );

        let rerun = client
            .import_notes_from_proposals(&[proposal_for(&note)])
            .await;
        assert_eq!(rerun.len(), 1);
        assert_eq!(rerun[0].status, NoteImportStatus::AlreadyPresent);
    }

    /// A committed PRIVATE note: the chain holds only the header, so the
    /// locally embedded bytes are the body and the fetched proof makes them
    /// importable — the branch that makes proposals recovery material for
    /// private notes at all.
    #[tokio::test]
    async fn imports_a_committed_private_note_from_local_bytes() {
        use miden_protocol::note::NoteType as ProtoNoteType;
        use miden_protocol::transaction::RawOutputNote;

        let dir = tempfile::tempdir().unwrap();
        let target = crate::client::test_support::test_wallet(42);
        let note = crate::client::test_support::p2id_note_for(&target, 2, ProtoNoteType::Private);
        let api =
            crate::client::test_support::chain_with_notes(vec![RawOutputNote::Full(note.clone())]);
        let mut client = behavioral_client(dir.path(), api).await;

        let outcomes = client
            .import_notes_from_proposals(&[proposal_for(&note)])
            .await;
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        assert_eq!(outcomes[0].status, NoteImportStatus::Imported);

        let records = client
            .miden_client
            .get_input_notes(StoreNoteFilter::All)
            .await
            .unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].inclusion_proof().is_some());
    }

    /// A note the chain does not know yet is recorded in `Expected` state
    /// with its sync-hint tag and reported `NotCommitted`/retryable.
    #[tokio::test]
    async fn uncommitted_note_is_recorded_as_expected_and_retryable() {
        use miden_protocol::note::NoteType as ProtoNoteType;

        let dir = tempfile::tempdir().unwrap();
        let target = crate::client::test_support::test_wallet(43);
        let note = crate::client::test_support::p2id_note_for(&target, 3, ProtoNoteType::Private);
        // Chain with no notes: the proof fetch finds nothing.
        let api = crate::client::test_support::chain_with_notes(vec![]);
        let mut client = behavioral_client(dir.path(), api).await;

        let outcomes = client
            .import_notes_from_proposals(&[proposal_for(&note)])
            .await;
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        assert_eq!(outcomes[0].status, NoteImportStatus::NotCommitted);
        assert!(outcomes[0].retryable);

        let records = client
            .miden_client
            .get_input_notes(StoreNoteFilter::Expected)
            .await
            .unwrap();
        assert_eq!(records.len(), 1, "expected record must exist for sync");
    }

    /// A note the store already tracks as consumed is reported
    /// `AlreadyConsumed` without touching the network.
    #[tokio::test]
    async fn store_tracked_consumed_note_reports_already_consumed() {
        use miden_client::store::Store;
        use miden_client::store::input_note_states::ConsumedExternalNoteState;
        use miden_protocol::note::NoteType as ProtoNoteType;

        let dir = tempfile::tempdir().unwrap();
        let target = crate::client::test_support::test_wallet(44);
        let note = crate::client::test_support::p2id_note_for(&target, 4, ProtoNoteType::Private);
        let api = crate::client::test_support::chain_with_notes(vec![]);
        let (mut client, store) =
            crate::client::test_support::offline_client_with_node_parts(dir.path(), api).await;
        client.miden_client.sync_state().await.unwrap();

        let metadata = *note.metadata();
        let record = InputNoteRecord::new(
            note.clone().into(),
            note.attachments().clone(),
            None,
            ConsumedExternalNoteState {
                nullifier_block_height: BlockNumber::from(1u32),
                consumer_account: None,
                consumed_tx_order: None,
                metadata: Some(metadata),
            }
            .into(),
        );
        store.upsert_input_notes(&[record]).await.unwrap();

        let outcomes = client
            .import_notes_from_proposals(&[proposal_for(&note)])
            .await;
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        assert_eq!(outcomes[0].status, NoteImportStatus::AlreadyConsumed);
    }

    #[test]
    fn status_and_source_render_stable_strings() {
        assert_eq!(NoteImportSource::Proposal.to_string(), "proposal");
        assert_eq!(NoteImportSource::Backfill.to_string(), "backfill");
        let expected = [
            (NoteImportStatus::Imported, "imported"),
            (NoteImportStatus::AlreadyPresent, "already-present"),
            (NoteImportStatus::AlreadyConsumed, "already-consumed"),
            (NoteImportStatus::NotCommitted, "not-committed"),
            (NoteImportStatus::Invalid, "invalid"),
            (NoteImportStatus::Failed, "failed"),
        ];
        for (status, s) in expected {
            assert_eq!(status.to_string(), s);
        }
    }
}
