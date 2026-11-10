# Tasks: Block-Driven Switch Detection (Block Watch)

**Feature**: 012-block-streaming | **Input**: spec.md, plan.md, contracts/

**Status (2026-11-02)**: phases A–G done except the 24 h devnet soak (G3)
and the follow-up issues (H). Where the code lives differs from the
layout below in one place: the Miden sources (`stream.rs`, `poll.rs`, the
factory) are in `crates/server/src/network/miden/block_feed/` (AGENTS.md:
shared layers stay network-agnostic); `jobs/block_watch/` keeps the
traits, the transport policy and the consumer. See `plan.md`
"Implementation notes" for the other deviations.

Ordered for incremental, independently testable delivery. Each task names
its validation. `[P]` = can run in parallel with its neighbours. Follow
AGENTS.md: server changes first, tests in the changed layer, docs with the
change, `cargo run --features evm --bin gen-openapi -- docs` after the
dashboard info change, full CI matrix before pushing (nextest, `integration`
/ `e2e` features, `postgres` suite, clippy `--all-features`).

## Phase A — node client (crates/miden-rpc-client)

- [x] **A1** `account_id_from_proto`, `block` decode helper: proto
  `SignedBlock` → `(block_num, Vec<(account_id_hex, commitment_hex,
  is_public)>)`, erroring on missing header/body/id/word. Unit tests with
  hand-built protos, including a private update and a malformed one.
- [x] **A2** `MidenRpcClient::get_block(block_num, read_mode)` wrapping
  `get_block_by_number { include_proof: Some(false) }` through `retry_read`;
  `connect_streaming` (own channel: TLS native roots, `CONNECT_TIMEOUT`,
  HTTP/2 keep-alive 30 s / timeout 10 s, no `.timeout`) and
  `StreamingClient::subscribe_blocks(from)`. Record RPC metrics like the
  other wrappers (`record_rpc` in the server's network layer).
- [x] **A3** `ScriptedNode`: `with_blocks(Vec<ScriptedBlock>)`,
  `with_stream_terminal(Status)`, `with_block_subscription_disabled()`,
  scripted `get_block_by_number`, scripted tip advancing with blocks. Tests:
  replay then live, terminal status propagation, `UNIMPLEMENTED`.

## Phase B — stores and schema (crates/server)

- [x] **B1** Migration `2026-11-XX-000001_block_watch` (contracts/db-schema.md);
  schema.rs entries.
- [x] **B2** `BlockWatchStore` trait + `InMemoryBlockWatchStore` (cursor file
  under the data dir, `Mutex<BTreeMap>` observations) + `PgBlockWatchStore`
  (fenced, forward-only cursor; idempotent record; due/defer/delete/count).
  Wire through `CoordinationHandles` → `AppState.block_watch_store`. Tests:
  in-memory unit tests; Postgres `#[ignore]` tests under a `postgres` module
  path: fenced write refused with a stale lease, forward-only cursor,
  idempotent record, due ordering.
- [x] **B3 [P]** `MetadataStore::filter_release_sweep_ids` (Postgres chunked
  `eq_any` over `release_sweepable_rows()`; filesystem predicate filter;
  mock builder). Tests: released / pending-candidate / EVM rows excluded,
  chunk boundary at 1,000.
- [x] **B4 [P]** `StorageBackend::pull_state_commitments_batch` (Postgres
  chunked column read; filesystem default; mock builder). Tests: missing
  rows omitted, errors propagated.

## Phase C — block sources (crates/server/src/jobs/block_watch)

- [x] **C1** `source.rs`: `ObservedBlock`, `AccountUpdate`, `Transport`,
  `BlockSourceError`, `BlockSource`, `BlockSourceFactory`; proto decode via
  A1 with `validate_account_id`.
- [x] **C2** `stream.rs` `StreamSource`: receive task → `mpsc(64)`; maps
  terminal statuses; sequence guard. Tests against `ScriptedNode` (A3):
  replay + live, `RESOURCE_EXHAUSTED` surfaces as `Status`, out-of-sequence
  block errors.
- [x] **C3** `poll.rs` `PollSource`: tip every `poll_seconds`, fetch
  `cursor+1..=min(tip, cursor+batch)`; idle when caught up. Tests against
  `ScriptedNode`.
- [x] **C4** `auto.rs` `AutoSource`: transport state machine with backoff,
  ban cooldown, stream retry cooldown, `transport` override (`stream` /
  `poll`). Pure-state unit tests (no I/O) for every transition in plan D3;
  integration test with a `ScriptedBlockSourceFactory`.

## Phase D — consumer and cursor policy

- [x] **D1** `builder/block_watch.rs` `BlockWatchConfig` + env parsing +
  `AppState.block_watch`; startup gate `release_sweep.enabled ||
  block_watch.enabled`; `docs/CONFIGURATION.md` rows. Tests mirror
  `builder/release_sweep.rs` (ranges, trimming, bad values stop startup).
- [x] **D2** `consumer.rs`: start position (cursor / init / jump / pruned),
  rotation request flag, `process(block)` per plan D4 (filter → compare →
  record → cursor), stall guard, metrics, logs. Tests with mocks: at-stored
  no-op, observed upsert, pending-candidate skip, chunking, cursor fenced
  error stops the consumer, stall → reopen, jump requests a rotation,
  processing never awaits detector RPCs (assert no `verify_commitment` /
  `GetAccount` calls on the mock network client).
- [x] **D3** Wire into `release_sweep/worker.rs`: spawn consumer under the
  lease cancel token; `hold_lease` gains the observation visit source with
  the documented turn order; rotation request flag honoured. Tests with
  paused time like the existing worker tests: observation vs re-check vs
  rotation alternation, rate floor kept.

## Phase E — visitor and release origin

- [x] **E1** `ReleaseSweeper::visit_observed` (plan D6) + `ReleaseOrigin`
  threading into `ReleaseWrite` and the audit payload (`source`,
  `observed_block_num`); rotation and push path record their origins.
  Tests: stored-caught-up → `Checked`; pending candidate → `Checked`;
  proposal match at the observed commitment → `proposal_match` release with
  `switch_block_num = observed block` and the audit `source=block_watch`;
  storage read + confirmations → `chain_sweep`; private + no candidate →
  `storage_opaque` `Checked`; moved-between-reads → `Deferred`.
- [x] **E2** Visitor outcome handling: delete / defer with backoff / drop at
  `max_attempts` (metric + warn). Tests.

## Phase F — observability and dashboard

- [x] **F1** Metrics names, labels (closed sets), registry help, `infra/observability.tf`
  allowlist + lag alarm; `metrics` tests that assert the allowlist covers the
  new names.
- [x] **F2** `GET /dashboard/info` `backend.block_watch` (nullable, additive),
  `ToSchema` update, regenerate `docs/openapi-dashboard.json`,
  `docs/DASHBOARD.md`; check `packages/guardian-operator-client` tests.
- [x] **F3** `docs/TROUBLESHOOTING.md` event list; `spec/processes.md` new
  "Block watch" section before "Release sweep" and a rotation paragraph
  update; `docs/architecture/services.md`; `infra/variables.tf`, `ecs.tf`,
  `terraform.tfvars.example`, `infra/README.md`.

## Phase G — e2e and live validation

- [x] **G1** `testing/e2e/block_watch.rs` (feature `e2e`): scripted factory
  streams blocks over the existing sweep harness accounts: US1 scenarios
  1–5, US3 scenarios 1–6 (two in-process holders for failover), SC-002 (100k
  synthetic ids, 1% touched, zero `GetAccount` for untouched), SC-003 bench
  (5,000 updates/block under 1 s on Postgres, `#[ignore]`).
- [x] **G2** Existing `switch_guardian_release_sweep.rs` suite still green
  with block watch disabled and enabled.
- [ ] **G3** Live (done 2026-11-02 except the 24 h soak: devnet stream +
  poll + restart/resume; local miden-node v0.17.3 with a fault-injecting
  proxy (connection cut, frozen stream → stall); a real switch executed
  offline after its proposal was created on guardian A, released by A's
  block watch at the switch block with `proposal_match`; two Postgres
  replicas, holder SIGKILLed, successor resumed at cursor+1; a chain reset
  followed onto the new genesis). Original task: local miden-node compose (0.17/0.18 image) + two replicas
  (`docs/guides/horizontal-scaling`), execute a real switch through the demo
  harness, observe `block_watch_observation_recorded` → release within
  blocks; kill the holder mid-stream, confirm resume from the cursor. Then
  devnet (quickstart.md) with the real server against
  `rpc.devnet.miden.io`, 24 h soak for SC-004.

## Phase H — follow-ups to file (not in this PR)

- [ ] Phase 2 issue: candidate confirmation from the block feed (retire
  fast-promotion polling).
- [ ] Phase 3 issue (link #502, #345): `AccountPatch` reconciliation for
  public accounts from the feed.
- [ ] Block signature / validator-set verification of streamed blocks.
- [ ] Retire or lengthen the rotation after mainnet validation.
- [ ] Upstream miden-node: filtered account-update subscription.
- [ ] Account ids stored under a non-canonical spelling (upper-case hex,
  which `/configure` accepts) are not matched by the membership query;
  normalize ids at `/configure` or match case-insensitively.
