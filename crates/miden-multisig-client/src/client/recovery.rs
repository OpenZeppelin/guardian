//! Recovery primitives for restoring note state after device loss.
//!
//! After recovery on a fresh device the local store has no note-transport
//! cursor — and in a shared dirty store another account's sync may have
//! advanced the cursor past notes belonging to the newly recovered account.
//! The primitives here rescan sources that normal forward sync would skip
//! (issue #414, sub-issue of #357).

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
    use miden_protocol::Word;
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
}
