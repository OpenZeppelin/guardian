# Tasks: Reduce Per-Read Cost of Replay-Protection Auth Writes

**Input**: Design documents from `speckit/features/365-read-path-auth-write/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/auth-replay-contract.md, quickstart.md

**Tests**: Test tasks are REQUIRED — FR-009 and the CAS return-mapping contract (contracts/auth-replay-contract.md) explicitly mandate them.

**Organization**: The core change is one atomic foundational unit (schema + struct + trait + both backends — every story depends on all of it), so Phase 2 carries the implementation and the user-story phases carry each story's verification slice plus residual work. Story order runs US2 (security) before US1 (performance): benchmarking an unverified security control is wasted work.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (US1–US4)

## Path Conventions

Single Rust workspace; all implementation under `crates/server/`. Feature docs under `speckit/features/365-read-path-auth-write/`.

---

## Phase 1: Setup (Preconditions)

**Purpose**: Pin the green baseline and confirm the external verification dependency.

- [x] T001 Record the pre-change baseline: run `cargo test -p guardian-server` and `cargo test -p guardian-server --features postgres,integration` on unmodified `main`, confirm green, and note the commit hash in `speckit/features/365-read-path-auth-write/verification-log.md` (create the file; it accumulates evidence for Phases 3–7)
- [x] T002 [P] Confirm `benchmarks/diagnostic-stack/` harness status (lands via separate pending PR — see plan.md External Dependency): record in `speckit/features/365-read-path-auth-write/verification-log.md` whether the harness and the baseline result dirs (`read-128-t4-20260730T083554Z`, `nocas-128-t4-20260730T092323Z`) are merged; if result dirs are absent, flag that T018 must regenerate baselines on `main`

---

## Phase 2: Foundational (Blocking — the split itself)

**Purpose**: The complete storage split. No story can be verified until this compiles green with existing tests passing.

**⚠️ CRITICAL**: Sequential except where marked; T005 is the keystone (trait + struct) that T006–T009 depend on.

- [x] T003 Create migration `crates/server/migrations/2026-07-31-000001_account_auth_state/up.sql` and `down.sql` per data-model.md: `CREATE TABLE account_auth_state (account_id VARCHAR(128) PRIMARY KEY REFERENCES account_metadata(account_id) ON DELETE CASCADE, last_auth_timestamp BIGINT NOT NULL) WITH (fillfactor = 50)`; backfill `INSERT … SELECT … WHERE last_auth_timestamp IS NOT NULL`; `ALTER TABLE account_metadata DROP COLUMN last_auth_timestamp`; down.sql restores column, backfills from the table, drops it
- [x] T004 Update `crates/server/src/schema.rs`: add the `account_auth_state` diesel table, remove `last_auth_timestamp` from `account_metadata` (currently line 86), add the joinable/allow-tables entries
- [x] T005 In `crates/server/src/metadata/mod.rs`: remove `pub last_auth_timestamp: Option<i64>` from `AccountMetadata` (line 24); change trait signature to `update_last_auth_timestamp_cas(&self, account_id: &str, new_timestamp: i64) -> Result<bool, String>` (line 179, drop `now`); rewrite the doc comment to state the return-mapping contract table and the negative obligation (no other method may read/write replay state) from contracts/auth-replay-contract.md
- [x] T006 Mechanical sweep of `AccountMetadata` construction sites — delete the `last_auth_timestamp` field everywhere: `crates/server/src/api/grpc.rs` (~580), `api/http.rs` (~769), `api/dashboard_feeds.rs` (~453), `api/dashboard.rs` (~602, ~1141), `evm/service.rs` (~133 — delete the preserve workaround — and ~519), `jobs/canonicalization/processor.rs` (~1458), `services/configure_account.rs` (~150 — delete the carry-forward), `services/sign_delta_proposal.rs` (~214), `services/dashboard_account_deltas.rs` (~361, ~479), `testing/` fixtures/mocks/helpers as flagged by the compiler
- [x] T007 Rewrite the Postgres backend in `crates/server/src/metadata/postgres.rs`: CAS becomes the single-statement upsert on `account_auth_state` (data-model.md §Postgres CAS — affected-rows 0 ⇒ `Ok(false)`); remove `last_auth_timestamp` from `MetadataRow`/`NewMetadataRow` and from `set()` (lines ~55, ~71, ~93, ~162, ~178) so `set()` can never clobber replay state; CAS no longer touches `updated_at`
- [x] T008 Rewrite the filesystem backend in `crates/server/src/metadata/filesystem.rs`: in-memory auth-state map + `auth_state.json` persisted atomically (write-temp-rename) on each successful CAS instead of `persist(&cache)` (line ~205); startup: seed once from legacy `last_auth_timestamp` values via a file-format-only deserializer when `auth_state.json` is absent, persist immediately even when empty, and log the fail-open-guard warning when the file is missing after first boot (data-model.md §Filesystem)
- [x] T009 Update the call site in `crates/server/src/services/mod.rs` (line ~166): drop the `now_str` argument; keep the error mapping and the replay `AuthenticationFailed` message byte-identical (frozen per contracts/auth-replay-contract.md)
- [x] T010 Compile-and-green gate: `cargo test -p guardian-server` and `cargo clippy -p guardian-server --all-targets` pass; all pre-existing auth tests pass **unchanged** (any test that asserted the old CAS signature/`updated_at` side effect is updated only where the spec respecified behavior — FR-008)

**Checkpoint**: The split is complete and invisible — existing suites green, wire contract untouched. Story verification can begin.

---

## Phase 3: User Story 2 — Replay protection remains intact across replicas (Priority: P1) 🎯 security gate

**Goal**: Prove the guarantee survived the move — exactly-once per timestamp, atomic, durable, cross-replica on Postgres.

**Independent Test**: FR-009 suites pass on both backends; two-replica replay checks pass per quickstart.md §2.

- [x] T011 [P] [US2] CAS return-mapping unit tests for the filesystem backend (contract table: no-row→`Ok(true)`+created, `>T`→`Ok(true)`, `==T`→`Ok(false)`, `<T`→`Ok(false)`, stored value unchanged after every `Ok(false)`, replay never surfaces as `Err`) in `crates/server/src/metadata/filesystem.rs` test module
- [x] T012 [P] [US2] CAS return-mapping unit tests for the Postgres backend (same table, asserting the affected-rows mapping) in `crates/server/src/metadata/postgres.rs` / `crates/server/src/testing/integration/` (feature-gated `postgres,integration`)
- [x] T013 [P] [US2] End-to-end replay tests through `resolve_account` — same-timestamp replay, older-timestamp, first-request-accept-then-retry-reject — asserting the byte-identical `AuthenticationFailed` replay error, in the existing auth test home under `crates/server/src/testing/`
- [x] T014 [US2] Concurrency race tests: exactly one winner for identical concurrent timestamps — filesystem via concurrent in-process tasks; Postgres via parallel connections (feature-gated) — in `crates/server/src/testing/`
- [x] T015 [US2] Postgres migration backfill test (FR-006): seed the legacy column pre-migration, run migration, assert the timestamp is enforced from `account_auth_state`, in `crates/server/src/testing/integration/` (feature-gated)
- [x] T016 [US2] Filesystem legacy-seed and fail-open-guard tests: legacy value in metadata file → populated `auth_state.json` + enforcement on first run; file created even when seed is empty; post-first-boot absence → warning + empty state (never stale re-seed), in `crates/server/src/metadata/filesystem.rs` test module
- [ ] T017 [US2] Manual two-replica verification per quickstart.md §2 (Postgres; needs the diagnostic-stack harness from T002): replay to each replica rejected, concurrent cross-replica race admits one winner, Postgres-restart durability check; record evidence in `speckit/features/365-read-path-auth-write/verification-log.md`

**Checkpoint**: SC-003 satisfied; the security control is proven before any performance claim is made.

---

## Phase 4: User Story 1 — Read-heavy load without database saturation (Priority: P1) 🎯 acceptance gate

**Goal**: The measured A/B that accepts or escalates the feature (SC-001, SC-002, SC-006).

**Independent Test**: quickstart.md §5 against same-machine baselines.

- [ ] T018 [US1] Ensure same-machine baselines exist (depends on T002 finding): if the recorded result dirs did not ship with the merged harness, re-run `profiles/diag-read-128.toml` (and the no-CAS variant if reproducible) against unmodified `main` in `benchmarks/diagnostic-stack/`; record dirs in `speckit/features/365-read-path-auth-write/verification-log.md`
- [ ] T019 [US1] Run the split leg: `TOKIO_WORKER_THREADS=4 ./run-diag.sh split-128 profiles/diag-read-128.toml` in `benchmarks/diagnostic-stack/`, capturing `pg_stat_statements` for the run
- [ ] T020 [US1] Compute and record the acceptance report in `speckit/features/365-read-path-auth-write/verification-log.md`: SC-001 (throughput ≥ +25%, p95 no worse), SC-002 (auth share ≤25% AND auth DB time per accepted read −40% from ~0.16ms; auth = replay-state write + `account_metadata` SELECT), SC-006 (headroom = 1 − 2,000/max throughput; ≥30% floor; report the 40% stretch marker either way). **If SC-001 or SC-002 misses: STOP and report the residual to the user — guarantee-weakening options require a spec amendment (FR-001), not a follow-on task**

**Checkpoint**: Feature performance accepted (or escalated with measurements in hand).

---

## Phase 5: User Story 3 — One fix covers every authenticated read endpoint (Priority: P2)

**Goal**: Demonstrate — not infer — that all five read endpoints (and no write endpoint regression) got the improvement.

**Independent Test**: per-endpoint replay coverage + the SC-007 mixed-profile A/B.

- [x] T021 [P] [US3] Per-endpoint integration coverage: replay rejection exercised on each of `get_state`, `get_delta_since`, `get_delta_proposals`, `get_delta_proposal`, `get_delta`, plus one authenticated write endpoint (`push_delta`) asserting unchanged behavior, extending the existing suites in `crates/server/src/testing/integration/`
- [ ] T022 [US3] Author the mixed profile `benchmarks/diagnostic-stack/profiles/diag-read-mixed-128.toml`: 40% `get_state` / 40% `get_delta_since` / 20% `get_delta_proposals`, 128 closed-loop readers, 100s (SC-007 definition in spec.md)
- [ ] T023 [US3] Run the SC-007 mixed A/B — same machine, once against unmodified `main`, once against the change: pass = auth DB time per accepted read −40% and no endpoint p95 regression; record both result dirs and the verdict in `speckit/features/365-read-path-auth-write/verification-log.md`

**Checkpoint**: The polled-endpoint saving is measured, not extrapolated.

---

## Phase 6: User Story 4 — Storage behavior consistent with a read-only workload (Priority: P3)

**Goal**: Reads stop churning configuration records; `updated_at` means what it says.

**Independent Test**: FR-008 tests + quickstart.md §4 spot-checks.

- [x] T024 [P] [US4] FR-008 tests on both backends: `updated_at` does NOT advance on authenticated reads; DOES advance on non-auth metadata mutations (configuration change, pause/release, pending-candidate transitions), in `crates/server/src/testing/`
- [ ] T025 [US4] Storage-churn spot-check during a sustained read run (quickstart.md §4): `n_dead_tup` ≈ 0 on `account_metadata`, updates on `account_auth_state` overwhelmingly HOT (`n_tup_hot_upd`/`n_tup_upd`); record in `speckit/features/365-read-path-auth-write/verification-log.md` (SC-004)

**Checkpoint**: All four stories independently verified.

---

## Phase 7: Polish & Cross-Cutting

**Purpose**: Operator-visible documentation, client-compatibility proof, upgrade rehearsal, final gates.

- [x] T026 [P] Operator docs (per CONTRIBUTING.md docs table): document the rolling-deploy fail-closed behavior (old replicas error after the column drop until replaced) and the filesystem `auth_state.json` file (including the deletion caveat) in the horizontal-scaling / deployment docs under `docs/`
- [ ] T027 [P] SDK smoke flows (SC-005) with unmodified clients against the changed server: Rust via `smoke-test-rust-multisig-sdk` (`examples/demo`), TypeScript via `smoke-test-ts-multisig-sdk` (`examples/smoke-web`); record outcomes in `speckit/features/365-read-path-auth-write/verification-log.md`
- [ ] T028 Upgrade rehearsal per quickstart.md §3 (FR-006): Postgres pre→post binary swap with captured-request replay rejected; filesystem equivalent showing seeded `auth_state.json`; record in `speckit/features/365-read-path-auth-write/verification-log.md`
- [x] T029 Final validation gate: `cargo test -p guardian-server`, `cargo test -p guardian-server --features postgres,integration`, `cargo clippy -p guardian-server --all-targets`, `cargo fmt --check`; confirm the diff touches no wire contract (no `guardian.proto`, no TS package changes) — the frozen-surface assertion of contracts/auth-replay-contract.md

---

## Dependencies & Execution Order

### Phase Dependencies

- **Phase 1**: none — start immediately. T002 also gates T017/T018 (harness availability).
- **Phase 2**: after Phase 1. Internal order: T003→T004→T005→{T006, T007, T008}→T009→T010 (T007 ∥ T008 after T005; T006 overlaps their files' test fixtures, so run T006 before or with compiler guidance, not concurrently with T007/T008).
- **Phase 3 (US2)**: after T010. T011/T012/T013 parallel; T014–T016 next; T017 needs the harness (T002).
- **Phase 4 (US1)**: after Phase 3 (benchmark only verified code). T018→T019→T020.
- **Phase 5 (US3)**: T021 after T010 (can overlap Phases 3–4); T022→T023 after T020's methodology is settled and the harness is merged.
- **Phase 6 (US4)**: T024 after T010 (can overlap Phases 3–5); T025 needs a running read workload (pairs naturally with T019).
- **Phase 7**: T026/T027 after T010; T028/T029 last.

### Parallel Opportunities

- After T005: T007 ∥ T008 (different backend files).
- After T010: T011 ∥ T012 ∥ T013 ∥ T021 ∥ T024 (different test homes), while T026/T027 can also start.
- The two benchmark phases (T018–T020, T022–T023) are serial on the one benchmark machine.

## Implementation Strategy

- **Security-first increment**: Phases 1–3 form the minimal defensible unit — the split implemented and its guarantee proven (SC-003). Nothing ships on performance claims alone.
- **Acceptance**: Phase 4 is the gate that converts the row-width hypothesis into a measured result; its STOP rule (T020) is binding — a miss escalates to the user with numbers, never to a weaker design.
- **US3/US4 as overlap work**: their test tasks (T021, T024) are independent of the benchmark machine and slot into any idle capacity after T010.
- **Total**: 29 tasks — Setup 2, Foundational 8, US2 7, US1 3, US3 3, US4 2, Polish 4.
