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

- **T026 (docs)**: `docs/guides/horizontal-scaling/README.md` — added `account_auth_state` to the shared-tables matrix and an "Upgrading across schema migrations" fail-closed note; `spec/processes.md` — removed `last_auth_timestamp` from the configure diagram's `set(...)` (the abstract "update last_auth_timestamp" metadata-op lines remain accurate). The filesystem `auth_state.json` layout is not documented anywhere because no doc describes the `.metadata/` file layout at that level of detail — nothing to extend (per AGENTS.md §9, noting the reason).
- **T027 (SDK smokes)**: pending — see final report.
- **T028 (upgrade rehearsal)**: filesystem half covered by the legacy-seed/strip/file-loss tests; Postgres half covered by the migration backfill test (revert → legacy row → re-apply → enforced). A full binary-swap rehearsal with a live server rides with T017 once the harness lands.
- **T029 (final gate)**: **PASS** (2026-07-31 ~13:20 CET). `cargo test -p guardian-server --features postgres,integration` (against `guardian_365_test`, skipping the pre-existing hung audit test documented below): **458 passed / 0 failed** (16 `#[ignore]` — the DATABASE_URL-gated tests, which were run separately with `--include-ignored`: 51 passed / 0 failed). Default-feature suite: **783 passed / 0 failed**. `cargo clippy --all-targets --features postgres,integration,evm`: no errors or warnings. `cargo fmt --check`: clean. Wire contract untouched: zero changes under `proto/` or `packages/` (`git status` verified).

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
