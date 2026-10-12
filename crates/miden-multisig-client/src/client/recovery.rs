//! Recovery primitives for restoring note state after device loss.
//!
//! After recovery on a fresh device, or after adding a recovered account to a
//! shared store, the account's note tag has no note-transport cursor yet.
//! Since miden-client 0.17.1 the transport keeps one cursor per tag and scans
//! a tag without one from the first retained note, but each sync fetches a
//! bounded number of pages. The drain here keeps fetching until a fetch moves
//! no tag's cursor, so a recovery flow does not have to wait for later
//! background syncs.

use miden_client::ClientError;
use miden_client::note_transport::{NOTE_TRANSPORT_CURSORS_KEY, NoteTransportError};
use miden_client::store::SettingScope;

use super::MultisigClient;
use crate::error::{Result, error_chain};
use crate::rpc::{is_transient_note_transport_error, is_transient_rpc_error};

/// Upper bound on transport fetches one drain runs. Each fetch reads up to 32
/// pages per request group of up to 128 tags; a backlog whose cursors still
/// move after this many fetches is reported as a retryable failure, and the
/// stored per-tag cursors let the next drain or sync continue from there.
const MAX_DRAIN_PASSES: usize = 64;

/// Outcome class of a private-note transport backlog drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportRecoveryStatus {
    /// The transport backlog was fetched until a fetch moved no tag's cursor;
    /// every note the transport still holds for the tracked tags is now in
    /// the local store.
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
    /// drain. Can be non-zero even on `Failed`: pages fetched before the
    /// failure stay imported.
    pub imported: usize,
    /// Whether rerunning the drain can plausibly succeed (transient
    /// connectivity failures, a backlog longer than one drain's fetch
    /// budget). Always `false` on `Completed`.
    pub retryable: bool,
    /// Human-readable cause when the drain did not complete.
    pub reason: Option<String>,
}

impl MultisigClient {
    /// Fetches the private-note transport backlog for every tracked note tag
    /// and imports what it finds.
    ///
    /// Use after account recovery: the recovered account must already be in
    /// the store so its note tag is tracked ([`MultisigClient::pull_account`]
    /// does this). Its tag has no transport cursor yet, so the transport
    /// serves it from the first retained note. The drain runs transport
    /// fetches until one moves no tag's cursor, which only an exhausted
    /// backlog leaves unchanged: a fetch whose pages were all duplicates or
    /// screened out still advances it. The drain is idempotent: imports
    /// dedupe, and per-tag cursors only move forward.
    ///
    /// Transport recovery is bounded by the transport service's retention:
    /// senders may bypass the transport entirely and relayed blobs are pruned
    /// after the retention window, so this is a best-effort rescan, **not** a
    /// backup. Transport-disabled and transport-unreachable outcomes are
    /// reported in the [`TransportRecoveryReport`] rather than returned as
    /// errors; an `Err` from this method means the local store itself failed.
    pub(crate) async fn drain_private_note_backlog(&mut self) -> Result<TransportRecoveryReport> {
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
        let mut drain_result = Err(DrainStop::BudgetExhausted);
        let mut last = self.transport_cursors().await?;
        for _ in 0..MAX_DRAIN_PASSES {
            // A failed fetch still imports the pages fetched before it, so
            // the count below includes them.
            if let Err(err) = self.miden_client.fetch_private_notes().await {
                drain_result = Err(DrainStop::Client(err));
                break;
            }
            let cursors = self.transport_cursors().await?;
            if cursors == last {
                drain_result = Ok(());
                break;
            }
            last = cursors;
        }
        // A drain never removes records (and `&mut self` excludes concurrent
        // client operations), so the length delta is the count of newly
        // imported records.
        let imported = self.input_note_count().await?.saturating_sub(before);

        match drain_result {
            Ok(()) => Ok(TransportRecoveryReport {
                status: TransportRecoveryStatus::Completed,
                imported,
                retryable: false,
                reason: None,
            }),
            Err(DrainStop::BudgetExhausted) => Ok(TransportRecoveryReport {
                status: TransportRecoveryStatus::Failed,
                imported,
                retryable: true,
                reason: Some(format!(
                    "the transport backlog was not exhausted after {MAX_DRAIN_PASSES} fetches; \
                     rerun the drain to continue from the stored cursors"
                )),
            }),
            // A broken local store is an environment failure, not a transport
            // outcome: the whole recovery flow needs to know, so it
            // propagates instead of being folded into the report.
            Err(DrainStop::Client(err @ ClientError::StoreError(_))) => {
                Err(crate::error::MultisigError::miden_client_with_context(
                    "local store failed during the transport drain",
                    err,
                ))
            }
            Err(DrainStop::Client(err)) => {
                let (status, retryable) = classify_drain_failure(&err);
                // `Unavailable` promises "nothing was imported"; pages
                // imported before the failure make it an interrupted drain,
                // reported as a failure with the classified retryability.
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

    /// The stored per-tag note-transport cursors, as the raw bytes upstream
    /// keeps in its client-owned settings. Compared, never decoded: a fetch
    /// that leaves them unchanged found every tracked tag's history
    /// exhausted.
    async fn transport_cursors(&self) -> Result<Option<Vec<u8>>> {
        self.miden_store
            .get_setting(SettingScope::Client, NOTE_TRANSPORT_CURSORS_KEY.to_string())
            .await
            .map_err(|e| {
                crate::error::MultisigError::miden_client_with_context(
                    "failed to read the note transport cursors",
                    ClientError::StoreError(e),
                )
            })
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

/// Why a drain stopped before a fetch came back empty.
enum DrainStop {
    /// Every fetch in the budget still moved a cursor.
    BudgetExhausted,
    /// A transport fetch failed.
    Client(ClientError),
}

/// Maps a transport-drain (`fetch_private_notes`) failure onto a report
/// class: `(status, retryable)`.
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
            // Deprecated since miden-client 0.17.1, which no longer raises
            // it: a bounded fetch continues on the next call instead.
            #[allow(deprecated)]
            NoteTransportError::PaginationDidNotTerminate(_) => {
                (TransportRecoveryStatus::Failed, true)
            }
            // Undecodable or inconsistent payloads: a rerun would hit the same
            // bytes again.
            NoteTransportError::Deserialization(_)
            | NoteTransportError::NoteDetailsMismatch { .. }
            | NoteTransportError::InvalidFetchedNote(_)
            | NoteTransportError::UnrequestedTag(_) => (TransportRecoveryStatus::Failed, false),
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use miden_client::ClientError;
    use miden_client::note_transport::{NoteTransportClient, NoteTransportError};
    use miden_client::testing::note_transport::{MockNoteTransportApi, MockNoteTransportNode};
    use miden_protocol::note::{Note, NoteTag, NoteType};
    use miden_tx::utils::sync::RwLock;

    use super::*;
    use crate::client::test_support::{
        add_to_transport, mock_transport, offline_client, p2id_note_for, test_wallet,
    };

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

    /// A distinct (per `seed`) private P2ID note addressed at `target`.
    fn private_note_for(target: &miden_protocol::account::Account, seed: u32) -> Note {
        p2id_note_for(target, seed, NoteType::Private)
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

    /// One transport fetch reads at most 32 pages per request group, so a
    /// backlog served one note per page needs several fetches. The drain
    /// must keep fetching until the backlog stops yielding notes instead of
    /// reporting `Completed` after the first fetch.
    #[tokio::test]
    async fn drain_covers_a_backlog_longer_than_one_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let node = Arc::new(RwLock::new(MockNoteTransportNode::with_max_batch(1)));
        let api: Arc<dyn NoteTransportClient> = Arc::new(MockNoteTransportApi::new(node.clone()));
        let mut client = offline_client(dir.path(), Some(api)).await;

        let account = test_wallet(4);
        client.add_or_update_account(&account, false).await.unwrap();
        let count = 40;
        for seed in 0..count {
            add_to_transport(&node, private_note_for(&account, seed + 1));
            // Distinct cursor values require distinct insertion timestamps.
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        let report = client.drain_private_note_backlog().await.unwrap();
        assert_eq!(report.status, TransportRecoveryStatus::Completed);
        assert_eq!(
            report.imported, count as usize,
            "the whole backlog must be recovered, not just the first fetch's pages"
        );
    }

    /// A store upgraded from miden-client 0.17.0 has no per-tag cursors, so
    /// every tag restarts from the first retained note while the store
    /// already holds that history. The first fetch then walks a full page
    /// budget of duplicates and imports nothing; the drain must keep going
    /// on the moved cursors to reach the note that arrived after them.
    #[tokio::test]
    async fn drain_reaches_new_notes_behind_a_backlog_the_store_already_holds() {
        let dir = tempfile::tempdir().unwrap();
        let node = Arc::new(RwLock::new(MockNoteTransportNode::with_max_batch(1)));
        let api: Arc<dyn NoteTransportClient> = Arc::new(MockNoteTransportApi::new(node.clone()));
        let mut client = offline_client(dir.path(), Some(api)).await;

        let account = test_wallet(8);
        client.add_or_update_account(&account, false).await.unwrap();
        let held = 40;
        for seed in 0..held {
            add_to_transport(&node, private_note_for(&account, seed + 1));
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let report = client.drain_private_note_backlog().await.unwrap();
        assert_eq!(report.imported, held as usize);

        client
            .miden_store
            .remove_setting(SettingScope::Client, NOTE_TRANSPORT_CURSORS_KEY.to_string())
            .await
            .unwrap();
        add_to_transport(&node, private_note_for(&account, held + 1));

        let report = client.drain_private_note_backlog().await.unwrap();
        assert_eq!(report.status, TransportRecoveryStatus::Completed);
        assert_eq!(
            report.imported, 1,
            "the note behind the already-held backlog must be recovered"
        );
    }

    /// In a shared store another account's sync has already moved the
    /// transport forward. A recovered account added afterwards has no cursor
    /// for its tag, so the drain still recovers its older notes.
    #[tokio::test]
    async fn drain_recovers_a_new_account_after_another_account_synced() {
        let dir = tempfile::tempdir().unwrap();
        let (node, api) = mock_transport();
        let mut client = offline_client(dir.path(), Some(api)).await;

        let recovered = test_wallet(3);
        add_to_transport(&node, private_note_for(&recovered, 1));
        std::thread::sleep(std::time::Duration::from_millis(2));

        let existing = test_wallet(5);
        client
            .add_or_update_account(&existing, false)
            .await
            .unwrap();
        add_to_transport(&node, private_note_for(&existing, 2));
        let report = client.drain_private_note_backlog().await.unwrap();
        assert_eq!(report.status, TransportRecoveryStatus::Completed);
        assert_eq!(report.imported, 1);

        client
            .add_or_update_account(&recovered, false)
            .await
            .unwrap();
        let report = client.drain_private_note_backlog().await.unwrap();
        assert_eq!(report.status, TransportRecoveryStatus::Completed);
        assert_eq!(
            report.imported, 1,
            "the recovered account's note predates the other account's sync"
        );
    }

    /// Transport that serves a limited number of successful fetches, then
    /// fails with a connection-shaped error — a connection dropped mid-drain.
    struct InterruptibleTransport {
        inner: MockNoteTransportApi,
        fetches_before_failure: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl NoteTransportClient for InterruptibleTransport {
        async fn send_note_with_proof(
            &self,
            note: miden_client::note_transport::TransportNote,
            inclusion_proof: miden_protocol::note::NoteInclusionProof,
        ) -> std::result::Result<(), NoteTransportError> {
            self.inner.send_note_with_proof(note, &inclusion_proof);
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
    }

    /// A connection lost mid-drain keeps what was fetched: since
    /// miden-client 0.17.1 the pages fetched before a failed page are
    /// imported and their cursors saved. The drain is reported `Failed`
    /// (partial progress rules out `Unavailable`) and retryable; a rerun
    /// continues from the saved cursors.
    #[tokio::test]
    async fn interrupted_drain_keeps_fetched_pages_and_is_retryable() {
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
}
