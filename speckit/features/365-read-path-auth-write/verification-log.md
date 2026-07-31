# Verification Log: 365-read-path-auth-write

Evidence trail for Phases 1–7 (Constitution V). Each entry records what ran, where, and the outcome.

## Phase 1 — Setup

### T001 — Pre-change baseline (main @ `9dfdac8`)

- `cargo test -p guardian-server` on unmodified `main`: **PASS** (exit 0; 772 lib tests + doctests, 0 failures; run 2026-07-31 ~11:45 CET)
- Feature-gated baseline: covered by the post-change gate run instead — the Postgres/integration suites were run against the isolated database `guardian_365_test` (created on `guardian-postgres-1` :5432) after the change; all pre-existing tests in those suites pass unchanged, which is the property T001 exists to anchor.

### T002 — Diagnostic-stack harness status (2026-07-31)

- `git ls-files benchmarks/diagnostic-stack` → **0 tracked files**; the harness (compose files, `run-diag.sh`, `profiles/`) is NOT in this working tree — it lands via a separate pending PR (plan.md External Dependency).
- Local leftovers present: `ack-keys/`, `results/`, including **both baseline result dirs**:
  - `results/read-128-t4-20260730T083554Z` (with CAS, 2,314/s)
  - `results/nocas-128-t4-20260730T092323Z` (CAS removed, 3,957/s)
- Consequence: T017 (two-replica manual verification), T018–T020 (SC-001/002/006 A/B), T022–T023 (SC-007 mixed A/B), T025 (churn spot-check under load) are **blocked on the harness PR merge**. Baseline regeneration on `main` (T018) is NOT needed if the result dirs above are accepted — they exist locally on this machine, satisfying the same-machine A/B rule.

## Phase 2 — Foundational split (T003–T010), 2026-07-31

- Migration `2026-07-31-000001_account_auth_state` (create + backfill + column drop in one transaction; down.sql restores) — applied cleanly by `run_migrations` against `guardian_365_test`.
- `schema.rs`, `AccountMetadata` (field removed), trait signature (`now` param dropped), Postgres upsert-CAS (`sql_query` with binds — diesel 2.2's DSL rejects `.filter()` on this upsert shape, so the statement uses the same raw-SQL-with-binds pattern as `find_by_cosigner_commitment`), filesystem `auth_state.json` split with atomic writes + legacy seed + strip, `resolve_account` call site, mechanical sweep of ~30 construction sites.
- Gate: `cargo test -p guardian-server` **772 passed / 0 failed** (pre-existing tests pass unchanged); `cargo clippy --all-targets` clean.

## Phase 3 — US2 security verification, 2026-07-31

- **T011/T016 (filesystem)**: 7 new tests in `metadata/filesystem.rs` (`auth_state_tests`) — CAS return mapping (missing-row insert / equal / lower rejected with stored value unchanged / unknown account = `Err` not replay), CAS never touches `accounts.json` or `updated_at`, fresh store creates empty `auth_state.json`, legacy seed + strip, post-first-boot file loss starts empty (never stale re-seed). **PASS**.
- **T012 (Postgres)**: CAS return-mapping tests incl. FK rejection for unknown accounts and stored-value checks via the `account_auth_state` DSL. **PASS** (isolated DB `guardian_365_test`).
- **T013 (e2e through resolve_account, real filesystem store)**: first-accept → identical-replay-reject with the byte-exact frozen error message; older-timestamp reject; **plus a stale-clobber regression test** (`replay_state_survives_metadata_reconfiguration`) proving a read-modify-write `set()` can no longer regress replay state. **PASS**.
- **T014 (races)**: filesystem — 16 concurrent tasks, exactly 1 winner; Postgres — 4 rounds × 8 parallel pool connections, exactly 1 winner per round. **PASS**.
- **T015 (migration backfill)**: revert newest migration → insert legacy row (`last_auth_timestamp = 4242`) → re-run migrations → value backfilled into `account_auth_state`, equal timestamp rejected, higher accepted. **PASS**.
- Combined: `cargo test --features postgres --lib metadata:: -- --include-ignored` → **51 passed / 0 failed** (6.2s).
- **T017 (two-replica manual)**: BLOCKED on the diagnostic-stack harness PR (see T002).

## Phase 4 — US1 benchmark A/B

(blocked on harness merge — see T002)

## Phase 5 — US3 endpoint coverage / mixed profile

- **T021**: `every_authenticated_read_endpoint_rejects_a_replayed_request` (services/mod.rs) exercises all five read endpoints — `get_state`, `get_delta_since`, `get_delta_proposals`, `get_delta_proposal`, `get_delta` — against a real filesystem store: first call authenticates (never an auth failure), identical second call rejected with the byte-exact replay error. **PASS**. Write-endpoint non-regression: all pre-existing push/sign/abandon suites pass unchanged (T010 gate), and the write endpoints share the identical `resolve_account` path by construction.
- **T022/T023 (mixed-profile A/B)**: BLOCKED on the diagnostic-stack harness (see T002).

## Phase 6 — US4 storage behavior

- **T024**: covered by `cas_does_not_touch_account_metadata` + `non_auth_mutations_still_advance_updated_at` (filesystem) and `cas_does_not_advance_metadata_updated_at` (Postgres). **PASS**. The "advances on pause/release/candidate transitions" half is pre-existing behavior exercised by the existing pause/candidate suites, unchanged.
- **T025 (churn spot-check under load)**: BLOCKED on the diagnostic-stack harness (see T002).

## Phase 7 — Polish

- **T026 (docs)**: `docs/guides/horizontal-scaling/README.md` — added `account_auth_state` to the shared-tables matrix and an "Upgrading across schema migrations" fail-closed note (later strengthened to state the migration's table lock, see review fixes below); `docs/CONFIGURATION.md` — `GUARDIAN_METADATA_PATH` row now documents `accounts.json` + `auth_state.json`, the back-up-together requirement, and the fail-closed startup behavior; `docs/TROUBLESHOOTING.md` — "Server fails to start" item 8 covers the missing-`auth_state.json` refusal and the explicit `{}` operator override; `spec/processes.md` — removed `last_auth_timestamp` from the configure diagram's `set(...)` (the abstract "update last_auth_timestamp" metadata-op lines remain accurate).
- **T027 (SDK smokes)**: pending — see final report.
- **T028 (upgrade rehearsal)**: filesystem half covered by the legacy-seed/strip/file-loss tests; Postgres half covered by the migration backfill test (revert → legacy row → re-apply → enforced). A full binary-swap rehearsal with a live server rides with T017 once the harness lands.
- **T029 (final gate)**: **PASS** (2026-07-31 ~13:20 CET). `cargo test -p guardian-server --features postgres,integration` (against `guardian_365_test`, skipping the pre-existing hung audit test documented below): **458 passed / 0 failed** (16 `#[ignore]` — the DATABASE_URL-gated tests, which were run separately with `--include-ignored`: 51 passed / 0 failed). Default-feature suite: **783 passed / 0 failed**. `cargo clippy --all-targets --features postgres,integration,evm`: no errors or warnings. `cargo fmt --check`: clean. Wire contract untouched: zero changes under `proto/` or `packages/` (`git status` verified).

## Review fixes (2026-07-31, post-T029)

Three findings from external review, all confirmed against the code and fixed:

- **Migration lost-write race (FR-006)**: `up.sql` backfilled with `INSERT…SELECT` (ACCESS SHARE only), so CAS timestamps committed by old-binary replicas between the backfill snapshot and `DROP COLUMN` were silently discarded — a replay window on every rolling deploy, since migrations run at startup. Fixed by taking `LOCK TABLE account_metadata IN ACCESS EXCLUSIVE MODE` first (held to commit; queued legacy writes then fail closed on the dropped column). Verified by the existing revert→legacy-row→re-apply round-trip test (`migration_backfills_legacy_timestamps_into_account_auth_state`, PASS against real Postgres); the concurrent-writer window itself is not covered by an automated test — it would require pausing a migration transaction mid-flight — and rests on the lock's documented semantics.
- **Filesystem fail-open on lost `auth_state.json` (FR-001)**: startup re-seeded empty replay state when `auth_state.json` was missing from a migrated store, re-accepting previously seen timestamps. Pre-split `AccountMetadata` always serialized `last_auth_timestamp` (even as `null`), so key-presence in `accounts.json` is a durable first-migration marker: startup now fails closed when the file is missing, accounts exist, and no legacy keys remain. Explicitly recreating an empty `{}` file remains an operator override. New tests: `auth_state_file_loss_after_migration_fails_closed`, `pre_split_store_with_null_timestamps_migrates_cleanly`, `operator_recreated_empty_auth_state_is_honored` (replaces `auth_state_file_loss_after_first_boot_starts_empty_not_stale`, which asserted the fail-open behavior).
- **Seed/strip crash window**: a crash between writing `auth_state.json` and stripping legacy keys left stale timestamps in `accounts.json` that a later file loss would resurrect. Legacy residue is now merged (per-account max, never regressing) and stripped whenever found, not only on first seed. New tests: `interrupted_migration_residue_is_merged_and_stripped_on_next_boot`, `stale_legacy_residue_never_regresses_auth_state`.

## PR #368 review-comment fixes (2026-07-31, second round)

Four comments (1 Copilot, 3 CodeRabbit); three accepted, one declined:

- **`lock_timeout` before the migration lock (CodeRabbit, Major)**: an unbounded `LOCK TABLE … ACCESS EXCLUSIVE` at startup could queue indefinitely behind a long-running transaction and, while queued, block every new reader of `account_metadata` fleet-wide. Added `SET LOCAL lock_timeout = '5s'` so a conflicting session fails the migration fast for an orchestrator retry. Verified by the migration revert→re-apply round-trip test against real Postgres.
- **Filesystem CAS memory/disk divergence on persist failure (CodeRabbit, Major)**: accepted the defect, rejected the proposed fix — cloning the whole auth-state map per successful auth is an O(accounts) hot-path cost for a cold error path. Fixed instead by rolling back the in-memory entry (restore prior value or remove) before returning `Err`. Severity note: the divergence was fail-closed (memory stricter than disk), a consistency bug against the trait contract, not a replay window. New test `persist_failure_rolls_back_in_memory_state` (persist forced to fail by replacing `auth_state.json` with a directory; retry after recovery must not read as a replay; rollback must restore, not clear, the prior value).
- **Quickstart missing the ignored-test command (CodeRabbit, Minor)**: §1 advertised the DB-gated coverage above a command that never runs `#[ignore]` tests; added the explicit `DATABASE_URL=… -- --ignored` invocation.
- **`to_string()` allocation in filesystem CAS (Copilot)**: declined — the same call path serializes the entire map to pretty JSON and fsyncs it; one `String` allocation is far below that noise floor, and the perf-relevant backend is Postgres.

### Pre-existing hang found during the gate (not caused by this change)

`audit::postgres::tests::postgres_write_failure_emits_log_fallback`
(`crates/server/src/audit/postgres.rs:205`) hung the full
`--features postgres,integration` run for 36+ minutes (test binary parked at
0% CPU; sampled stack shows the libtest harness waiting on a test thread stuck
in `futures::executor::block_on` inside the tokio current-thread test runtime —
the spawned task it awaits can never be polled). Evidence it is pre-existing
and unrelated to this feature: the file and `build_postgres_pool_lazy` are
untouched by this diff; the test is `postgres`-feature-gated and **no CI
workflow runs `cargo test --features postgres`** (the feature appears only in
the Docker build), so the deadlock has been latent since the test landed
(#231/#264 era); the port-refusal premise of the test is fine on this machine
(`nc 127.0.0.1:1` refuses in 0.1s), ruling out the network path this feature
could not have touched anyway. The gate was re-run with
`-- --skip postgres_write_failure_emits_log_fallback`; the hang is flagged for
a separate issue rather than fixed here (out of scope, unrelated subsystem).
