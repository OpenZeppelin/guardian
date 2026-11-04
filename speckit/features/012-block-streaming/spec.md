# Feature Specification: Block-Driven Switch Detection (Block Watch)

**Feature Branch**: `012-block-streaming`
**Created**: 2026-11-02
**Status**: Implemented on branch `012-block-streaming` (phase 1; see
`tasks.md`)
**Input**: User description: "Replace periodic per-account polling in the
switch-detection sweep with a block-streaming subscription: Guardians scan
blocks as they arrive and compare local state commitments against on-chain
state, rather than querying each account in turn."
**Tracking issue**: [#585](https://github.com/OpenZeppelin/guardian/issues/585)

## Overview

Today the release sweep (issue #434, `crates/server/src/jobs/release_sweep/`)
finds guardian switches that never reached the push path by **walking the
fleet**: one `GetAccount` probe per unreleased Miden account, paced so the
whole fleet is visited once per `rotation_seconds` (6 h by default, 5
visits/s at most). Its cost is `O(accounts)` RPC calls per rotation whether or
not anything changed, and its detection latency is the rotation length. At
the account volumes expected on mainnet (issue #317 targets 100k guarded
accounts) a rotation at the rate cap takes ~5.5 h of continuous RPC traffic
and the sweep can never go faster without taking more of the node's
capacity.

This feature makes switch detection **block-driven**. The server consumes the
node's committed-block feed (`BlockSubscription`, a server-streaming RPC the
pinned node proto already defines, with `GetBlockByNumber` polling as the
fallback transport), reads the `updated_accounts` list of every block, and
reacts only to guarded accounts whose on-chain commitment moved somewhere
this server does not expect. The cost becomes `O(blocks + unexplained moves)`
regardless of fleet size, and detection latency drops from hours to seconds.

The **detectors stay exactly as they are**. A commitment change in a block is
never, by itself, evidence of a switch (`spec/processes.md` "Release sweep":
the sweep never infers a switch from a commitment mismatch alone). The block
feed replaces the *probe* and the *rotation* — how the sweep learns that an
account moved — not the proof: candidate match, stored-state check, and the
published-storage read with confirmations are reused unchanged, invoked per
observed move instead of per rotation visit. Release writes, audit rows
(`detected_by`), and the `released_at` semantics do not change.

Block streaming also puts every on-chain account update (public accounts
carry their `AccountPatch`, the absolute post-state of what changed) in
front of the server as it happens. That is the natural place to reconcile
delta-derived state with chain-derived patches (issue #502) and to confirm
candidate landings without per-candidate polling. Those are **explicit
follow-on phases** of this feature (see "Out of Scope" and `plan.md`
"Phases"): the first delivery wires the feed and moves switch detection onto
it, and leaves well-defined hooks for the rest.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - A switch is detected within blocks, not hours (Priority: P1)

An account guarded by this server switches guardian through a path the push
hook never sees (offline switch, failed best-effort push, a client predating
the push). The switch transaction lands in block N. The old guardian releases
the account within a few blocks of N instead of within one 6 h rotation.

**Why this priority**: this is the feature. Faster release means fewer stale
reads, fewer dead pending proposals, and capacity slots freed promptly.

**Independent Test**: scripted node streams a block whose
`updated_accounts` moves a guarded public account to a commitment holding a
foreign guardian key; a second streamed block (or a timed re-check) supplies
the confirmation; the account is released with `detected_by: chain_sweep`
and an `accounts.release` audit row naming the observed block. Same for a
private account whose pending switch proposal's post-state is the streamed
commitment: released at once with `detected_by: proposal_match`.

**Acceptance Scenarios**:

1. **Given** block watch is running and a guarded public account with no
   candidate in flight, **When** a streamed block carries an update for it
   whose `final_state_commitment` differs from the stored commitment and
   its published storage shows a foreign guardian key at `confirmations`
   strictly increasing blocks, **Then** the account is released with
   `detected_by: chain_sweep` and the audit payload carries the block the
   move was observed in.
2. **Given** a private account with a switch proposal pending here, **When**
   a streamed block carries an update whose commitment equals the
   proposal's post-state, **Then** the account is released at once with
   `detected_by: proposal_match` and `switch_block_num` set to that block,
   and the proposal is finalized as today.
3. **Given** a guarded account whose streamed update commitment equals the
   stored commitment (this server's own canonical delta landed), **When**
   the block is processed, **Then** no chain RPC is made for it and nothing
   is written.
4. **Given** a guarded account with `has_pending_candidate = true`, **When**
   a streamed block updates it, **Then** block watch leaves it to the push
   path and the canonicalization worker (no observation recorded), exactly
   as the rotation skips it today.
5. **Given** an account not guarded by this server, **When** it appears in a
   block, **Then** it costs one membership lookup and nothing else.

---

### User Story 2 - Node load no longer grows with the fleet (Priority: P1)

An operator runs a Guardian with 100k guarded accounts against a node whose
RPC capacity they share with wallets and other services. Switch detection
costs a bounded, fleet-independent amount of node traffic.

**Why this priority**: the per-account rotation is the one background load
that scales with fleet size; issue #317's target cannot be met with it.

**Independent Test**: unit test over a synthetic fleet of 100k ids and a
stream of blocks touching 1% of them: chain RPCs issued = one subscription
(or one `GetBlockByNumber` per block in fallback) plus detector reads for
the moved-and-unexplained accounts only. No `GetAccount` is issued for
accounts the blocks did not touch.

**Acceptance Scenarios**:

1. **Given** block watch is healthy, **When** blocks arrive, **Then** the
   only per-block node calls are the stream itself (or one
   `GetBlockByNumber` per block in fallback mode) and the only per-account
   calls are the existing detector reads for accounts that moved
   unexplained.
2. **Given** the legacy rotation remains enabled as a safety net, **When**
   block watch reports healthy, **Then** operators can raise
   `GUARDIAN_RELEASE_SWEEP_ROTATION_SECONDS` (or disable the rotation) and
   detection latency stays at block cadence.

---

### User Story 3 - The feed survives node hiccups, lag, and failover (Priority: P1)

The node restarts, the stream is closed for lag (`RESOURCE_EXHAUSTED`,
followed by a 10-minute IP ban on the node side), the public RPC does not
serve the subscription at all, or the replica holding the block-watch lease
dies. Detection resumes from the last processed block without losing or
duplicating observations, and without hammering the node.

**Why this priority**: a feed that silently stalls is worse than the
rotation it replaces; the node's slow-subscriber ban makes careless
consumers lose the feed for 10 minutes at a time.

**Independent Test**: scripted node that (a) closes the stream with
`DATA_LOSS` / `RESOURCE_EXHAUSTED` / `UNAVAILABLE` after k blocks, (b)
answers `BlockSubscription` with `UNIMPLEMENTED`, (c) stalls. The consumer
resubscribes from `cursor + 1` with backoff, falls back to
`GetBlockByNumber` polling, detects the stall via the chain tip, and never
processes a block twice. Lease failover test: replica B takes over and
resumes from the cursor replica A persisted.

**Acceptance Scenarios**:

1. **Given** the stream closes with any status, **When** the consumer
   reconnects, **Then** it resubscribes from `cursor + 1` after an
   exponential backoff (capped), and for `RESOURCE_EXHAUSTED` it waits at
   least the node's ban duration before trying the stream again, polling
   `GetBlockByNumber` in the meantime.
2. **Given** `BlockSubscription` answers `UNIMPLEMENTED`, `PERMISSION_DENIED`
   or is refused as over capacity, **When** block watch starts, **Then** it
   runs on the polling transport and retries the stream on a long cooldown.
3. **Given** the stream delivers nothing for longer than
   `stall_seconds` while `GetBlockHeaderByNumber(latest)` shows the tip
   advanced, **When** the stall is detected, **Then** the stream is dropped
   and reopened from `cursor + 1`.
4. **Given** the lease holder dies after persisting cursor N, **When** another
   replica acquires the lease, **Then** it resumes from N + 1 and the
   observations A recorded but had not yet visited are visited by B.
5. **Given** a block arrives out of sequence (`block_num != cursor + 1`),
   **When** it is received, **Then** it is rejected, the stream reopened,
   and the event counted; the cursor never skips a block.
6. **Given** a server starting with no cursor (first deploy) or a cursor more
   than `max_catchup_blocks` behind the tip, **When** block watch starts,
   **Then** it starts at the tip (no genesis replay), logs the gap, and
   triggers one legacy rotation so the gap is covered by the existing walk.

---

### User Story 4 - Operators can see the feed working (Priority: P2)

An operator can tell from metrics, `/dashboard/info` and logs whether block
watch is attached, which transport it is on, how far behind the chain tip it
is, and how many observations await a detector visit.

**Independent Test**: scrape `/metrics` and `GET /dashboard/info` during an
e2e run; alarms fire on lag.

**Acceptance Scenarios**:

1. **Given** block watch is running, **When** `/metrics` is scraped, **Then**
   `guardian_block_watch_lag_blocks`, `guardian_block_watch_blocks_total`,
   `guardian_block_watch_account_updates_total{outcome}`,
   `guardian_block_watch_reconnects_total{reason}` and
   `guardian_block_watch_transport` are present with the documented labels.
2. **Given** block watch is running, **When** `GET /dashboard/info` is read,
   **Then** `backend.block_watch` reports `enabled`, `transport`,
   `last_block`, `chain_tip`, `lag_blocks`, `pending_observations` (`null`
   when disabled), next to the existing `backend.release_sweep`.
3. Stable `event=block_watch_*` log records exist for attach, transport
   change, reconnect, stall, cursor jump, and per-observation outcomes, and
   are listed in `docs/TROUBLESHOOTING.md`.

---

### User Story 5 - Nothing changes for clients (Priority: P2)

No client-facing API, payload, auth, or error surface changes. Release
semantics (`released_at`, `/configure` re-onboarding clearing a release,
audit `detected_by` vocabulary) are unchanged; the audit payload gains an
`observed_block_num` field for block-watch-originated releases.

**Independent Test**: the existing release-sweep e2e suite
(`testing/e2e/switch_guardian_release_sweep.rs`) passes unchanged; the TS and
Rust client suites are unaffected (no contract change).

---

### Edge Cases

- **Own transaction landing before promotion**: between the block that lands
  this server's canonical delta and the canonicalization pass that promotes
  it, the stored commitment lags the chain. The account has
  `has_pending_candidate = true` for that window (the delta is pushed before
  the transaction is submitted), so the membership filter excludes it; no
  observation is recorded. A candidate pushed *after* its transaction landed
  (abnormal, but possible with old clients) is recorded as an observation and
  the visit finds the stored commitment already promoted, or the candidate
  match finds the delta: no release in either case.
- **A switch lands while a candidate of this server's is in flight** (an
  offline switch while a stuck candidate holds `has_pending_candidate`):
  the block commitment is compared with the account's candidate
  post-states. A match is the candidate landing (left to the push path);
  anything else is an observation, whose visit waits while the
  candidate is in flight and then runs the detectors (PR #586 review,
  I2; the comparison half of `plan.md` D10's phase 2).
- **Account moves twice before its observation is visited**: observations
  are keyed `(account_id, block_num)`, so both are kept and visited in block
  order. The candidate match checks each observed commitment, so a switch
  followed by a transaction under the new guardian is still matched at the
  switch block without a history search.
- **Observation visited after `/configure` re-onboarded the account**: the
  visit re-reads the stored commitment; the release write is conditional on
  the stored state the evidence was proved against (unchanged rule), so a
  re-onboarding racing a release never ends up released.
- **Released or EVM account appears in a block**: filtered by the membership
  query (same predicate as `release_sweepable_rows`).
- **Block with thousands of account updates**: membership is one batched
  query per block (`WHERE account_id = ANY($1)`), chunked at 1,000 ids to
  keep the parameter list bounded. Processing stays off the gRPC receive
  loop so the node's slow-subscriber tracker never sees the consumer stall.
- **Node answers blocks faster than the detectors visit**: the stream
  consumer only filters and records; detector visits are paced by the
  existing `max_rate_per_second`. A backlog of observations is a metric and
  a dashboard field, never back-pressure on the stream.
- **Cursor persisted, observations not yet visited, process dies**: the
  observations are durable rows (Postgres) so another replica continues
  them. On the filesystem backend (single process, dev-only) they live in
  memory and are regenerated by a one-off rotation on restart.
- **Node pruned the blocks behind the cursor** (an old cursor after a long
  outage against a node with bounded block retention): `block_from` older
  than what the node serves fails the subscription or `GetBlockByNumber`;
  treated as the catch-up-too-large case: jump to the tip and trigger a
  rotation.
- **Stream replays protocol config**: `protocol_config` on the first message
  is ignored by block watch (not needed for commitments).
- **Clock-free**: block watch uses block numbers only; no wall-clock
  comparisons against block timestamps.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The server MUST consume committed blocks from the configured
  Miden node as a background task ("block watch"), starting from a
  persisted cursor (`last processed block`), processing blocks strictly in
  sequence (`cursor + 1`), and persisting the cursor after each processed
  block.
- **FR-002**: Block watch MUST support two transports behind one internal
  `BlockSource` abstraction: the node's `BlockSubscription` server stream
  (primary) and `GetBlockByNumber` polling driven by
  `GetBlockHeaderByNumber(latest)` (fallback). Transport selection MUST be
  automatic (stream first; fall back on `UNIMPLEMENTED`, `PERMISSION_DENIED`,
  `RESOURCE_EXHAUSTED`, repeated connect failures) and MUST retry the
  primary on a cooldown. The active transport MUST be observable.
- **FR-003**: For every block, block watch MUST extract
  `(account_id, final_state_commitment)` for each entry of
  `body.updated_accounts` (private and public alike) and determine, with one
  batched metadata query per block (chunked), which of those accounts are
  *sweepable* here: Miden network, `released_at IS NULL`,
  `has_pending_candidate = false` — the same predicate the rotation uses.
- **FR-004**: For each sweepable account in a block, block watch MUST compare
  the observed commitment with the stored state commitment (batched read).
  Equal: nothing is recorded. Different: an **observation**
  `(account_id, block_num, on_chain_commitment)` is recorded for a detector
  visit. No chain RPC is made on this path.
- **FR-005**: Observations MUST be durable on the Postgres backend (table
  `block_watch_observations`, PK `(account_id, block_num)`) and in-memory on
  the filesystem backend (documented backend limitation). Recording an
  observation MUST be idempotent (upsert) so a replayed block never
  duplicates work.
- **FR-006**: A separate paced **visitor** loop (same lease holder) MUST
  consume observations oldest due first (then in block order), so ones that
  keep coming back never hold a newer one back, at most `max_rate_per_second` visits
  per second (shared with the legacy rotation and the confirmation
  re-checks, as today), and run the existing per-account detectors with the
  observed commitment standing in for the head probe: stored-state check,
  candidate match (head first, then the history search only if needed),
  published-storage read with `confirmations` at strictly increasing blocks.
  A visit MUST delete its observation on `Checked`, keep it with backoff on
  `Deferred`/`Failed`, and MUST re-read the stored commitment first, treating
  "stored now equals observed" as `Checked` with nothing to do.
- **FR-007**: Release writes, evidence variants, `detected_by` values, proposal
  finalization, and the conditional-on-stored-state rule MUST remain those of
  `services::release_on_switch`. The audit payload for a release proved from
  an observation MUST add `observed_block_num` (the block the move was seen
  in) and `source: "block_watch"`; rotation-originated releases keep
  `source: "rotation"`.
- **FR-008**: Exactly one replica MUST run block watch at a time: the
  holder of the existing `release_sweep` lease (plan D1; Postgres row
  lease with fencing on the Postgres backend, `AlwaysLeader` on the
  filesystem backend), which runs the consumer and the observation
  visitor together. The cursor row MUST be written under the holder's
  lease (checked, and the lease row held, in the cursor write's
  transaction) so a superseded holder cannot move it after losing the
  lease; a refused write stops the holder.
- **FR-009**: The gRPC receive path MUST be decoupled from processing: a
  bounded in-process channel between the transport task and the processing
  task, sized so the consumer drains the node's 32-message subscriber buffer
  well within the node's 10 s send timeout; processing of one block MUST not
  await detector RPCs.
- **FR-010**: On stream termination block watch MUST reconnect from
  `cursor + 1` with exponential backoff (base 1 s, cap 60 s, jitter). On
  `RESOURCE_EXHAUSTED` (slow-subscriber disconnect; the node bans the IP for
  10 min) it MUST NOT retry the stream for at least `ban_cooldown_seconds`
  (default 600) and MUST run the polling transport meanwhile.
- **FR-011**: Block watch MUST detect a silent stall: if no block arrives for
  `stall_seconds` (default 60) it reads the chain tip; if the tip is ahead of
  the cursor the stream is reopened from `cursor + 1`.
- **FR-012**: Block watch MUST validate sequence (`header.block_num ==
  cursor + 1`); a violation drops the source and reopens it from
  `cursor + 1`. Parent linkage (`prev_block_commitment`), block signatures
  and body commitments are NOT verified in this phase (the node is trusted
  the same way its `GetAccount` answers are trusted today, and Guardian has
  no proto→`BlockHeader` conversion to compute header commitments); this is
  recorded as an explicit trust assumption and a follow-up. A release still
  requires detector proof (an exact candidate post-state, or a confirmed
  foreign key read from published storage), never the block alone.
- **FR-013**: With no cursor, or with the tip more than `max_catchup_blocks`
  (default 10,000) ahead of the cursor, block watch MUST start at the
  current tip, log the jump with the gap, and trigger one legacy rotation
  (`run_release_sweep_now`-style, paced as usual) so the skipped range is
  covered by the walk. It MUST NOT replay from genesis.
- **FR-014**: The legacy rotation (`ReleaseSweepConfig`) MUST remain available
  and configured independently; its defaults are unchanged by this feature.
  `docs/CONFIGURATION.md` MUST document the recommended pairing (rotation as a
  weekly safety net once block watch is healthy).
- **FR-015**: Configuration: `GUARDIAN_BLOCK_WATCH_ENABLED` (default `true`),
  `GUARDIAN_BLOCK_WATCH_TRANSPORT` (`auto` | `stream` | `poll`, default
  `auto`), `GUARDIAN_BLOCK_WATCH_POLL_SECONDS` (default 3),
  `GUARDIAN_BLOCK_WATCH_POLL_MAX_RATE_PER_SECOND` (default 10),
  `GUARDIAN_BLOCK_WATCH_STALL_SECONDS` (default 60),
  `GUARDIAN_BLOCK_WATCH_MAX_CATCHUP_BLOCKS` (default 10000),
  `GUARDIAN_BLOCK_WATCH_BAN_COOLDOWN_SECONDS` (default 600),
  `GUARDIAN_BLOCK_WATCH_STREAM_RETRY_SECONDS` (default 600),
  `GUARDIAN_BLOCK_WATCH_MAX_ATTEMPTS` (default 20), with the same
  trim/parse/range/startup-failure rules as `GUARDIAN_RELEASE_SWEEP_*`
  (ranges in `contracts/config-contract.md`), and Terraform variables for
  `enabled` and `transport`. `GUARDIAN_RELEASE_SWEEP_ENABLED` MUST come to
  mean "run the rotation"; the task runs when either flag is on.
- **FR-016**: Observability: metrics (`guardian_block_watch_blocks_total{transport}`,
  `guardian_block_watch_lag_blocks` gauge, `guardian_block_watch_last_block`
  gauge, `guardian_block_watch_account_updates_total{outcome=
  not_guarded|at_stored|own_history|observed|skipped_pending_candidate|
  unfinished_configuration}`,
  `guardian_block_watch_observations_pending` gauge,
  `guardian_block_watch_reconnects_total{reason}`,
  `guardian_block_watch_transport{transport}` gauge), `backend.block_watch`
  in `GET /dashboard/info`, stable `event=block_watch_*` log records, the
  Prometheus/ADOT metric allowlist in `infra/observability.tf`, and a lag
  alarm.
- **FR-017**: Storage and metadata backend parity: new trait methods
  (`MetadataStore::filter_release_sweep_ids`, block-watch cursor and
  observation stores) MUST be implemented for both Postgres and filesystem
  with the same externally observable semantics, except durability of
  observations across restarts on the filesystem backend (documented).
- **FR-018**: Block watch MUST be network-scoped: it runs for the configured
  Miden network only; EVM accounts are never observed (no EVM block
  feed in scope).

### Key Entities

- **Block cursor**: `(network, last_block, updated_at, fence_token)`; one row
  per network. The last block fully processed (observations recorded).
- **Observation**: `(account_id, block_num, on_chain_commitment, observed_at,
  attempts, next_attempt_at)`. One per guarded account per block in which it
  moved to a commitment the server did not hold. Deleted when visited
  (`Checked`); retried with backoff otherwise.
- **BlockSource** (internal): a transport yielding `ObservedBlock {
  block_num, committed_chain_tip, updates: Vec<AccountUpdate {
  account_id_hex, commitment_hex, is_public }> }` in sequence.
- **ReleaseSweeper** (existing): gains `visit_observed(account_id,
  on_chain, block_num)`; the rotation keeps `visit_account`.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: With block watch healthy, a switch landing in block N is
  released by the old guardian no later than block N + `confirmations`
  (public account, storage read) or block N (private account with a pending
  proposal), plus visitor pacing; measured in the e2e suite with the scripted
  node and live on devnet.
- **SC-002**: Steady-state node traffic attributable to switch detection is
  one open stream (or one `GetBlockByNumber` per block) plus detector reads
  for unexplained moves only; for a synthetic 100k-account fleet with 1% of
  accounts touched per block, zero `GetAccount` calls are made for untouched
  accounts (unit test).
- **SC-003**: Block processing (filter + batched stored-commitment read +
  observation upsert) completes in under 1 s per block at 5,000 updated
  accounts per block on the Postgres backend (bench in the e2e suite), far
  inside the node's 10 s send timeout.
- **SC-004**: The consumer never trips the node's slow-subscriber ban in a
  24 h devnet soak (zero `RESOURCE_EXHAUSTED` reconnects).
- **SC-005**: Lease failover resumes from the persisted cursor with no block
  processed twice and no observation lost (e2e, two in-process holders).
- **SC-006**: Existing release-sweep e2e tests, the full nextest suite, the
  Postgres feature suite, and clippy `--all-features` pass.

## Assumptions

- The pinned node proto (`miden-node-proto-build 0.17.3`) defines
  `BlockSubscription`; the node implementation (miden-node v0.17.x,
  `crates/rpc/src/server/api/subscription/`) serves it from the public RPC
  component with: at most 10 concurrent subscriptions per RPC instance, a
  32-message per-subscriber buffer, a 10 s send timeout, disconnect when the
  subscriber's accumulated lag exceeds 100 blocks (`RESOURCE_EXHAUSTED`) and a
  10-minute IP ban after such a disconnect, and rejection of `block_from`
  more than 100 blocks past the tip. See `research.md` for the live probe
  against the public devnet and testnet endpoints.
- `BlockAccountUpdate` carries `account_id` and `final_state_commitment` for
  private accounts too (`AccountUpdateDetails::Private`), so block watch sees
  every guarded account's moves without published storage.
- The node is trusted for block contents to the same degree it is trusted
  for `GetAccount` answers today (no signature verification in this phase).
- Transactions on guarded multisig accounts are pushed to this server
  before they are submitted, so a landed own-delta is covered by
  `has_pending_candidate` until promotion (today's rotation relies on the
  same).

## Dependencies

- Issue #434 release sweep (detectors, evidence, conditional release write).
- Issue #242 / feature 010 coordination (leases, fencing, migration lock).
- `miden-rpc-client` generated `NodeServiceClient::block_subscription` and
  `get_block_by_number` (already generated; not yet wrapped).
- Node version on the target network: devnet/testnet run miden-node 0.17.x
  (probe in `research.md`); a self-hosted node for mainnet is recommended
  because the public RPC's 10 subscription slots are shared.

## Out of Scope

- Candidate confirmation from the block feed (promote a candidate when its
  `new_commitment` appears in a block instead of polling
  `verify_commitment` on the fast-promotion timer). Phase 2; the observation
  hook is designed for it (`plan.md` "Phases").
- `AccountPatch`-based state reconciliation for public accounts (issue #502,
  #345 "still bound" healing). Phase 3; block watch already decodes whether
  an update is public and can hand the patch over.
- Verifying block signatures / validator set.
- An EVM block feed.
- Removing the legacy rotation (kept as a safety net; a later issue can
  retire it once block watch has run on mainnet).
- Persisting confirmation streaks across failover (replica-local today;
  unchanged).
