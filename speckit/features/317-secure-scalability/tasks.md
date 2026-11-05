# Tasks: Security-Preserving Scalability for Reads, Replay Protection and Canonicalization

**Input**: Design documents from `speckit/features/317-secure-scalability/`
**Prerequisites**: [plan.md](./plan.md), [spec.md](./spec.md), [research.md](./research.md), [data-model.md](./data-model.md), [contracts/](./contracts/), [quickstart.md](./quickstart.md)

**Tests**: Included. Constitution principles IV and V require updated tests in every auth layer plus one upstream consumer, and the spec defines acceptance scenarios for every story.

**Organization**: Grouped by user story, but **ordered by the plan's delivery phases, not strictly by priority**: measurement (US6) comes first because every other story is judged by it, and the replay-store gate blocks US1 and US3. Server unit tests live in `#[cfg(test)]` modules next to the code; live-Postgres tests are `#[ignore]` per repository convention. Paths are relative to the repository root.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: can run in parallel (different files, no dependency on an incomplete task)
- **[Story]**: user story from spec.md (US1 to US7)

---

## Phase 1: Setup

**Purpose**: Branch, dependency, and module scaffolding.

- [X] T001 Create branch `317-secure-scalability` from `main` and confirm `cargo build --workspace` and `npm ci && npm run build -w @openzeppelin/guardian-client` pass in `packages/`
- [X] T002 Add `moka = { version = "0.12", features = ["future"] }` to `crates/server/Cargo.toml` and refresh `Cargo.lock`
  - _Status (2026-10-30): Added as an optional dependency enabled by the `postgres` feature._
- [ ] T003 [P] Create empty module scaffolds with `mod` declarations: `crates/server/src/replay/mod.rs`, `crates/server/src/auth_cache/mod.rs`, registered in `crates/server/src/lib.rs`
  - _Status (2026-10-30): Not needed: the cache lives inside `PostgresMetadataStore`; `replay/` is created when US1 starts._
- [X] T004 [P] Create benchmark profile skeleton `benchmarks/prod-server/profiles/target-317.toml` (100,000 accounts, 20,000 readers at 2 s, 100 writers at one push per 10 s, 30 min sustained) with fields to be wired in US6
  - _Status (2026-10-30): Done as two profiles, `target-317-mix.toml` and `target-317-full-state.toml` (one per SC-003 leg)._

---

## Phase 2: User Story 6 - Operators can measure every acceptance criterion (Priority: P2, delivered first)

**Goal**: Per-route percentiles, auth rejection metrics, pool and limiter metrics, a target-shaped benchmark, a v1 baseline, and the direct DB route.

**Independent Test**: Run the benchmark against a prod-like stack; dashboard per-route p95 matches server span p95 within 10%; auth, rate-limit and pool numbers appear under bounded labels (quickstart section 7).

### Tests for User Story 6

- [X] T005 [P] [US6] Test that an empty-body gRPC request returns `INVALID_ARGUMENT` and is not counted in `SERVER_FAULT_CODES`, in `crates/server/src/metrics/grpc.rs` test module
- [X] T006 [P] [US6] Test that `guardian_auth_rejections_total` increments with `{transport, policy, reason}` for replay, timestamp-window and invalid-signature rejections on HTTP and gRPC, in `crates/server/src/services/mod.rs` test module
  - _Status (2026-10-30): Done for wallet credentials (replay, timestamp_window, invalid_signature). Labels are `credential` and `reason`; a `transport` label is not available at the auth layer and is left to the request histograms._
- [X] T007 [P] [US6] Test reader/writer workload scheduling (poll interval, nonce-then-state on change, full-`get_state` leg, writer rate, hot-account leg) in `benchmarks/prod-server/tests/workload_roles.rs`

### Implementation for User Story 6

- [X] T008 [P] [US6] Add metric names `guardian_auth_rejections_total`, `guardian_session_read_records_total`, `guardian_replay_fence_advances_total`, `guardian_metadata_cache_requests_total`, `guardian_session_read_record_seconds`, `guardian_read_auth_lookup_seconds`, and pool gauges (`size`, `in_use`, `pending_acquires`, label `pool` in {`storage`, `metadata`}) in `crates/server/src/metrics/names.rs` per `contracts/observability.md`
  - _Status (2026-11-02): Done as implemented (see `contracts/observability.md`): pool acquire histogram, rate-limiter lock wait, `guardian_auth_rejections_total{credential, reason}`, metadata cache and lookup metrics. Read-record and fence counters were not built: duplicates and fenced reads show as auth rejections with reason `replay` or `stale`._
- [X] T009 [US6] Record auth rejections with bounded labels at every rejection point in `crates/server/src/services/mod.rs` (`resolve_account_with`, `verify_session_request`, `validate_request_timestamp`, `consume_auth_timestamp`); never label with account IDs, keys, signatures or payloads
- [X] T010 [US6] Add a tower layer on the gRPC service mapping tonic `Internal("Missing request message.")` to `InvalidArgument` in `crates/server/src/metrics/grpc.rs` and mount it in `crates/server/src/builder/handle.rs` near the `GrpcRequestTracker` layer (~line 279); pin the message string to tonic 0.14 with a comment-free test
- [X] T011 [P] [US6] Export pool size, in-use and pending-acquire gauges and an acquire-wait histogram for both deadpool pools in `crates/server/src/builder/storage.rs` and `crates/server/src/metrics/refresher.rs`
  - _Status (2026-10-30): Size/in-use/pending gauges already existed; added `guardian_db_pool_acquire_duration_seconds` via a timed checkout used by every production pool checkout._
- [X] T012 [P] [US6] Add a lock-wait histogram and rejection counter (`reason` = burst/sustained) to `RateLimitStore` in `crates/server/src/middleware/rate_limit.rs` (around the `entries.write()` at ~line 281)
- [ ] T013 [P] [US6] Export request histograms to CloudWatch with a bounded route/method dimension, health checks (`GET /`, `GetPubkey`) excluded, and distribution values for p50/p95/p99 in `infra/observability.tf` (EMF config ~lines 178-187); replace the averaged-seconds widget (~lines 369-378) with per-route percentile widgets in milliseconds
  - _Status (2026-10-30): PARTIAL: per-route (HTTP) and per-method (gRPC) average latency exported with health checks excluded, plus auth rejections and limiter wait. CloudWatch percentiles are NOT possible from Prometheus histograms over ADOT EMF (sum/count only); getting them needs summaries, which would break the Grafana `histogram_quantile` panels. Percentiles stay in Grafana and the benchmark report. Decision needed._
- [X] T014 [P] [US6] Add per-route p50/p95/p99, auth rejections by reason, pool wait, limiter wait and accepted-to-canonical panels to `docs/guides/observability/grafana/dashboards/guardian.json`
- [X] T015 [US6] Add `reader` and `writer` roles, `nonce_poll` and `full_get_state` read modes, poll interval, writer rate and account distribution (distinct, burst, hot-account) to `benchmarks/prod-server/src/config.rs` and `benchmarks/prod-server/src/workload.rs`
  - _Status (2026-10-30): Done except the hot-account leg: seeded accounts have one signer each, so several writers on one account need multi-signer seeding (follow-up)._
- [X] T016 [US6] Implement the role loops and sustained (fixed-duration) mode in `benchmarks/prod-server/src/runner.rs` and `benchmarks/prod-server/src/operations.rs` (add `GetCanonicalNonce`)
- [X] T017 [P] [US6] Parallel sharded seeding of 100,000 accounts in `benchmarks/prod-server/src/seed.rs` and `benchmarks/prod-server/src/distributed.rs`
  - _Status (2026-10-30): No change needed: seeding is already sharded per ECS worker and runs configure calls concurrently._
- [ ] T018 [US6] Report client latency, server span latency and dashboard latency side by side; first-attempt vs retried errors; auth rejections by reason; rate-limit rejections separately; DB reads/writes per request; accepted-to-canonical and chain-inclusion-to-canonical separately, in `benchmarks/prod-server/src/report.rs` and `benchmarks/prod-server/src/run.rs`
  - _Status (2026-10-30): PARTIAL: replay and rate-limit rejections are now separate failure categories. Still open: server-span and dashboard latency side by side, first-attempt vs SDK-retried errors, DB reads/writes per request, chain-inclusion-to-canonical._
- [X] T019 [US6] Complete `benchmarks/prod-server/profiles/target-317.toml` with both SC-003 legs (realistic mix and full `get_state`), burst and hot-account writer legs
- [ ] T020 [US6] Record RDS `max_connections` for devnet and prod; set `GUARDIAN` storage and metadata pool sizes so `max tasks x (storage + metadata) <= 80%` of it, in `infra/ecs.tf` env and `docs/CONFIGURATION.md`
  - _Status (2026-10-30): Skipped for now (RDS Proxy work deferred by request)._
- [ ] T021 [US6] Run the v1 baseline on a prod-like stack through RDS Proxy and again direct (`rds_proxy_route_database_url = false`); write `benchmarks/prod-server/results/2026-11-target-317-baseline.md` including limiter and pool wait at target rate (FR-028a)
  - _Status (2026-10-30): Not run: needs a prod-like AWS stack; proxy-vs-direct comparison deferred by request._
- [ ] T022 [US6] If T021 shows limiter p99 wait > 1 ms: replace the single `RwLock` with a sharded map keyed by hash of the limit key and remove per-request `String` key allocation in `crates/server/src/middleware/rate_limit.rs`; size benchmark and prod per-IP limits for 20,000 readers behind few IPs
- [ ] T023 [US6] Switch devnet then prod to the direct DB route after confirming security groups, in `infra/variables.tf`/stack tfvars (`rds_proxy_route_database_url = false`, `sslmode` kept per `infra/data.tf:161-194`); document in `docs/SERVER_AWS_DEPLOY.md`
  - _Status (2026-10-30): Skipped for now (RDS Proxy work deferred by request)._

**Checkpoint**: Every SC can be read from the dashboard and benchmark report; baseline recorded.

---

## Phase 3: Replay-Store Gate (blocks US1 and US3)

**Purpose**: Prove capacity and correctness of the duplicate record before any protocol or SDK work (research R4, R16, R17). **Hard stop at T033.**

- [X] T024 Record the agreed failure decision (option 1 with monthly cost ceiling, 2 or 3) and a gate deadline in `speckit/features/317-secure-scalability/research.md` R16 before running the experiment
  - _Status (2026-10-30): OPEN: needs your decision before T032._
  - _Decision (2026-11-02): `unique` for every new session, not configurable (the setting was removed; the filesystem backend is development only); `strict` remained only for sessions stored by earlier releases (removed later the same day: those sessions read under the record too). Based on the local sustained comparison (one database call per session read against three, 1.5 ms against 7.5 ms mean server time). The absolute capacity check moves to the separately planned deployed run._
  - _Status (2026-10-30): Moves to before choosing the default read policy after T099._
- [X] T025 Draft migration `crates/server/migrations/2026-11-xx-000001_session_read_seen/up.sql` and `down.sql`: partitioned `session_read_seen (record_key bytea, request_ts_ms bigint, PRIMARY KEY (record_key, request_ts_ms)) PARTITION BY RANGE (request_ts_ms)`, `replay_fence (fence text PRIMARY KEY, watermark_ms bigint NOT NULL)` seeded with `session_read` and `session_floor`, and partition-create helper function
  - _Status (2026-10-30): Done in `benchmarks/replay-store-gate/sql/replay_store.sql` instead of `crates/server/migrations/`, because embedded migrations would run on any server started from this branch. `sql/check.sql` exercises every outcome on Postgres 17._
- [X] T026 In the same migration, add `plpgsql` function `guardian_authorize_and_record_session_read(account_id, session_key, record_key, ts, skew_ms)`: account and session lookup with early return (no insert) when missing/expired/revoked; `pg_advisory_xact_lock_shared(K_READ)`; freshness via `clock_timestamp()` after the lock; fence `ts <= watermark`; `INSERT .. ON CONFLICT DO NOTHING`; returns `(auth_version, session row, outcome)`
- [X] T027 In the same migration, add `guardian_advance_replay_fence(fence, cutoff_ms)` taking the exclusive advisory lock, raising the watermark monotonically, and dropping read partitions wholly below it
- _Note (2026-11-02): the replay-store gate harness (T025 to T032) was a prototype and was moved out of this repository; the paths below are where it lived._
- [X] T028 Create standalone harness crate `benchmarks/replay-store-gate/` (`Cargo.toml`, `src/main.rs`, workspace member) that applies T025 to an empty database and drives 3 processes at 12,000 calls/s combined, 20,000 session keys over 100,000 accounts, 10% cross-process duplicates, 1 h with partition rollover and fence advances under load
  - _Status (2026-10-30): Done: `benchmarks/replay-store-gate/` (setup, run, verify, stall). Local smoke: 3 processes, 12,170 shared requests each recorded exactly once, no errors._
- [X] T029 Add to the harness: forced stalls past the window with and without an intervening purge, and a comparison mode running the current floor UPSERT plus separate lookups at the same rate, in `benchmarks/replay-store-gate/src/main.rs`
- [X] T030 [P] Extend `.claude/skills/verify-auth-replay/model/AuthReplay.tla` with a read-record action, split read/write policies, admission freshness (FR-011), purge fence (FR-011a) and filesystem restart plus re-registration as three separate properties; confirm disabling each check alone fails its property; mirror to `.agents/skills/verify-auth-replay/`
  - _Status (2026-10-30): Done as a separate design model `benchmarks/replay-store-gate/model/ReplayStore.tla` with `run.sh` and `expected.txt` (6 cells, all as expected). The skill model in `.claude/skills/verify-auth-replay/` mirrors server code, so it is updated when the code lands (T086)._
- [ ] T031 [P] Add a "unique" read-policy variant to `docs/architecture/scalability/tla/SessionReplayDesign.tla` and `docs/architecture/scalability/tla/README.md`
  - _Status (2026-10-30): Superseded by the T030 design model._
- [ ] T032 Run the harness on `db.r6g.large` and `db.r6g.xlarge` from 3 tasks with R17 pool sizes; write `benchmarks/replay-store-gate/results/2026-11-gate.md` with record p99, lost/doubled acceptances, rollover errors, DB CPU and WAL headroom, storage trend, pool pending acquires, and the comparison to the UPSERT
  - _Status (2026-10-30): Replaced (2026-10-30) by the local comparison in `benchmarks/replay-store-gate/results/2026-10-30-local-comparison.md`; the absolute RDS answer comes from T099 run once per read policy._
- [X] T033 Gate decision: pass (record p99 <= 10 ms, zero doubled/lost acceptances, no rollover errors, >= 30% CPU/WAL headroom, flat storage, model properties pass) on the smallest passing class; otherwise apply the T024 decision and update spec/plan before Phase 6
  - _Status (2026-10-30): Decided 2026-10-30: go, with the read duplicate record behind a `strict`/`unique` server setting so the end-to-end benchmark can compare both._

**Checkpoint**: Go/no-go recorded. US1 and US3 do not start without "go".

---

## Phase 4: User Story 4 - Reads are fast at target load without stale authorization (Priority: P2)

**Goal**: One read-auth lookup on the primary plus a version-validated metadata cache; works on today's v1 auth.

**Independent Test**: Two servers on one Postgres; signer removal and session revocation via one are refused on the other's next request; DB down fails closed; paused accounts stay readable (quickstart section 4).

### Tests for User Story 4

- [X] T034 [P] [US4] Ignored Postgres test with two `AppState`s sharing one database: signer removed through A is refused on B's next read (not a cosigner); revoked session refused on B, in `crates/server/src/auth_cache/mod.rs` test module
  - _Status (2026-10-30): Done at the store level: two `PostgresMetadataStore`s on one database; a signer change through one is served by the other's next read._
- [X] T035 [P] [US4] Tests: lookup error fails closed; paused account readable while mutations refused; concurrent misses for one account cause one load; cache stays within its weight bound, in `crates/server/src/auth_cache/mod.rs` test module
  - _Status (2026-10-30): Done: fails closed with the pool closed; pause behavior unchanged (reads never consulted pause). Single-flight comes from moka `try_get_with`; no dedicated concurrency test._
- [X] T036 [P] [US4] Test that `auth_version` changes on configure set/create, pause, unpause, release set/clear and does not change on `has_pending_candidate` flips or replay CAS, in `crates/server/src/metadata/postgres.rs` and `crates/server/src/metadata/filesystem.rs` test modules
  - _Status (2026-10-30): Done with a `BEFORE UPDATE` trigger, so every update (including has_pending_candidate flips and writes from replicas on the previous version during a rolling deploy) raises the version._

### Implementation for User Story 4

- [X] T037 [US4] Migration `crates/server/migrations/2026-11-xx-000002_account_auth_version/` adding `auth_version bigint NOT NULL DEFAULT 0` to `account_metadata`; update `crates/server/src/schema.rs`
  - _Status (2026-10-30): Migration `2026-10-30-000001_account_auth_version` (column + trigger); revert/re-apply exercised by the Postgres suite._
- [X] T038 [US4] Bump `auth_version` in every qualifying metadata write in `crates/server/src/metadata/postgres.rs` (`set`/upsert ~161-196, `create` ~212, `set_pause` ~508, `clear_pause` ~584, `set_released_if_state` ~632, `clear_released_if_state` ~693) and the filesystem equivalents in `crates/server/src/metadata/filesystem.rs`; add `auth_version` to `AccountMetadata` in `crates/server/src/metadata/mod.rs`
  - _Status (2026-10-30): Superseded by the trigger in T037: no application write path needs to remember to bump._
- [X] T039 [US4] Add `read_auth_lookup(account_id, Option<session_key>) -> ReadAuthLookup` to the metadata store trait in `crates/server/src/metadata/mod.rs` with a single joined query in `crates/server/src/metadata/postgres.rs` and an equivalent in `crates/server/src/metadata/filesystem.rs` composed with the in-memory session store
  - _Status (2026-10-30): Done as `MetadataStore::get_for_read` plus a version lookup; joining the session row into the same query is deferred to the combined call in US1 (it must run after signature verification)._
- [X] T040 [US4] Implement `ReadAuthorizer` with a weight-bounded `moka::future::Cache<AccountId, (u64, Arc<AccountMetadata>)>` (default 256 MB, `get_with` single flight), reload on version mismatch, in `crates/server/src/auth_cache/mod.rs`; add `GUARDIAN_METADATA_CACHE_MAX_BYTES` and document it in `docs/CONFIGURATION.md`
  - _Status (2026-10-30): Done inside `PostgresMetadataStore` (moka, weight-bounded, `GUARDIAN_METADATA_CACHE_MAX_BYTES`, default 256 MiB); filesystem `get_for_read` is a plain `get`._
- [X] T041 [US4] Route the read services through `ReadAuthorizer` in `crates/server/src/services/mod.rs` (`resolve_account_allowing_session` for read operations only); keep all write services and canonicalization on `MetadataStore`; preserve error distinctions (expired, revoked, not a cosigner, Guardian/network mismatch)
  - _Status (2026-10-30): Done: `resolve_account_for_read` for get_state, get_canonical_nonce, get_delta, get_delta_since, get_delta_history, get_delta_proposals, get_delta_proposal and both execution-status reads; writes keep `resolve_account_allowing_session`._
- [ ] T042 [US4] Add `VerifyPool` (cores-sized semaphore plus `spawn_blocking`) for wallet and session signature verification in `crates/server/src/auth_verify.rs`, used from `crates/server/src/services/mod.rs` (~lines 206, 262); keep only if the US6 benchmark shows a p99 improvement, otherwise drop the task's code
  - _Status (2026-10-30): Deferred: keep only with benchmark evidence (needs the phase 1 baseline run)._
- [X] T043 [US4] Record cache hit/reload/miss and lookup latency metrics in `crates/server/src/auth_cache/mod.rs`

**Checkpoint**: US4 independently testable on v1 auth.

---

## Phase 5: User Story 7 - Background jobs complete at 100,000 accounts (Priority: P3)

**Goal**: Sweep, metrics refresher and dashboard stats stay within their schedules at 100,000 accounts.

**Independent Test**: Seed 100,000 accounts locally; measure each job's duration and concurrent read p95 (quickstart section 6).

### Tests for User Story 7

- [X] T044 [P] [US7] Test effective sweep rate `max(configured, ceil(accounts / (rotation / 2)))` in `crates/server/src/jobs/release_sweep/sweep.rs` test module
  - _Status (2026-10-30): Done as a headroom check: the sweep spreads each walk over the whole rotation by design, so the fix is a rate cap with 2x headroom (default 5 -> 10, about 216,000 accounts per 6 h) plus a warning at rotation start, not a faster walk._
- [X] T045 [P] [US7] Test dashboard stats fetch full state only for accounts whose commitment changed since the previous payload, in `crates/server/src/dashboard/stats.rs` test module

### Implementation for User Story 7

- [X] T046 [P] [US7] Adaptive release-sweep rate using `count_release_sweep_accounts` in `crates/server/src/jobs/release_sweep/sweep.rs` and `crates/server/src/builder/release_sweep.rs`; document in `docs/CONFIGURATION.md`
  - _Status (2026-10-30): Done as above; FR-031 reworded accordingly._
- [X] T047 [P] [US7] Replace `metadata.list().len()` with a count query (new `MetadataStore::count`) in `crates/server/src/metrics/refresher.rs`, `crates/server/src/metadata/mod.rs`, `crates/server/src/metadata/postgres.rs`, `crates/server/src/metadata/filesystem.rs`
  - _Status (2026-10-30): `list()` returned ids only, so the gain is smaller than estimated; added `MetadataStore::count` (required method, no default)._
- [X] T048 [US7] In `build_payload` (`crates/server/src/dashboard/stats.rs` ~845-930) read commitments first (state-head query like `pull_state_head` in `crates/server/src/storage/postgres.rs` ~1321) and fetch/decrypt full state only for changed accounts; use SQL aggregates for column-backed counts; keep vault values from decoded summaries
  - _Status (2026-10-30): Done with a batched `pull_state_heads_batch` (Postgres one query; forwarded explicitly by the metrics and encryption wrappers). Column-backed SQL aggregates were not needed for this change._
- [ ] T049 [US7] Measure the three jobs at 100,000 accounts and record results in `benchmarks/prod-server/results/2026-11-target-317-jobs.md`

**Checkpoint**: US7 independently testable.

---

## Phase 6: User Story 2 - Operation-bound session requests (Priority: P1, breaking)

**Goal**: `session-v2` envelope with transport and operation, registry of 13 operations, typed cutover error, policy advertisement, Rust/TS parity.

**Independent Test**: Shared vectors verify only for their own `(transport, operation)` in Rust, TS and the server; `session` format returns `unsupported_auth_version` (quickstart section 1).

**Depends on**: T033 = go (decided 2026-10-30).

### Tests for User Story 2

- [X] T050 [P] [US2] Create `crates/shared/tests/session_request_vectors.rs` with regeneration under `GUARDIAN_REGEN_SESSION_FIXTURES=1` following `crates/shared/tests/session_grant_vectors.rs`, producing `crates/shared/tests/fixtures/session_request_vectors.json` (all 13 operations x 2 transports, same-payload pairs, synthetic same-payload write pair, high-S variant, single-field mutations; `envelope_hex`, `signed_digest_hex`, `record_key_hex`)
- [X] T051 [P] [US2] TS vector test reading the fixture in `packages/miden-multisig-client/tests/session-request-vectors.test.ts` (envelope bytes, record key, verification only for the own pair, via `@noble/curves`)
- [X] T052 [P] [US2] TS encoder unit tests in `packages/guardian-client/src/session-request.test.ts`
- [X] T053 [P] [US2] Server tests: v2 accepted per operation on HTTP and gRPC; cross-operation and cross-transport reuse fails `authentication_failed`; `x-auth-format: session` returns `unsupported_auth_version` (HTTP 400, gRPC `INVALID_ARGUMENT`, not retryable); v2 on wallet-only routes returns `wallet_signature_required`; logout still accepts its own format, in `crates/server/src/metadata/auth/credentials.rs` and `crates/server/src/services/mod.rs` test modules
  - _Status (2026-10-30): Done in `testing/integration/session_http.rs` and `session_grpc.rs` (operation and transport binding, version 1 refused as `unsupported_auth_version`). Wallet-only routes keep checking a session credential over the bare request word, so unauthenticated callers still learn nothing about route policy and ended sessions are still reported as ended._
- [X] T054 [P] [US2] Rust client tests: policy fields validated at `start_session`; missing/zero/unknown version is a terminal compatibility error; `unsupported_auth_version` keeps the grant and never falls back to wallet signing, in `crates/client/src/client.rs` test module
- [X] T055 [P] [US2] TS client tests for the same three behaviors in `packages/guardian-client/src/http.test.ts`

### Implementation for User Story 2

- [X] T056 [US2] Implement `SessionOperation` (u16 IDs 1-13 with `ReplayPolicy`), `Transport`, and `SessionRequestEnvelope { to_bytes, signed_digest, record_key }` in `crates/shared/src/session_request.rs`; export from `crates/shared/src/lib.rs`
- [X] T057 [US2] Add byte-level `sign_bytes`/`verify_bytes` beside the word API in `crates/shared/src/session_key.rs` (grant and logout signing unchanged)
- [X] T058 [US2] Add `RequestAuthFormat::SessionV2`, transport capture in both extractors, and map `session` to `GuardianError::UnsupportedAuthVersion` on per-account routes in `crates/server/src/metadata/auth/credentials.rs` (~11-25, 159-230)
  - _Status (2026-10-30): Done: `session-v2` parses to `Session`, the old `session` to a new `LegacySession` that every per-account path refuses; `Credentials` carries the transport set by the HTTP or gRPC extractor. Logout ignores the format and is unchanged._
- [X] T059 [US2] Add `GuardianError::UnsupportedAuthVersion` with `http_status` 400, `grpc_status` `InvalidArgument`, `code` `unsupported_auth_version`, `user_message`, `Display`, `retryable = false`, meta `{supported: [2]}` in `crates/server/src/error.rs`
  - _Status (2026-10-30): Done without `meta.supported`: the standard `meta` (retryable false) is enough and avoids changing the shared error meta shape._
- [X] T060 [US2] Change `resolve_account_allowing_session` to take `SessionOperation` and verify the v2 envelope in `verify_session_request`/`authenticate_delegated_signer` in `crates/server/src/services/mod.rs` (~154-335); `resolve_account` (wallet-only) refuses v2 by type
- [X] T061 [US2] Pass the operation from every session-eligible service: `crates/server/src/services/get_state.rs`, `get_canonical_nonce.rs`, `get_delta.rs`, `get_delta_since.rs`, `get_delta_history.rs`, `get_delta_proposals.rs`, `get_delta_proposal.rs`, `push_delta.rs`, `push_delta_proposal.rs`, `sign_delta_proposal.rs`, `execute_proposal/service.rs` (~133), `execution_status.rs` (~9, ~35)
- [X] T062 [US2] Add `request_auth_version`, `read_replay_policy`, `read_timestamp_skew_ms` to `CreateSessionResponse` fields 3-5 in `crates/server/proto/guardian.proto`, HTTP `CreateSessionResponse` in `crates/server/src/api/http.rs` (~500), `CreateSessionResult` in `crates/server/src/services/create_session.rs`, gRPC mapping in `crates/server/src/api/grpc.rs`, and `SessionsStatus` in `crates/server/src/services/status.rs` (~61) and `api/http.rs` (~690)
  - _Status (2026-10-30): Done; the read policy is reported as `strict` until US1 ships the read record._
- [X] T063 [US2] Update `utoipa` annotations and the `x-auth-format` description in `crates/server/src/openapi.rs` (~449); regenerate with `cargo run --features evm --bin gen-openapi -- docs`
- [X] T064 [P] [US2] Rust client: sign v2 for session requests with `Transport::Grpc` and the call's operation in `add_auth_metadata` (`crates/client/src/client.rs` ~194-235); validate advertised policy in `start_session` (~363); add `ClientError::UnsupportedAuthVersion` in `crates/client/src/error.rs` and classify it as terminal (not `is_rejected_session`)
  - _Status (2026-10-30): Done; `ClientError::UnsupportedAuthVersion` added, server `unsupported_auth_version` is terminal and keeps the grant._
- [X] T065 [P] [US2] TS client: `SessionOperation`, `SessionTransport` and envelope encoder in `packages/guardian-client/src/session-request.ts`; export from `packages/guardian-client/src/index.ts`
- [X] T066 [US2] TS client: `signRequest(operation, accountId, timestamp, payload)` in `packages/guardian-client/src/types.ts` (~100-119); `x-auth-format: session-v2` and per-route operation in `packages/guardian-client/src/http.ts` (`authHeaders` ~296, `fetchSigned` ~893); policy fields in `packages/guardian-client/src/server-types.ts` (~228) and `fromServerCreateSessionResponse` (`http.ts` ~391); `unsupported_auth_version` in `packages/guardian-client/src/error-codes.ts`
  - _Status (2026-10-30): Done; `createSession` throws `UnsupportedAuthVersionError` on a server without version 2 or with an unknown read policy._
- [X] T067 [US2] TS multisig: `GuardianSession.signRequest` signs envelope bytes with WebCrypto `{name: 'ECDSA', hash: 'SHA-256'}` in `packages/miden-multisig-client/src/session/key.ts` (~42-98); adjust `packages/miden-multisig-client/src/session/manager.ts` and `packages/miden-multisig-client/src/utils/digest.ts` as needed
- [X] T068 [US2] Rust multisig: confirm per-operation clients built in `crates/miden-multisig-client/src/client/helpers.rs` (~52-57) pick up v2 signing through the shared `SessionSlot`; re-export the new error variant

**Checkpoint**: US2 testable with strict replay checks still in place.

---

## Phase 7: User Story 1 - Parallel session reads never fail replay checks (Priority: P1)

**Goal**: Session reads reject exact duplicates fleet-wide instead of using the timestamp floor.

**Independent Test**: 50 parallel identical-timestamp distinct reads succeed across two replicas; resends and high-S variants are rejected; reads delayed past the window are rejected with and without a purge (quickstart sections 0, 2, 3).

**Depends on**: US2 (envelope), T033 = go, US4 lookup (T039) for the combined call.

### Tests for User Story 1

- [X] T069 [P] [US1] Ignored Postgres tests: 50 parallel distinct reads with equal timestamps accepted; resend (both S forms, other `AppState`) rejected; stalled request rejected without purge (freshness) and across a purge (fence); partition missing fails closed, in `crates/server/src/replay/postgres.rs` test module
  - _Status (2026-10-30): Done in `coordination/postgres/read_records.rs` (one request admitted once across two store instances, freshness by the database clock, fence after a purge, missing partition fails closed) and the HTTP integration test under `unique`._
- [X] T070 [P] [US1] Filesystem tests: same cases plus restart with a request timestamped 2 min ahead, grant re-registered, resend rejected, in `crates/server/src/replay/filesystem.rs` test module
- [X] T071 [P] [US1] SDK tests for FR-015a: identical parallel reads get distinct timestamps; different accounts do not advance each other's counter; counter never exceeds local clock + 1,000 ms (waits instead), in `crates/client/src/session_sequencer.rs` test module and `packages/guardian-client/src/write-sequencer.test.ts`

### Implementation for User Story 1

- [X] T072 [US1] Finalize the T025-T027 migration as production migration `crates/server/migrations/2026-11-xx-000003_session_read_seen/` (rename from draft, keep SQL identical to the gated version) and add tables to `crates/server/src/schema.rs`
  - _Status (2026-10-30): Migration `2026-10-30-000002_session_read_records` (read record and fence only; the fenced write floor comes with US3)._
- [X] T073 [US1] Define `ReadRecordStore` trait (`authorize_and_record`, `advance_fence`, `ensure_partitions`) and outcome enum in `crates/server/src/replay/mod.rs`
  - _Status (2026-10-30): Done as `coordination::read_records::ReadRecordStore` (`record`, `maintain`), carried by `CoordinationHandles` and `MidenSessions`._
- [X] T074 [US1] Postgres implementation calling `guardian_authorize_and_record_session_read` in one round trip in `crates/server/src/replay/postgres.rs`
  - _Status (2026-10-30): Done as a record-only call (`guardian_record_session_read`): the session and metadata lookups stay where they were. Folding them into one database call (the combined function the local comparison measured) is a follow-up optimization._
  - _Status (2026-10-30, later): combined call done: `guardian_admit_session_read` (migration `2026-10-30-000004_admit_session_read`) returns the metadata version, the session and the record outcome in one round trip; `ReadRecordStore::admit_session_read` (Postgres one call, local stores lookups then record); metadata served at that version via `MetadataStore::get_for_read_at`. A session read under `unique` is now two round trips: this call and the state or state-head read._
  - _Status (2026-11-02): a nonce poll is now one round trip: with `p_with_head` the admission also returns the canonical state head for a recorded read (`PrefetchedHead`, used by `get_canonical_nonce`); a full `get_state` stays at two. Pools reuse connections without the `SELECT 1` test query (`RecyclingMethod::Fast`). SC-006 (two round trips) is met, and exceeded for nonce polls. Measured in `benchmarks/prod-server/results/2026-11-02-local-317-ramp.md`._
- [X] T075 [US1] Filesystem implementation with durable per-5-minute bucket files under `.metadata/session_read_seen/`, persisted watermark in `.metadata/replay_fence.json`, freshness and fence under one lock, in `crates/server/src/replay/filesystem.rs`
  - _Status (2026-10-30): Done as `FileReadRecords`: one append-only file per 5-minute bucket, flushed before admission, fence in `fence`, loaded on start._
- [X] T076 [US1] Use `ReadRecordStore::authorize_and_record` for read-policy operations in `crates/server/src/services/mod.rs` (replacing `consume_auth_timestamp` for session reads); then grant Guardian-key, network and cosigner checks against `ReadAuthorizer` metadata; map `duplicate`/`stale` to `AuthenticationReplay`
  - _Status (2026-11-02): Done. Every new session reads under `unique` (not configurable; the setting was removed); sessions stored by earlier releases keep `strict`; writes always keep the floor._
  - _Status (2026-11-02, later): the `strict` carry-over is removed: the per-session policy field, the `Strict` admission outcome, the floor fallback for session reads and the SQL `strict` branch are gone. Sessions stored by earlier releases keep working (FR-007): their reads are admitted by the record like any other session's, so no migration revokes them (a revoke migration was tried and dropped after review). The auth-replay model drops `ReadPolicies` and `BindPolicy` and the policy-change cells._
  - _Status (2026-11-02, later): `read_replay_policy` and `read_timestamp_skew_ms` removed from the wire (`CreateSessionResponse` in proto and HTTP, `/status` `sessions`) and from both SDKs: with one fixed policy they carried nothing `request_auth_version: 2` does not, and SDKs only validated them. Unreleased, so no compatibility cost._
  - _Status (2026-11-03): second review round: a missing `replay_fence` row now fails closed in both admission functions (`INTO STRICT`, tested); `spec/api.md` Replay Protection now scopes the floor to wallet requests and session writes; vector tests cover every single-field mutation and both signature s-forms. auth-replay deep tier passes (session-reads-writes 323M states, session-reads 87M), now over an hour._
  - _Status (2026-11-03): pre-PR checks: `cargo test --workspace` (2,278 passed), all example typechecks and builds, and the live qualification scenario `live-session-execute-2of3-ecdsa` on devnet against a local Postgres-backed server from this tree passed for both SDKs (Rust over gRPC, TypeScript over HTTP; zero auth rejections, delta canonical). The other TLA+ skills' drift and the git index are still to align._
- [X] T077 [US1] Extend the session sweep worker in `crates/server/src/builder/handle.rs` (~733-768): create read partitions 1 h ahead idempotently on every replica, advance the `session_read` fence to a partition boundary at or below `db_now - skew - margin`, drop partitions below it; emit `guardian_replay_fence_advances_total`
- [X] T078 [US1] Add an alarm when the partition horizon is under 30 minutes in `infra/alerting.tf`
  - _Status (2026-11-02): Done as `guardian_session_read_maintenance_timestamp_seconds` (set on each successful maintenance) and the `<stack>-session-read-maintenance-stale` alarm in `infra/observability.tf`: no replica maintained the records for 10 minutes, about 50 minutes before reads would fail (partitions run an hour ahead). Grafana panel and TROUBLESHOOTING entry added._
- [X] T079 [US1] Per-(session, account) timestamp source with 1 s drift guard: Rust `crates/client/src/session_sequencer.rs` owned by `ActiveSession` in `SessionSlot` (`crates/client/src/client.rs` ~91, ~160); TS `packages/guardian-client/src/write-sequencer.ts` exposed via `SessionRequestSigner.nextTimestamp(accountId)` and owned by `GuardianSession` in `packages/miden-multisig-client/src/session/key.ts`
  - _Status (2026-10-30): Done in both SDKs: Rust `SessionTimestamps` in `ActiveSession` (shared through `SessionSlot`), TS `SessionTimestamps` owned by `GuardianSession` behind the new required `SessionRequestSigner.nextTimestamp(accountId)`._
- [X] T080 [US1] Report the read policy as `"unique"` in the session creation and status fields added in T062

**Checkpoint**: US1 independently testable; zero ordering-related read rejections.

---

## Phase 8: User Story 3 - Session writes stay strictly replay-protected without spurious failures (Priority: P1)

**Goal**: Strict write floor with admission freshness and purge fence (F2 closed); SDK write ordering.

**Independent Test**: Concurrent writes from one client produce zero replay rejections; resent writes are rejected before any business effect on any replica; reads never touch the write floor (quickstart sections 2, 5).

**Depends on**: US2, T033 = go.

### Tests for User Story 3

- [X] T081 [P] [US3] Server tests: resent write rejected before effects across two `AppState`s; stalled write rejected without purge and across a floor purge; reads do not read or advance the write floor and writes do not touch the read record, in `crates/server/src/metadata/postgres.rs` and `crates/server/src/metadata/filesystem.rs` test modules
  - _Status (2026-10-30): Done: filesystem (delayed write refused without a purge, purged write refused, fence survives restart) and Postgres (also a resend through a second store instance). Reads never touch the write floor under `unique` by construction (separate record)._
- [X] T082 [P] [US3] SDK tests: parallel writes for one account serialize with strictly increasing timestamps and zero replay rejections; order held across bounded retries; unrelated accounts and reads not serialized; ambiguous timeout not re-signed, in `crates/client/src/session_sequencer.rs` test module and `packages/guardian-client/src/write-sequencer.test.ts`

### Implementation for User Story 3

- [X] T083 [US3] Fenced CAS: run `update_last_auth_timestamp_cas` for `session-` keys under `pg_advisory_xact_lock_shared(K_FLOOR)` with freshness on `clock_timestamp()` after the lock and `ts > session_floor watermark`, in `crates/server/src/metadata/postgres.rs` (~359-393) (SQL function in the T072 migration)
  - _Status (2026-10-30): Done as a separate required method `advance_session_floor` (Postgres `guardian_advance_session_floor`, migration `2026-10-30-000003_session_floor_fence`); wallet floors keep the plain CAS since they are never purged._
- [X] T084 [US3] Purge: advance the `session_floor` fence under the exclusive lock before deleting in `purge_session_floors` (`crates/server/src/metadata/postgres.rs` ~396-412) and the filesystem equivalent (`crates/server/src/metadata/filesystem.rs` ~447-500, one clock read inside the `auth_state` lock)
  - _Status (2026-10-30): Done: Postgres `guardian_purge_session_floors`, filesystem fence in `.metadata/session_floor_fence`, both raised under the same lock as the delete._
- [X] T085 [US3] `WriteSequencer` holding a per-account async lock from timestamp mint through the last bounded retry: Rust in `crates/client/src/session_sequencer.rs` wired into `send_with_replay_retry_as` (`crates/client/src/client.rs` ~316-354) for write operations; TS in `packages/guardian-client/src/write-sequencer.ts` wired into `fetchSigned` (`packages/guardian-client/src/http.ts` ~893-946), minting the timestamp inside the lock
  - _Status (2026-10-30): Done: Rust per-account `tokio::sync::Mutex` in `ActiveSession` (shared through `SessionSlot`), TS `WriteSequencer` owned by `GuardianSession` (new required `SessionRequestSigner.writeSequencer`); session writes mint their timestamp inside the turn._
- [X] T086 [US3] Update `.claude/skills/verify-auth-replay/mirrored.txt`, `fingerprints.txt`, `expected.txt` and `SKILL.md` action map to the real code; run `check.sh --deep` with every cell passing, including the former F2 cell; mirror to `.agents/skills/verify-auth-replay/`
  - _Status (2026-10-30): Pending with US3: the auth-replay model must gain the `unique` read branch (record instead of floor, sweep maintenance). Its fingerprints are deliberately left drifted until then._
  - _Status (2026-10-30): Done: model gains `FloorChecks`, the floor fence, `ReadPolicy` with the `unique` read branch and read purge. `session-unbounded` now PASS (F2 closed), new `session-unbounded-unfenced` FAIL reproduces F2, `session-reads-strict` and `session-reads-unique` PASS (deep tier). Fingerprints re-recorded (23)._

**Checkpoint**: US3 independently testable; F2 closed for reads and writes.

---

## Phase 9: User Story 5 - Canonicalization keeps up with 100 concurrent writers (Priority: P2)

**Goal**: CPU-aware rebuild admission and a documented leader size.

**Independent Test**: 100 concurrent candidates on 4 and 8 vCPU leaders; accepted-to-canonical p95 and foreground `push_delta` p95 measured; models pass.

### Tests for User Story 5

- [X] T087 [P] [US5] Test effective concurrency `min(admission, cores/2)` and that admission above the CPU limit is clamped, in `crates/server/src/network/mod.rs` test module

### Implementation for User Story 5

- [X] T088 [US5] Add `GUARDIAN_CANONICALIZATION_REBUILD_ADMISSION` (default `auto`, see T090) in `crates/server/src/builder/canonicalization.rs` and use it for the background admission gate in `crates/server/src/network/mod.rs` (~90-120); keep the CPU gate host-derived; document in `docs/CONFIGURATION.md`
  - _Status (2026-10-30): Done: `GUARDIAN_CANONICALIZATION_REBUILD_ADMISSION` (1 to 64, invalid stops startup), applied once at worker start via `Reconstructor::allow_background_admission`, capped at the CPU gate, never lowered._
- [X] T089 [US5] Run `.claude/skills/verify-candidate-queue/check.sh` and `.claude/skills/verify-worker-lease/check.sh`; update models only if the change is mirrored code
  - _Status (2026-10-30): candidate-queue (4 cells), worker-lease (1) and auth-replay (6) all match expected; drift was the timed pool checkout and the read-copy metadata option, reviewed and re-recorded._
  - _Status (2026-10-30): Re-run Oct 30 after US1 to US3: candidate-queue (4 cells) and worker-lease (1) as expected; drift was the `PushDelta` operation argument and the cache/read-record wiring in `build`, re-recorded._
- [X] T090 [US5] Measure admission 1, 2, `cores/2` on 4 and 8 vCPU leaders with 100 concurrent candidates and foreground push p95; record in `benchmarks/prod-server/results/2026-11-target-317-canonicalization.md`; document the required leader size in `docs/SERVER_AWS_DEPLOY.md`; change the default only with evidence
  - _Status (2026-10-30): Done as a local scheduling measurement (`benchmarks/prod-server/results/2026-10-30-rebuild-admission-local.md`, ignored test `rebuild_admission_scheduling_report`): drain time scales with admission, pushes never wait while one slot stays free. Default changed to `auto` (every slot but one: 1 on 2 and 4 vCPU, unchanged; 3 on 8 vCPU); leader-size table in `docs/SERVER_AWS_DEPLOY.md`. Real rebuild times still come from the end-to-end benchmark._

**Checkpoint**: US5 independently testable.

---

## Phase 10: Polish, Docs, Release and Qualification

- [X] T091 [P] Update `spec/api.md` (session-v2, operation registry, policy fields, `unsupported_auth_version`) and `speckit/features/011-miden-account-sessions/spec.md` (pointer to v2)
  - _Status (2026-10-30): Partly done with US2: `spec/api.md` (envelope, operation ids, policy fields, error code), `docs/architecture/services.md`._
  - _Status (2026-11-05): Done: `spec/api.md` carries the nonce poll window exception; `speckit/features/011-miden-account-sessions/spec.md` points to session-v2._
- [X] T092 [P] Update `docs/MULTISIG_SDK.md`, `docs/TROUBLESHOOTING.md` (new code, `stale`/duplicate replay causes), `docs/CONFIGURATION.md` (all new env vars)
  - _Status (2026-10-30): Partly done with US2: `docs/MULTISIG_SDK.md`, `docs/TROUBLESHOOTING.md`._
  - _Status (2026-11-05): Done: every new server env var is in `docs/CONFIGURATION.md` (`GUARDIAN_SESSION_READ_REPLAY_POLICY` was removed before release); `docs/TROUBLESHOOTING.md` covers `authentication_replay`, `timestamp_out_of_window` and the nonce poll._
- [x] T093 [P] Update READMEs for `crates/shared`, `crates/client`, `crates/miden-multisig-client`, `packages/guardian-client`, `packages/miden-multisig-client` (signer interface break, sequencer, policy fields); no relative links in npm READMEs
  - _Status (2026-10-30): Partly done with US2: `crates/shared/README.md`, `packages/guardian-client/README.md`._
- [X] T094 [P] Add the cutover to `docs/MIDEN_COMPATIBILITY.md`: old browser SDKs lose session requests until upgraded; homogeneous blue/green server cutover; rollback fails explicitly; phases 1 and 3 ship independently
  - _Status (2026-10-30): Partly done with US2: cutover section in `docs/MIDEN_COMPATIBILITY.md` (under the after-0.18.3 heading, like its neighbours)._
  - _Status (2026-11-05): Done: rollback failing explicitly and the nonce poll exception added. "Phases 1 and 3 ship independently" no longer applies: everything ships together in 0.19.0._
- [X] T095 [P] Add a blue/green cutover runbook section to `docs/SERVER_AWS_DEPLOY.md`
  - _Status (2026-11-05): Done as "Cutting Over To Session-v2 (0.19.0)". The stack has no blue/green controller (ECS rolling, minimum healthy 100%), so the section bounds the overlap instead: SDKs first, wait for `rolloutState` `COMPLETED`, check `/status`, watch the rejection and session-read metrics, explicit rollback._
- [ ] T096 Manual smoke with sessions on `examples/demo`, `examples/web`, `examples/smoke-web` (create, parallel multi-account sync, propose, sign, execute, revoke; Falcon and ECDSA grant signers) via the `smoke-test-rust-multisig-sdk` and `smoke-test-ts-multisig-sdk` skills
- [ ] T097 Run the qualification suite (deterministic and live) against devnet via the `run-guardian-qualification` skill
- [ ] T098 Security review of the v2 envelope, registry, duplicate record, fence and freshness; record outcome in `docs/architecture/scalability/session-auth-rfc.md`
  - _Status (2026-10-30): Review fixes applied: (1) a session keeps the read replay policy in force when it was created, on every replica, so a policy change (or a mixed fleet) never admits a read under both checks; modeled as F3 in verify-auth-replay (`policy-change` passes, `policy-change-unbound` reproduces). (2) Both SDKs only send a session write signed by the session whose write turn it holds; a write queued on a replaced session moves to the new session's turn. (3) Filesystem and in-memory read records and the filesystem session floor read the clock after taking their lock._
  - _Status (2026-11-05): Internal reviews only: three fresh reviewers on 2026-11-02 found no replay or bypass (fixes applied), and an independent review on 2026-11-05 accepted the window-only nonce poll on two conditions: a 30 s future bound (applied) and a duplicate-poll counter (not adopted, see `decision-nonce-read-window.md`). The external review and its record in `session-auth-rfc.md` are still open._
- [ ] T099 Full sustained benchmark, three legs (v1 strict baseline build, v2 without record via test flag, v2 with record), both SC-003 legs, 2+ replicas, direct DB; publish `benchmarks/prod-server/results/2026-11-target-317-final.md` with every SC-001 to SC-009 result
  - _Status (2026-10-30): Harness fixed after review: profiles now name a `[credential]` (wallet or session; session runs start sessions before warmup and the report records the credential), and `cosigners_per_writer` seeds readers as cosigners of writer accounts so nonce polls see new state. `target-317-mix` (session) and `target-317-mix-wallet` are the same load under both credentials. Session runs report `wallet_signed_operations` (requests signed by the wallet after a user lost its session; non-zero means not a clean session run). Local A/B run 2026-11-02 (`benchmarks/prod-server/results/2026-11-02-local-317-sustained.md`): 2,020 users, main vs 317 wallet vs session strict vs session unique; `unique` is the cheapest (1.5 ms mean server time per nonce poll, one database call) and `strict` the most expensive (7.5 ms, three calls); wallet unchanged. The deployed run at the full target is planned separately._
  - _Status (2026-11-02, later): Docker-capped local ramp (`benchmarks/prod-server/results/2026-11-02-local-317-ramp.md`, 1 and 2 servers at 2 CPUs each, Postgres at 2 CPUs): Postgres saturates first. After the pool and nonce-head fixes, about 0.3 ms of Postgres CPU per poll (0.4 ms before) and about 5,000 polls/s per 2 Postgres CPUs (about 4,000 before); a 64-connection pool was worse than 32. The remaining cost is the read-record write and its commit; slimming the SQL function gave nothing. 10,000 polls/s needs about 3 Postgres cores; the deployed run sizes RDS._
  - _Status (2026-11-04): Deployed devnet ramp (`benchmarks/prod-server/results/2026-11-04-devnet-317-bench.md`): one m7g.large task saturates at about 4,100 session reads/s, bound by WAL writes from the per-read record commit. That led to the window-only nonce poll proposal (`decision-nonce-read-window.md`); its deployed A/B is next. The full three-leg run is still open._
  - _Status (2026-11-05): Window-only nonce poll on AWS (results doc, "Deployed runs of the window-only nonce poll"): `db.m7g.large` 5,000/s p95 7 ms, 7,500/s p95 91 ms, saturated near 10,000/s; `db.m7g.xlarge` 10,000/s at 51% DB CPU, but one of 4 server tasks saturated (per-worker nonce p95 75 ms to 1,356 ms), so not qualified for latency. The harness averages per-worker percentiles; fleet percentiles need merged histograms. Sizing guidance added to `docs/SERVER_AWS_DEPLOY.md`. Still open: an evenly spread r5 and the sustained 10,000/s run, the full `get_state` leg, canonicalization on, 100,000 accounts._
- [ ] T100 Run `cargo test --workspace`, `npm test -w @openzeppelin/guardian-client`, `npm test -w @openzeppelin/miden-multisig-client`, OpenAPI drift check, and the TLA+ skills listed in quickstart section 3
  - _Status (2026-10-30): Model policy (2026-10-30, per user): keep the models this feature depends on current while implementing (auth-replay now; candidate-queue and worker-lease with US5) and re-record the others once at the end. Pending at that point: candidate-queue, guardian-execution, multisig-push-gate, guardian-switch, proposal-lifecycle, client-sync (drift from the timed pool checkout and the operation argument, mechanical so far)._

---

## Dependencies & Execution Order

### Phase dependencies

```text
Setup (1)
  -> US6 measurement (2) ------------------------------+
  -> Replay gate (3) [needs US6 T020 pool sizing] --+  |
  -> US4 read path (4)      (parallel with 3)       |  |
  -> US7 jobs (5)           (parallel with 3, 4)    |  |
                                                    v  |
                     US2 envelope (6) [needs T033 = go]
                       -> US1 reads (7) [needs US2, T039]
                       -> US3 writes (8) [needs US2]
  -> US5 canonicalization (9) (any time after Setup; final numbers need US6)
  -> Polish and qualification (10) [needs all]
```

### Story independence

- US6, US4, US7, US5 do not depend on the protocol change and can ship in any release.
- US2 is the breaking change; US1 and US3 build on it. US1 and US3 are independent of each other after US2, except T072 (shared migration) which US3's T083 extends.

### Within each story

Tests first and failing, then storage/models, then services, then transports and SDKs.

## Parallel Opportunities

- **US6**: T005, T006, T007 together; then T008, T011, T012, T013, T014, T017 together.
- **Gate**: T030 and T031 (models) in parallel with T028-T029 (harness).
- **Phases 3, 4, 5** run side by side after US6's T020.
- **US2**: T050-T055 together; T064 (Rust) and T065 (TS) in parallel after T056-T057.
- **US1/US3**: test tasks T069-T071 and T081-T082 in parallel.
- **Polish**: T091-T095 together.

### Parallel example: User Story 2

```text
T050 Rust vector fixture        | T052 TS encoder tests
T053 server auth tests          | T054 Rust client tests | T055 TS client tests
then: T064 Rust client signing  | T065 TS encoder
```

## Implementation Strategy

### MVP

US6 (measurement plus direct DB route) is the first shippable increment: it alone removes about 13 ms per request and makes every target measurable. US4 is the second, still on v1 auth.

### Incremental delivery

1. Setup, US6: baseline and direct DB.
2. Replay gate in parallel with US4 and US7; stop at T033.
3. On "go": US2, then US1 and US3, shipped together in the 0.19.0 rc.
4. US5 and the final benchmark before claiming #317.

### Open decisions carried into tasks

- T024: failure decision and deadline for the gate.
- Thresholds proposed but not yet confirmed: one push per 10 s per writer; 4 vCPU leader threshold; 20% push slowdown limit; gate p99 <= 10 ms with 30% headroom; pool wait p99 <= 5 ms; limiter wait p99 <= 1 ms.
