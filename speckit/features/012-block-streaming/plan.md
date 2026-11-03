# Implementation Plan: Block-Driven Switch Detection (Block Watch)

**Feature**: 012-block-streaming | **Date**: 2026-11-02 | **Status**:
Implemented (phase 1). Deviations from the design below are listed under
"Implementation notes" at the end.
**Spec**: [spec.md](spec.md) · **Research**: [research.md](research.md) ·
**Data model**: [data-model.md](data-model.md) · **Contracts**:
[contracts/](contracts/) · **Tasks**: [tasks.md](tasks.md)

## Summary

Add a background **block watch** to `crates/server` that consumes the Miden
node's committed-block feed, records an *observation* for every guarded
account whose on-chain commitment moved somewhere this server does not hold,
and feeds those observations to the existing release-sweep detectors instead
of walking the fleet. The rotation stays as a safety net. No client-facing
contract changes.

```text
              ┌──────────────────────── lease holder (release_sweep lease) ────────────────────────┐
  node ──────▶│ BlockSource (stream | poll) ──▶ bounded chan ──▶ Consumer: per block                │
              │                                                   filter sweepable ids (1 query)   │
              │                                                   compare stored commitments (1 q) │
              │                                                   upsert observations, set cursor  │
              │                                                            │                        │
              │   Visitor (same paced loop as rotation + re-checks) ◀───────┘ block_watch_observations
              │     └─▶ ReleaseSweeper::visit_observed ─▶ existing detectors ─▶ release_switched_account
              └────────────────────────────────────────────────────────────────────────────────────┘
                              Postgres: block_watch_cursor · block_watch_observations · worker_leases
```

## Technical Context

- **Language / runtime**: Rust 2024 (workspace `rust-version` 1.98.1), tokio,
  tonic 0.14, prost 0.14, diesel-async (Postgres), filesystem backend.
- **Node API**: `miden-node-proto-build 0.17.3` descriptors compiled in
  `crates/miden-rpc-client` (`NodeServiceClient::block_subscription`,
  `get_block_by_number`, `get_block_header_by_number`). Verified live against
  devnet and testnet nodes running 0.18.0 (research.md §1.4).
- **Affected layers**: `crates/server` only (jobs, coordination, storage,
  metadata, builder/config, metrics, dashboard info), `crates/miden-rpc-client`
  (stream + block wrappers, scripted node), `infra/` (Terraform variables,
  metric allowlist, alarm), docs (`CONFIGURATION.md`, `TROUBLESHOOTING.md`,
  `DASHBOARD.md`, `spec/processes.md`, `docs/architecture/services.md`).
  No `crates/client`, `packages/*`, or SDK change (Constitution I/II: proven
  unaffected — no contract moves).
- **Testing**: unit (mocks), scripted node (`miden-rpc-client`
  `scripted-node` feature), server `e2e` feature suite, Postgres ignored
  tests via `scripts/test-postgres.sh`, live devnet smoke.

## Constitution Check

- **I. Bottom-up propagation**: server-internal; `GET /dashboard/info` gains
  an additive `backend.block_watch` object (nullable) — the operator client
  parses `info` loosely; verify `packages/guardian-operator-client` tests
  still pass (additive field). No other upstream impact.
- **II. Transport parity**: n/a (no per-account API change).
- **III. Append-only / explicit lifecycles**: observations are a new
  append-then-delete worklist, not custody data; release transitions reuse
  the documented `released_at` path. No implicit fallback in user-visible
  flows; the transport fallback is internal and observable (FR-002).
- **IV. Auth / errors**: untouched.
- **V. Evidence-driven**: user stories in spec.md, validation in
  quickstart.md, docs updates listed in tasks.md.
- **Invariants**: storage/metadata backend parity kept except documented
  filesystem observation durability (spec FR-005/FR-017).

## Design

### D1. Where it runs: the `release_sweep` lease holder

Block watch is part of the release sweep task, not a new leased task. The
holder of `RELEASE_SWEEP_LEASE` spawns the **consumer** (stream/poll →
observations) alongside the existing paced loop, and the paced loop gains a
third visit source (observations) next to re-checks and the rotation. Both
stop on the lease's cancel token; a lost lease stops the stream, and the
next holder resumes from the persisted cursor.

Why one lease: a single pacing budget (`max_rate_per_second`) governs every
detector visit whatever triggered it; one leader to reason about in the
runbook; the filesystem backend (single process, `AlwaysLeader`) needs the
consumer and visitor in one process because its observations are in memory.
Rejected: a second lease (`block_watch`) that could land on another replica —
possible with durable observations, but adds a second failover domain for no
gain. Rejected: every replica subscribing — 10 subscription slots per RPC
instance shared with the world, N× bandwidth, N× detector reads, and
release-write races (safe because writes are conditional, but wasteful).

Startup gate: the worker starts when `release_sweep.enabled ||
block_watch.enabled`; `GUARDIAN_RELEASE_SWEEP_ENABLED=false` now disables the
rotation only (doc update), `GUARDIAN_BLOCK_WATCH_ENABLED=false` disables
the feed only.

### D2. Cluster state: what is shared, what is replica-local

| State | Where | Why |
|---|---|---|
| Cursor (last processed block) | Postgres `block_watch_cursor`, fenced by the lease token; filesystem: `<data_dir>/block_watch/cursor.json` | Failover resumes without replay or gaps. |
| Observations (account, block, commitment, attempts) | Postgres `block_watch_observations`; filesystem: in-memory `BTreeMap` | The handoff from consumer to visitor survives a holder crash; another replica continues. |
| Guarded-account membership | Already shared: `account_metadata` | A `/configure` on any replica is visible to the holder at the next block via the batched filter query; no cross-replica messaging. |
| Confirmation streaks, candidate post-state cache, history-search positions | Replica-local `SweepState` (unchanged) | A failover restarts streaks and caches, which only delays a release (same as today). |
| Transport state (which source, backoff, ban cooldown) | Holder memory | Rebuilt on failover: a new holder tries the stream first. |

This answers the feature's open question ("how state is synced across a
cluster"): Postgres is the system of record for everything a successor needs;
the lease picks the one consumer; conditional release writes make a
superseded holder harmless.

### D3. Transports: `BlockSource`

```rust
pub struct ObservedBlock {
    pub block_num: u32,
    pub committed_chain_tip: u32,
    pub updates: Vec<AccountUpdate>,        // every entry of body.updated_accounts
}
pub struct AccountUpdate { pub account_id: String /* hex */, pub commitment: String /* word_to_hex */, pub is_public: bool }

#[async_trait]
pub trait BlockSource: Send {
    /// Next block, strictly `previous + 1`. `Err` is terminal for this source
    /// instance: the caller decides whether to reopen or switch transport.
    async fn next(&mut self) -> Result<ObservedBlock, BlockSourceError>;
    fn transport(&self) -> Transport;  // Stream | Poll
}
```

- **StreamSource** (`BlockSubscription { block_from: cursor + 1 }`): own
  tonic `Channel` (TLS native roots, `connect_timeout`, HTTP/2 keep-alive
  ~30 s with timeout, **no per-request deadline**), a receive task that
  decodes each `BlockSubscriptionResponse` into `ObservedBlock` and sends it
  into `mpsc::channel(64)`; `next()` reads the channel. Decoding is cheap
  (ids + words only); notes/nullifiers/transactions are dropped unread.
- **PollSource** (`GetBlockHeaderByNumber(latest)` every `poll_seconds`, then
  `GetBlockByNumber(b, include_proof: false)` for `cursor+1..=min(tip,
  cursor+batch)`): uses the existing `MidenRpcClient` (retry policy
  `SingleAttempt` per call; the loop is the retry).
- **AutoSource** (default): state machine — try Stream; on
  `UNIMPLEMENTED | PERMISSION_DENIED | UNAUTHENTICATED` → Poll, retry Stream
  after `stream_retry_seconds` (default 600); on `RESOURCE_EXHAUSTED`
  (slow-subscriber disconnect, refused as banned, or no slot) → Poll, retry
  Stream after `ban_cooldown_seconds` (600); on any other terminal status or
  transport error → reopen Stream from `cursor + 1` with exponential backoff
  (1 s → 60 s, jitter), and after 5 consecutive failures → Poll for
  `stream_retry_seconds`. Every transition logs `event=block_watch_transport`
  and bumps `reconnects_total{reason}`.
- **Sequence guard** (both transports): a block whose `block_num !=
  cursor + 1` is a `BlockSourceError::OutOfSequence` → reopen. (Parent
  linkage via `prev_block_commitment` needs a proto→`BlockHeader` conversion
  Guardian does not have; listed as a follow-up with signature verification.)
- **Stall guard**: if `next()` yields nothing for `stall_seconds` (60) the
  consumer reads the tip through `MidenRpcClient::get_chain_tip`; if the tip
  is ahead of the cursor it drops and reopens the source
  (`reconnects_total{reason="stall"}`), otherwise it keeps waiting (an idle
  chain is not a stall).

### D4. Consumer: per-block processing (hot path, no chain RPC)

```text
process(block):
  ids        = block.updates.map(account_id)
  sweepable  = metadata.filter_release_sweep_ids(ids)            // chunks of ≤1000; Postgres: release_sweepable_rows().filter(account_id = ANY)
  if sweepable non-empty:
      stored = storage.pull_state_commitments_batch(sweepable)   // commitment column only
      for (id, on_chain) in updates where id ∈ sweepable:
          match stored.get(id):
             Some(c) if c == on_chain → outcome=at_stored (own canonical state landed; nothing)
             Some(_)                  → block_watch.record_observation(id, block_num, on_chain) ; outcome=observed
             None                     → outcome=unfinished_configuration (no state row yet; nothing)
      the rest → outcome=not_guarded / skipped_pending_candidate (counted from the filter)
  block_watch.set_cursor(block_num, lease_fence)                 // forward-only, fenced
  gauges: last_block, lag = committed_chain_tip - block_num, observations_pending
```

- Per-block cost: one metadata query (chunked), one storage query when any
  guarded account moved, k upserts (k = unexplained moves, normally 0), one
  cursor update. SC-003 bounds this at 5,000 updates/block.
- Why compare against the stored commitment here: it turns "our own
  transaction landed" (the overwhelming majority of guarded-account moves)
  into a no-op before it costs a detector visit. The `has_pending_candidate`
  filter already excludes the in-flight window; the comparison is the second
  line for candidates pushed late or promoted between block and visit.
- Observations are keyed `(account_id, block_num)` so two moves of one
  account before its first visit are both kept, in order (spec "Edge
  Cases").
- **Never block the stream**: `process` holds no lock across awaits other
  than the two store calls; the receive task keeps draining the node's
  32-message buffer into the local channel while `process` runs. If the
  local channel fills (processing slower than 3 s/block for a sustained
  period) the receive task blocks, the node's lag tracker grows, and the
  stream is closed for lag — this is the documented, metric-visible failure
  mode (`reconnects_total{reason="resource_exhausted"}`), and the poll
  transport carries on. Keep `process` cheap; SC-003 is the budget.

### D5. Cursor policy

- No cursor row (first run) → `start = tip` (from `get_chain_tip`), log
  `event=block_watch_cursor_init`, request one rotation.
- `tip - cursor > max_catchup_blocks` (10,000 ≈ 8.3 h at 3 s) → `start = tip`,
  log `event=block_watch_cursor_jump gap=...`, request one rotation.
- Else `start = cursor + 1` (the stream replays the gap at full speed; the
  lag tracker only penalizes a *growing* gap).
- A subscription rejected because `block_from` is behind what the node
  retains → treated as the jump case.
- "Request one rotation" = set a flag the paced loop reads to start the next
  walk now (`Rotation::next_start = now`) — same walk, same pacing, so
  cost is bounded as today. On the filesystem backend a restart also
  requests a rotation (in-memory observations were lost).

### D6. Visitor: observations in the paced loop

`worker.rs` `hold_lease` turn order: due re-check → due observation →
rotation turn (when its turn is due), alternating so no source starves,
never closer than `min_spacing`. Observations are taken with
`block_watch.next_due_observation(now)` (oldest `block_num` first among
`next_attempt_at <= now`).

`ReleaseSweeper::visit_observed(account_id, on_chain, block_num) ->
VisitOutcome`:

1. `metadata.get`: missing → forget + `Checked`; EVM / released → `Checked`;
   `has_pending_candidate` → `Checked` (push path owns it; the observation is
   consumed, as the rotation's skip is today).
2. `pull_state_commitment`: not found → `Checked` (unfinished configuration);
   `== on_chain` → `clear_streak` + `Checked` (state caught up).
3. `pull_full_state`; if its commitment differs from step 2's → `Deferred`
   (moved between reads; retry).
4. `check_stored_state(metadata, &stored, false)` → `Released` → `Checked`.
5. `visit_diverged(metadata, &stored, on_chain, base)` unchanged, with
   `ReleaseOrigin::BlockWatch { observed_block_num: block_num }` threaded to
   the release write for the audit payload.

Outcome handling: `Checked` → `delete_observation`; `Deferred | Failed` →
`defer_observation(attempts + 1, next_attempt_at = now + min(recheck_seconds
× 2^attempts, 1 h))`; after `max_attempts` (20) → record one walk, then
delete + `warn` `event=block_watch_observation_dropped` + metric (kept when
the walk cannot be recorded). `Waiting` (own candidate in flight, open
streak; PR #586 review) → `postpone_observation`, no attempt counted. Streak re-checks keep working as today (`schedule_recheck` when a
streak is open after the visit).

### D7. Release write and audit

`services::release_on_switch::ReleaseWrite` gains `origin: ReleaseOrigin`
(`Rotation` | `BlockWatch { observed_block_num }` | `PushPath`), serialized
into the `accounts.release` payload as `source` and, for block watch,
`observed_block_num`. `ReleaseEvidence` variants and `detected_by` values are
unchanged. `docs/DASHBOARD.md` audit paragraph updated.

### D8. Configuration (`builder/block_watch.rs`, mirrors `builder/release_sweep.rs`)

See [contracts/config-contract.md](contracts/config-contract.md). Defaults:
enabled `true`, transport `auto`, poll 3 s, stall 60 s, max catch-up 10,000
blocks, ban cooldown 600 s, stream retry 600 s, max attempts 20. Terraform:
`guardian_block_watch_enabled` (bool, default true) and
`guardian_block_watch_transport` (string, default `"auto"`), wired in
`infra/ecs.tf` next to the sweep variables.

### D9. Observability

Metrics (`metrics/names.rs` + registry help + `infra/observability.tf`
allowlist): see [contracts/observability.md](contracts/observability.md).
Dashboard `GET /dashboard/info` → `backend.block_watch` (nullable). Logs:
`event=block_watch_{attached,transport,reconnect,stall,cursor_init,
cursor_jump,block,observation_recorded,observation_visited,
observation_dropped}`. CloudWatch alarm on `guardian_block_watch_lag_blocks`
(> 200 for 5 min) in `infra/observability.tf`, routed like the existing
alarms (issue #443).

### D10. Phases

- **Phase 1 (this feature)**: everything above. Deliverable: switch
  detection at block cadence; rotation untouched.
- **Phase 2 (follow-up issue)**: candidate confirmation. In `process`, for
  accounts the filter excluded as `has_pending_candidate`, compare the block
  commitment with the queued candidates' `new_commitment` (batched read of
  candidate heads) and, on a hit, signal the canonicalization worker to
  `process_account(id)` now. Lets `fast_promotion_interval_seconds` polling
  be retired. The consumer already has the ids and commitments; the only new
  piece is the signal.
- **Phase 3 (follow-up, with #502)**: for public guarded accounts
  (`is_public`), hand the block's `AccountPatch` to a reconciliation hook:
  compare with the delta-derived patch of the canonical delta that produced
  the same commitment, and heal `still_bound` lag (#345) from chain truth.
  `AccountUpdate` gains `patch: Option<proto::AccountPatch>` then; nothing
  else in the consumer changes.
- **Later**: block signature / validator-set verification; retire the
  rotation; upstream filtered feed.

### D11. Rollout

1. Merge behind the default-on flag with Terraform `guardian_block_watch_enabled`
   set explicitly per environment.
2. Devnet first (memory: nothing touches prod until devnet is confirmed):
   watch `lag_blocks`, `reconnects_total`, `observations_pending`, and one
   real switch via the demo/smoke harness releasing at block cadence.
3. Testnet soak 24 h (SC-004), then prod with the rotation still at its
   default; raise the rotation to a weekly safety net in a later change.

## Project Structure

```text
crates/server/src/
├── jobs/
│   ├── release_sweep/
│   │   ├── mod.rs            # docs: feed + rotation; re-exports
│   │   ├── worker.rs         # spawn consumer under the lease; third visit source
│   │   └── sweep.rs          # visit_observed; ReleaseOrigin threading
│   └── block_watch/          # NEW
│       ├── mod.rs            # docs, BlockWatchConfig re-export, start/stop glue
│       ├── source.rs         # BlockSource, ObservedBlock, decode from proto
│       ├── stream.rs         # StreamSource (BlockSubscription, own channel, receive task)
│       ├── poll.rs           # PollSource (tip + GetBlockByNumber)
│       ├── auto.rs           # AutoSource transport state machine + backoff
│       ├── consumer.rs       # cursor policy, per-block processing, stall guard
│       └── tests.rs
├── coordination/
│   ├── block_watch_store.rs          # NEW trait: cursor + observations; InMemoryBlockWatchStore
│   └── postgres/block_watch_store.rs # NEW PgBlockWatchStore (fenced cursor)
├── builder/
│   ├── block_watch.rs        # NEW BlockWatchConfig + env parsing
│   ├── handle.rs             # start worker when either feature is enabled
│   └── state.rs              # AppState.block_watch: Option<BlockWatchConfig>
├── metadata/{mod,postgres,filesystem}.rs   # filter_release_sweep_ids
├── storage/{mod,postgres,filesystem}.rs    # pull_state_commitments_batch
├── services/release_on_switch.rs           # ReleaseOrigin in ReleaseWrite + payload
├── metrics/{names,labels}.rs               # block watch metrics + labels
├── dashboard/info.rs                       # backend.block_watch
└── testing/e2e/block_watch.rs              # NEW e2e scenarios
crates/server/migrations/2026-11-XX-000001_block_watch/{up,down}.sql
crates/miden-rpc-client/src/lib.rs          # subscribe_blocks(), get_block(), decode helpers, account id from proto
crates/miden-rpc-client/src/test_node.rs    # scripted BlockSubscription / GetBlockByNumber
infra/{variables,ecs,observability}.tf
docs/{CONFIGURATION,TROUBLESHOOTING,DASHBOARD}.md, docs/architecture/services.md, spec/processes.md
```

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Public RPC has no free subscription slot / bans the IP | AutoSource falls back to polling with cooldowns; metrics show the transport; recommend a dedicated node for prod. |
| Consumer stalls and gets banned | Receive task decoupled from processing; cheap per-block path; SC-003 budget; ban handled by cooldown. |
| Cursor far behind after an outage | Jump-to-tip + one rotation; no genesis replay. |
| Block content trusted without signature verification | Same trust as `GetAccount` today; releases still require detector proof (candidate match or confirmed foreign key); follow-up to verify signatures. |
| Observation backlog grows (slow node for detector reads) | Backlog is a gauge + dashboard field; capped attempts; rotation backstop. |
| Postgres load per block | One or two indexed queries per block, 28,800 blocks/day: negligible; measured in SC-003. |
| Filesystem backend loses observations on restart | Documented; restart requests a rotation. |

## Implementation notes (phase 1, as built)

Where the code differs from the design above, and why:

- **Release origin.** `ReleaseWrite` is the outcome enum of
  `release_switched_account`, so the origin is a parameter of that
  function instead (`ReleaseOrigin::{PushPath, Rotation, BlockWatch {
  observed_block_num }}`), annotated onto the audit payload as `source` /
  `observed_block_num`. A confirmation re-check of a streak an
  observation opened credits block watch (the sweeper remembers the
  observed block per open streak). A post-state matched at the observed
  commitment records the observed block as `switch_block_num`.
- **Membership query.** `MetadataStore::filter_release_sweep_ids`
  returns `ReleaseSweepMembership { sweepable, pending_candidate }` from
  one statement per 1,000-id chunk (`guarded_miden_rows()`, the rotation's
  predicate minus the pending flag), so `skipped_pending_candidate` and
  `not_guarded` are told apart without a second query.
- **Store API.** `set_cursor` takes a `LeaseFence` and returns
  `CursorWrite { Advanced, NotForward, StaleLease }`; the Postgres write
  checks the lease row `FOR SHARE` in the same transaction.
  `record_observations` is batched (one `INSERT … SELECT FROM unnest(…)`
  per block). Observation timestamps come from the server clock (`now`
  parameter), not the database's, so due times compare against the same
  clock that wrote them. `next_observation_due_at()` lets the visit loop
  sleep until the earliest due observation; the consumer wakes it through
  a `Notify` when it records observations or requests a rotation.
- **Visit turns.** Round robin over re-check, observation, rotation (the
  first due kind after the last served), so each due kind is served
  within three visits; the rate floor is unchanged.
- **Observed state caught up.** When the stored commitment equals the
  observed one, the visit takes the same path as a head probe that finds
  the chain at the stored base (the stored state's own key is checked
  once per stored state), not just `clear_streak`.
- **Rotation requests.** A request starts a walk now when the rotation is
  idle and right after the current walk otherwise (a walk already under
  way may have visited accounts before the skipped blocks), and it runs
  even with `GUARDIAN_RELEASE_SWEEP_ENABLED=false`. To keep the sweep's
  pacing settings when the rotation is off, `ServerBuilder` now keeps a
  disabled `ReleaseSweepConfig` (`enabled = false`) instead of dropping
  it; every reader already checked `enabled`.
- **Filesystem cursor file.** `<metadata dir>/block_watch/cursor.json`,
  one entry per network keyed by the `NetworkType` name (`MidenDevnet`,
  …): `{ "MidenDevnet": { "last_block": N, "updated_at": "…" } }`,
  written atomically. The Postgres `network` column uses the same names.
- **Decoding.** Account ids are decoded from the proto's felt pair into a
  typed `AccountId` and written with `to_hex()`; an account stored under
  a non-canonical spelling of its id (e.g. upper-case hex) is not matched
  by the membership query, and the rotation still covers it. The stream
  and `get_block` decode up to 64 MiB per block, so a large block cannot
  wedge the feed.
- **Observability.** Dashboard info adds `transport_mode` next to the
  live `transport`. A second alarm, `<stack>-block-watch-stalled`, fires
  when the cursor gauge stops advancing for 10 minutes: the lag gauge
  cannot see a feed that stopped. A holder whose feed stops resets its
  lag gauge to 0, so a replica that stepped down cannot hold the lag
  alarm open. The `source` label on
  `guardian_release_sweep_accounts_total` was not added: it would change
  the metric's CloudWatch dimension set.
- **Upstream.** `packages/guardian-operator-client` parses
  `backend.block_watch` (AGENTS.md: `/dashboard/*` shape changes reach the
  operator client in the same change).
- **Review fixes (code review of the first implementation).** The cursor
  records the genesis commitment (`block_watch_cursor.genesis_commitment`,
  `Cursor { last_block, genesis }`) and `BlockWatchStore::reset_cursor`
  moves it back when the chain was reset (another genesis, a cursor more
  than 100 blocks past the tip, or `OUT_OF_RANGE`, which also counts in
  `guardian_block_watch_reconnects_total{reason="status_out_of_range"}`).
  A stream that fails
  with `INTERNAL` asks the node whether it still serves the next block
  (`BlockSourceFactory::serves_block`), so a block the node no longer has
  is skipped even in `stream` mode. A move to the post-state of one of the
  account's own canonical deltas (`StorageBackend::
  filter_canonical_post_states`, outcome `own_history`) is no
  observation, so a replay after downtime does not spend detector visits
  on the account's own history. A caught-up observation no longer clears
  an open streak. `GUARDIAN_BLOCK_WATCH_ENABLED` unset follows
  `GUARDIAN_RELEASE_SWEEP_ENABLED` (Terraform: `guardian_block_watch_enabled`
  defaults to `null`), so the old kill switch still turns everything off on
  upgrade. `poll_seconds` must be below `stall_seconds`. The Miden sources
  live in `network/miden/block_feed/` (AGENTS.md: shared layers stay
  network-agnostic); `RpcSettings::block_source_factory` opens them.
- **PR #586 review fixes.** (B1) The stream open waits for the node's
  answer at most `GUARDIAN_MIDEN_RPC_TIMEOUT_MS` (`BlockSourceError::
  Timeout`, `reason=timeout`, counted as an ordinary failure), and the
  consumer's open races the lease's cancel, so `run_worker` never waits on
  a hung open. (B2) On a block watch visit a storage read whose
  `block_num` is before the observed block, or a history search that
  ends before it, is no answer: `Deferred` (`read_behind_observation`,
  `history_behind_observation`), not `Checked`. Keyed on the read's
  block rather than the origin alone, so a current read showing the
  account back at its stored base, or older than a re-onboarded state,
  still settles the observation. (I1) Walk requests are recorded in
  `block_watch_walks` (fenced; sequence numbers, so a request made during
  a walk stays owed and a stale holder's late completion clears
  nothing); a holder takes an owed walk over when it gains the lease, a
  walk records the sequence it started under and completes up to it.
  (I2) Accounts with a candidate in flight are compared with their
  stored commitment and their candidates' post-states
  (`StorageBackend::filter_candidate_post_states`); only an unexplained
  move is an observation, and its visit is `Deferred` while the
  candidate is in flight (canonicalization settles one within minutes,
  well inside `max_attempts`). (I3) A first foreign-key sighting keeps
  its observation (`Deferred`) while the streak is open. (I4)
  `block_account_updates` reports undecodable entries instead of failing
  the block (outcome `undecodable`, one walk requested); a block that
  fails to decode five times, on both transports in `auto` mode, is
  skipped with one walk (`block_watch_block_skipped`). (M1/M2)
  `last_block` is exported from the stored cursor before the node is
  reached and reset to 0 on step-down. (M3) The caught-up branch reports
  an own-key mismatch. (M5) `OUT_OF_RANGE` retries back off. (M7) The lag
  alarm breaches only while the lag shrinks by fewer than 100 blocks per
  5-minute period. The filesystem
  cursor writes its file before moving in memory. Deferred: case-
  insensitive account-id matching (M4; both SDKs send lowercase).
- **PR #586 review, round 2.** A visit that waits (own candidate in
  flight, open streak) returns `VisitOutcome::Waiting`, which postpones
  the observation by `recheck_seconds` without counting an attempt
  (`BlockWatchStore::postpone_observation`, visits outcome `waiting`),
  so a long wait never drops the only hint. An observation out of
  attempts is dropped only after a walk is recorded for it. A release
  write that finds the stored state replaced (`StateMoved`, also through
  `StoredState::Moved`) is `Deferred` on an observation visit, so the
  next visit re-reads the new base. Start position: with the same
  genesis on both sides the cursor is never rewound (a node behind is
  waited for, `block_watch_node_behind`); a genesis on one side only is
  a reset (`genesis_unknown`), and `chain_was_reset` counts a node that
  stops reporting its genesis; the height heuristic applies only when
  neither side knows one.
  Then, from the review of those fixes: a genesis missing on one read
  is read again before it counts; a genesis first reported where the
  cursor recorded none is adopted (height check only); a node behind on
  the same chain is waited for at most `NODE_BEHIND_LIMIT` (1 h,
  `node_behind_too_long`); a requested walk that stays partial is
  retried at most `MAX_PARTIAL_WALK_RETRIES` (5) times, the delay
  doubling, then counts as done (`release_sweep_walk_unchecked`); a
  waiting observation older than `LONG_WAIT_BLOCKS` (28,800) is
  reported and visited hourly; an observation whose drop cannot record
  its walk is kept (`block_watch_observation_kept`).
