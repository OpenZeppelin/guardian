//! Historical public-note backfill by tag.
//!
//! Public notes addressed to an account are on chain, but normal forward
//! sync starts from the store's **global** cursor: in a shared dirty store
//! the cursor may already be past blocks containing a recovered account's
//! notes, and a fresh store has no efficient path to them at all. This
//! module rescans a historical block range with the account's standard note
//! tag and imports what it finds with on-chain inclusion proofs, without
//! ever touching the global sync height.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use miden_client::rpc::RpcError;
use miden_client::rpc::domain::note::FetchedNote;
use miden_protocol::account::AccountId;
use miden_protocol::block::BlockNumber;
use miden_protocol::note::{
    Note, NoteDetailsCommitment, NoteId, NoteInclusionProof, NoteTag, NoteType,
};

use super::MultisigClient;
use super::proposal_note_import::{NoteImportOutcome, NoteImportSource, NoteImportStatus};
use crate::error::{MultisigError, Result};
use crate::rpc::is_transient_rpc_error;

/// A contiguous block range, inclusive on both ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockRange {
    /// First block of the range.
    pub from: u32,
    /// Last block of the range.
    pub to: u32,
}

/// Options for [`MultisigClient::backfill_public_notes_by_tag`]. Every field
/// has a default, so `None` (or `PublicBackfillOptions::default()`) scans the
/// whole chain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PublicBackfillOptions {
    /// First block of the scan range; defaults to genesis.
    pub from_block: Option<BlockNumber>,
    /// Last block of the scan range; defaults to the current chain tip.
    pub to_block: Option<BlockNumber>,
}

/// Result of [`MultisigClient::backfill_public_notes_by_tag`].
///
/// Scan problems are reported here rather than returned as errors so a
/// partially failing scan never aborts the rest of a recovery flow: notes
/// discovered in the covered ranges are imported regardless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicBackfillReport {
    /// First block of the requested scan range.
    pub scanned_from: u32,
    /// Last block of the requested scan range.
    pub scanned_to: u32,
    /// Unique tag-matching notes the scan discovered, of every visibility.
    pub discovered: usize,
    /// Unique non-public matches skipped: the chain does not hold their
    /// bodies, so they cannot be rebuilt from a scan. Private notes are
    /// covered by the transport drain and proposal-import primitives instead.
    pub skipped_private: usize,
    /// Unique public matches the relevance screener rejected: tags are
    /// best-effort, truncated filters, so unrelated notes can carry this
    /// account's tag. Like normal sync, only notes the account could
    /// actually consume are imported; the rest are counted here.
    pub skipped_irrelevant: usize,
    /// Unique public matches no screener could judge. Always `0` in this
    /// SDK — the execution-based screener judges every note; the TS SDK
    /// counts its statically unscreenable custom-script notes here so the
    /// two skip classes stay distinguishable across SDKs.
    pub skipped_unscreenable: usize,
    /// One outcome per unique public note that passed the relevance screen —
    /// `outcomes.len() == discovered - skipped_private - skipped_irrelevant`.
    /// Screened-out and private matches get no outcome, only their counters.
    pub outcomes: Vec<NoteImportOutcome>,
    /// Sub-ranges of `[scanned_from, scanned_to]` the scan could not cover
    /// (RPC failures, or the scan budget ran out while splitting around the
    /// node's pagination cap). Empty when the whole range was scanned. Notes
    /// committed in these ranges may be missing from `outcomes`.
    pub uncovered: Vec<BlockRange>,
    /// Whether rerunning the backfill can plausibly improve the result:
    /// cover `uncovered` ranges, or retry outcomes whose own `retryable`
    /// flag is set. Always `false` when the scan fully covered the range and
    /// no outcome is retryable.
    pub retryable: bool,
    /// Human-readable cause when the scan did not cover the whole range.
    pub reason: Option<String>,
}

/// Upper bound on `SyncNotes` requests per backfill. Splitting around the
/// node's pagination cap halves ranges, so the budget is only approachable
/// when nearly every sub-range is dense enough to trip the cap; exhausting it
/// reports the remaining ranges as uncovered instead of scanning forever.
const MAX_SCAN_REQUESTS: usize = 128;

impl MultisigClient {
    /// Scans a historical block range for public notes addressed at
    /// `account_id`'s standard note tag and imports what it finds with their
    /// on-chain inclusion proofs.
    ///
    /// Use after account recovery: normal forward sync starts from the
    /// store's **global** cursor, so in a shared dirty store the cursor may
    /// already be past blocks containing the recovered account's notes, and a
    /// fresh store would need to replay the whole chain state to see them.
    /// The scan is tag-scoped and its cost grows with the number of matching
    /// notes, not the range length, which makes genesis an
    /// acceptable default lower bound. The global sync height is never
    /// touched — run normal sync afterwards to verify the imported notes.
    /// The store must have synced at least once
    /// ([`MultisigClient::recover_notes`] syncs the chain before this
    /// strategy runs): importing a proof into a store that has never seen
    /// the chain fails, and such failures surface as `Failed` outcomes.
    ///
    /// The scan range comes from `options` (pass `None` for the defaults:
    /// genesis through the current chain tip). Notes are discovered by tag only — a best-effort filter: notes
    /// sent with unrelated custom tags are outside this scan's guarantee,
    /// and, exactly like normal sync, every new discovery is screened with
    /// the [`miden_client::note::NoteScreener`] before import — tag-colliding
    /// notes the account can never consume are counted as
    /// `skipped_irrelevant` instead of polluting the store (screening
    /// requires the account to be tracked in the store, which recovery via
    /// [`MultisigClient::pull_account`] guarantees). Only public notes can
    /// be rebuilt from chain data; private matches are counted as
    /// `skipped_private` and are covered by the transport drain and
    /// proposal-import primitives instead.
    ///
    /// A range dense enough to trip the node's internal pagination cap is
    /// split client-side and rescanned as narrower requests; ranges that
    /// still cannot be covered are reported in
    /// [`PublicBackfillReport::uncovered`] rather than failing the recovery
    /// flow. An `Err` from this method means the scan range itself could not
    /// be established (chain-tip lookup failed, or `from_block > to_block`).
    pub(crate) async fn backfill_public_notes_by_tag(
        &mut self,
        account_id: AccountId,
        options: Option<PublicBackfillOptions>,
    ) -> Result<PublicBackfillReport> {
        let PublicBackfillOptions {
            from_block,
            to_block,
        } = options.unwrap_or_default();
        let rpc = self.node_rpc_client();

        let from = from_block.unwrap_or(BlockNumber::from(0u32)).as_u32();
        let to = match to_block {
            Some(to) => to.as_u32(),
            None => rpc
                .get_block_header_by_number(None, false)
                .await
                .map_err(|e| {
                    MultisigError::miden_rpc_with_context(
                        "failed to resolve the chain tip for the backfill scan",
                        e,
                    )
                })?
                .0
                .block_num()
                .as_u32(),
        };
        if from > to {
            return Err(MultisigError::InvalidConfig(format!(
                "backfill range is inverted: from_block {from} > to_block {to}"
            )));
        }

        let tag = NoteTag::with_account_target(account_id);
        let tags: BTreeSet<NoteTag> = std::iter::once(tag).collect();

        // Work queue of inclusive sub-ranges, split in half whenever the node
        // reports its pagination cap for one of them.
        let mut queue: VecDeque<(u32, u32)> = VecDeque::from([(from, to)]);
        let mut discovered = BTreeMap::new();
        let mut uncovered: Vec<BlockRange> = Vec::new();
        let mut scan_reasons: Vec<String> = Vec::new();
        let mut retryable = false;
        let mut requests = 0usize;
        let mut budget_exhausted = false;

        while let Some((lo, hi)) = queue.pop_front() {
            if requests >= MAX_SCAN_REQUESTS {
                budget_exhausted = true;
                uncovered.push(BlockRange { from: lo, to: hi });
                continue;
            }
            requests += 1;
            match rpc
                .sync_notes(BlockNumber::from(lo), BlockNumber::from(hi), &tags)
                .await
            {
                Ok(blocks) => {
                    for block in blocks {
                        discovered.extend(block.notes);
                    }
                }
                // The node caps internal pagination per request rather than
                // truncating; a single-block range cannot be split further
                // (and cannot realistically hold that many pages), so only
                // splittable ranges take this branch.
                Err(RpcError::PaginationError(_)) if lo < hi => {
                    let mid = lo + (hi - lo) / 2;
                    queue.push_front((mid + 1, hi));
                    queue.push_front((lo, mid));
                }
                Err(e) => {
                    retryable |= is_transient_rpc_error(&e);
                    scan_reasons.push(format!("blocks [{lo}, {hi}]: {e}"));
                    uncovered.push(BlockRange { from: lo, to: hi });
                }
            }
        }
        if budget_exhausted {
            retryable = true;
            scan_reasons.push(format!(
                "scan budget of {MAX_SCAN_REQUESTS} requests exhausted while splitting around \
                 the node's pagination cap; rerun the backfill over the uncovered ranges"
            ));
        }

        let public_ids: Vec<NoteId> = discovered
            .iter()
            .filter(|(_, committed)| committed.note_type() == NoteType::Public)
            .map(|(id, _)| *id)
            .collect();
        let skipped_private = discovered.len() - public_ids.len();

        let mut outcomes: Vec<NoteImportOutcome> = Vec::new();

        // One batched body fetch — the upstream client chunks internally by
        // the node's negotiated note-ids limit. The node returns full bodies
        // for public notes, so the scan's ID + proof is all this path needs.
        let mut pending: Vec<(Note, NoteInclusionProof)> = Vec::new();
        if !public_ids.is_empty() {
            match rpc.get_notes_by_id(&public_ids).await {
                Ok(fetched) => {
                    let mut bodies: BTreeMap<NoteId, (Note, NoteInclusionProof)> = fetched
                        .into_iter()
                        .filter_map(|fetched| match fetched {
                            FetchedNote::Public(note, proof) => Some((note.id(), (note, proof))),
                            FetchedNote::Private(..) => None,
                        })
                        .collect();
                    for id in &public_ids {
                        match bodies.remove(id) {
                            Some(entry) => pending.push(entry),
                            // Discovered as public by the scan but returned
                            // without a body — not expected for a committed
                            // public note.
                            None => outcomes.push(NoteImportOutcome {
                                identifier: id.to_hex(),
                                source: NoteImportSource::Backfill,
                                status: NoteImportStatus::Failed,
                                retryable: true,
                                reason: Some(
                                    "the node did not return a body for this public note"
                                        .to_string(),
                                ),
                            }),
                        }
                    }
                }
                Err(e) => {
                    let fetch_retryable = is_transient_rpc_error(&e);
                    let reason = format!("failed to fetch note bodies: {e}");
                    for id in &public_ids {
                        outcomes.push(NoteImportOutcome {
                            identifier: id.to_hex(),
                            source: NoteImportSource::Backfill,
                            status: NoteImportStatus::Failed,
                            retryable: fetch_retryable,
                            reason: Some(reason.clone()),
                        });
                    }
                }
            }
        }

        let mut skipped_irrelevant = 0usize;
        let commitments: Vec<NoteDetailsCommitment> = pending
            .iter()
            .map(|(note, _)| note.details_commitment())
            .collect();
        match self.existing_records_by_commitment(commitments).await {
            Err(e) => {
                let reason = format!("failed to read local store: {}", e);
                for (note, _) in &pending {
                    outcomes.push(NoteImportOutcome {
                        identifier: note.id().to_hex(),
                        source: NoteImportSource::Backfill,
                        status: NoteImportStatus::Failed,
                        retryable: false,
                        reason: Some(reason.clone()),
                    });
                }
            }
            Ok(existing) => {
                // Split the fetched notes into ones the store already tracks
                // and genuinely new discoveries. Only the new ones go through
                // relevance screening: tracked records are material the user
                // already chose to track (a proposal import, an earlier
                // backfill, normal sync).
                let mut tracked: Vec<(Note, NoteInclusionProof)> = Vec::new();
                let mut fresh: Vec<(Note, NoteInclusionProof)> = Vec::new();
                for (note, proof) in pending {
                    match existing.get(&note.details_commitment()) {
                        Some(record)
                            if record.is_consumed() || record.inclusion_proof().is_some() =>
                        {
                            let status = if record.is_consumed() {
                                NoteImportStatus::AlreadyConsumed
                            } else {
                                NoteImportStatus::AlreadyPresent
                            };
                            outcomes.push(NoteImportOutcome {
                                identifier: note.id().to_hex(),
                                source: NoteImportSource::Backfill,
                                status,
                                retryable: false,
                                reason: None,
                            });
                        }
                        // Unlike the proposal import, a proof-less
                        // (`Expected`) record is NOT skipped: this primitive
                        // exists because forward sync will never revisit the
                        // note's block, so the freshly fetched proof is
                        // applied to upgrade the record in place (upstream
                        // import handles existing records). No screening
                        // either — the record is already-tracked material.
                        Some(_) => tracked.push((note, proof)),
                        None => fresh.push((note, proof)),
                    }
                }

                // Screen new discoveries for relevance the same way normal
                // sync does before it stores a tag match (tags are shared,
                // truncated filters — anyone can commit public notes carrying
                // this account's tag, and unscreened imports would pollute
                // the store and pay per-note import RPC for junk). Notes the
                // account cannot ever consume are counted, not imported.
                if !fresh.is_empty() {
                    let notes: Vec<Note> = fresh.iter().map(|(note, _)| note.clone()).collect();
                    match self
                        .miden_client
                        .note_screener()
                        .get_batch_consumability_for_account(account_id, &notes)
                        .await
                    {
                        Ok(relevant) => {
                            for (note, proof) in fresh {
                                if relevant.contains_key(&note.id()) {
                                    tracked.push((note, proof));
                                } else {
                                    skipped_irrelevant += 1;
                                }
                            }
                        }
                        Err(e) => {
                            // Screening needs the account's state in the
                            // store; without a verdict nothing is imported.
                            let reason = format!("relevance screening failed: {}", e);
                            for (note, _) in &fresh {
                                outcomes.push(NoteImportOutcome {
                                    identifier: note.id().to_hex(),
                                    source: NoteImportSource::Backfill,
                                    status: NoteImportStatus::Failed,
                                    retryable: false,
                                    reason: Some(reason.clone()),
                                });
                            }
                        }
                    }
                }

                // Provisionally `Imported` outcomes, re-classified in one
                // batched consumed-state check below.
                let mut imported: Vec<(usize, NoteDetailsCommitment)> = Vec::new();
                for (note, proof) in tracked {
                    let commitment = note.details_commitment();
                    let (outcome, was_imported) = self
                        .import_note_with_proof(NoteImportSource::Backfill, note, proof)
                        .await;
                    if was_imported {
                        imported.push((outcomes.len(), commitment));
                    }
                    outcomes.push(outcome);
                }

                self.reclassify_consumed_imports(&imported, &mut outcomes)
                    .await;
            }
        }

        let reason = match scan_reasons.len() {
            0 => None,
            n if n <= 3 => Some(scan_reasons.join("; ")),
            n => Some(format!(
                "{}; …and {} more",
                scan_reasons[..3].join("; "),
                n - 3
            )),
        };
        // Rerunning can help when scan ranges were left uncovered OR when any
        // per-note outcome is itself retryable — surface both at report level
        // so orchestration keyed on the report alone reruns when it should.
        let retryable = retryable || outcomes.iter().any(|outcome| outcome.retryable);
        Ok(PublicBackfillReport {
            scanned_from: from,
            scanned_to: to,
            discovered: discovered.len(),
            skipped_private,
            skipped_irrelevant,
            skipped_unscreenable: 0,
            outcomes,
            uncovered,
            retryable,
            reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use miden_client::testing::mock::MockRpcApi;
    use miden_protocol::account::Account;
    use miden_protocol::transaction::RawOutputNote;

    use super::*;
    use crate::client::test_support::{
        chain_with_notes, offline_client_with_node, offline_client_with_node_parts, p2id_note_for,
        test_wallet,
    };

    fn public_note_for(target: &Account, seed: u32) -> Note {
        p2id_note_for(target, seed, NoteType::Public)
    }

    /// The dirty-store scenario from #416: tag coverage starts at `H`, a
    /// note commits at `B`, and the store's global cursor is already at `T`
    /// with `H < B <= T` (another account's sync advanced it before this
    /// account existed in the store). The backfill must discover the note at
    /// `B` without rewinding `T`.
    #[tokio::test]
    async fn backfill_recovers_a_note_behind_the_global_cursor_without_rewinding_it() {
        let dir = tempfile::tempdir().unwrap();
        let target = test_wallet(11);
        let note = public_note_for(&target, 1);
        let api = chain_with_notes(vec![RawOutputNote::Full(note.clone())]);
        let mut client = offline_client_with_node(dir.path(), api.clone()).await;

        // Advance the store's global cursor to the tip before the recovered
        // account (and therefore its tag) exists in this store: the note's
        // block is now behind the cursor and forward sync will never revisit
        // it.
        client.miden_client.sync_state().await.unwrap();
        let cursor = client.miden_client.get_sync_height().await.unwrap();
        assert!(
            note.metadata().tag() == NoteTag::with_account_target(target.id()),
            "p2id notes carry the standard account-target tag"
        );
        client.add_or_update_account(&target, false).await.unwrap();
        assert!(
            client
                .miden_client
                .get_input_notes(miden_client::store::NoteFilter::All)
                .await
                .unwrap()
                .is_empty(),
            "the dirty store must start without the note"
        );

        let report = client
            .backfill_public_notes_by_tag(target.id(), None)
            .await
            .unwrap();

        assert_eq!(report.scanned_from, 0);
        assert_eq!(report.scanned_to, cursor.as_u32());
        assert_eq!(report.discovered, 1);
        assert_eq!(report.skipped_private, 0);
        assert_eq!(report.skipped_irrelevant, 0);
        assert!(report.uncovered.is_empty());
        assert!(!report.retryable, "{report:?}");
        assert_eq!(report.reason, None);
        assert_eq!(report.outcomes.len(), 1);
        assert_eq!(
            report.outcomes[0].status,
            NoteImportStatus::Imported,
            "{report:?}"
        );
        assert_eq!(report.outcomes[0].source, NoteImportSource::Backfill);
        assert_eq!(report.outcomes[0].identifier, note.id().to_hex());

        // The note is in the store, and the global cursor did not move.
        let records = client
            .miden_client
            .get_input_notes(miden_client::store::NoteFilter::All)
            .await
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(
            client.miden_client.get_sync_height().await.unwrap(),
            cursor,
            "the backfill must never mutate the global sync height"
        );

        // Rerunning tolerates the duplicate discovery: the note is reported
        // as already present, not re-imported.
        let report = client
            .backfill_public_notes_by_tag(target.id(), None)
            .await
            .unwrap();
        assert_eq!(report.outcomes.len(), 1);
        assert_eq!(report.outcomes[0].status, NoteImportStatus::AlreadyPresent);
        let records = client
            .miden_client
            .get_input_notes(miden_client::store::NoteFilter::All)
            .await
            .unwrap();
        assert_eq!(records.len(), 1);
    }

    /// Tags are best-effort filters: an unrelated note sharing the tag is
    /// screened out for relevance (exactly like normal sync) instead of
    /// polluting the store, and private matches are counted but skipped —
    /// the chain does not hold their bodies.
    #[tokio::test]
    async fn backfill_tolerates_tag_collisions_and_skips_private_matches() {
        let dir = tempfile::tempdir().unwrap();
        let target = test_wallet(13);
        let tag = NoteTag::with_account_target(target.id());

        let own = public_note_for(&target, 2);
        // A real P2ID note addressed at a DIFFERENT account, re-tagged with
        // the target's tag: a genuine tag collision the screener must reject
        // (the target can never consume it).
        let mis_addressed = public_note_for(&test_wallet(201), 7);
        let colliding = Note::new(
            mis_addressed.assets().clone(),
            miden_protocol::note::PartialNoteMetadata::new(
                mis_addressed.metadata().sender(),
                NoteType::Public,
            )
            .with_tag(tag),
            mis_addressed.recipient().clone(),
        );
        let private = p2id_note_for(&target, 3, NoteType::Private);
        let api = chain_with_notes(vec![
            RawOutputNote::Full(own.clone()),
            RawOutputNote::Full(colliding.clone()),
            RawOutputNote::Full(private),
        ]);
        let mut client = offline_client_with_node(dir.path(), api.clone()).await;
        // Imports need the store to know the chain; a recovered store has
        // synced at least once by the time recovery primitives run.
        client.miden_client.sync_state().await.unwrap();
        client.add_or_update_account(&target, false).await.unwrap();

        let report = client
            .backfill_public_notes_by_tag(target.id(), None)
            .await
            .unwrap();

        assert_eq!(report.discovered, 3);
        assert_eq!(report.skipped_private, 1);
        assert_eq!(
            report.skipped_irrelevant, 1,
            "the tag-colliding note must be screened out, not imported: {report:?}"
        );
        assert_eq!(report.outcomes.len(), 1);
        assert_eq!(
            report.outcomes[0].status,
            NoteImportStatus::Imported,
            "{report:?}"
        );
        assert_eq!(report.outcomes[0].identifier, own.id().to_hex());
        // The screened-out note must not be in the store.
        let records = client
            .miden_client
            .get_input_notes(miden_client::store::NoteFilter::All)
            .await
            .unwrap();
        assert_eq!(records.len(), 1);
    }

    /// A proof-less `Expected` record (e.g. left by a proposal import that
    /// ran while the note was uncommitted) whose note committed behind the
    /// cursor must be upgraded with the freshly fetched proof — not skipped
    /// as already-present, because forward sync will never revisit its block
    /// and the note would stay non-consumable forever.
    #[tokio::test]
    async fn backfill_upgrades_a_proofless_expected_record_with_the_fetched_proof() {
        use miden_client::store::input_note_states::ExpectedNoteState;
        use miden_client::store::{InputNoteRecord, Store};

        let dir = tempfile::tempdir().unwrap();
        let target = test_wallet(19);
        let note = public_note_for(&target, 5);
        let api = chain_with_notes(vec![RawOutputNote::Full(note.clone())]);

        // The parts variant keeps a handle on the store for seeding the
        // proof-less record.
        let (mut client, store) = offline_client_with_node_parts(dir.path(), api.clone()).await;

        client.miden_client.sync_state().await.unwrap();
        client.add_or_update_account(&target, false).await.unwrap();

        // Seed the proof-less Expected record directly — a details import
        // through the client would immediately proof-back it because this
        // node already knows the note; the record models a proposal import
        // that ran while the node did not.
        let metadata = *note.metadata();
        let record = InputNoteRecord::new(
            note.clone().into(),
            note.attachments().clone(),
            None,
            ExpectedNoteState {
                metadata: Some(metadata),
                after_block_num: BlockNumber::from(0u32),
                tag: Some(metadata.tag()),
            }
            .into(),
        );
        store.upsert_input_notes(&[record]).await.unwrap();

        let report = client
            .backfill_public_notes_by_tag(target.id(), None)
            .await
            .unwrap();

        assert_eq!(report.outcomes.len(), 1);
        assert_eq!(
            report.outcomes[0].status,
            NoteImportStatus::Imported,
            "{report:?}"
        );

        let records = client
            .miden_client
            .get_input_notes(miden_client::store::NoteFilter::All)
            .await
            .unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            records[0].inclusion_proof().is_some(),
            "the record must now be proof-backed"
        );
    }

    /// Node stub that reports the upstream pagination cap for any range wider
    /// than `max_span` blocks and otherwise delegates to the mock chain, so
    /// the client-side range-splitting fallback can be driven offline.
    struct PaginationCappedNode {
        inner: Arc<MockRpcApi>,
        max_span: u32,
        scan_requests: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl miden_client::rpc::NodeRpcClient for PaginationCappedNode {
        async fn sync_notes(
            &self,
            block_from: BlockNumber,
            block_to: BlockNumber,
            note_tags: &BTreeSet<NoteTag>,
        ) -> std::result::Result<Vec<miden_client::rpc::domain::note::SyncNotesBlock>, RpcError>
        {
            self.scan_requests
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if block_to.as_u32() - block_from.as_u32() + 1 > self.max_span {
                return Err(RpcError::PaginationError(
                    "too many pagination iterations, possible infinite loop".to_string(),
                ));
            }
            self.inner.sync_notes(block_from, block_to, note_tags).await
        }

        async fn get_notes_by_id(
            &self,
            note_ids: &[NoteId],
        ) -> std::result::Result<Vec<FetchedNote>, RpcError> {
            self.inner.get_notes_by_id(note_ids).await
        }

        async fn get_block_header_by_number(
            &self,
            block_num: Option<BlockNumber>,
            include_mmr_proof: bool,
        ) -> std::result::Result<
            (
                miden_protocol::block::BlockHeader,
                Option<miden_protocol::crypto::merkle::mmr::MmrProof>,
            ),
            RpcError,
        > {
            self.inner
                .get_block_header_by_number(block_num, include_mmr_proof)
                .await
        }

        // Note import consults nullifiers and chain data internally; delegate.
        async fn sync_nullifiers(
            &self,
            prefix: &[u16],
            block_from: BlockNumber,
            block_to: BlockNumber,
        ) -> std::result::Result<Vec<miden_client::rpc::domain::nullifier::NullifierUpdate>, RpcError>
        {
            self.inner
                .sync_nullifiers(prefix, block_from, block_to)
                .await
        }

        async fn sync_chain_mmr(
            &self,
            current_block_height: BlockNumber,
            upper_bound: miden_client::rpc::domain::sync::SyncTarget,
        ) -> std::result::Result<miden_client::rpc::domain::sync::ChainMmrInfo, RpcError> {
            self.inner
                .sync_chain_mmr(current_block_height, upper_bound)
                .await
        }

        async fn get_block_by_number(
            &self,
            block_num: BlockNumber,
            include_proof: bool,
        ) -> std::result::Result<miden_protocol::block::ProvenBlock, RpcError> {
            self.inner
                .get_block_by_number(block_num, include_proof)
                .await
        }

        fn has_genesis_commitment(&self) -> Option<miden_protocol::Word> {
            self.inner.has_genesis_commitment()
        }

        fn has_rpc_limits(&self) -> Option<miden_client::rpc::domain::limits::RpcLimits> {
            self.inner.has_rpc_limits()
        }

        async fn get_network_id(
            &self,
        ) -> std::result::Result<miden_protocol::address::NetworkId, RpcError> {
            self.inner.get_network_id().await
        }

        async fn get_rpc_limits(
            &self,
        ) -> std::result::Result<miden_client::rpc::domain::limits::RpcLimits, RpcError> {
            self.inner.get_rpc_limits().await
        }

        async fn set_rpc_limits(&self, limits: miden_client::rpc::domain::limits::RpcLimits) {
            self.inner.set_rpc_limits(limits).await;
        }

        async fn get_status_unversioned(
            &self,
        ) -> std::result::Result<miden_client::rpc::RpcStatusInfo, RpcError> {
            self.inner.get_status_unversioned().await
        }

        // Nothing below is exercised by the backfill tests.
        async fn set_genesis_commitment(
            &self,
            _commitment: miden_protocol::Word,
        ) -> std::result::Result<(), RpcError> {
            unimplemented!("not exercised by the backfill tests")
        }

        async fn get_transaction_encryption_key(
            &self,
        ) -> std::result::Result<
            miden_client::rpc::encryption::AttestedTransactionEncryptionKey,
            RpcError,
        > {
            unimplemented!("not exercised by the backfill tests")
        }

        async fn submit_proven_transaction(
            &self,
            _proven_transaction: miden_protocol::transaction::ProvenTransaction,
            _transaction_inputs: miden_client::rpc::encryption::SealedTransactionInputs,
        ) -> std::result::Result<BlockNumber, RpcError> {
            unimplemented!("not exercised by the backfill tests")
        }

        async fn submit_proven_batch(
            &self,
            _proven_batch: miden_protocol::batch::ProvenBatch,
            _proposed_batch: miden_protocol::batch::ProposedBatch,
            _transaction_inputs: Vec<miden_client::rpc::encryption::SealedTransactionInputs>,
        ) -> std::result::Result<BlockNumber, RpcError> {
            unimplemented!("not exercised by the backfill tests")
        }

        async fn get_account(
            &self,
            _account_id: AccountId,
            _request: miden_client::rpc::domain::account::GetAccountRequest,
        ) -> std::result::Result<
            (
                BlockNumber,
                miden_client::rpc::domain::account::AccountProof,
            ),
            RpcError,
        > {
            unimplemented!("not exercised by the backfill tests")
        }

        async fn get_note_script_by_root(
            &self,
            _root: miden_protocol::Word,
        ) -> std::result::Result<Option<miden_protocol::note::NoteScript>, RpcError> {
            unimplemented!("not exercised by the backfill tests")
        }

        async fn sync_storage_maps(
            &self,
            _block_from: BlockNumber,
            _block_to: BlockNumber,
            _account_id: AccountId,
        ) -> std::result::Result<miden_client::rpc::domain::storage_map::StorageMapInfo, RpcError>
        {
            unimplemented!("not exercised by the backfill tests")
        }

        async fn sync_account_vault(
            &self,
            _block_from: BlockNumber,
            _block_to: BlockNumber,
            _account_id: AccountId,
        ) -> std::result::Result<miden_client::rpc::domain::account_vault::AccountVaultInfo, RpcError>
        {
            unimplemented!("not exercised by the backfill tests")
        }

        async fn sync_transactions(
            &self,
            _block_from: BlockNumber,
            _block_to: BlockNumber,
            _account_ids: Vec<AccountId>,
        ) -> std::result::Result<
            Vec<miden_client::rpc::domain::transaction::TransactionRecord>,
            RpcError,
        > {
            unimplemented!("not exercised by the backfill tests")
        }

        async fn get_network_note_status(
            &self,
            _note_id: NoteId,
        ) -> std::result::Result<miden_client::rpc::NetworkNoteStatusInfo, RpcError> {
            unimplemented!("not exercised by the backfill tests")
        }
    }

    /// The pagination-cap fallback: a range dense enough to trip the node's
    /// cap is split client-side until the sub-ranges fit, and the note is
    /// still recovered with nothing left uncovered.
    #[tokio::test]
    async fn backfill_splits_the_range_around_the_pagination_cap() {
        let dir = tempfile::tempdir().unwrap();
        let target = test_wallet(15);
        let note = public_note_for(&target, 4);
        let api = chain_with_notes(vec![RawOutputNote::Full(note.clone())]);
        let capped = Arc::new(PaginationCappedNode {
            inner: api.clone(),
            max_span: 2,
            scan_requests: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut client = offline_client_with_node(dir.path(), api.clone()).await;
        // Imports need the store to know the chain; a recovered store has
        // synced at least once by the time recovery primitives run. Only the
        // backfill's direct node channel gets the pagination-capped stub.
        client.miden_client.sync_state().await.unwrap();
        client.add_or_update_account(&target, false).await.unwrap();
        client.set_node_rpc_client(capped.clone());

        let report = client
            .backfill_public_notes_by_tag(target.id(), None)
            .await
            .unwrap();

        assert!(report.uncovered.is_empty());
        assert!(!report.retryable);
        assert_eq!(report.reason, None);
        assert_eq!(report.discovered, 1);
        assert_eq!(report.outcomes.len(), 1);
        assert_eq!(
            report.outcomes[0].status,
            NoteImportStatus::Imported,
            "{report:?}"
        );
        assert!(
            capped
                .scan_requests
                .load(std::sync::atomic::Ordering::SeqCst)
                > 1,
            "the range must have been rescanned as narrower requests"
        );
    }

    /// A pagination failure that persists down to single-block ranges cannot
    /// be split further: the blocks are reported uncovered instead of
    /// failing the recovery flow.
    #[tokio::test]
    async fn backfill_reports_unsplittable_pagination_failures_as_uncovered() {
        let dir = tempfile::tempdir().unwrap();
        let target = test_wallet(16);
        let api = chain_with_notes(vec![]);
        let capped = Arc::new(PaginationCappedNode {
            inner: api.clone(),
            max_span: 0,
            scan_requests: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut client = offline_client_with_node(dir.path(), api).await;
        client.set_node_rpc_client(capped.clone());

        let report = client
            .backfill_public_notes_by_tag(
                target.id(),
                Some(PublicBackfillOptions {
                    from_block: Some(BlockNumber::from(0u32)),
                    to_block: Some(BlockNumber::from(3u32)),
                }),
            )
            .await
            .unwrap();

        assert_eq!(report.discovered, 0);
        assert!(report.outcomes.is_empty());
        assert_eq!(
            report.uncovered,
            vec![
                BlockRange { from: 0, to: 0 },
                BlockRange { from: 1, to: 1 },
                BlockRange { from: 2, to: 2 },
                BlockRange { from: 3, to: 3 },
            ]
        );
        assert!(report.reason.is_some());
    }

    /// An unreachable node: with an explicit range the scan failure is
    /// reported (whole range uncovered, retryable), and without one the
    /// method errors because the chain tip cannot be resolved.
    #[tokio::test]
    async fn backfill_against_an_unreachable_node_reports_or_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut client = crate::MultisigClient::builder()
            .miden_endpoint(miden_client::rpc::Endpoint::try_from("http://127.0.0.1:1").unwrap())
            .guardian_endpoint("http://127.0.0.1:1")
            .account_dir(dir.path())
            .generate_key()
            .build()
            .await
            .unwrap();
        let target = test_wallet(17);

        let report = client
            .backfill_public_notes_by_tag(
                target.id(),
                Some(PublicBackfillOptions {
                    to_block: Some(BlockNumber::from(9u32)),
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        assert_eq!(report.scanned_from, 0);
        assert_eq!(report.scanned_to, 9);
        assert_eq!(report.discovered, 0);
        assert!(report.outcomes.is_empty());
        assert_eq!(report.uncovered, vec![BlockRange { from: 0, to: 9 }]);
        assert!(report.retryable, "a connection failure must be retryable");
        assert!(report.reason.is_some());

        let err = client
            .backfill_public_notes_by_tag(target.id(), None)
            .await
            .expect_err("tip resolution failure must error");
        assert!(err.to_string().contains("chain tip"));
    }

    #[tokio::test]
    async fn backfill_rejects_an_inverted_range() {
        let dir = tempfile::tempdir().unwrap();
        let api = chain_with_notes(vec![]);
        let mut client = offline_client_with_node(dir.path(), api).await;
        let target = test_wallet(18);

        let err = client
            .backfill_public_notes_by_tag(
                target.id(),
                Some(PublicBackfillOptions {
                    from_block: Some(BlockNumber::from(5u32)),
                    to_block: Some(BlockNumber::from(1u32)),
                }),
            )
            .await
            .expect_err("inverted range must error");
        assert!(err.to_string().contains("inverted"));
    }
}
