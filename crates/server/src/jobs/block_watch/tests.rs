//! Block watch consumer: per-block processing, the cursor policy, and the
//! feed's transport handling, over mocks and a scripted chain.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use super::testing::{Open, ScriptedBlockSourceFactory, block, status};
use super::*;
use crate::block_watch::{BlockWatchConfig, TransportMode};
use crate::coordination::{
    BlockWatchStore, Cursor, CursorWrite, FencedWrite, InMemoryBlockWatchStore, NewObservation,
    Observation, WalkRequests,
};
use crate::error::Result;
use crate::metadata::auth::Auth;
use crate::metadata::{AccountMetadata, NetworkConfig};
use crate::network::NetworkType;
use crate::storage::LeaseFence;
use crate::testing::helpers::create_test_app_state_with_mocks;
use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};

const NETWORK: NetworkType = NetworkType::MidenLocal;

fn miden_meta(account_id: &str) -> AccountMetadata {
    AccountMetadata {
        account_id: account_id.to_string(),
        auth: Auth::MidenFalconRpo {
            cosigner_commitments: vec!["0xc1".into()],
        },
        network_config: NetworkConfig::miden_default(),
        created_at: "2026-11-01T00:00:00Z".into(),
        updated_at: "2026-11-01T00:00:00Z".into(),
        has_pending_candidate: false,
        paused_at: None,
        paused_reason: None,
        released_at: None,
    }
}

fn fence() -> LeaseFence {
    LeaseFence {
        lease_name: crate::coordination::RELEASE_SWEEP_LEASE.to_string(),
        holder_id: "test-holder".to_string(),
        fence_token: 1,
    }
}

/// Delegates to the in-memory store, records every cursor write, and
/// refuses writes once `stale_from` is reached (a lost lease).
#[derive(Default)]
struct RecordingStore {
    inner: InMemoryBlockWatchStore,
    cursor_writes: Mutex<Vec<u32>>,
    resets: Mutex<Vec<u32>>,
    stale_from: Mutex<Option<u32>>,
    /// Walk requests still to fail.
    walk_request_failures: Mutex<u32>,
}

impl RecordingStore {
    fn cursor_writes(&self) -> Vec<u32> {
        self.cursor_writes.lock().unwrap().clone()
    }
}

#[async_trait]
impl BlockWatchStore for RecordingStore {
    async fn cursor(&self, network: NetworkType) -> Result<Option<Cursor>> {
        self.inner.cursor(network).await
    }

    async fn reset_cursor(
        &self,
        network: NetworkType,
        block: u32,
        genesis: Option<&str>,
        fence: &LeaseFence,
    ) -> Result<CursorWrite> {
        self.resets.lock().unwrap().push(block);
        self.inner
            .reset_cursor(network, block, genesis, fence)
            .await
    }

    async fn set_cursor(
        &self,
        network: NetworkType,
        block: u32,
        genesis: Option<&str>,
        fence: &LeaseFence,
    ) -> Result<CursorWrite> {
        if self
            .stale_from
            .lock()
            .unwrap()
            .is_some_and(|from| block >= from)
        {
            return Ok(CursorWrite::StaleLease);
        }
        self.cursor_writes.lock().unwrap().push(block);
        self.inner.set_cursor(network, block, genesis, fence).await
    }

    async fn record_observations(
        &self,
        observations: &[NewObservation],
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.inner.record_observations(observations, now).await
    }

    async fn next_due_observation(&self, now: DateTime<Utc>) -> Result<Option<Observation>> {
        self.inner.next_due_observation(now).await
    }

    async fn next_observation_due_at(&self) -> Result<Option<DateTime<Utc>>> {
        self.inner.next_observation_due_at().await
    }

    async fn delete_observation(&self, account_id: &str, block_num: u32) -> Result<()> {
        self.inner.delete_observation(account_id, block_num).await
    }

    async fn defer_observation(
        &self,
        account_id: &str,
        block_num: u32,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<()> {
        self.inner
            .defer_observation(account_id, block_num, next_attempt_at)
            .await
    }

    async fn count_pending_observations(&self) -> Result<u64> {
        self.inner.count_pending_observations().await
    }

    async fn walk_requests(&self, network: NetworkType) -> Result<Option<WalkRequests>> {
        self.inner.walk_requests(network).await
    }

    async fn request_walk(
        &self,
        network: NetworkType,
        now: DateTime<Utc>,
        fence: &LeaseFence,
    ) -> Result<FencedWrite> {
        {
            let mut failures = self.walk_request_failures.lock().unwrap();
            if *failures > 0 {
                *failures -= 1;
                return Err(crate::error::GuardianError::StorageError(
                    "walk request: database down".to_string(),
                ));
            }
        }
        self.inner.request_walk(network, now, fence).await
    }

    async fn complete_walk(
        &self,
        network: NetworkType,
        started_under: i64,
        fence: &LeaseFence,
    ) -> Result<FencedWrite> {
        self.inner
            .complete_walk(network, started_under, fence)
            .await
    }
}

struct Rig {
    state: crate::state::AppState,
    network: Arc<MockNetworkClient>,
    metadata: Arc<MockMetadataStore>,
    storage: Arc<MockStorageBackend>,
    store: Arc<RecordingStore>,
    factory: Arc<ScriptedBlockSourceFactory>,
    signals: Arc<BlockWatchSignals>,
    handle: BlockWatchHandle,
}

impl Rig {
    fn new(
        metadata: MockMetadataStore,
        storage: MockStorageBackend,
        factory: ScriptedBlockSourceFactory,
        config: BlockWatchConfig,
    ) -> Self {
        let network = Arc::new(MockNetworkClient::new());
        let metadata = Arc::new(metadata);
        let storage = Arc::new(storage);
        let state =
            create_test_app_state_with_mocks(storage.clone(), network.clone(), metadata.clone());
        let store = Arc::new(RecordingStore::default());
        let factory = Arc::new(factory);
        let handle = BlockWatchHandle {
            config,
            network: NETWORK,
            store: store.clone(),
            factory: factory.clone(),
            status: Arc::new(BlockWatchStatus::default()),
            durable_observations: true,
        };
        Self {
            state,
            network,
            metadata,
            storage,
            store,
            factory,
            signals: Arc::new(BlockWatchSignals::default()),
            handle,
        }
    }

    fn consumer(&self) -> Consumer {
        self.consumer_with(self.handle.clone(), false)
    }

    fn consumer_with(&self, handle: BlockWatchHandle, request_rotation_on_start: bool) -> Consumer {
        Consumer::new(
            self.state.clone(),
            handle,
            self.signals.clone(),
            fence(),
            request_rotation_on_start,
        )
    }

    fn spawn(&self) -> (CancellationToken, tokio::task::JoinHandle<Stopped>) {
        let cancel = CancellationToken::new();
        let task = tokio::spawn(self.consumer().run(cancel.clone()));
        (cancel, task)
    }

    async fn set_cursor(&self, block: u32) {
        self.store
            .inner
            .set_cursor(NETWORK, block, None, &fence())
            .await
            .unwrap();
    }

    async fn cursor(&self) -> Option<u32> {
        self.store
            .cursor(NETWORK)
            .await
            .unwrap()
            .map(|cursor| cursor.last_block)
    }

    /// Let the consumer run (virtual time) until the cursor reaches
    /// `block`.
    async fn wait_for_cursor(&self, block: u32) {
        for _ in 0..100_000 {
            if self.cursor().await.is_some_and(|cursor| cursor >= block) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!(
            "cursor never reached {block}: {:?}, opens {:?}",
            self.cursor().await,
            self.factory.opens()
        );
    }

    fn assert_no_chain_reads(&self) {
        assert!(self.network.get_verify_commitment_calls().is_empty());
        assert!(
            self.network
                .get_fetch_on_chain_guardian_binding_calls()
                .is_empty()
        );
        assert!(
            self.network
                .get_find_transaction_ending_at_calls()
                .is_empty()
        );
    }
}

fn config() -> BlockWatchConfig {
    BlockWatchConfig::default()
        .with_stall_seconds(60)
        .with_ban_cooldown_seconds(600)
        .with_stream_retry_seconds(300)
}

// --- Per-block processing --------------------------------------------------

#[tokio::test]
async fn a_block_records_only_the_moves_this_server_cannot_explain() {
    // Two accounts with a candidate in flight: one moved to that
    // candidate's post-state (its own transaction landed), the other to a
    // state no candidate explains (PR #586 review, I2).
    let mut busy = miden_meta("0xbusy");
    busy.has_pending_candidate = true;
    let mut busy_moved = miden_meta("0xbusy_moved");
    busy_moved.has_pending_candidate = true;
    let mut released = miden_meta("0xreleased");
    released.released_at = Some(Utc::now());
    let metadata = MockMetadataStore::new()
        .with_row(miden_meta("0xlanded"))
        .with_row(miden_meta("0xmoved"))
        .with_row(miden_meta("0xunconfigured"))
        .with_row(busy)
        .with_row(busy_moved)
        .with_row(released);
    let storage = MockStorageBackend::new()
        .with_state_commitment("0xlanded", "0xc_landed")
        .with_state_commitment("0xmoved", "0xc_stored")
        .with_state_commitment("0xbusy", "0xc_busy_base")
        .with_state_commitment("0xbusy_moved", "0xc_busy_moved_base")
        .with_candidate_post_state("0xbusy", "0xc_busy_landed")
        .with_candidate_post_state("0xbusy_moved", "0xc_busy_moved_candidate");
    let rig = Rig::new(
        metadata,
        storage,
        ScriptedBlockSourceFactory::new(),
        config(),
    );

    let summary = rig
        .consumer()
        .process_for_test(&block(
            42,
            &[
                ("0xlanded", "0xC_LANDED"),
                ("0xmoved", "0xc_elsewhere"),
                ("0xbusy", "0xc_busy_landed"),
                ("0xbusy_moved", "0xc_switched"),
                ("0xstranger", "0xc_any"),
                ("0xunconfigured", "0xc_any"),
                ("0xreleased", "0xc_any"),
                // An account listed twice is looked up once.
                ("0xlanded", "0xc_landed"),
            ],
        ))
        .await
        .expect("processed");

    assert_eq!(
        summary,
        BlockSummary {
            updates: 8,
            guarded: 6,
            observed: 2,
        }
    );
    let observation = rig
        .store
        .next_due_observation(Utc::now())
        .await
        .unwrap()
        .expect("the unexplained moves are queued");
    assert_eq!(
        observation,
        Observation {
            account_id: "0xbusy_moved".into(),
            block_num: 42,
            on_chain_commitment: "0xc_switched".into(),
            attempts: 0,
        },
        "a move no candidate explains is queued despite the candidate"
    );
    rig.store
        .delete_observation("0xbusy_moved", 42)
        .await
        .unwrap();
    assert_eq!(
        rig.store
            .next_due_observation(Utc::now())
            .await
            .unwrap()
            .expect("the other unexplained move"),
        Observation {
            account_id: "0xmoved".into(),
            block_num: 42,
            on_chain_commitment: "0xc_elsewhere".into(),
            attempts: 0,
        }
    );
    assert_eq!(rig.store.count_pending_observations().await.unwrap(), 1);
    assert_eq!(rig.cursor().await, Some(42));
    assert_eq!(
        rig.metadata.get_filter_release_sweep_ids_calls(),
        vec![vec![
            "0xlanded".to_string(),
            "0xmoved".into(),
            "0xbusy".into(),
            "0xbusy_moved".into(),
            "0xstranger".into(),
            "0xunconfigured".into(),
            "0xreleased".into(),
        ]],
        "one membership query per block, each account once"
    );
    assert_eq!(
        rig.storage.get_state_commitments_calls(),
        vec![vec![
            "0xlanded".to_string(),
            "0xmoved".into(),
            "0xunconfigured".into(),
            "0xbusy".into(),
            "0xbusy_moved".into(),
        ]],
        "stored commitments of guarded accounts only"
    );
    rig.assert_no_chain_reads();
    assert!(
        rig.metadata.get_get_calls().is_empty(),
        "no per-account metadata read on the hot path"
    );
}

#[tokio::test]
async fn a_replayed_block_queues_nothing_twice() {
    let rig = Rig::new(
        MockMetadataStore::new().with_row(miden_meta("0xmoved")),
        MockStorageBackend::new().with_state_commitment("0xmoved", "0xc_stored"),
        ScriptedBlockSourceFactory::new(),
        config(),
    );
    let consumer = rig.consumer();
    let moved = block(7, &[("0xmoved", "0xc_elsewhere")]);
    consumer.process_for_test(&moved).await.unwrap();
    consumer.process_for_test(&moved).await.unwrap();
    assert_eq!(rig.store.count_pending_observations().await.unwrap(), 1);
    assert_eq!(rig.cursor().await, Some(7));
}

#[tokio::test]
async fn an_empty_block_only_moves_the_cursor() {
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        ScriptedBlockSourceFactory::new(),
        config(),
    );
    let summary = rig
        .consumer()
        .process_for_test(&block(3, &[]))
        .await
        .unwrap();
    assert_eq!(summary, BlockSummary::default());
    assert!(rig.metadata.get_filter_release_sweep_ids_calls().is_empty());
    assert!(rig.storage.get_state_commitments_calls().is_empty());
    assert_eq!(rig.cursor().await, Some(3));
}

#[tokio::test]
async fn a_store_failure_leaves_the_block_unprocessed() {
    let rig = Rig::new(
        MockMetadataStore::new().with_row(miden_meta("0xmoved")),
        MockStorageBackend::new()
            .with_state_commitment("0xmoved", "0xc_stored")
            .with_state_commitments_error("database unavailable"),
        ScriptedBlockSourceFactory::new(),
        config(),
    );
    let consumer = rig.consumer();
    let moved = block(9, &[("0xmoved", "0xc_elsewhere")]);
    let error = consumer
        .process_for_test(&moved)
        .await
        .expect_err("the storage read failed");
    assert!(error.contains("database unavailable"), "{error}");
    assert_eq!(rig.cursor().await, None, "the cursor did not move");
    assert_eq!(rig.store.count_pending_observations().await.unwrap(), 0);

    // The membership query failing is the same.
    let rig = Rig::new(
        MockMetadataStore::new().with_filter_release_sweep_ids(Err("metadata down".into())),
        MockStorageBackend::new(),
        ScriptedBlockSourceFactory::new(),
        config(),
    );
    assert!(rig.consumer().process_for_test(&moved).await.is_err());
    assert_eq!(rig.cursor().await, None);
}

#[tokio::test]
async fn a_large_fleet_costs_one_query_per_block_and_no_chain_reads() {
    // SC-002: 100k guarded accounts, blocks touching 1% of them. Only the
    // moves this server cannot explain become observations; nothing is
    // read from chain for any account while blocks are processed.
    let mut metadata = MockMetadataStore::new();
    for n in 0..100_000 {
        metadata = metadata.with_row(miden_meta(&format!("0xacct{n:06}")));
    }
    let mut storage = MockStorageBackend::new();
    for n in 0..1_000 {
        storage = storage.with_state_commitment(&format!("0xacct{n:06}"), &format!("0xc{n}"));
    }
    let rig = Rig::new(
        metadata,
        storage,
        ScriptedBlockSourceFactory::new(),
        config(),
    );
    let consumer = rig.consumer();
    for block_index in 0..10u32 {
        let updates: Vec<(String, String)> = (0..100)
            .map(|offset| {
                let n = block_index as usize * 100 + offset;
                // Two moves per block land somewhere this server did not
                // put them; the rest are this server's own transactions.
                let commitment = if offset < 2 {
                    format!("0xforeign{n}")
                } else {
                    format!("0xc{n}")
                };
                (format!("0xacct{n:06}"), commitment)
            })
            .collect();
        let refs: Vec<(&str, &str)> = updates
            .iter()
            .map(|(id, commitment)| (id.as_str(), commitment.as_str()))
            .collect();
        let summary = consumer
            .process_for_test(&block(1_000 + block_index, &refs))
            .await
            .unwrap();
        assert_eq!(summary.observed, 2);
    }
    assert_eq!(rig.store.count_pending_observations().await.unwrap(), 20);
    assert_eq!(rig.metadata.get_filter_release_sweep_ids_calls().len(), 10);
    assert_eq!(rig.storage.get_state_commitments_calls().len(), 10);
    rig.assert_no_chain_reads();
}

// --- Cursor policy -----------------------------------------------------------

#[tokio::test]
async fn the_first_start_begins_at_the_tip_and_asks_for_one_rotation() {
    let factory = ScriptedBlockSourceFactory::new();
    factory.chain.push_empty(1, 500);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    assert_eq!(rig.consumer().start_position_for_test().await, Ok(500));
    assert!(rig.signals.take_rotation_request(), "no genesis replay");
    assert_eq!(
        rig.store.walk_requests(NETWORK).await.unwrap(),
        Some(WalkRequests {
            requested_seq: 1,
            completed_seq: 0
        }),
        "the walk is recorded, so a successor still owes it (PR #586 review, I1)"
    );
}

#[tokio::test]
async fn a_cursor_within_reach_is_resumed_without_a_rotation() {
    let factory = ScriptedBlockSourceFactory::new();
    factory.chain.push_empty(1, 500);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config().with_max_catchup_blocks(100),
    );
    rig.set_cursor(400).await;
    assert_eq!(rig.consumer().start_position_for_test().await, Ok(401));
    assert!(!rig.signals.take_rotation_request());
    // A cursor at the tip resumes too, even with no catch-up allowed.
    rig.set_cursor(500).await;
    let at_tip = rig.consumer_with(
        BlockWatchHandle {
            config: config().with_max_catchup_blocks(0),
            ..rig.handle.clone()
        },
        false,
    );
    assert_eq!(at_tip.start_position_for_test().await, Ok(501));
    assert!(!rig.signals.take_rotation_request());
}

#[tokio::test]
async fn a_gap_past_the_catch_up_limit_jumps_to_the_tip_and_asks_for_a_rotation() {
    let factory = ScriptedBlockSourceFactory::new();
    factory.chain.push_empty(1, 500);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config().with_max_catchup_blocks(99),
    );
    rig.set_cursor(400).await;
    assert_eq!(rig.consumer().start_position_for_test().await, Ok(500));
    assert!(rig.signals.take_rotation_request());
}

#[tokio::test]
async fn observations_lost_with_the_process_ask_for_a_rotation() {
    let factory = ScriptedBlockSourceFactory::new();
    factory.chain.push_empty(1, 10);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    rig.set_cursor(8).await;
    let restarted = rig.consumer_with(rig.handle.clone(), true);
    assert_eq!(restarted.start_position_for_test().await, Ok(9));
    assert!(rig.signals.take_rotation_request());
}

// --- The feed ----------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn the_feed_replays_from_the_cursor_then_follows_the_chain_once_per_block() {
    let factory = ScriptedBlockSourceFactory::new();
    factory.chain.push_empty(1, 5);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    rig.set_cursor(2).await;
    let (cancel, task) = rig.spawn();
    rig.wait_for_cursor(5).await;
    rig.factory.chain.push_empty(6, 7);
    rig.wait_for_cursor(7).await;
    let snapshot = rig.handle.status.snapshot();
    assert_eq!(snapshot.transport, Some(Transport::Stream));
    assert_eq!(snapshot.last_block, Some(7));
    assert_eq!(snapshot.lag_blocks(), Some(0));
    cancel.cancel();
    assert_eq!(task.await.unwrap(), Stopped::Cancelled);
    assert_eq!(rig.store.cursor_writes(), vec![3, 4, 5, 6, 7]);
    assert_eq!(rig.factory.opens(), vec![(Transport::Stream, 3)]);
    assert_eq!(
        rig.handle.status.snapshot(),
        StatusSnapshot::default(),
        "a stopped feed reports nothing"
    );
}

#[tokio::test(start_paused = true)]
async fn a_lag_disconnect_polls_through_the_ban_then_returns_to_the_stream() {
    let factory = ScriptedBlockSourceFactory::new().with_streams(vec![Open::then_fail(
        2,
        status(tonic::Code::ResourceExhausted),
    )]);
    factory.chain.push_empty(1, 3);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    rig.set_cursor(0).await;
    let (cancel, task) = rig.spawn();
    rig.wait_for_cursor(3).await;
    // Blocks keep coming while the feed polls.
    for block in 4..=30 {
        tokio::time::sleep(Duration::from_secs(30)).await;
        rig.factory.chain.push_empty(block, block);
    }
    rig.wait_for_cursor(30).await;
    cancel.cancel();
    task.await.unwrap();

    let opens = rig.factory.opens();
    assert_eq!(
        &opens[..2],
        &[(Transport::Stream, 1), (Transport::Poll, 3)],
        "the lagging stream hands over to polling at the cursor"
    );
    let back = opens
        .iter()
        .skip(2)
        .position(|(transport, _)| *transport == Transport::Stream)
        .map(|offset| offset + 2)
        .expect("the stream is tried again");
    let times = rig.factory.open_times();
    assert!(
        times[back] - times[1] >= Duration::from_secs(600),
        "the stream is not retried inside the node's ban: {:?}",
        times[back] - times[1]
    );
    let writes = rig.store.cursor_writes();
    assert_eq!(
        writes,
        (1..=30).collect::<Vec<_>>(),
        "every block once, in order"
    );
}

#[tokio::test(start_paused = true)]
async fn a_node_without_the_stream_is_polled_and_the_stream_retried_later() {
    let factory = ScriptedBlockSourceFactory::new().with_streams(vec![
        Open::Refuse(status(tonic::Code::Unimplemented)),
        Open::Refuse(status(tonic::Code::Unimplemented)),
    ]);
    factory.chain.push_empty(1, 2);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    rig.set_cursor(0).await;
    let (cancel, task) = rig.spawn();
    rig.wait_for_cursor(2).await;
    tokio::time::sleep(Duration::from_secs(700)).await;
    cancel.cancel();
    task.await.unwrap();
    let opens = rig.factory.opens();
    assert_eq!(
        &opens[..4],
        &[
            (Transport::Stream, 1),
            (Transport::Poll, 1),
            // After the stream retry cooldown (300 s) the stream is tried
            // again, still refused, and polling resumes.
            (Transport::Stream, 3),
            (Transport::Poll, 3),
        ]
    );
    let times = rig.factory.open_times();
    assert!(times[2] - times[1] >= Duration::from_secs(300));
}

#[tokio::test(start_paused = true)]
async fn a_silent_stream_is_reopened_from_the_cursor_once_the_chain_moves_on() {
    let factory = ScriptedBlockSourceFactory::new().with_streams(vec![Open::then_stall(2)]);
    factory.chain.push_empty(1, 4);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    rig.set_cursor(0).await;
    let (cancel, task) = rig.spawn();
    rig.wait_for_cursor(4).await;
    cancel.cancel();
    task.await.unwrap();
    assert_eq!(
        rig.factory.opens(),
        vec![(Transport::Stream, 1), (Transport::Stream, 3)]
    );
    let times = rig.factory.open_times();
    assert!(
        times[1] - times[0] >= Duration::from_secs(60),
        "the stall timeout passed first"
    );
    assert_eq!(rig.store.cursor_writes(), vec![1, 2, 3, 4]);
}

#[tokio::test(start_paused = true)]
async fn an_idle_chain_is_not_a_stall() {
    let factory = ScriptedBlockSourceFactory::new();
    factory.chain.push_empty(1, 3);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    rig.set_cursor(0).await;
    let (cancel, task) = rig.spawn();
    rig.wait_for_cursor(3).await;
    tokio::time::sleep(Duration::from_secs(600)).await;
    cancel.cancel();
    task.await.unwrap();
    assert_eq!(rig.factory.opens(), vec![(Transport::Stream, 1)]);
}

#[tokio::test(start_paused = true)]
async fn repeated_stream_failures_back_off_and_then_fall_back_to_polling() {
    let failures: Vec<Open> = (0..5)
        .map(|_| Open::Refuse(status(tonic::Code::Unavailable)))
        .collect();
    let factory = ScriptedBlockSourceFactory::new().with_streams(failures);
    factory.chain.push_empty(1, 2);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    rig.set_cursor(0).await;
    let (cancel, task) = rig.spawn();
    rig.wait_for_cursor(2).await;
    cancel.cancel();
    task.await.unwrap();
    let opens = rig.factory.opens();
    assert_eq!(
        opens,
        vec![
            (Transport::Stream, 1),
            (Transport::Stream, 1),
            (Transport::Stream, 1),
            (Transport::Stream, 1),
            (Transport::Stream, 1),
            (Transport::Poll, 1),
        ]
    );
    let times = rig.factory.open_times();
    let gaps: Vec<Duration> = times.windows(2).map(|pair| pair[1] - pair[0]).collect();
    // Jittered within [half, full] of 1, 2, 4, 8 s; then polling at once.
    for (gap, full) in gaps.iter().zip([1u64, 2, 4, 8]) {
        assert!(
            *gap >= Duration::from_millis(full * 500) && *gap <= Duration::from_secs(full),
            "{gap:?} outside the backoff for {full} s"
        );
    }
    assert!(gaps[4] < Duration::from_millis(10));
}

#[tokio::test(start_paused = true)]
async fn a_block_out_of_sequence_reopens_from_the_cursor_without_skipping() {
    let factory = ScriptedBlockSourceFactory::new().with_streams(vec![Open::then_fail(
        1,
        BlockSourceError::OutOfSequence {
            expected: 2,
            got: 3,
        },
    )]);
    factory.chain.push_empty(1, 3);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    rig.set_cursor(0).await;
    let (cancel, task) = rig.spawn();
    rig.wait_for_cursor(3).await;
    cancel.cancel();
    task.await.unwrap();
    assert_eq!(
        rig.factory.opens(),
        vec![(Transport::Stream, 1), (Transport::Stream, 2)]
    );
    assert_eq!(rig.store.cursor_writes(), vec![1, 2, 3]);
}

#[tokio::test(start_paused = true)]
async fn a_refused_cursor_write_stops_the_consumer() {
    let factory = ScriptedBlockSourceFactory::new();
    factory.chain.push_empty(1, 5);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    rig.set_cursor(0).await;
    *rig.store.stale_from.lock().unwrap() = Some(3);
    let (_cancel, task) = rig.spawn();
    assert_eq!(task.await.unwrap(), Stopped::LostLease);
    assert_eq!(rig.store.cursor_writes(), vec![1, 2]);
}

#[tokio::test(start_paused = true)]
async fn a_block_the_node_no_longer_serves_jumps_to_the_tip_and_asks_for_a_rotation() {
    let factory = ScriptedBlockSourceFactory::new();
    factory.chain.push_empty(1, 12);
    factory.chain.prune_below(10);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    rig.set_cursor(2).await;
    let (cancel, task) = rig.spawn();
    rig.wait_for_cursor(12).await;
    cancel.cancel();
    task.await.unwrap();
    assert!(rig.signals.take_rotation_request());
    assert_eq!(
        rig.factory.opens(),
        vec![(Transport::Stream, 3), (Transport::Stream, 12)]
    );
    assert_eq!(rig.store.cursor_writes(), vec![12]);
}

#[tokio::test(start_paused = true)]
async fn poll_mode_never_subscribes() {
    let factory = ScriptedBlockSourceFactory::new();
    factory.chain.push_empty(1, 3);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config().with_transport(TransportMode::Poll),
    );
    rig.set_cursor(0).await;
    let (cancel, task) = rig.spawn();
    rig.wait_for_cursor(3).await;
    assert_eq!(
        rig.handle.status.snapshot().transport,
        Some(Transport::Poll)
    );
    cancel.cancel();
    task.await.unwrap();
    assert_eq!(rig.factory.opens(), vec![(Transport::Poll, 1)]);
}

#[tokio::test(start_paused = true)]
async fn a_successor_resumes_from_the_cursor_and_inherits_the_observations() {
    // Two holders share one store. A processes blocks 1-5 (recording an
    // observation at 4) and stops; B starts from 6 and finds it.
    let factory = ScriptedBlockSourceFactory::new();
    factory.chain.push_empty(1, 3);
    factory
        .chain
        .push(block(4, &[("0xmoved", "0xc_elsewhere")]));
    factory.chain.push_empty(5, 5);
    let rig = Rig::new(
        MockMetadataStore::new().with_row(miden_meta("0xmoved")),
        MockStorageBackend::new().with_state_commitment("0xmoved", "0xc_stored"),
        factory,
        config(),
    );
    rig.set_cursor(0).await;
    let (cancel_a, task_a) = rig.spawn();
    rig.wait_for_cursor(5).await;
    cancel_a.cancel();
    task_a.await.unwrap();

    rig.factory.chain.push_empty(6, 8);
    let (cancel_b, task_b) = rig.spawn();
    rig.wait_for_cursor(8).await;
    cancel_b.cancel();
    task_b.await.unwrap();

    assert_eq!(
        rig.factory.opens(),
        vec![(Transport::Stream, 1), (Transport::Stream, 6)],
        "the successor resumes after the cursor"
    );
    assert_eq!(
        rig.store.cursor_writes(),
        (1..=8).collect::<Vec<_>>(),
        "no block processed twice"
    );
    let inherited = rig
        .store
        .next_due_observation(Utc::now())
        .await
        .unwrap()
        .expect("the predecessor's observation survives");
    assert_eq!(
        (inherited.account_id.as_str(), inherited.block_num),
        ("0xmoved", 4)
    );
}

#[tokio::test(start_paused = true)]
async fn the_start_waits_for_a_readable_tip() {
    let factory = ScriptedBlockSourceFactory::new();
    factory.chain.push_empty(1, 3);
    factory.set_tip(Err(status(tonic::Code::Unavailable)));
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    rig.set_cursor(0).await;
    let (cancel, task) = rig.spawn();
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert!(
        rig.factory.opens().is_empty(),
        "nothing opens before the start position is known"
    );
    rig.factory.set_tip(Ok(3));
    rig.wait_for_cursor(3).await;
    cancel.cancel();
    task.await.unwrap();
    assert_eq!(rig.factory.opens(), vec![(Transport::Stream, 1)]);
}

#[tokio::test(start_paused = true)]
async fn a_failed_poll_is_retried_with_a_backoff() {
    let factory = ScriptedBlockSourceFactory::new().with_polls(vec![
        Open::Refuse(status(tonic::Code::Unavailable)),
        Open::then_fail(1, status(tonic::Code::Internal)),
    ]);
    factory.chain.push_empty(1, 3);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config().with_transport(TransportMode::Poll),
    );
    rig.set_cursor(0).await;
    let (cancel, task) = rig.spawn();
    rig.wait_for_cursor(3).await;
    cancel.cancel();
    task.await.unwrap();
    assert_eq!(
        rig.factory.opens(),
        vec![
            (Transport::Poll, 1),
            (Transport::Poll, 1),
            (Transport::Poll, 2),
        ]
    );
    let times = rig.factory.open_times();
    assert!(times[1] - times[0] >= Duration::from_millis(500));
    assert!(
        times[2] - times[1] >= Duration::from_millis(500),
        "each failure waits at least the base backoff"
    );
    assert_eq!(rig.store.cursor_writes(), vec![1, 2, 3]);
}

#[cfg(feature = "postgres")]
mod postgres {
    use super::*;
    use crate::coordination::LeaderElector;
    use crate::coordination::postgres::{PgBlockWatchStore, PgLeaseElector};
    use crate::metadata::postgres::PostgresMetadataStore;
    use crate::storage::postgres::PostgresService;
    use crate::testing::pg::test_database_url;
    use diesel_async::RunQueryDsl;

    /// SC-003: a block updating 5,000 accounts (4,900 guarded ones landing
    /// at their stored state, 50 moving elsewhere, 50 strangers) is
    /// processed in well under the node's 10 s send timeout, on real
    /// Postgres stores with the cursor fenced by a real lease.
    #[tokio::test]
    #[ignore = "requires Postgres; run ./scripts/test-postgres.sh"]
    async fn a_block_of_five_thousand_updates_is_processed_within_a_second() {
        let url = test_database_url().await;
        let metadata = Arc::new(PostgresMetadataStore::new(&url, 4).await.unwrap());
        let storage = Arc::new(PostgresService::new(&url, 4).await.unwrap());
        let pool = metadata.pool_handle();
        let prefix = format!("0xbench{}", Utc::now().timestamp_micros());
        {
            let mut conn = pool.get().await.unwrap();
            diesel::sql_query(format!(
                "INSERT INTO account_metadata \
                 (account_id, auth, network_config, created_at, updated_at, has_pending_candidate) \
                 SELECT '{prefix}-' || lpad(n::text, 5, '0'), \
                        '{{\"MidenFalconRpo\":{{\"cosigner_commitments\":[\"0x{c}\"]}}}}'::jsonb, \
                        '{{\"kind\":\"miden\",\"network_type\":\"local\"}}'::jsonb, now(), now(), false \
                 FROM generate_series(0, 4949) AS n",
                c = "aa".repeat(32)
            ))
            .execute(&mut conn)
            .await
            .unwrap();
            diesel::sql_query(format!(
                "INSERT INTO states (account_id, state_json, commitment, created_at, updated_at) \
                 SELECT '{prefix}-' || lpad(n::text, 5, '0'), '{{}}'::jsonb, \
                        '0xc' || n::text, now(), now() \
                 FROM generate_series(0, 4949) AS n"
            ))
            .execute(&mut conn)
            .await
            .unwrap();
        }
        let updates: Vec<(String, String)> = (0..5_000)
            .map(|n| {
                let id = format!("{prefix}-{n:05}");
                let commitment = if n < 4_900 {
                    format!("0xc{n}")
                } else {
                    format!("0xelsewhere{n}")
                };
                (id, commitment)
            })
            .collect();
        let refs: Vec<(&str, &str)> = updates
            .iter()
            .map(|(id, commitment)| (id.as_str(), commitment.as_str()))
            .collect();
        let moved = block(77, &refs);

        let lease_name = format!("block-watch-bench-{}", Utc::now().timestamp_micros());
        let elector = PgLeaseElector::new(pool.clone(), &lease_name, "bench-holder");
        let lease = elector
            .try_acquire(Duration::from_secs(60))
            .await
            .unwrap()
            .expect("lease");
        let store = Arc::new(PgBlockWatchStore::new(pool.clone()));
        diesel::sql_query("DELETE FROM block_watch_observations")
            .execute(&mut pool.get().await.unwrap())
            .await
            .unwrap();
        diesel::sql_query("DELETE FROM block_watch_cursor")
            .execute(&mut pool.get().await.unwrap())
            .await
            .unwrap();
        let state = create_test_app_state_with_mocks(
            storage.clone(),
            Arc::new(MockNetworkClient::new()),
            metadata.clone(),
        );
        let handle = BlockWatchHandle {
            config: config(),
            network: NETWORK,
            store: store.clone(),
            factory: Arc::new(ScriptedBlockSourceFactory::new()),
            status: Arc::default(),
            durable_observations: true,
        };
        let consumer = Consumer::new(
            state,
            handle,
            Arc::new(BlockWatchSignals::default()),
            LeaseFence {
                lease_name: lease.name.clone(),
                holder_id: lease.holder_id.clone(),
                fence_token: lease.fence_token,
            },
            false,
        );

        let started = std::time::Instant::now();
        let summary = consumer.process_for_test(&moved).await.unwrap();
        let elapsed = started.elapsed();
        assert_eq!(
            summary,
            BlockSummary {
                updates: 5_000,
                guarded: 4_950,
                observed: 50,
            }
        );
        assert_eq!(store.count_pending_observations().await.unwrap(), 50);
        assert_eq!(
            store
                .cursor(NETWORK)
                .await
                .unwrap()
                .map(|cursor| cursor.last_block),
            Some(77)
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "5,000 updates took {elapsed:?}"
        );
        eprintln!("block of 5000 updates processed in {elapsed:?}");

        elector.release(lease).await.unwrap();
        let mut conn = pool.get().await.unwrap();
        for table in ["states", "account_metadata"] {
            diesel::sql_query(format!(
                "DELETE FROM {table} WHERE account_id LIKE '{prefix}-%'"
            ))
            .execute(&mut conn)
            .await
            .unwrap();
        }
        diesel::sql_query("DELETE FROM block_watch_observations")
            .execute(&mut conn)
            .await
            .unwrap();
        diesel::sql_query("DELETE FROM block_watch_cursor")
            .execute(&mut conn)
            .await
            .unwrap();
    }
}

// --- Review fixes: own history, chain resets, unserved blocks ---------------

#[tokio::test]
async fn a_move_to_an_earlier_state_of_the_accounts_own_is_not_an_observation() {
    // Replayed after downtime (or behind an optimistic commit): the block
    // shows the account at a state one of its canonical deltas produced,
    // and the store has moved past it. A move nothing here produced is
    // still observed.
    let rig = Rig::new(
        MockMetadataStore::new()
            .with_row(miden_meta("0xold"))
            .with_row(miden_meta("0xswitched")),
        MockStorageBackend::new()
            .with_state_commitment("0xold", "0xc3")
            .with_state_commitment("0xswitched", "0xc3")
            .with_canonical_post_state("0xold", "0xc1"),
        ScriptedBlockSourceFactory::new(),
        config(),
    );
    let summary = rig
        .consumer()
        .process_for_test(&block(
            20,
            &[("0xold", "0xc1"), ("0xswitched", "0xc_foreign")],
        ))
        .await
        .unwrap();
    assert_eq!(summary.observed, 1);
    let observation = rig
        .store
        .next_due_observation(Utc::now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(observation.account_id, "0xswitched");
    assert_eq!(rig.store.count_pending_observations().await.unwrap(), 1);
}

#[tokio::test]
async fn a_new_genesis_starts_over_at_the_tip_and_asks_for_a_rotation() {
    let factory = ScriptedBlockSourceFactory::new();
    factory.chain.push_empty(1, 500);
    factory.set_genesis(Some("0xnew"));
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config().with_max_catchup_blocks(1_000),
    );
    rig.store
        .inner
        .set_cursor(NETWORK, 400, Some("0xold"), &fence())
        .await
        .unwrap();
    assert_eq!(rig.consumer().start_position_for_test().await, Ok(500));
    assert!(rig.signals.take_rotation_request());
    assert_eq!(rig.store.resets.lock().unwrap().clone(), vec![499]);
    assert_eq!(
        rig.store.cursor(NETWORK).await.unwrap(),
        Some(Cursor {
            last_block: 499,
            genesis: Some("0xnew".into())
        }),
        "the cursor now counts the new chain"
    );
    // The same chain resumes.
    assert_eq!(rig.consumer().start_position_for_test().await, Ok(500));
    assert!(!rig.signals.take_rotation_request());
}

#[tokio::test]
async fn a_cursor_far_past_the_tip_starts_over_and_one_just_ahead_waits() {
    let factory = ScriptedBlockSourceFactory::new();
    factory.chain.push_empty(1, 100);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    // A node a little behind: wait for it.
    rig.set_cursor(150).await;
    assert_eq!(rig.consumer().start_position_for_test().await, Ok(151));
    assert!(rig.store.resets.lock().unwrap().is_empty());
    // A cursor the node would refuse to stream from: another chain.
    rig.set_cursor(50_000).await;
    assert_eq!(rig.consumer().start_position_for_test().await, Ok(100));
    assert_eq!(rig.store.resets.lock().unwrap().clone(), vec![99]);
    assert!(rig.signals.take_rotation_request());
}

#[tokio::test(start_paused = true)]
async fn a_chain_reset_while_running_is_followed_onto_the_new_chain() {
    let factory = ScriptedBlockSourceFactory::new()
        .with_streams(vec![Open::then_fail(5, status(tonic::Code::Unavailable))]);
    factory.chain.push_empty(1, 5);
    factory.set_genesis(Some("0xa"));
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    rig.set_cursor(0).await;
    let (cancel, task) = rig.spawn();
    rig.wait_for_cursor(5).await;
    // The node restarts on a reset chain while block watch backs off.
    rig.factory.reset_chain(3, Some("0xb"));
    for _ in 0..1_000 {
        if !rig.store.resets.lock().unwrap().is_empty() && rig.cursor().await == Some(3) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let snapshot = rig.handle.status.snapshot();
    assert_eq!(
        (
            snapshot.last_block,
            snapshot.chain_tip,
            snapshot.lag_blocks()
        ),
        (Some(3), Some(3), Some(0)),
        "the old chain's tip no longer inflates the lag"
    );
    cancel.cancel();
    task.await.unwrap();
    assert_eq!(rig.store.resets.lock().unwrap().clone(), vec![2]);
    assert_eq!(rig.store.cursor_writes(), vec![1, 2, 3, 4, 5, 3]);
    assert!(rig.signals.take_rotation_request());

    assert_eq!(
        rig.factory.opens(),
        vec![(Transport::Stream, 1), (Transport::Stream, 3)]
    );
}

#[tokio::test(start_paused = true)]
async fn a_refusal_to_stream_from_the_cursor_re_evaluates_the_start() {
    // A reset chain whose node reports no genesis: the next subscription
    // starts far past the new tip, the node refuses it as out of range,
    // and the start position sees a cursor that belongs to another chain.
    let recorder = crate::metrics::build_recorder();
    let metrics_handle = recorder.handle();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let factory = ScriptedBlockSourceFactory::new()
        .with_streams(vec![Open::then_fail(1, status(tonic::Code::Unavailable))]);
    factory.chain.push_empty(1, 500);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    rig.set_cursor(499).await;
    let (cancel, task) = rig.spawn();
    rig.wait_for_cursor(500).await;
    rig.factory.reset_chain(3, None);
    for _ in 0..1_000 {
        if rig.cursor().await == Some(3) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    cancel.cancel();
    task.await.unwrap();
    assert_eq!(rig.store.resets.lock().unwrap().clone(), vec![2]);
    assert_eq!(rig.cursor().await, Some(3));
    assert!(rig.signals.take_rotation_request());
    let rendered = metrics_handle.render();
    assert!(
        rendered.contains(&format!(
            "{}{{reason=\"status_out_of_range\"}}",
            crate::metrics::names::BLOCK_WATCH_RECONNECTS_TOTAL
        )),
        "the refusal is counted as a reconnect:\n{rendered}"
    );
}

#[tokio::test(start_paused = true)]
async fn polling_a_reset_chain_far_below_the_cursor_re_evaluates_the_start() {
    let factory = ScriptedBlockSourceFactory::new();
    factory.chain.push_empty(1, 500);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config().with_transport(TransportMode::Poll),
    );
    rig.set_cursor(499).await;
    let (cancel, task) = rig.spawn();
    rig.wait_for_cursor(500).await;
    rig.factory.reset_chain(3, None);
    for _ in 0..1_000 {
        if rig.cursor().await == Some(3) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    cancel.cancel();
    task.await.unwrap();
    assert_eq!(rig.store.resets.lock().unwrap().clone(), vec![2]);
    assert_eq!(rig.cursor().await, Some(3));
}

#[tokio::test(start_paused = true)]
async fn an_internal_error_on_a_block_the_node_still_serves_is_retried_not_skipped() {
    let factory = ScriptedBlockSourceFactory::new()
        .with_streams(vec![Open::then_fail(1, status(tonic::Code::Internal))]);
    factory.chain.push_empty(1, 3);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    rig.set_cursor(0).await;
    let (cancel, task) = rig.spawn();
    rig.wait_for_cursor(3).await;
    cancel.cancel();
    task.await.unwrap();
    assert_eq!(
        rig.factory.opens(),
        vec![(Transport::Stream, 1), (Transport::Stream, 2)]
    );
    assert_eq!(rig.store.cursor_writes(), vec![1, 2, 3]);
    assert!(!rig.signals.take_rotation_request(), "nothing was skipped");
}

#[tokio::test(start_paused = true)]
async fn a_pruned_block_is_skipped_on_the_stream_alone_too() {
    // `stream` mode never polls, so only the probe can tell that the node
    // no longer has the next block.
    let factory = ScriptedBlockSourceFactory::new();
    factory.chain.push_empty(1, 12);
    factory.chain.prune_below(10);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config().with_transport(TransportMode::Stream),
    );
    rig.set_cursor(2).await;
    let (cancel, task) = rig.spawn();
    rig.wait_for_cursor(12).await;
    cancel.cancel();
    task.await.unwrap();
    assert!(rig.signals.take_rotation_request());
    assert_eq!(rig.store.cursor_writes(), vec![12]);
}

#[tokio::test(start_paused = true)]
async fn an_open_that_never_returns_still_stops_with_the_lease() {
    // A node that takes the subscription and never answers it holds the
    // open; the lease's cancel must still stop the consumer, or the
    // release sweep worker that waits for it never competes for the lease
    // again (PR #586 review, B1).
    let factory = ScriptedBlockSourceFactory::new().with_streams(vec![Open::Hang]);
    factory.chain.push_empty(1, 10);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    rig.set_cursor(5).await;
    let (cancel, task) = rig.spawn();
    for _ in 0..1_000 {
        if !rig.factory.opens().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(rig.factory.opens(), vec![(Transport::Stream, 6)]);
    tokio::time::sleep(Duration::from_secs(3_600)).await;
    assert!(!task.is_finished(), "the open is still waiting");
    assert_eq!(rig.handle.status.snapshot().transport, None);
    cancel.cancel();
    let stopped = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("the cancel reaches the open")
        .unwrap();
    assert_eq!(stopped, Stopped::Cancelled);
}

#[tokio::test]
async fn a_block_with_unreadable_entries_is_processed_and_asks_for_a_walk() {
    // One entry this server cannot read (a newer account id version): the
    // rest of the block is processed, the cursor moves on, and one walk
    // covers the account it may have missed (PR #586 review, I4).
    let rig = Rig::new(
        MockMetadataStore::new().with_row(miden_meta("0xmoved")),
        MockStorageBackend::new().with_state_commitment("0xmoved", "0xc_stored"),
        ScriptedBlockSourceFactory::new(),
        config(),
    );
    let mut partly = block(42, &[("0xmoved", "0xc_elsewhere")]);
    partly.undecodable_updates = 1;
    let summary = rig
        .consumer()
        .process_for_test(&partly)
        .await
        .expect("processed");
    assert_eq!(summary.observed, 1);
    assert_eq!(rig.cursor().await, Some(42));
    assert!(rig.signals.take_rotation_request());
    assert_eq!(
        rig.store
            .walk_requests(NETWORK)
            .await
            .unwrap()
            .map(|walks| walks.requested_seq),
        Some(1)
    );

    // A block read in full asks for nothing.
    rig.consumer()
        .process_for_test(&block(43, &[]))
        .await
        .expect("processed");
    assert!(!rig.signals.take_rotation_request());
}

#[tokio::test(start_paused = true)]
async fn a_block_that_never_decodes_is_skipped_once_every_transport_failed_on_it() {
    // Block 6 fails to decode wherever it is read. In `auto` mode it is
    // skipped only once the polling fallback failed on it too; with one
    // transport, after the attempts on that one. Then the feed carries on
    // from block 7 and one walk covers block 6 (PR #586 review, I4).
    let malformed = || BlockSourceError::Malformed("scripted".to_string());
    for (mode, streams, polls, expected) in [
        (
            TransportMode::Auto,
            5,
            1,
            vec![
                (Transport::Stream, 6),
                (Transport::Stream, 6),
                (Transport::Stream, 6),
                (Transport::Stream, 6),
                (Transport::Stream, 6),
                (Transport::Poll, 6),
                (Transport::Poll, 7),
            ],
        ),
        (
            TransportMode::Stream,
            5,
            0,
            vec![
                (Transport::Stream, 6),
                (Transport::Stream, 6),
                (Transport::Stream, 6),
                (Transport::Stream, 6),
                (Transport::Stream, 6),
                (Transport::Stream, 7),
            ],
        ),
        (
            TransportMode::Poll,
            0,
            5,
            vec![
                (Transport::Poll, 6),
                (Transport::Poll, 6),
                (Transport::Poll, 6),
                (Transport::Poll, 6),
                (Transport::Poll, 6),
                (Transport::Poll, 7),
            ],
        ),
    ] {
        let factory = ScriptedBlockSourceFactory::new()
            .with_streams(
                (0..streams)
                    .map(|_| Open::then_fail(0, malformed()))
                    .collect(),
            )
            .with_polls(
                (0..polls)
                    .map(|_| Open::then_fail(0, malformed()))
                    .collect(),
            );
        factory.chain.push_empty(1, 10);
        let rig = Rig::new(
            MockMetadataStore::new(),
            MockStorageBackend::new(),
            factory,
            config().with_transport(mode),
        );
        rig.set_cursor(5).await;
        let (cancel, task) = rig.spawn();
        rig.wait_for_cursor(10).await;
        cancel.cancel();
        task.await.unwrap();
        assert_eq!(rig.factory.opens(), expected, "{mode:?}");
        assert_eq!(
            rig.store.cursor_writes(),
            vec![7, 8, 9, 10],
            "{mode:?}: block 6 is skipped"
        );
        assert!(rig.signals.take_rotation_request(), "{mode:?}");
        assert_eq!(
            rig.store
                .walk_requests(NETWORK)
                .await
                .unwrap()
                .map(|walks| walks.requested_seq),
            Some(1),
            "{mode:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn the_position_gauge_shows_the_cursor_without_a_node_and_clears_on_step_down() {
    // The stall alarm watches `last_block`: a feed that never reaches the
    // node exports the stored cursor (M1), and a replica that stepped down
    // stops exporting its old position (M2, PR #586 review).
    let recorder = crate::metrics::build_recorder();
    let metrics_handle = recorder.handle();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let factory = ScriptedBlockSourceFactory::new();
    factory.set_tip(Err(BlockSourceError::Transport("unreachable".to_string())));
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    rig.set_cursor(41).await;
    let (cancel, task) = rig.spawn();
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert!(rig.factory.opens().is_empty(), "never attached");
    let gauge = format!("{} ", crate::metrics::names::BLOCK_WATCH_LAST_BLOCK);
    let position = |rendered: &str| {
        rendered
            .lines()
            .find_map(|line| line.strip_prefix(&gauge))
            .map(str::to_string)
    };
    assert_eq!(position(&metrics_handle.render()).as_deref(), Some("41"));
    cancel.cancel();
    assert_eq!(task.await.unwrap(), Stopped::Cancelled);
    assert_eq!(position(&metrics_handle.render()).as_deref(), Some("0"));
}

#[tokio::test(start_paused = true)]
async fn a_node_that_keeps_refusing_the_cursor_is_asked_less_and_less_often() {
    // The cursor stands just past what the node serves (a node behind):
    // every subscription is refused as out of range. The retries back off
    // instead of hitting the node every second (PR #586 review, M5).
    let factory = ScriptedBlockSourceFactory::new();
    factory.chain.push_empty(1, 100);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    rig.set_cursor(200).await;
    let (cancel, task) = rig.spawn();
    tokio::time::sleep(Duration::from_secs(120)).await;
    cancel.cancel();
    task.await.unwrap();
    let times = rig.factory.open_times();
    assert!(times.len() >= 6, "{} opens", times.len());
    let gaps: Vec<Duration> = times.windows(2).map(|pair| pair[1] - pair[0]).collect();
    assert!(gaps[0] <= Duration::from_secs(1), "{gaps:?}");
    assert!(
        gaps[4] >= Duration::from_secs(8),
        "the fifth retry waits at least half of 16 s: {gaps:?}"
    );
    assert!(
        rig.factory
            .opens()
            .iter()
            .all(|open| *open == (Transport::Stream, 201)),
        "{:?}",
        rig.factory.opens()
    );
}

#[tokio::test]
async fn a_walk_request_that_cannot_be_recorded_holds_the_cursor_back() {
    // The walk must be recorded before the consumer moves past what it
    // covers; otherwise a failover would lose it (PR #586 review).
    let factory = ScriptedBlockSourceFactory::new();
    factory.chain.push_empty(1, 500);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    *rig.store.walk_request_failures.lock().unwrap() = 1;
    assert!(
        rig.consumer().start_position_for_test().await.is_err(),
        "the start is retried"
    );
    assert!(!rig.signals.take_rotation_request());
    assert_eq!(rig.consumer().start_position_for_test().await, Ok(500));
    assert!(rig.signals.take_rotation_request());

    // A block with entries this server cannot read is not done until its
    // walk is recorded.
    *rig.store.walk_request_failures.lock().unwrap() = 1;
    let mut partly = block(501, &[]);
    partly.undecodable_updates = 1;
    assert!(rig.consumer().process_for_test(&partly).await.is_err());
    assert_eq!(rig.cursor().await, None, "the cursor did not move");
    rig.consumer()
        .process_for_test(&partly)
        .await
        .expect("the retry processes it");
    assert_eq!(rig.cursor().await, Some(501));
    assert_eq!(
        rig.store
            .walk_requests(NETWORK)
            .await
            .unwrap()
            .map(|walks| walks.requested_seq),
        Some(2)
    );
}

#[tokio::test]
async fn the_walk_a_restart_owes_is_requested_once_however_often_the_start_runs() {
    // `start` runs again on every re-evaluation (an out-of-range refusal, a
    // chain far behind): only the first one follows the restart.
    let factory = ScriptedBlockSourceFactory::new();
    factory.chain.push_empty(1, 500);
    let rig = Rig::new(
        MockMetadataStore::new(),
        MockStorageBackend::new(),
        factory,
        config(),
    );
    rig.set_cursor(400).await;
    let consumer = rig.consumer_with(rig.handle.clone(), true);
    for _ in 0..3 {
        assert_eq!(consumer.start_position_for_test().await, Ok(401));
    }
    assert_eq!(
        rig.store
            .walk_requests(NETWORK)
            .await
            .unwrap()
            .map(|walks| walks.requested_seq),
        Some(1)
    );
}
