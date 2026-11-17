//! Recovery primitives for restoring note state after device loss.
//!
//! After recovery on a fresh device, or after adding a recovered account to a
//! shared store, the account's note tag is not covered by the stored
//! note-transport cursor yet. Since miden-client 0.18.0 the transport keeps
//! one cursor with the tags it covers, reads a tag outside that set from the
//! first retained note, and one fetch reads every tracked tag up to the
//! transport's head. The drain here runs that fetch and reports its outcome,
//! so a recovery flow does not have to wait for later background syncs.

use miden_client::ClientError;
use miden_client::note_transport::NoteTransportError;

use super::MultisigClient;
use crate::error::{Result, error_chain};
use crate::rpc::{is_transient_note_transport_error, is_transient_rpc_error};

/// Outcome class of a private-note transport backlog drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportRecoveryStatus {
    /// The transport backlog was fetched up to the transport's head at the
    /// drain's first request; every note the transport held for the tracked
    /// tags at that point that a tracked account can consume is now in the
    /// local store.
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
    /// connectivity failures). Always `false` on `Completed`.
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
    /// does this). Its tag is not covered by the stored transport cursor
    /// yet, so the transport serves it from the first retained note. One
    /// transport fetch reads every tracked tag, page by page, up to the
    /// transport's head at its first request, so a fetch that succeeds has
    /// exhausted the backlog as of that head. The head is shared by every
    /// tag on the transport, so it is not a signal of this client's own
    /// backlog and the drain does not wait for it to stop moving. The drain
    /// is idempotent: imports dedupe, and the cursor only moves forward.
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
        // A failed fetch still imports the pages fetched before it, so the
        // count below includes them.
        let fetched = self.miden_client.fetch_private_notes().await;
        // A drain never removes records (and `&mut self` excludes concurrent
        // client operations), so the length delta is the count of newly
        // imported records.
        let imported = self.input_note_count().await?.saturating_sub(before);

        match fetched {
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
    use miden_client::note_transport::{
        NOTE_TRANSPORT_SYNC_STATE_KEY, NoteTransportClient, NoteTransportError,
    };
    use miden_client::store::SettingScope;
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

    /// A backlog served one note per page takes many pages. The drain must
    /// read all of them before it reports `Completed`.
    #[tokio::test]
    async fn drain_covers_a_backlog_longer_than_one_page() {
        let dir = tempfile::tempdir().unwrap();
        let node = Arc::new(RwLock::new(MockNoteTransportNode::with_max_batch(1)));
        let api: Arc<dyn NoteTransportClient> = Arc::new(MockNoteTransportApi::new(node.clone()));
        let mut client = offline_client(dir.path(), Some(api)).await;

        let account = test_wallet(4);
        client.add_or_update_account(&account, false).await.unwrap();
        let count = 40;
        for seed in 0..count {
            add_to_transport(&node, private_note_for(&account, seed + 1));
        }

        let report = client.drain_private_note_backlog().await.unwrap();
        assert_eq!(report.status, TransportRecoveryStatus::Completed);
        assert_eq!(
            report.imported, count as usize,
            "the whole backlog must be recovered, not just the first page"
        );
    }

    /// A store with no transport sync state restarts every tag from the first
    /// retained note while the store already holds that history. The drain
    /// walks those duplicates, imports none of them, and still reaches the
    /// note that arrived after them.
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
        }
        let report = client.drain_private_note_backlog().await.unwrap();
        assert_eq!(report.imported, held as usize);

        client
            .miden_store
            .remove_setting(
                SettingScope::Client,
                NOTE_TRANSPORT_SYNC_STATE_KEY.to_string(),
            )
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

    /// Transport whose head moves on every request, as a shared transport's
    /// does while other accounts receive notes: each page request first
    /// stores a note for an account this client does not track.
    struct BusyTransport {
        inner: MockNoteTransportApi,
        node: Arc<RwLock<MockNoteTransportNode>>,
        unrelated: miden_protocol::account::Account,
        requests: std::sync::atomic::AtomicU32,
    }

    #[async_trait::async_trait]
    impl NoteTransportClient for BusyTransport {
        async fn send_note_with_proof(
            &self,
            note: miden_client::note_transport::TransportNote,
            inclusion_proof: miden_protocol::note::NoteInclusionProof,
        ) -> std::result::Result<(), NoteTransportError> {
            self.inner.send_note_with_proof(note, &inclusion_proof);
            Ok(())
        }

        async fn fetch_notes_page(
            &self,
            tags: &[NoteTag],
            cursor: Option<miden_client::note_transport::NoteTransportCursor>,
            sync_to: Option<miden_client::note_transport::NoteTransportCursor>,
        ) -> std::result::Result<miden_client::note_transport::NoteTransportPage, NoteTransportError>
        {
            let seed = self
                .requests
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            add_to_transport(&self.node, private_note_for(&self.unrelated, 1_000 + seed));
            self.inner.fetch_notes_page(tags, cursor, sync_to).await
        }
    }

    /// The transport's head is shared by every tag, so it keeps moving while
    /// other accounts receive notes. The drain completes on its own backlog
    /// and does not wait for the head to settle.
    #[tokio::test]
    async fn drain_completes_while_unrelated_tags_keep_receiving_notes() {
        let dir = tempfile::tempdir().unwrap();
        let node = Arc::new(RwLock::new(MockNoteTransportNode::new()));
        let api: Arc<dyn NoteTransportClient> = Arc::new(BusyTransport {
            inner: MockNoteTransportApi::new(node.clone()),
            node: node.clone(),
            unrelated: test_wallet(12),
            requests: std::sync::atomic::AtomicU32::new(0),
        });
        let mut client = offline_client(dir.path(), Some(api)).await;

        let account = test_wallet(11);
        client.add_or_update_account(&account, false).await.unwrap();
        add_to_transport(&node, private_note_for(&account, 1));

        let report = client.drain_private_note_backlog().await.unwrap();
        assert_eq!(report.status, TransportRecoveryStatus::Completed);
        assert_eq!(report.imported, 1);
        assert!(!report.retryable);

        let report = client.drain_private_note_backlog().await.unwrap();
        assert_eq!(report.status, TransportRecoveryStatus::Completed);
        assert_eq!(report.imported, 0);
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

        async fn fetch_notes_page(
            &self,
            tags: &[NoteTag],
            cursor: Option<miden_client::note_transport::NoteTransportCursor>,
            sync_to: Option<miden_client::note_transport::NoteTransportCursor>,
        ) -> std::result::Result<miden_client::note_transport::NoteTransportPage, NoteTransportError>
        {
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
            self.inner.fetch_notes_page(tags, cursor, sync_to).await
        }
    }

    /// A connection lost mid-drain keeps what was fetched: the pages fetched
    /// before a failed page are imported. The drain is reported `Failed`
    /// (partial progress rules out `Unavailable`) and retryable; the sync
    /// state does not move past a tag that failed, so a rerun fetches it
    /// again.
    #[tokio::test]
    async fn interrupted_drain_keeps_fetched_pages_and_is_retryable() {
        let dir = tempfile::tempdir().unwrap();
        // Cap each response at one note so the two-note backlog needs two
        // page requests; the transport dies after the first.
        let node = Arc::new(RwLock::new(MockNoteTransportNode::with_max_batch(1)));
        let api: Arc<dyn NoteTransportClient> = Arc::new(InterruptibleTransport {
            inner: MockNoteTransportApi::new(node.clone()),
            fetches_before_failure: std::sync::atomic::AtomicUsize::new(1),
        });
        let mut client = offline_client(dir.path(), Some(api)).await;

        let account = test_wallet(6);
        client.add_or_update_account(&account, false).await.unwrap();
        add_to_transport(&node, private_note_for(&account, 1));
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
