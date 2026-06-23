---
description: "Task list for 010-horizontal-scaling implementation"
---

# Tasks: Horizontal Scaling Correctness Across Multiple Guardian Instances

**Input**: Design documents from `speckit/features/010-horizontal-scaling/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/
**Branch**: `010-horizontal-scaling` | **Tracking**: issue #242 (subsumes #190)

**Tests**: INCLUDED. Required by Constitution Principle V (Evidence-Driven
Delivery) for high-risk areas (auth, canonicalization lifecycle, Rust/TS parity)
and by the spec's Success Criteria (SC-001..SC-008) + quickstart scenarios.

**Organization**: Tasks are grouped by user story (priority order from spec.md)
so each story can be implemented, tested, and shipped independently as its own PR.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependency on incomplete tasks)
- **[Story]**: US1..US6 (user-story phase tasks only)

## Path Conventions

Server-only change, single Rust crate: `crates/server/`. Migrations under
`crates/server/migrations/`. Integration tests under `crates/server/tests/`; unit
tests inline (`#[cfg(test)]`). Infra under `infra/`. Docs under `docs/`.

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Module scaffolding and dependencies needed by the coordination layer.

- [ ] T001 Create the `coordination` module skeleton: add `crates/server/src/coordination/mod.rs` (with `session_store`, `challenge_store`, `leader`, and `postgres` submodule declarations, `postgres` behind `#[cfg(feature = "postgres")]`) and register `mod coordination;` in `crates/server/src/lib.rs`.
- [ ] T002 [P] Ensure a cooperative-cancellation primitive is available: add `tokio-util` (feature `rt`, for `CancellationToken`) to `crates/server/Cargo.toml` if not already present.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Trait abstractions, in-memory implementations (the dev/single-replica
baseline and the test substrate), `AppState` wiring, the stage helper, and the
migration-concurrency guard. Everything here is shared by multiple user stories.

**⚠️ CRITICAL**: No user-story work can begin until this phase is complete.

> **Implementation sequencing note (2026-06-22)**: T003 (stage helper) and T009
> (migration advisory lock) landed first — both are surgical, self-contained, and
> independently verifiable. The coordination trait layer + in-memory impls
> (T001/T002/T004/T005/T006) and the `AppState` wiring (T007/T008/T008a) are
> **deferred to land together with US1** (Phase 3): the trait shape (generic vs.
> realm-discriminated `Subject`, in-store vs. passed clock) should be designed
> against the real `DashboardState`/`EvmSessionState` consumers rather than
> speculatively, and the `AppState` fields would be dead weight (rippling to 5
> construction sites) until US1 consumes them. This keeps edits surgical per
> AGENTS.md §3/§8.

- [X] T003 Create shared stage helper `crates/server/src/config/stage.rs` by extracting `is_prod_environment()` from `crates/server/src/ack/mod.rs` (reads `GUARDIAN_ENV`, prod = case-insensitive "prod"); update `ack/mod.rs` to call the shared helper. (research R5) — DONE: `config::stage::is_prod()`; `ack` rewired; 27 ack tests green; default + postgres builds clean.
- [ ] T004 Define coordination traits and shared types per `contracts/coordination-traits.md`: `SessionStore`, `ChallengeStore`, `LeaderElector` traits plus `Realm` enum (`operator`|`evm`), `Subject`, `SessionRecord`, `Lease { name, holder_id, fence_token, expires_at }` in `crates/server/src/coordination/{session_store.rs,challenge_store.rs,leader.rs}`. Note `ChallengeStore::consume(realm, signing_digest)` and `LeaderElector::verify_held(lease)`.
- [ ] T005 [P] Implement `InMemorySessionStore` + `InMemoryChallengeStore` in `crates/server/src/coordination/{session_store.rs,challenge_store.rs}`, porting today's `Arc<Mutex<HashMap>>` semantics byte-for-byte (operator + EVM behavior preserved). (depends on T004)
- [ ] T006 [P] Implement `AlwaysLeader` in `crates/server/src/coordination/leader.rs` (`try_acquire`/`renew`/`verify_held` always succeed — single-replica/dev). (depends on T004)
- [ ] T007 Add coordination handles to `AppState` in `crates/server/src/builder/state.rs`: `Arc<dyn SessionStore>`, `Arc<dyn ChallengeStore>`, `Arc<dyn LeaderElector>`.
- [ ] T008 Add the coordination-family selection point in `crates/server/src/builder/storage.rs` (and construct handles in `crates/server/src/builder/mod.rs` / `handle.rs`), defaulting to the in-memory impls + `AlwaysLeader`. (Postgres impls are slotted in per story at this same point.) (depends on T005, T006, T007)
- [ ] T008a Emit one startup log line in `crates/server/src/builder/handle.rs` reporting the active coordination mode ("shared" vs "single-process") plus resolved backend, stage, rate-limit partition count, and cursor-secret source, from the actual resolved state. (FR-019, SC-009; depends on T008)
- [X] T009 Guard migrations against concurrent multi-replica startup: wrap `run_pending_migrations` in `crates/server/src/storage/postgres.rs` with a Postgres session-level advisory lock (`pg_advisory_lock(<fixed_key>)` → migrate → `pg_advisory_unlock`, with connection-drop as backstop). (FR-017, research R7) — DONE: postgres build clean. NOTE: kept session-level lock on the dedicated migration `PgConnection` (RDS Proxy pins on advisory-lock use); did not switch to `pg_advisory_xact_lock` since diesel runs each migration in its own transaction.

**Checkpoint**: Foundation ready — single-replica/dev path works on in-memory
impls; per-story Postgres impls and consumer rewiring can now begin.

---

## Phase 3: User Story 1 - Operator login succeeds with multiple replicas (Priority: P1) 🎯 MVP

**Goal**: Operator (and EVM) auth challenges and sessions live in the shared store
so a challenge issued on replica A verifies on replica B and a session is honored
on every replica, with logout/expiry effective fleet-wide.

**Independent Test**: With 2+ replicas behind one Postgres, complete challenge→sign
→verify→authenticated-request with each step forced onto a different replica;
logout on one replica is rejected on all (quickstart §2; SC-001).

### Implementation for User Story 1

- [ ] T010 [P] [US1] Add migration `crates/server/migrations/<date>_auth_sessions/{up,down}.sql` creating `auth_sessions` (PK `token_digest BYTEA`, `realm TEXT`, `subject JSONB`, `issued_at`, `expires_at`, `revoked_at` nullable; indexes on `expires_at` and `(realm, expires_at)`). (data-model.md, db-schema.md)
- [ ] T011 [P] [US1] Add migration `crates/server/migrations/<date>_auth_challenges/{up,down}.sql` creating `auth_challenges` (PK `signing_digest BYTEA`, `realm TEXT`, `principal TEXT`, `issued_at`, `expires_at`, `consumed_at` nullable; indexes on `(realm, principal)` and `expires_at`).
- [ ] T012 [US1] Add Diesel schema + row models for `auth_sessions` and `auth_challenges` in `crates/server/src/storage/postgres/` (schema + model structs). (depends on T010, T011)
- [ ] T013 [P] [US1] Implement `PgSessionStore` in `crates/server/src/coordination/postgres/session_store.rs`: `put`/`get` (valid iff `revoked_at IS NULL AND now() < expires_at`)/`revoke` (idempotent, keep row until natural `expires_at`)/`sweep_expired` (delete `expires_at < now()`); errors → fail-closed. (depends on T012)
- [ ] T014 [P] [US1] Implement `PgChallengeStore` in `crates/server/src/coordination/postgres/challenge_store.rs`: `issue`/`consume(realm, signing_digest)` (atomic single-use `UPDATE ... SET consumed_at = now() WHERE ... consumed_at IS NULL AND now() < expires_at RETURNING`)/`list_for`/`sweep_expired`. (depends on T012)
- [ ] T015 [US1] Select `PgSessionStore`/`PgChallengeStore` under `#[cfg(feature = "postgres")]` + `DATABASE_URL` at the selection point in `crates/server/src/builder/storage.rs`. (depends on T013, T014, T008)
- [ ] T016 [US1] Rewire operator `DashboardState` in `crates/server/src/dashboard/state.rs` to use the `SessionStore`/`ChallengeStore` from `AppState` instead of its in-process `challenges`/`sessions` HashMaps; preserve `authenticate_session` permission re-resolution from the live allowlist and the existing auth boundary errors. (depends on T015)
- [ ] T017 [US1] Rewire EVM `EvmSessionState` in `crates/server/src/evm/session.rs` (`#[cfg(feature = "evm")]`) to use the shared stores via `realm = evm`. (depends on T015)
- [ ] T018 [US1] Schedule periodic `sweep_expired` for sessions and challenges where background tasks are started in `crates/server/src/builder/handle.rs`. (depends on T015)

### Tests for User Story 1

- [ ] T019 [P] [US1] Port existing operator auth unit tests onto `InMemorySessionStore`/`InMemoryChallengeStore` and assert no behavior change (regression baseline) in `crates/server/src/dashboard/state.rs` `#[cfg(test)]`.
- [ ] T020 [P] [US1] Multi-replica integration test in `crates/server/tests/multi_replica_auth.rs`: challenge-on-A / verify-on-B succeeds, session honored on any replica, logout rejected cross-replica, expired challenge replay rejected (FR-001/002/003, SC-001).
- [ ] T021 [P] [US1] Integration test in `crates/server/tests/multi_replica_auth.rs`: authenticated request and login fail closed (rejected) while Postgres is unavailable, recover when it returns (FR-018).

**Checkpoint**: Operator + EVM auth work across replicas. MVP for multi-replica
dashboard usability is shippable.

---

## Phase 4: User Story 2 - A delta is canonicalized exactly once (Priority: P1)

**Goal**: Exactly one replica runs canonicalization at a time via a Postgres lease;
renewal is concurrent with the pass, the pass is cooperatively cancellable, and
every state-mutating write is fence-gated so a superseded holder can never commit.

**Independent Test**: With 2+ replicas, each pending candidate transitions exactly
once; killing the lease holder transfers leadership within the TTL; a fenced write
by a superseded holder is blocked (quickstart §3/§3b; SC-002/SC-003).

### Implementation for User Story 2

- [ ] T022 [P] [US2] Add migration `crates/server/migrations/<date>_worker_leases/{up,down}.sql` creating `worker_leases` (PK `lease_name TEXT`, `holder_id TEXT`, `acquired_at`, `renewed_at`, `expires_at`, `fence_token BIGINT NOT NULL DEFAULT 0`). (data-model.md, db-schema.md)
- [ ] T023 [US2] Add Diesel schema + row model for `worker_leases` in `crates/server/src/storage/postgres/`. (depends on T022)
- [ ] T024 [P] [US2] Implement `PgLeaseElector` in `crates/server/src/coordination/postgres/lease.rs`: `try_acquire` (atomic `INSERT ... ON CONFLICT DO UPDATE ... WHERE expires_at < now() OR holder_id = excluded.holder_id`, increment `fence_token`), `renew` (`UPDATE ... WHERE lease_name AND holder_id AND now() < expires_at`), `verify_held` (`SELECT 1 ... WHERE lease_name AND holder_id AND fence_token AND now() < expires_at`), `release`. (depends on T023)
- [ ] T025 [US2] Generate a per-process `holder_id` at boot and select `PgLeaseElector` under `#[cfg(feature = "postgres")]` at the selection point in `crates/server/src/builder/storage.rs`. (depends on T024, T008)
- [ ] T026 [US2] Refactor the canonicalization worker `crates/server/src/jobs/canonicalization/worker.rs`: acquire the lease, spawn a renewal task on its own timer (`renew_interval`) concurrent with the pass sharing a `CancellationToken`, and run `process_all_accounts()` only while holding the lease; a failed renew trips cancellation. (depends on T025; coordination-traits.md "Renewal concurrency")
- [ ] T027 [US2] Add a cooperative cancellation check polled between accounts in `crates/server/src/jobs/canonicalization/processor.rs` so an aborted pass stops promptly. (depends on T026)
- [ ] T028 [US2] Add the MANDATORY fence check (`verify_held`) immediately before every on-chain submission / canonical promotion in `crates/server/src/jobs/canonicalization/processor.rs`; skip the write if it returns false. (depends on T027; FR-005, data-model invariant)

### Tests for User Story 2

- [ ] T029 [P] [US2] Integration test in `crates/server/tests/multi_replica_canonicalization.rs`: 2 replicas, ≥50 candidates each canonicalized exactly once (zero duplicate promotions/discards/submissions) (SC-002).
- [ ] T030 [P] [US2] Integration test in `crates/server/tests/multi_replica_canonicalization.rs`: kill the lease holder, leadership transfers and canonicalization resumes within the TTL with no manual action (SC-003).
- [ ] T031 [P] [US2] Integration test in `crates/server/tests/multi_replica_canonicalization.rs`: a superseded holder's write is blocked by the fence check during the cancellation window; a store outage makes the holder step down (no double-processing) and recover (FR-005/FR-018).
- [ ] T032 [P] [US2] Regression test: single-replica canonicalization via `AlwaysLeader` is unchanged from today (SC-008) in `crates/server/src/jobs/canonicalization/` `#[cfg(test)]`.

**Checkpoint**: Canonicalization is exactly-once and self-healing under multiple
replicas; both P1 stories deliver the core multi-replica correctness.

---

## Phase 5: User Story 3 - Pagination cursors valid across all replicas (Priority: P2)

**Goal**: The cursor secret is enforced so cursors are valid on any replica; an
unset secret fails startup in prod and warns in dev.

**Independent Test**: With a shared secret, page-1-on-A / page-2-on-B continues
correctly; unset + prod fails startup; unset + non-prod warns (quickstart §4; SC-004).

### Implementation for User Story 3

- [ ] T033 [US3] In `crates/server/src/dashboard/config.rs`, fail startup (actionable error) when `GUARDIAN_DASHBOARD_CURSOR_SECRET` is unset and `config::stage::is_prod()` is true; keep the warn + ephemeral per-process fallback in non-prod. (FR-008/FR-013, depends on T003)

### Tests for User Story 3

- [ ] T034 [P] [US3] Integration test in `crates/server/tests/multi_replica_cursor.rs`: with a shared secret a cursor from one replica verifies on another (SC-004); prod + unset secret fails startup; non-prod + unset warns and starts; tampered/expired cursor rejected.

**Checkpoint**: Cross-replica pagination works and the silent-misconfig footgun is closed.

---

## Phase 6: User Story 4 - Rate limits enforced consistently across replicas (Priority: P2)

**Goal**: Per-process limiter partitions the global limit by the autoscaling max
capacity so aggregate enforcement stays at or below the global limit; the default
ships from Terraform.

**Independent Test**: With `GUARDIAN_RATE_LIMIT_PARTITIONS` = max capacity, traffic
above the global limit across replicas is capped at or below the global limit
(SC-005; quickstart §5).

### Implementation for User Story 4

- [ ] T035 [US4] In `crates/server/src/middleware/rate_limit.rs`, read `GUARDIAN_RATE_LIMIT_PARTITIONS` and divide `GUARDIAN_RATE_BURST_PER_SEC` / `GUARDIAN_RATE_PER_MIN` by it (unset or `1` => current behavior). (FR-009, research R4)
- [ ] T036 [P] [US4] In `infra/data.tf`, add `local.effective_guardian_rate_limit_partitions = var.guardian_rate_limit_partitions != null ? var.guardian_rate_limit_partitions : local.effective_server_autoscaling_max_capacity` and the `var.guardian_rate_limit_partitions` variable (default `null`).
- [ ] T037 [US4] In `infra/ecs.tf`, inject `GUARDIAN_RATE_LIMIT_PARTITIONS = tostring(local.effective_guardian_rate_limit_partitions)` into the rate-limit env block (after `GUARDIAN_RATE_PER_MIN`). (depends on T036)

### Tests for User Story 4

- [ ] T038 [P] [US4] Unit test of partition arithmetic in `crates/server/src/middleware/rate_limit.rs` `#[cfg(test)]` + integration test in `crates/server/tests/multi_replica_rate_limit.rs` asserting aggregate accepted rate stays at or below the global limit across replicas (and stricter below max capacity) (SC-005).

**Checkpoint**: Rate limiting no longer multiplies with replica count and the
default is correct on every deploy.

---

## Phase 7: User Story 5 - Filesystem backend refused in the prod stage (Priority: P3)

**Goal**: Fail fast in prod when the filesystem backend is active; keep it as the
dev default in non-prod.

**Independent Test**: prod + filesystem fails startup with an actionable error;
non-prod + filesystem starts; prod + Postgres is unaffected (quickstart §6; SC-006).

### Implementation for User Story 5

- [ ] T039 [US5] In `crates/server/src/builder/storage.rs`, refuse to start (actionable error naming the backend + remedy) when the active backend is the filesystem backend and `config::stage::is_prod()` is true; allow it in non-prod. (FR-012, depends on T003)

### Tests for User Story 5

- [ ] T040 [P] [US5] Integration/startup test in `crates/server/tests/prod_guards.rs`: prod + filesystem fails fast; non-prod + filesystem starts; prod + Postgres backend starts cleanly (SC-006).

**Checkpoint**: The unshared-backend-in-prod footgun is closed; dev flow preserved.

---

## Phase 8: User Story 6 - Operator HA configuration runbook (Priority: P3)

**Goal**: A single runbook + config docs covering every env var and state-store
dependency for a correct HA deployment, with the consequence of omitting each.

**Independent Test**: A reviewer stands up a correct 2+ replica deployment using
only the runbook and all P1/P2 scenarios pass (SC-007).

### Implementation for User Story 6

- [ ] T041 [P] [US6] Write `docs/runbooks/horizontal-scaling.md`: required env vars, shared-Postgres dependency, `GUARDIAN_RATE_LIMIT_PARTITIONS` override guidance + under-enforcement/keep-alive tolerance, pool sizing vs `max_connections` (and RDS Proxy), lease failover behavior, auth fail-closed on outage, filesystem = dev-only.
- [ ] T042 [P] [US6] Update `docs/CONFIGURATION.md`: add `GUARDIAN_RATE_LIMIT_PARTITIONS` (default from autoscaling max capacity), document the prod startup guards and the fail-closed auth behavior.
- [ ] T043 [P] [US6] Update `docs/SERVER_AWS_DEPLOY.md`: HA notes referencing the prod profile (`infra/data.tf` desired 2 / max 6), the partition default sourced from it, and pool-sizing guidance.

**Checkpoint**: The feature is safely operable by someone other than its author.

---

## Phase 9: Polish & Cross-Cutting Concerns

**Purpose**: Constitution gates and final validation across all stories.

- [ ] T044 Verify no client wire-contract drift: run the OpenAPI spec-drift gate and confirm zero diff; confirm no proto/HTTP/JSON/status/error-surface change (Constitution I/II, FR-015).
- [ ] T045 [P] Run Rust operator + multisig smoke flows and the TS operator smoke flow; confirm behavior unchanged (no cross-language drift).
- [ ] T046 [P] `cargo fmt --check` and `cargo clippy` clean on `crates/server` for all changed files.
- [ ] T047 Execute the full `quickstart.md` two-replica validation end to end (SC-001..SC-008).

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: no dependencies.
- **Foundational (Phase 2)**: depends on Setup; BLOCKS all user stories.
- **User Stories (Phases 3-8)**: all depend on Foundational. P1 stories (US1, US2)
  first for the MVP; P2 (US3, US4) and P3 (US5, US6) follow.
- **Polish (Phase 9)**: depends on all targeted stories.

### User Story Dependencies

- **US1 (P1)** and **US2 (P1)**: independent in behavior, but both add a migration
  (so both rely on the migration-lock T009) and both touch the selection point in
  `builder/storage.rs` (T015 vs T025) — sequence those two edits, don't parallelize
  the builder edit. Otherwise independently testable.
- **US3 (P2)** and **US5 (P3)**: both depend on the stage helper (T003); they edit
  different files (`dashboard/config.rs` vs `builder/storage.rs`) so are otherwise
  independent.
- **US4 (P2)**: independent (`rate_limit.rs` + `infra/`).
- **US6 (P3)**: docs only; best done last so it documents the shipped reality.

### Within Each User Story

- Migrations → Diesel schema/models → store/elector impls → selection wiring →
  consumer rewiring → background scheduling → tests.

### Parallel Opportunities

- Setup: T002 [P].
- Foundational: T005 and T006 [P] (after T004).
- US1: migrations T010/T011 [P]; impls T013/T014 [P]; tests T019/T020/T021 [P].
- US2: T024 [P]; tests T029/T030/T031/T032 [P].
- US4: T036 [P] alongside T035; test T038 [P].
- US6: T041/T042/T043 all [P].
- Across stories: once Foundational is done, US3, US4, US5, US6 can proceed in
  parallel with the US1/US2 work by different developers (mind the shared
  `builder/storage.rs` edits between US1 and US2).

---

## Parallel Example: User Story 1

```bash
# After T012 (schema/models), implement the two Postgres stores together:
Task: "Implement PgSessionStore in crates/server/src/coordination/postgres/session_store.rs"
Task: "Implement PgChallengeStore in crates/server/src/coordination/postgres/challenge_store.rs"

# Add both migrations together (independent files):
Task: "Add migration <date>_auth_sessions/{up,down}.sql"
Task: "Add migration <date>_auth_challenges/{up,down}.sql"
```

---

## Implementation Strategy

### MVP First (User Stories 1 + 2)

1. Phase 1 Setup → Phase 2 Foundational (CRITICAL — blocks all stories).
2. Phase 3 US1 (auth across replicas) → validate (SC-001, FR-018).
3. Phase 4 US2 (canonicalize exactly once) → validate (SC-002/SC-003).
4. **STOP and VALIDATE**: the two P1 stories are the core of issue #242 — the
   dashboard is usable and custody state is correct under 2+ replicas.

### Incremental Delivery

Each story is its own PR with its own tests and a "no wire diff" check:
Foundational → US1 (MVP) → US2 → US3 → US4 → US5 → US6 → Polish. Every story adds
value without breaking earlier ones; single-replica/dev behavior is preserved
throughout (AlwaysLeader + in-memory stores; no new infra required for dev).

### Constitution gates per PR

- No client wire-contract drift (OpenAPI gate, T044) — every PR.
- Auth/canonicalization PRs (US1, US2) are high-risk: tests in the changed layer
  plus an upstream consumer smoke (T045) are mandatory before merge.

---

## Notes

- [P] = different files, no dependency on incomplete tasks.
- Replace `<date>` in migration directory names with the actual creation date at
  implementation time (Diesel convention `YYYY-MM-DD-HHMMSS_name`).
- The filesystem/dev backend creates none of the new tables and uses the in-memory
  impls + `AlwaysLeader` — this is the explicitly accepted backend-specific
  limitation (single-replica only) recorded in plan.md.
- Commit after each task or logical group; stop at any checkpoint to validate a
  story independently.
