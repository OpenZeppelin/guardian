# Research: Block-Driven Switch Detection

**Feature**: 012-block-streaming | **Date**: 2026-11-02

Facts established while designing the feature, with their sources, so the
implementation does not have to re-derive them. Anything marked *assumption*
still needs confirming during implementation.

## 1. What the node offers

### 1.1 `BlockSubscription` exists in the pinned proto

`miden-node-proto-build 0.17.3` (`proto/rpc.proto`, service `Rpc`):

```proto
// Streams committed blocks starting from the given block number (inclusive).
// Replays historical blocks first, then streams live blocks as they are committed.
// On lag (replica falls too far behind), the stream is closed with a DATA_LOSS error and
// the client should reconnect from its local tip.
rpc BlockSubscription(BlockSubscriptionRequest) returns (stream BlockSubscriptionResponse) {}

message BlockSubscriptionRequest { fixed32 block_from = 1; }
message BlockSubscriptionResponse {
    blockchain.SignedBlock block = 1;
    fixed32 committed_chain_tip = 2;
    optional protocol_config.ProtocolConfig protocol_config = 3; // first item + on change
}
```

`GetBlockByNumber(GetBlockByNumberRequest { fixed32 block_num; optional bool
include_proof }) -> { SignedBlock block; ExecutionProof proof }` returns the
same block type one at a time (the polling fallback).

Guardian's `crates/miden-rpc-client` already generates
`NodeServiceClient::block_subscription` (returns
`tonic::codec::Streaming<BlockSubscriptionResponse>`) and
`get_block_by_number`; neither is wrapped by `MidenRpcClient` yet. The
scripted test node (`test_node.rs`) answers both with `UNIMPLEMENTED` today.

### 1.2 What a streamed block carries

Generated `blockchain::SignedBlock { header: BlockHeader, body: BlockBody,
signatures: Vec<Signature> }`, `BlockBody { updated_accounts:
Vec<BlockAccountUpdate>, output_note_batches, created_nullifiers,
transactions: Vec<TransactionHeader> }`, `BlockAccountUpdate { account_id:
account::AccountId (oneof V1 { suffix: Felt, prefix: Felt }),
final_state_commitment: primitives::Word, details: AccountUpdateDetails
(oneof Private(PrivateAccountUpdate) | Public(AccountPatch)) }`.

So for **every** account touched by a block, private ones included, the
block names the account and its post-state commitment. Public accounts
additionally carry the `AccountPatch` (absolute values of what changed,
including storage, i.e. the guardian key slot when it changed). The header
carries `block_num`, `timestamp`, `prev_block_commitment`, `account_root`,
etc.

`miden_rpc_client::word_to_hex` is the formatter `get_account_commitment`
uses, so a block's `final_state_commitment` formatted with it compares
byte-for-byte with the stored state commitment the sweep already compares
against. `proto_account_id` (lib.rs) converts domain → proto; the reverse
(proto `(prefix, suffix)` felts → `AccountId` → `to_hex`) is needed and is a
few lines (`AccountId::new_unchecked([prefix, suffix])` or the checked
constructor; round-trip through `validate_account_id`).

### 1.3 Node-side limits (miden-node v0.17.3, `crates/rpc/src/server/api/subscription/`)

| Constant | Value | Consequence for Guardian |
|---|---|---|
| `MAX_REPLICA_SUBSCRIPTIONS` | 10 per RPC instance | A public RPC's slots are shared with everyone; one subscription per Guardian cluster, with a polling fallback when refused. |
| `SUBSCRIBER_CHANNEL_CAPACITY` | 32 messages | The consumer must drain promptly; processing must not run on the receive path. |
| `SEND_TIMEOUT` | 10 s | A stalled client is disconnected. |
| `SubscriberLagTracker::MAX_RUNNING_GAP` | 100 blocks (accumulated *growth* of the gap to the tip) | Falling behind by a growing margin ends the stream with `RESOURCE_EXHAUSTED` ("client is too slow to keep up with the chain"). Replaying from far behind is fine as long as the gap keeps shrinking. |
| `BAN_DURATION` | 10 min, keyed by client IP | After a slow-subscriber disconnect the IP cannot resubscribe for 10 min: Guardian must switch to polling and not retry the stream before the cooldown. |
| `MAX_FUTURE_GAP_IN_SUBSCRIPTIONS` | 100 blocks | `block_from` more than 100 past the tip is rejected. |

Terminal statuses (`StreamError::into_status`): server shutdown →
`UNAVAILABLE`; client closed → `ABORTED`; slow subscriber →
`RESOURCE_EXHAUSTED`; internal → `INTERNAL`. The `DATA_LOSS` mentioned in
the proto comment is not emitted by v0.17.3 but should be handled like any
terminal status (reconnect from the cursor).

### 1.4 Live probe of the public endpoints (2026-11-02)

A throwaway example built on `crates/miden-rpc-client` (source in
`quickstart.md`) opened a subscription from `tip - 3` and read 12 items.

| | devnet (`rpc.devnet.miden.io`) | testnet (`rpc.testnet.miden.io`) |
|---|---|---|
| Node version (`Status`) | 0.18.0 | 0.18.0 |
| Chain tip at probe | 574,033 | 769,690 |
| `GetBlockByNumber(tip)` | OK, 141 ms, 391 bytes (empty block) | OK, 120 ms, 497 bytes |
| `BlockSubscription` open | OK, 122 ms | OK, 121 ms |
| Historical replay (4 blocks) | all within 0.2 ms of open | same |
| Live cadence | 3 s per block (timestamps 3 s apart) | 3 s |
| First item | 2,968 bytes (carries `protocol_config`) | 3,074 bytes |
| Empty live block | 396 bytes | 502 bytes (2 signatures) |
| Block with 1–2 account updates, 2–4 txs | — | 1.8–4.0 KB |
| `committed_chain_tip` | tracks the tip | tracks the tip |

Takeaways: the public RPCs serve the stream to a plain client; a
subscription slot was available; blocks decode with the 0.17.3-generated
types against 0.18.0 nodes; replay-then-live works as documented; the
channel built by `MidenRpcClient::connect` (10 s per-request timeout) kept
the stream open for the whole 24 s run, so tonic's channel timeout bounds
the response headers, not the stream body (the implementation should still
give the stream its own channel with HTTP/2 keep-alive and no per-request
deadline, so a hung connection is detected without cutting a healthy one).

### 1.5 Bandwidth

At 3 s cadence a chain produces 28,800 blocks/day. Empty blocks cost
~11.5 MB/day. Block size grows with chain activity (notes and patches
dominate), not with Guardian's fleet; a busy mainnet block with hundreds of
transactions is *estimated* in the hundreds of KB to low MB (*assumption*:
measure on a busy chain before mainnet). The feed is therefore
`O(chain activity)` where the rotation was `O(fleet)`. The node offers no
"headers + account updates only" feed; if mainnet block sizes make the full
feed expensive, an upstream request for a filtered account-update
subscription (the "node-side filtered events API" issues #305 and #369
anticipated) is the lever.

## 2. What Guardian has today

### 2.1 Release sweep (issue #434)

`crates/server/src/jobs/release_sweep/{mod,worker,sweep}.rs`, spec in
`spec/processes.md` "Release sweep", config in `builder/release_sweep.rs`
(`GUARDIAN_RELEASE_SWEEP_{ENABLED,ROTATION_SECONDS,MAX_RATE_PER_SECOND,
PAGE_SIZE,RECHECK_SECONDS,CONFIRMATIONS}`), lease `release_sweep`
(`coordination::RELEASE_SWEEP_LEASE`, 30 s TTL, renewed every 10 s).

- `worker.rs`: one paced loop per lease holder; each turn visits one
  account: a due confirmation re-check or the next account of the rotation;
  never closer than `1 / max_rate_per_second`.
- `sweep.rs`: `ReleaseSweeper::visit_account(id)` → `visit(metadata)`:
  early-outs (EVM, released, pending candidate, unfinished configuration),
  `verify_commitment(stored)` **probe**, then on `Mismatch { on_chain }`:
  `check_stored_state` (promoted switch whose release was never written),
  then `visit_diverged(metadata, stored, on_chain, base)`: detector 1
  `find_executed_switch` (candidate post-states vs `on_chain`, then the
  `SyncTransactions` history search for key-moving candidates), detector 2
  `fetch_on_chain_guardian_binding` (published storage; `confirmations`
  observations at strictly later blocks via `SweepState` streaks and
  re-checks).
- `SweepState` (replica-local): streaks, re-check queue, candidate
  post-state cache (`switch_evidence` with `resume_from_block`),
  `checked_stored`.
- Release write: `services::release_on_switch::release_switched_account`
  with `ReleaseEvidence::{Delta, ChainSweep, ProposalMatch,
  RecoverableDelta}`; conditional on the stored state
  (`set_released_if_state`); audit `accounts.release` with `detected_by`.
- Metadata listing: `release_sweepable_rows()` (Postgres) = `released_at IS
  NULL AND has_pending_candidate = false AND network_config->>'kind' =
  'miden'`; `list_release_sweep_ids(after, limit)`,
  `count_release_sweep_accounts()`.
- Metrics: `guardian_release_sweep_{rotations_total,rotation_duration_seconds,
  accounts_total}`; dashboard `backend.release_sweep` in `GET /dashboard/info`;
  log events `release_sweep_*` listed in `docs/TROUBLESHOOTING.md`.

The probe + rotation is what this feature replaces; everything from
`check_stored_state` onward is reused.

### 2.2 Coordination (feature 010 / issue #242)

`coordination::LeaderElector` (`PgLeaseElector` over `worker_leases` with
`fence_token`; `AlwaysLeader` on the filesystem backend),
`jobs::canonicalization::spawn_lease_renewal`, `storage::LeaseFence` for
fenced writes inside the write transaction, migrations under an advisory
lock. `CoordinationHandles` carries per-task leases
(`leader`, `stats_leader`, `release_sweep_leader`).

### 2.3 Storage / metadata reads needed

- `StorageBackend::pull_state_commitment(id)` (Postgres: column read, no
  decrypt; filesystem: default pulls the state) — batched variant needed.
- `StorageBackend::pull_states_batch(ids)` exists (decrypts; used by the
  dashboard) — too heavy for the per-block path.
- `MetadataStore::get(id)`, `list_release_sweep_ids`, `set_released_if_state`,
  `clear_released_if_state`.

### 2.4 Canonicalization worker (for phase 2 context)

`jobs/canonicalization/worker.rs` runs a full pass every
`check_interval_seconds` (10 s), a fast-promotion pass every 3 s for the
first 30 s of a candidate's life, and a reconcile pass every 60 s; each
candidate is probed with `verify_commitment`. A block feed can replace the
fast-promotion polling with an exact "your candidate landed" signal (phase
2, out of scope here).

## 3. Related issues

- #305 (split-brain on switch; listed "node provides filtered events API" as
  an approach), #311 (push-path release), #369 (the rotation; its "v3
  alternative" is this feature), #434 (shipped rotation, PR #484), #504
  (queued switch delta).
- #317 (scalability target: 100k accounts), #242 / feature 010 (leases).
- #502 (`AccountPatch` for state updates) and #345 (stored state permanently
  behind): phase 3 consumers of the same feed.
- #418 (creation-block anchor): a persisted block cursor is a step toward
  bounded scans.

## 4. Decisions taken in this design (see plan.md for rationale)

1. One feed per cluster under the **existing `release_sweep` lease**; the
   holder runs the stream consumer and the paced visitor.
2. **Durable observations** (`block_watch_observations`) and a **fenced
   cursor** (`block_watch_cursor`) in Postgres; in-memory + file on the
   filesystem backend.
3. **Detectors unchanged**; the observation stands in for the head probe.
4. **Two transports** behind one `BlockSource`, automatic fallback with
   cooldowns matching the node's ban.
5. **No genesis replay**: start at the tip; large gaps hand off to one
   rotation.
6. Rotation **kept** as a safety net; defaults unchanged.
7. Sequence validation only; block signature verification is a follow-up.

## 5. Open questions (not blocking)

- Mainnet block sizes under load (bandwidth of the full-block feed).
- Whether the production deployment should run its own node/RPC replica so
  the subscription slot is guaranteed (recommended; the fallback covers the
  public RPC either way).
- Whether to retire the rotation after block watch proves itself (separate
  issue).
- Upstream: a filtered account-update subscription in miden-node.
