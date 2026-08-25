//! Historical public-note backfill by tag (issue #416, sub-issue of #357).
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
use super::proposal_import::{NoteImportOutcome, NoteImportSource, NoteImportStatus};
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
    /// One outcome per unique public note discovered.
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
    /// on-chain inclusion proofs (issue #416).
    ///
    /// Use after account recovery: normal forward sync starts from the
    /// store's **global** cursor, so in a shared dirty store the cursor may
    /// already be past blocks containing the recovered account's notes, and a
    /// fresh store would need to replay the whole chain state to see them.
    /// The scan is tag-scoped and its cost grows with the number of matching
    /// notes, not the range length (spike #412), which makes genesis an
    /// acceptable default lower bound. The global sync height is never
    /// touched — run normal sync afterwards to verify the imported notes.
    /// The store must have synced at least once (recovery via
    /// [`MultisigClient::pull_account`] does this): importing a proof into a
    /// store that has never seen the chain fails, and such failures surface
    /// as `Failed` outcomes.
    ///
    /// `from_block` defaults to genesis and `to_block` to the current chain
    /// tip. Notes are discovered by tag only — a best-effort filter: notes
    /// sent with unrelated custom tags are outside this scan's guarantee, and
    /// unrelated notes whose tag collides with the account's are imported
    /// harmlessly (they simply never become consumable). Only public notes
    /// can be rebuilt from chain data; private matches are counted as
    /// `skipped_private` and are covered by the transport drain and
    /// proposal-import primitives instead.
    ///
    /// A range dense enough to trip the node's internal pagination cap is
    /// split client-side and rescanned as narrower requests; ranges that
    /// still cannot be covered are reported in
    /// [`PublicBackfillReport::uncovered`] rather than failing the recovery
    /// flow. An `Err` from this method means the scan range itself could not
    /// be established (chain-tip lookup failed, or `from_block > to_block`).
    pub async fn backfill_public_notes_by_tag(
        &mut self,
        account_id: AccountId,
        from_block: Option<BlockNumber>,
        to_block: Option<BlockNumber>,
    ) -> Result<PublicBackfillReport> {
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
                // Provisionally `Imported` outcomes, re-classified in one
                // batched consumed-state check below.
                let mut imported: Vec<(usize, NoteDetailsCommitment)> = Vec::new();
                for (note, proof) in pending {
                    let commitment = note.details_commitment();
                    // Unlike the proposal import, a proof-less (`Expected`)
                    // record is NOT skipped here: this primitive exists
                    // because forward sync will never revisit the note's
                    // block, so the freshly fetched proof is applied to
                    // upgrade the record in place (upstream import handles
                    // existing records).
                    if let Some(record) = existing.get(&commitment)
                        && (record.is_consumed() || record.inclusion_proof().is_some())
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
                        continue;
                    }
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
            outcomes,
            uncovered,
            retryable,
            reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use miden_client::builder::ClientBuilder;
    use miden_client::keystore::FilesystemKeyStore;
    use miden_client::rpc::Endpoint;
    use miden_client::testing::mock::MockRpcApi;
    use miden_client::testing::{MockChain, NoteBuilder};
    use miden_client_sqlite_store::SqliteStore;
    use miden_protocol::Word;
    use miden_protocol::account::Account;
    use miden_protocol::account::auth::AuthSecretKey;
    use miden_protocol::asset::FungibleAsset;
    use miden_protocol::crypto::rand::RandomCoin;
    use miden_protocol::transaction::RawOutputNote;
    use miden_standards::account::auth::AuthSingleSig;
    use miden_standards::note::P2idNote;
    use rand::SeedableRng;

    use super::*;
    use crate::keystore::GuardianKeyStore;
    use crate::prover::ProverConfig;
    use crate::rpc::RpcConfig;

    /// Fully offline MultisigClient with the given node API injected into
    /// both the inner Miden client and the multisig client's direct node
    /// channel, so node-backed primitives and store syncs see the same mock
    /// chain.
    async fn offline_client_with_node(
        dir: &Path,
        node: Arc<dyn miden_client::rpc::NodeRpcClient>,
    ) -> MultisigClient {
        let store = SqliteStore::new(dir.join("store.sqlite3"))
            .await
            .expect("sqlite store opens");
        let keystore_dir = dir.join("keys");
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");

        let miden_client = ClientBuilder::<FilesystemKeyStore>::new()
            .rpc(node.clone())
            .store(Arc::new(store))
            .filesystem_keystore(keystore_dir)
            .expect("keystore opens")
            .build()
            .await
            .expect("miden client builds");

        let mut client = MultisigClient::new(
            miden_client,
            Arc::new(GuardianKeyStore::generate()),
            "http://localhost:1".to_string(),
            dir.to_path_buf(),
            Endpoint::localhost(),
            None,
            ProverConfig::new(),
            RpcConfig::new(),
        );
        client.set_node_rpc_client(node);
        client
    }

    /// A plain wallet account with a fresh seed; enough for the store-side
    /// account/tag behavior under test (no multisig components needed).
    fn test_wallet(seed: u8) -> Account {
        use miden_client::account::component::BasicWallet;
        use miden_client::account::{
            AccountBuilder, AccountBuilderSchemaCommitmentExt, AccountType,
        };
        use miden_protocol::account::auth::AuthScheme;
        use miden_standards::account::auth::Approver;

        let key_pair = AuthSecretKey::new_falcon512_poseidon2();
        let auth_component = AuthSingleSig::new(Approver::new(
            key_pair.public_key().to_commitment(),
            AuthScheme::Falcon512Poseidon2,
        ));
        AccountBuilder::new([seed; 32])
            .account_type(AccountType::Private)
            .with_component(auth_component)
            .with_component(BasicWallet)
            .build_with_schema_commitment()
            .expect("test wallet builds")
    }

    /// A distinct (per `seed`) P2ID note addressed at `target`.
    fn p2id_note_for(target: &Account, seed: u32, note_type: NoteType) -> Note {
        let mut rng = RandomCoin::new(Word::from(&[seed, 0, 0, 0]));
        P2idNote::builder()
            .sender(test_wallet(200).id())
            .target(target.id())
            .asset(FungibleAsset::mock(1))
            .note_type(note_type)
            .generate_serial_number(&mut rng)
            .build()
            .expect("p2id note builds")
            .into()
    }

    fn public_note_for(target: &Account, seed: u32) -> Note {
        p2id_note_for(target, seed, NoteType::Public)
    }

    /// A mock chain holding the given output notes in its first
    /// post-genesis block, with the tip advanced a few blocks past it — the
    /// note-bearing block sits strictly below the tip.
    fn chain_with_notes(notes: Vec<RawOutputNote>) -> Arc<MockRpcApi> {
        let mut builder = MockChain::builder();
        for note in notes {
            builder.add_output_note(note);
        }
        let api = Arc::new(MockRpcApi::new(builder.build().expect("mock chain builds")));
        api.advance_blocks(4);
        api
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
            .backfill_public_notes_by_tag(target.id(), None, None)
            .await
            .unwrap();

        assert_eq!(report.scanned_from, 0);
        assert_eq!(report.scanned_to, cursor.as_u32());
        assert_eq!(report.discovered, 1);
        assert_eq!(report.skipped_private, 0);
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
            .backfill_public_notes_by_tag(target.id(), None, None)
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
    /// imported harmlessly, and private matches are counted but skipped —
    /// the chain does not hold their bodies.
    #[tokio::test]
    async fn backfill_tolerates_tag_collisions_and_skips_private_matches() {
        let dir = tempfile::tempdir().unwrap();
        let target = test_wallet(13);
        let tag = NoteTag::with_account_target(target.id());

        let own = public_note_for(&target, 2);
        let colliding =
            NoteBuilder::new(test_wallet(201).id(), rand::rngs::StdRng::seed_from_u64(7))
                .tag(tag.as_u32())
                .note_type(NoteType::Public)
                .build()
                .expect("colliding note builds");
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
            .backfill_public_notes_by_tag(target.id(), None, None)
            .await
            .unwrap();

        assert_eq!(report.discovered, 3);
        assert_eq!(report.skipped_private, 1);
        assert_eq!(report.outcomes.len(), 2);
        assert!(
            report
                .outcomes
                .iter()
                .all(|o| o.status == NoteImportStatus::Imported),
            "{report:?}"
        );
        let imported: BTreeSet<String> = report
            .outcomes
            .iter()
            .map(|o| o.identifier.clone())
            .collect();
        assert!(imported.contains(&own.id().to_hex()));
        assert!(imported.contains(&colliding.id().to_hex()));
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

        // Built inline instead of via `offline_client_with_node` to keep a
        // handle on the store for seeding the proof-less record.
        let store = Arc::new(
            SqliteStore::new(dir.path().join("store.sqlite3"))
                .await
                .unwrap(),
        );
        let keystore_dir = dir.path().join("keys");
        std::fs::create_dir_all(&keystore_dir).unwrap();
        let miden_client = ClientBuilder::<FilesystemKeyStore>::new()
            .rpc(api.clone())
            .store(store.clone())
            .filesystem_keystore(keystore_dir)
            .unwrap()
            .build()
            .await
            .unwrap();
        let mut client = MultisigClient::new(
            miden_client,
            Arc::new(GuardianKeyStore::generate()),
            "http://localhost:1".to_string(),
            dir.path().to_path_buf(),
            Endpoint::localhost(),
            None,
            ProverConfig::new(),
            RpcConfig::new(),
        );
        client.set_node_rpc_client(api.clone());

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
            .backfill_public_notes_by_tag(target.id(), None, None)
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
            .backfill_public_notes_by_tag(target.id(), None, None)
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
                Some(BlockNumber::from(0u32)),
                Some(BlockNumber::from(3u32)),
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
            .backfill_public_notes_by_tag(target.id(), None, Some(BlockNumber::from(9u32)))
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
            .backfill_public_notes_by_tag(target.id(), None, None)
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
                Some(BlockNumber::from(5u32)),
                Some(BlockNumber::from(1u32)),
            )
            .await
            .expect_err("inverted range must error");
        assert!(err.to_string().contains("inverted"));
    }
}
