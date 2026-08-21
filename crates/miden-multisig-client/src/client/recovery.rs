//! Recovery primitives for restoring note state after device loss.
//!
//! After recovery on a fresh device the local store has no note-transport
//! cursor — and in a shared dirty store another account's sync may have
//! advanced the cursor past notes belonging to the newly recovered account.
//! The primitives here rescan sources that normal forward sync would skip:
//! the private-note transport backlog (issue #414) and the notes embedded in
//! v2 `consume_notes` proposals (issue #415) — sub-issues of #357.

use miden_client::ClientError;
use miden_client::note_transport::NoteTransportError;

use super::MultisigClient;
use crate::error::{Result, error_chain};
use crate::rpc::{is_transient_note_transport_error, is_transient_rpc_error};

/// Outcome class of a private-note transport backlog drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportRecoveryStatus {
    /// The full transport backlog was scanned; every note the transport still
    /// holds for the tracked tags is now in the local store.
    Completed,
    /// The transport could not be consulted at all: it is disabled for this
    /// client (no endpoint configured) or unreachable before anything was
    /// imported (`imported` is always 0 — a connection lost mid-drain after
    /// partial progress reports `Failed` instead). The rest of a recovery
    /// flow should proceed without transport notes.
    Unavailable,
    /// The drain started but did not finish; the backlog may be partially
    /// imported. `retryable` distinguishes transient failures (rerun the
    /// drain) from permanent ones.
    Failed,
}

/// Result of [`MultisigClient::drain_private_note_backlog`].
///
/// Transport problems are reported here rather than returned as errors so a
/// transport failure never aborts the rest of a recovery flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportRecoveryReport {
    /// Outcome class of the drain.
    pub status: TransportRecoveryStatus,
    /// Number of note records newly imported into the local store by this
    /// drain. Can be non-zero even on `Failed`: batches imported before the
    /// failure stay imported.
    pub imported: usize,
    /// Whether rerunning the drain can plausibly succeed (transient
    /// connectivity failures, the upstream pagination convergence guard).
    /// Always `false` on `Completed`.
    pub retryable: bool,
    /// Human-readable cause when the drain did not complete.
    pub reason: Option<String>,
}

impl MultisigClient {
    /// Rescans the full private-note transport backlog for every tracked note
    /// tag and imports what it finds, regardless of the stored transport
    /// cursor (issue #414).
    ///
    /// Use after account recovery: a fresh store has no transport cursor, and
    /// in a shared store another account's sync may have advanced the cursor
    /// past this account's notes. The drain is idempotent, tag-scoped (the
    /// recovered account must already be in the store so its note tag is
    /// tracked — [`MultisigClient::pull_account`] does this), and never
    /// regresses an already-advanced cursor.
    ///
    /// Transport recovery is bounded by the transport service's retention:
    /// senders may bypass the transport entirely and relayed blobs are pruned
    /// after the retention window, so this is a best-effort rescan, **not** a
    /// backup. Transport-disabled and transport-unreachable outcomes are
    /// reported in the [`TransportRecoveryReport`] rather than returned as
    /// errors; an `Err` from this method means the local store itself failed.
    pub async fn drain_private_note_backlog(&mut self) -> Result<TransportRecoveryReport> {
        if !self.miden_client.is_note_transport_enabled() {
            return Ok(TransportRecoveryReport {
                status: TransportRecoveryStatus::Unavailable,
                imported: 0,
                retryable: false,
                reason: Some(
                    "note transport is not configured for this client; \
                     set a transport endpoint to relay private notes"
                        .to_string(),
                ),
            });
        }

        let before = self.input_note_count().await?;
        let drain_result = self.miden_client.fetch_all_private_notes().await;
        // Count even when the drain failed: each fetched batch is imported as
        // it arrives, so notes recovered before the failure stay in the store.
        // A drain never removes records (and `&mut self` excludes concurrent
        // client operations), so the length delta is the count of newly
        // imported records.
        let after = self.input_note_count().await?;
        let imported = after.saturating_sub(before);

        match drain_result {
            Ok(()) => Ok(TransportRecoveryReport {
                status: TransportRecoveryStatus::Completed,
                imported,
                retryable: false,
                reason: None,
            }),
            // A broken local store is an environment failure, not a transport
            // outcome: the whole recovery flow needs to know, so it
            // propagates instead of being folded into the report.
            Err(err @ ClientError::StoreError(_)) => {
                Err(crate::error::MultisigError::miden_client_with_context(
                    "local store failed during the transport drain",
                    err,
                ))
            }
            Err(err) => {
                let (status, retryable) = classify_drain_failure(&err);
                // `Unavailable` promises "nothing was imported"; a connection
                // lost mid-drain after partial progress is an interrupted
                // drain, so report it as a retryable failure instead.
                let status = if imported > 0 && status == TransportRecoveryStatus::Unavailable {
                    TransportRecoveryStatus::Failed
                } else {
                    status
                };
                Ok(TransportRecoveryReport {
                    status,
                    imported,
                    retryable,
                    reason: Some(error_chain(&err)),
                })
            }
        }
    }

    /// Number of input note records currently in the local store.
    async fn input_note_count(&mut self) -> Result<usize> {
        let records = self
            .miden_client
            .get_input_notes(miden_client::store::NoteFilter::All)
            .await
            .map_err(|e| {
                crate::error::MultisigError::miden_client_with_context(
                    "failed to list input notes",
                    e,
                )
            })?;
        Ok(records.len())
    }
}

/// Maps a `fetch_all_private_notes` failure onto a report class:
/// `(status, retryable)`.
fn classify_drain_failure(err: &ClientError) -> (TransportRecoveryStatus, bool) {
    match err {
        // The match is deliberately exhaustive so a new upstream variant is a
        // compile error here rather than a silently wrong classification.
        ClientError::NoteTransportError(transport_err) => match transport_err {
            // No transport configured — retrying cannot help until the client
            // is rebuilt with an endpoint.
            NoteTransportError::Disabled => (TransportRecoveryStatus::Unavailable, false),
            // `Connection` wraps endpoint parsing, TLS configuration, and
            // actual connect failures indiscriminately; the shared classifier
            // inspects the cause chain to tell a retry-worthy dropped
            // connection from a permanently misconfigured endpoint.
            NoteTransportError::Connection(_) => (
                TransportRecoveryStatus::Unavailable,
                is_transient_note_transport_error(transport_err),
            ),
            // The transport answered with an error — worth retrying once the
            // service recovers.
            NoteTransportError::Network(_) => (TransportRecoveryStatus::Unavailable, true),
            // The upstream convergence guard tripped (the server cursor kept
            // advancing for 1000 iterations without an empty batch) — a
            // server-side bug, not an honest backlog. Retryable in the sense
            // that a rerun is safe (imports are idempotent) and succeeds once
            // the server recovers; while the server misbehaves, each rerun
            // repeats the same full scan and trips the same guard.
            NoteTransportError::PaginationDidNotTerminate(_) => {
                (TransportRecoveryStatus::Failed, true)
            }
            // Undecodable payloads: a rerun would hit the same bytes again.
            NoteTransportError::Deserialization(_) => (TransportRecoveryStatus::Failed, false),
        },
        // Each fetched batch is imported through the node (inclusion-proof
        // lookup), so a node RPC failure interrupts the drain mid-way; the
        // shared gRPC classifier decides whether a rerun can help.
        ClientError::RpcError(rpc_err) => (
            TransportRecoveryStatus::Failed,
            is_transient_rpc_error(rpc_err),
        ),
        _ => (TransportRecoveryStatus::Failed, false),
    }
}

use std::collections::{BTreeMap, BTreeSet, HashSet};

use miden_client::note::NoteFile;
use miden_client::store::NoteFilter as StoreNoteFilter;
use miden_protocol::block::BlockNumber;
use miden_protocol::note::{Note, NoteDetailsCommitment, NoteId, NoteInclusionProof};

use crate::proposal::{Proposal, ProposalMetadata};

/// Where a recovered note's bytes came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteImportSource {
    /// Embedded in a v2 `consume_notes` proposal (`consume_notes_notes`).
    Proposal,
}

impl NoteImportSource {
    /// Stable string form, shared with the TS SDK's `source` union.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Proposal => "proposal",
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

/// Decodes and deduplicates the embedded notes of v2 `consume_notes`
/// proposals. Undecodable entries are reported as `Invalid` outcomes with a
/// positional identifier; a note embedded by several proposals folds into its
/// first occurrence.
///
/// Decoding is deliberately permissive: the strict `note.id() == note_ids[i]`
/// binding check belongs to the verify/execute path (`execution.rs`), where a
/// mismatch must block signing. Recovery imports whatever real notes the
/// bytes decode to — a note is self-validating (its ID derives from its
/// contents), so importing it is harmless even when the proposal's declared
/// note IDs disagree.
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
            match serialized.to_note() {
                Ok(note) => {
                    if seen.insert(note.id()) {
                        decoded.push(note);
                    }
                }
                Err(e) => outcomes.push(NoteImportOutcome {
                    identifier: format!("proposal {} notes[{}]", proposal_id, index),
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
    /// Imports the notes embedded in v2 `consume_notes` proposals into the
    /// local Miden store (issue #415), typically after key-based recovery
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
    pub async fn import_notes_from_proposals(
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

        // Look up existing records by details commitment, not note ID: records
        // the store keeps without metadata (a note details import in Expected
        // state, or a note observed as consumed on chain) have no note ID, and
        // an ID lookup would keep re-importing them forever.
        let commitments: Vec<NoteDetailsCommitment> =
            decoded.iter().map(Note::details_commitment).collect();
        let existing = match self
            .miden_client
            .get_input_notes(StoreNoteFilter::DetailsCommitments(commitments))
            .await
        {
            Ok(records) => records
                .into_iter()
                .map(|record| (record.details_commitment(), record))
                .collect::<BTreeMap<_, _>>(),
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
                    let file = NoteFile::NoteWithProof(note, proof);
                    match self
                        .miden_client
                        .import_notes(std::slice::from_ref(&file))
                        .await
                    {
                        Ok(_) => {
                            imported.push((outcomes.len(), details_commitment));
                            NoteImportOutcome {
                                identifier,
                                source: NoteImportSource::Proposal,
                                status: NoteImportStatus::Imported,
                                retryable: false,
                                reason: None,
                            }
                        }
                        Err(e) => NoteImportOutcome {
                            identifier,
                            source: NoteImportSource::Proposal,
                            status: NoteImportStatus::Failed,
                            retryable: import_error_retryable(&e),
                            reason: Some(format!("failed to import note: {}", e)),
                        },
                    }
                }
                None => {
                    // The tag makes the resulting `Expected` record reachable
                    // by sync (upstream registers a note-source tag record
                    // only when the details file carries a tag).
                    let tag = note.metadata().tag();
                    let file = NoteFile::NoteDetails {
                        details: note.into(),
                        after_block_num: BlockNumber::from(0u32),
                        tag: Some(tag),
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

        // A note the chain already nullified is stored as consumption history,
        // not as a consumable note — report that honestly instead of
        // `Imported`. One batched store read covers every imported note.
        if !imported.is_empty() {
            let check = self
                .miden_client
                .get_input_notes(StoreNoteFilter::DetailsCommitments(
                    imported.iter().map(|(_, commitment)| *commitment).collect(),
                ))
                .await;
            match check {
                Ok(records) => {
                    let consumed: BTreeSet<NoteDetailsCommitment> = records
                        .iter()
                        .filter(|record| record.is_consumed())
                        .map(|record| record.details_commitment())
                        .collect();
                    for (index, commitment) in &imported {
                        if consumed.contains(commitment) {
                            outcomes[*index].status = NoteImportStatus::AlreadyConsumed;
                            outcomes[*index].reason = Some(
                                "note was already consumed on chain; recorded as consumption \
                                 history"
                                    .to_string(),
                            );
                        }
                    }
                }
                Err(e) => {
                    // The imports themselves succeeded; stay `Imported` but
                    // flag that the consumed-state classification is unknown.
                    for (index, _) in &imported {
                        outcomes[*index].reason = Some(format!(
                            "imported, but the consumed-state check failed ({}); run sync to \
                             confirm the note's status",
                            e
                        ));
                    }
                }
            }
        }

        outcomes
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use miden_client::builder::ClientBuilder;
    use miden_client::keystore::FilesystemKeyStore;
    use miden_client::note_transport::{NoteTransportClient, NoteTransportError};
    use miden_client::rpc::Endpoint;
    use miden_client::testing::mock::MockRpcApi;
    use miden_client::testing::note_transport::{MockNoteTransportApi, MockNoteTransportNode};
    use miden_client::{ClientError, Serializable};
    use miden_client_sqlite_store::SqliteStore;
    use miden_protocol::{Felt, Word};
    use miden_protocol::account::Account;
    use miden_protocol::account::auth::AuthSecretKey;
    use miden_protocol::crypto::rand::RandomCoin;
    use miden_protocol::note::{Note, NoteDetails, NoteTag, NoteType};
    use miden_standards::account::auth::AuthSingleSig;
    use miden_standards::note::P2idNote;
    use miden_tx::utils::sync::RwLock;

    use super::*;
    use crate::keystore::GuardianKeyStore;
    use crate::prover::ProverConfig;
    use crate::rpc::RpcConfig;

    // ---------------------------------------------------------------------
    // classification
    // ---------------------------------------------------------------------

    fn transport_error(err: NoteTransportError) -> ClientError {
        ClientError::NoteTransportError(err)
    }

    #[test]
    fn disabled_transport_is_unavailable_and_not_retryable() {
        let (status, retryable) =
            classify_drain_failure(&transport_error(NoteTransportError::Disabled));
        assert_eq!(status, TransportRecoveryStatus::Unavailable);
        assert!(!retryable);
    }

    #[test]
    fn unreachable_transport_is_unavailable_and_retryable() {
        let connection = NoteTransportError::Connection(Box::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "connection refused",
        )));
        let (status, retryable) = classify_drain_failure(&transport_error(connection));
        assert_eq!(status, TransportRecoveryStatus::Unavailable);
        assert!(retryable);

        let network = NoteTransportError::Network("transport 503".to_string());
        let (status, retryable) = classify_drain_failure(&transport_error(network));
        assert_eq!(status, TransportRecoveryStatus::Unavailable);
        assert!(retryable);
    }

    /// `Connection` also wraps endpoint-parse/TLS misconfiguration; the
    /// shared cause-chain classifier marks those permanent so a recovery
    /// flow does not loop retrying a client that can never connect.
    #[test]
    fn misconfigured_transport_endpoint_is_unavailable_and_not_retryable() {
        let connection = NoteTransportError::Connection(Box::new(std::io::Error::other(
            "invalid uri: missing scheme",
        )));
        let (status, retryable) = classify_drain_failure(&transport_error(connection));
        assert_eq!(status, TransportRecoveryStatus::Unavailable);
        assert!(!retryable);
    }

    #[test]
    fn pagination_convergence_guard_is_a_retryable_failure() {
        let (status, retryable) = classify_drain_failure(&transport_error(
            NoteTransportError::PaginationDidNotTerminate(1_000),
        ));
        assert_eq!(status, TransportRecoveryStatus::Failed);
        assert!(retryable);
    }

    #[test]
    fn node_rpc_failure_mid_drain_is_a_retryable_failure() {
        let err: ClientError = miden_client::rpc::RpcError::RequestError {
            endpoint: miden_client::rpc::RpcEndpoint::SyncChainMmr,
            error_kind: miden_client::rpc::GrpcError::Unavailable,
            endpoint_error: None,
            source: None,
        }
        .into();
        let (status, retryable) = classify_drain_failure(&err);
        assert_eq!(status, TransportRecoveryStatus::Failed);
        assert!(retryable);
    }

    /// The shared gRPC classifier decides retryability for node failures:
    /// a permanent status (bad request, wrong node version) must not tell
    /// callers to rerun the drain.
    #[test]
    fn permanent_node_rpc_failure_mid_drain_is_not_retryable() {
        let err: ClientError = miden_client::rpc::RpcError::RequestError {
            endpoint: miden_client::rpc::RpcEndpoint::SyncChainMmr,
            error_kind: miden_client::rpc::GrpcError::InvalidArgument,
            endpoint_error: None,
            source: None,
        }
        .into();
        let (status, retryable) = classify_drain_failure(&err);
        assert_eq!(status, TransportRecoveryStatus::Failed);
        assert!(!retryable);
    }

    #[test]
    fn other_client_errors_are_permanent_failures() {
        let (status, retryable) = classify_drain_failure(&ClientError::AddNewAccountWithoutSeed);
        assert_eq!(status, TransportRecoveryStatus::Failed);
        assert!(!retryable);
    }

    // ---------------------------------------------------------------------
    // offline behavioral tests (mock chain + mock transport)
    // ---------------------------------------------------------------------

    /// Fully offline MultisigClient: mock chain RPC, SQLite store in a temp
    /// dir, and (optionally) the upstream mock note transport.
    async fn offline_client(
        dir: &Path,
        transport: Option<Arc<dyn NoteTransportClient>>,
    ) -> MultisigClient {
        let store = SqliteStore::new(dir.join("store.sqlite3"))
            .await
            .expect("sqlite store opens");
        let keystore_dir = dir.join("keys");
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");

        let mut builder = ClientBuilder::<FilesystemKeyStore>::new()
            .rpc(Arc::new(MockRpcApi::default()))
            .store(Arc::new(store))
            .filesystem_keystore(keystore_dir)
            .expect("keystore opens");
        if let Some(transport) = transport {
            builder = builder.note_transport(transport);
        }
        let miden_client = builder.build().await.expect("miden client builds");

        MultisigClient::new(
            miden_client,
            Arc::new(GuardianKeyStore::generate()),
            "http://localhost:1".to_string(),
            dir.to_path_buf(),
            Endpoint::localhost(),
            None,
            ProverConfig::new(),
            RpcConfig::new(),
        )
    }

    fn mock_transport() -> (
        Arc<RwLock<MockNoteTransportNode>>,
        Arc<dyn NoteTransportClient>,
    ) {
        let node = Arc::new(RwLock::new(MockNoteTransportNode::new()));
        let api: Arc<dyn NoteTransportClient> = Arc::new(MockNoteTransportApi::new(node.clone()));
        (node, api)
    }

    /// A plain wallet account with a fresh seed; enough for the store-side
    /// account/tag behavior under test (no multisig components needed).
    fn test_wallet(seed: u8) -> Account {
        use miden_client::account::component::BasicWallet;
        use miden_client::account::{
            AccountBuilder, AccountBuilderSchemaCommitmentExt, AccountType,
        };
        use miden_client::auth::AuthSchemeId;

        let key_pair = AuthSecretKey::new_falcon512_poseidon2();
        let auth_component = AuthSingleSig::new(
            key_pair.public_key().to_commitment(),
            AuthSchemeId::Falcon512Poseidon2,
        );
        AccountBuilder::new([seed; 32])
            .account_type(AccountType::Private)
            .with_auth_component(auth_component)
            .with_component(BasicWallet)
            .build_with_schema_commitment()
            .expect("test wallet builds")
    }

    /// A distinct (per `seed`) private P2ID note addressed at `target`.
    fn private_note_for(target: &Account, seed: u32) -> Note {
        let mut rng = RandomCoin::new(Word::from(&[seed, 0, 0, 0]));
        P2idNote::create(
            target.id(),
            target.id(),
            vec![],
            NoteType::Private,
            Default::default(),
            &mut rng,
        )
        .expect("p2id note builds")
    }

    fn add_to_transport(node: &Arc<RwLock<MockNoteTransportNode>>, note: Note) {
        let header = *note.header();
        let details_bytes = NoteDetails::from(note).to_bytes();
        node.write().add_note(header, details_bytes);
    }

    #[tokio::test]
    async fn drain_reports_unavailable_when_transport_is_not_configured() {
        let dir = tempfile::tempdir().unwrap();
        let mut client = offline_client(dir.path(), None).await;

        let report = client
            .drain_private_note_backlog()
            .await
            .expect("disabled transport is reported, not thrown");

        assert_eq!(report.status, TransportRecoveryStatus::Unavailable);
        assert_eq!(report.imported, 0);
        assert!(!report.retryable);
        assert!(report.reason.unwrap().contains("not configured"));
    }

    #[tokio::test]
    async fn drain_recovers_the_backlog_for_a_tracked_account_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let (node, api) = mock_transport();
        let mut client = offline_client(dir.path(), Some(api)).await;

        // The recovered account is in the store (as after pull_account), so
        // its standard note tag is tracked.
        let account = test_wallet(1);
        client.add_or_update_account(&account, false).await.unwrap();
        add_to_transport(&node, private_note_for(&account, 1));

        let report = client.drain_private_note_backlog().await.unwrap();
        assert_eq!(report.status, TransportRecoveryStatus::Completed);
        assert_eq!(report.imported, 1);
        assert!(!report.retryable);
        assert_eq!(report.reason, None);

        // Idempotence: draining again re-fetches the same backlog but imports
        // nothing new.
        let report = client.drain_private_note_backlog().await.unwrap();
        assert_eq!(report.status, TransportRecoveryStatus::Completed);
        assert_eq!(report.imported, 0);
    }

    #[tokio::test]
    async fn drain_is_tag_scoped_a_store_with_no_tracked_tags_imports_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (node, api) = mock_transport();
        let mut client = offline_client(dir.path(), Some(api)).await;

        // A note is waiting on the transport, but no account is in the store,
        // so no tag is tracked.
        add_to_transport(&node, private_note_for(&test_wallet(2), 1));

        let report = client.drain_private_note_backlog().await.unwrap();
        assert_eq!(report.status, TransportRecoveryStatus::Completed);
        assert_eq!(report.imported, 0);
    }

    #[tokio::test]
    async fn drain_never_regresses_an_advanced_transport_cursor() {
        use miden_client::note_transport::NOTE_TRANSPORT_CURSOR_STORE_SETTING;

        let dir = tempfile::tempdir().unwrap();
        let (node, api) = mock_transport();
        let mut client = offline_client(dir.path(), Some(api)).await;

        let account = test_wallet(3);
        client.add_or_update_account(&account, false).await.unwrap();
        add_to_transport(&node, private_note_for(&account, 1));

        // Simulate a store whose cursor another account's sync has already
        // advanced far past this backlog. The store encodes the cursor as raw
        // big-endian u64 bytes.
        let advanced: u64 = u64::MAX;
        client
            .miden_client
            .set_setting(
                NOTE_TRANSPORT_CURSOR_STORE_SETTING.to_string(),
                advanced.to_be_bytes(),
            )
            .await
            .unwrap();

        // The drain ignores the stored cursor for scanning (the backlogged
        // note is still recovered)...
        let report = client.drain_private_note_backlog().await.unwrap();
        assert_eq!(report.status, TransportRecoveryStatus::Completed);
        assert_eq!(report.imported, 1);

        // ...but persists max(drain_cursor, stored_cursor): the advanced
        // cursor survives.
        let stored: [u8; 8] = client
            .miden_client
            .get_setting(NOTE_TRANSPORT_CURSOR_STORE_SETTING.to_string())
            .await
            .unwrap()
            .expect("cursor setting exists");
        assert_eq!(u64::from_be_bytes(stored), advanced);
    }

    /// Transport that serves a limited number of successful fetches, then
    /// fails with a connection-shaped error — a connection dropped mid-drain.
    struct InterruptibleTransport {
        inner: MockNoteTransportApi,
        fetches_before_failure: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl NoteTransportClient for InterruptibleTransport {
        async fn send_note(
            &self,
            header: miden_protocol::note::NoteHeader,
            details: Vec<u8>,
        ) -> std::result::Result<(), NoteTransportError> {
            self.inner.send_note(header, details);
            Ok(())
        }

        async fn fetch_notes(
            &self,
            tags: &[NoteTag],
            cursor: miden_client::note_transport::NoteTransportCursor,
        ) -> std::result::Result<
            (
                Vec<miden_client::note_transport::NoteInfo>,
                miden_client::note_transport::NoteTransportCursor,
            ),
            NoteTransportError,
        > {
            let allowed = self
                .fetches_before_failure
                .fetch_update(
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                    |n| n.checked_sub(1),
                )
                .is_ok();
            if !allowed {
                return Err(NoteTransportError::Network(
                    "connection dropped mid-drain".to_string(),
                ));
            }
            Ok(self.inner.fetch_notes(tags, cursor))
        }

        async fn stream_notes(
            &self,
            _tag: NoteTag,
            _cursor: miden_client::note_transport::NoteTransportCursor,
        ) -> std::result::Result<
            Box<dyn miden_client::note_transport::NoteStream>,
            NoteTransportError,
        > {
            Ok(Box::new(
                miden_client::testing::note_transport::DummyNoteStream {},
            ))
        }
    }

    /// A connection lost mid-drain after partial progress must not report
    /// `Unavailable` ("nothing was imported"): it is an interrupted,
    /// retryable drain and the partial count is kept.
    #[tokio::test]
    async fn interrupted_drain_with_partial_progress_reports_a_retryable_failure() {
        let dir = tempfile::tempdir().unwrap();
        // Cap each response at one note so the two-note backlog needs two
        // fetches; the transport dies after the first.
        let node = Arc::new(RwLock::new(MockNoteTransportNode::with_max_batch(1)));
        let api: Arc<dyn NoteTransportClient> = Arc::new(InterruptibleTransport {
            inner: MockNoteTransportApi::new(node.clone()),
            fetches_before_failure: std::sync::atomic::AtomicUsize::new(1),
        });
        let mut client = offline_client(dir.path(), Some(api)).await;

        let account = test_wallet(6);
        client.add_or_update_account(&account, false).await.unwrap();
        add_to_transport(&node, private_note_for(&account, 1));
        // Distinct cursor values require distinct insertion timestamps.
        std::thread::sleep(std::time::Duration::from_millis(2));
        add_to_transport(&node, private_note_for(&account, 2));

        let report = client.drain_private_note_backlog().await.unwrap();

        assert_eq!(report.status, TransportRecoveryStatus::Failed);
        assert!(report.retryable);
        assert_eq!(report.imported, 1);
        assert!(report.reason.unwrap().contains("connection dropped"));
    }

    // ---------------------------------------------------------------------
    // load/tag regression suite: the store-side behavior pull_account relies
    // on, which gates whether a drain sees the recovered account at all.
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn adding_an_account_tracks_its_standard_note_tag() {
        let dir = tempfile::tempdir().unwrap();
        let mut client = offline_client(dir.path(), None).await;

        let account = test_wallet(4);
        client.add_or_update_account(&account, false).await.unwrap();

        let expected = NoteTag::with_account_target(account.id());
        let tags = client.miden_client.get_note_tags().await.unwrap();
        assert!(
            tags.iter().any(|record| record.tag == expected),
            "the recovered account's standard note tag must be tracked after insert"
        );
    }

    #[tokio::test]
    async fn re_adding_an_existing_account_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mut client = offline_client(dir.path(), None).await;

        let account = test_wallet(5);
        client.add_or_update_account(&account, false).await.unwrap();
        // Reload path: pull_account calls add_or_update_account(_, true) on
        // an account that is already present.
        client.add_or_update_account(&account, true).await.unwrap();

        let expected = NoteTag::with_account_target(account.id());
        let tags = client.miden_client.get_note_tags().await.unwrap();
        assert_eq!(
            tags.iter().filter(|record| record.tag == expected).count(),
            1,
            "reload must not duplicate or drop the account's note tag"
        );
        assert!(
            client
                .miden_client
                .get_account(account.id())
                .await
                .unwrap()
                .is_some()
        );
    }

    // ---------------------------------------------------------------------
    // live smoke (real testnet transport) — run explicitly:
    //   cargo test -p miden-multisig-client --lib live_testnet -- --ignored
    // ---------------------------------------------------------------------

    async fn live_client(dir: &Path, with_transport: bool) -> MultisigClient {
        use miden_client::note_transport::NOTE_TRANSPORT_TESTNET_ENDPOINT;

        let mut builder = MultisigClient::builder()
            .miden_endpoint(Endpoint::testnet())
            .guardian_endpoint("http://localhost:1")
            .account_dir(dir)
            .generate_key();
        if with_transport {
            builder = builder.note_transport_endpoint(NOTE_TRANSPORT_TESTNET_ENDPOINT);
        }
        builder.build().await.expect("live client builds")
    }

    /// The full device-loss round trip over the real testnet transport (the
    /// scenario spike #412 validated): relay a private note, recover it into
    /// a fresh store via the drain, and check idempotence plus the
    /// disabled-transport report. Network-dependent, hence ignored in CI.
    #[tokio::test]
    #[ignore = "requires network access to the Miden testnet"]
    async fn live_testnet_transport_drain_round_trip() {
        use miden_protocol::address::Address;

        let dir = tempfile::tempdir().unwrap();
        let account = test_wallet(9);

        // "Old device": relay a private note addressed at the account.
        // Transport delivery needs no on-chain transaction.
        let mut sender = live_client(&dir.path().join("sender"), true).await;
        sender
            .miden_client
            .send_private_note(private_note_for(&account, 77), &Address::new(account.id()))
            .await
            .expect("transport send succeeds");

        // "New device": fresh store; pulling the account tracks its tag.
        let mut recovered = live_client(&dir.path().join("recovered"), true).await;
        recovered
            .add_or_update_account(&account, false)
            .await
            .unwrap();

        let report = recovered.drain_private_note_backlog().await.unwrap();
        assert_eq!(report.status, TransportRecoveryStatus::Completed);
        assert!(
            report.imported >= 1,
            "the relayed note must be recovered, got {report:?}"
        );

        let report = recovered.drain_private_note_backlog().await.unwrap();
        assert_eq!(report.status, TransportRecoveryStatus::Completed);
        assert_eq!(report.imported, 0, "re-drain must be idempotent");

        // A custom node endpoint derives no transport service (the testnet
        // preset keeps the upstream default transport), so the drain reports
        // Unavailable without touching the network.
        let mut no_transport = MultisigClient::builder()
            .miden_endpoint(Endpoint::new(
                "http".to_string(),
                "node".to_string(),
                Some(1),
            ))
            .guardian_endpoint("http://localhost:1")
            .account_dir(dir.path().join("no-transport"))
            .generate_key()
            .build()
            .await
            .expect("custom-endpoint client builds");
        let report = no_transport.drain_private_note_backlog().await.unwrap();
        assert_eq!(report.status, TransportRecoveryStatus::Unavailable);
        assert!(!report.retryable);
    }

    // ---------------------------------------------------------------------
    // proposal-embedded note import (#415)
    // ---------------------------------------------------------------------

    use miden_protocol::account::AccountId;
    use miden_protocol::account::delta::{AccountDelta, AccountStorageDelta, AccountVaultDelta};
    use crate::proposal::SerializedNote;

    fn build_test_note(seed: u64) -> Note {
        let sender = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();
        let target = AccountId::from_hex("0x1b1b1b1a1b1b1b011b1b1b1b1b1b1b").unwrap();
        let mut rng = RandomCoin::new(Word::from([Felt::new_unchecked(seed); 4]));
        P2idNote::create(
            sender,
            target,
            vec![],
            NoteType::Private,
            Default::default(),
            &mut rng,
        )
        .unwrap()
    }

    fn v2_metadata(notes: Vec<SerializedNote>) -> ProposalMetadata {
        ProposalMetadata {
            consume_notes_metadata_version: Some(
                crate::proposal::CONSUME_NOTES_METADATA_VERSION_V2,
            ),
            consume_notes_notes: notes,
            ..Default::default()
        }
    }

    #[test]
    fn collects_decoded_notes_from_v2_metadata() {
        let note = build_test_note(1);
        let expected_id = note.id();
        let metadata = v2_metadata(vec![SerializedNote::from_note(&note)]);

        let mut outcomes = Vec::new();
        let decoded = collect_notes([("p-1", &metadata)], &mut outcomes);
        assert!(outcomes.is_empty());
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].id(), expected_id);
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
        let metadata = v2_metadata(vec![
            SerializedNote::from_base64("!!! not base64 !!!".to_string()),
            SerializedNote::from_note(&good),
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
        let first = v2_metadata(vec![SerializedNote::from_note(&shared)]);
        let second = v2_metadata(vec![
            SerializedNote::from_note(&shared),
            SerializedNote::from_note(&other),
        ]);

        let mut outcomes = Vec::new();
        let decoded = collect_notes([("p-1", &first), ("p-2", &second)], &mut outcomes);
        assert!(outcomes.is_empty());
        let ids: Vec<_> = decoded.iter().map(Note::id).collect();
        assert_eq!(ids, vec![shared.id(), other.id()]);
    }

    fn v2_proposal(id: &str, notes: Vec<SerializedNote>) -> Proposal {
        let account_id = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();
        let delta = AccountDelta::new(
            account_id,
            AccountStorageDelta::default(),
            AccountVaultDelta::default(),
            miden_protocol::Felt::ZERO,
        )
        .unwrap();
        let tx_summary = miden_protocol::transaction::TransactionSummary::new(
            delta,
            miden_protocol::transaction::InputNotes::new(Vec::new()).unwrap(),
            miden_protocol::transaction::RawOutputNotes::new(Vec::new()).unwrap(),
            Word::default(),
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
            metadata: v2_metadata(notes),
        }
    }

    /// Exercises the public method end to end against an unreachable node:
    /// decoding, deduplication, and invalid isolation must all happen before
    /// any network access, and the proof-fetch failure must surface as
    /// per-note retryable outcomes instead of aborting the batch. (The
    /// success-path branches are covered by the TS unit twin and were
    /// validated live against testnet; the in-repo scripted node cannot yet
    /// serve successful note responses.)
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
                    SerializedNote::from_note(&note),
                    SerializedNote::from_base64("!!! corrupt !!!".to_string()),
                ],
            ),
            // Duplicate embedding of the same note -> deduplicated.
            v2_proposal("p-2", vec![SerializedNote::from_note(&note)]),
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

    #[test]
    fn status_and_source_render_stable_strings() {
        assert_eq!(NoteImportSource::Proposal.to_string(), "proposal");
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
