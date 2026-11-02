# Contract: Internal Traits

**Feature**: 012-block-streaming

Internal Rust contracts the implementation adds. Names are proposals; keep
them if nothing better emerges so the spec, docs and tests line up.

## `coordination::block_watch_store::BlockWatchStore`

```rust
#[async_trait]
pub trait BlockWatchStore: Send + Sync {
    async fn cursor(&self, network: NetworkType) -> Result<Option<u32>>;
    /// Forward-only; fenced by `fence` on Postgres.
    async fn set_cursor(&self, network: NetworkType, block: u32, fence: &LeaseFence) -> Result<CursorWrite>;
    /// Idempotent per (account_id, block_num); due at `now`.
    async fn record_observations(&self, observations: &[NewObservation], now: DateTime<Utc>) -> Result<()>;
    /// Due observation of the lowest block (then account id).
    async fn next_due_observation(&self, now: DateTime<Utc>) -> Result<Option<Observation>>;
    /// Earliest `next_attempt_at`, due or not (the visit loop's wake-up).
    async fn next_observation_due_at(&self) -> Result<Option<DateTime<Utc>>>;
    async fn delete_observation(&self, account_id: &str, block_num: u32) -> Result<()>;
    /// `attempts += 1`, `next_attempt_at = next_attempt_at`.
    async fn defer_observation(&self, account_id: &str, block_num: u32, next_attempt_at: DateTime<Utc>) -> Result<()>;
    async fn count_pending_observations(&self) -> Result<u64>;
}

pub enum CursorWrite { Advanced, NotForward, StaleLease }
pub struct NewObservation { pub account_id: String, pub block_num: u32, pub on_chain_commitment: String }
pub struct Observation { pub account_id: String, pub block_num: u32, pub on_chain_commitment: String, pub attempts: u32 }
```

Implementations: `PgBlockWatchStore` (Postgres pool; fenced cursor as in
`contracts/db-schema.md`), `InMemoryBlockWatchStore` (filesystem backend:
`persistent(dir)` keeps the cursor in `<dir>/cursor.json`, observations in
memory). Built by `CoordinationHandles` (`block_watch_store`) and carried
on `AppState.block_watch: Option<BlockWatchHandle>` together with the
config, the source factory and the live status.

## `jobs::block_watch::source::BlockSource`

```rust
#[async_trait]
pub trait BlockSource: Send {
    /// The next block, which must be exactly one past the previous one.
    /// An `Err` ends this source instance.
    async fn next(&mut self) -> Result<ObservedBlock, BlockSourceError>;
    fn transport(&self) -> Transport;
}

/// Opens sources at a given start block; `AutoSource` implements the
/// transport state machine over the two concrete openers.
#[async_trait]
pub trait BlockSourceFactory: Send + Sync {
    async fn open_stream(&self, from: u32) -> Result<Box<dyn BlockSource>, BlockSourceError>;
    async fn open_poll(&self, from: u32) -> Result<Box<dyn BlockSource>, BlockSourceError>;
    async fn chain_tip(&self) -> Result<u32, BlockSourceError>;
}
```

`MidenBlockSourceFactory` wraps the node client; tests inject a
`ScriptedBlockSourceFactory` (scripted blocks, terminal statuses, stalls)
without a network.

## `miden-rpc-client` additions

```rust
impl MidenRpcClient {
    /// A dedicated channel for long-lived streams: TLS, connect timeout,
    /// HTTP/2 keep-alive, no per-request deadline, one connect attempt.
    pub async fn connect_streaming(endpoint: impl Into<String>) -> Result<StreamingClient, RpcClientError>;

    /// One block by number, without proof (`None`: not served). Retried
    /// per `read_mode`; decodes up to 64 MiB.
    pub async fn get_block(&self, block_num: u32, read_mode: RpcReadMode) -> Result<Option<blockchain::SignedBlock>, RpcClientError>;
}

impl StreamingClient {
    pub async fn subscribe_blocks(&mut self, block_from: u32) -> Result<Streaming<rpc::BlockSubscriptionResponse>, RpcClientError>;
}

/// Proto → typed helpers next to `word_to_hex` / `proto_account_id`.
pub fn account_id_from_proto(id: &account::AccountId) -> Option<AccountId>;
/// A block's number and `(account_id, commitment, is_public)` per update;
/// an error, never a partial answer, for a missing header/body/id/word.
pub fn block_account_updates(block: &blockchain::SignedBlock) -> Result<BlockAccountUpdates, BlockDecodeError>;
```

`test_node::ScriptedNode` gains `healthy()`, `with_blocks(Vec<SignedBlock>)`,
`with_live_blocks(blocks, interval)`, `with_stream_terminal(code, message)`,
`with_block_subscription_disabled()` (answers `UNIMPLEMENTED`), request
logs, and a scripted `get_block_by_number`, so the transport layer is
tested against a real tonic server in-process; `test_node::block` /
`account_update` build the protos.

## `jobs::release_sweep::sweep::ReleaseSweeper`

```rust
impl ReleaseSweeper {
    /// Visit one account for an observed move: the observation stands in
    /// for the head probe. Same detectors, same release write.
    pub async fn visit_observed(&self, account_id: &str, on_chain: &str, observed_block_num: u32) -> Result<VisitOutcome>;
}
```

## `services::release_on_switch`

```rust
pub enum ReleaseOrigin { PushPath, Rotation, BlockWatch { observed_block_num: u32 } }
// `release_switched_account(state, metadata, guardian, evidence, origin)`;
// the audit payload gains "source": "push_path" | "rotation" |
// "block_watch" and, for block_watch, "observed_block_num".
```

## `MetadataStore` / `StorageBackend`

```rust
// metadata: unreleased Miden accounts among `ids`, split by the pending flag
async fn filter_release_sweep_ids(&self, ids: &[String]) -> Result<ReleaseSweepMembership, String>;
// storage
async fn pull_state_commitments_batch(&self, ids: &[&str]) -> Result<HashMap<String, String>, String>;
```

Both with Postgres and filesystem implementations and mock support in
`testing/mocks.rs` (`MockMetadataStore::with_filter_release_sweep_ids`,
`MockStorageBackend::with_state_commitment`).
