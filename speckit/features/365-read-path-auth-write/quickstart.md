# Quickstart: Verifying the Replay-State Split

**Feature**: `365-read-path-auth-write` | **Date**: 2026-07-31
**Purpose**: The verification runbook — correctness first, then the benchmark A/B that is the acceptance gate (FR-007, SC-001/SC-002).

## 1. Unit & integration tests (fast loop)

```bash
cargo test -p guardian-server                       # filesystem backend (default)
cargo clippy -p guardian-server --all-targets
```

Postgres-gated coverage (CAS upsert, migration backfill, cross-process concurrency race). Note the `integration` feature is a separate gate (`crates/server/src/testing/integration/mod.rs:2`) — `postgres` alone does not run the integration suites:

```bash
# per docs/LOCAL_DEV.md — requires a local Postgres
cargo test -p guardian-server --features postgres,integration
```

Expected: all existing auth/replay tests pass **unchanged** (frozen contract), plus the new FR-009 tests listed in data-model.md §Validation rules.

## 2. Replay-protection verification on two replicas (SC-003, Postgres backend)

> **Dependency**: sections 2 and 5 require the `benchmarks/diagnostic-stack/` harness, which lands via a separate pending PR. Until it merges, these steps run against the local copy; the feature's acceptance A/B runs after the merge. Two-replica verification applies to Postgres only — the filesystem backend is single-process by design and is covered by the in-process concurrency tests in §1.

Bring up the two-replica diagnostic stack (same topology as the issue #365 measurements):

```bash
cd benchmarks/diagnostic-stack
docker compose --env-file .env --env-file variants/read-budgeted.env up -d
```

Checks (any authenticated endpoint; `get_state` is simplest):

1. Send a signed request through the proxy → accepted.
2. Re-send the identical request (same timestamp, same signature) → rejected with the replay `AuthenticationFailed` error — repeat against **each replica directly** to prove shared state.
3. Send a request with an older timestamp → rejected.
4. Fire two identical requests concurrently at the two replicas → exactly one accepted.
5. Restart the Postgres container, replay the pre-restart request → still rejected (durability).

## 3. Migration & upgrade checks (FR-006)

1. Start a pre-change server against a fresh Postgres, authenticate a few accounts (populates the legacy column).
2. Stop it, start the post-change server (migration runs at startup), and replay a captured pre-upgrade request → rejected.
3. Filesystem equivalent: run pre-change server with `~/.guardian`-style metadata dir, authenticate, switch binaries → `auth_state.json` appears seeded and the replayed request is rejected.

**Operator note (rolling deploys)**: once the migration has run, replicas still on the previous binary fail closed — their metadata reads error because the dropped column is in their query's column list. Plan for a coordinated replace (or tolerate errors from old replicas during the window). There is no replay exposure in either case.

## 4. Behavior spot-checks

- `updated_at`: read an account's dashboard listing position / `updated_at`, hammer it with authenticated reads, confirm `updated_at` did not move; make a configuration change, confirm it did (FR-008).
- Storage churn (SC-004): during a sustained read run, `SELECT n_dead_tup, n_tup_upd, n_tup_hot_upd FROM pg_stat_user_tables WHERE relname IN ('account_metadata','account_auth_state')` — dead tuples on `account_metadata` ≈ 0; updates on `account_auth_state` overwhelmingly HOT.

## 5. Benchmark A/B — the acceptance gate (FR-007, SC-001/SC-002)

```bash
cd benchmarks/diagnostic-stack
docker compose --env-file .env --env-file variants/read-budgeted.env up -d
TOKIO_WORKER_THREADS=4 ./run-diag.sh split-128 profiles/diag-read-128.toml
```

Compare against the baselines recorded in issue #365. **If those result directories did not ship with the merged harness, first regenerate both baseline legs on unmodified `main` on this machine** — every comparison below is same-machine A/B, never against numbers measured elsewhere:

| Leg | Result dir | Throughput | Meaning |
|---|---|---|---|
| With CAS on wide row | `results/read-128-t4-20260730T083554Z` (or regenerated) | 2,314/s | Baseline |
| CAS removed | `results/nocas-128-t4-20260730T092323Z` (or regenerated) | 3,957/s | Upper bound |
| **This change** | `results/split-128-…` | **target ≥ +25% over baseline** | SC-001 / SC-006 floor; p95 no worse |

From the same run, capture `pg_stat_statements` and compute both SC-002 measures — "authentication" is exactly two statements: the replay-state write (`INSERT … ON CONFLICT` on `account_auth_state`) and the `SELECT` on `account_metadata`:

- share of total DB time ≤ 25% (baseline 43.5%);
- combined mean DB time per accepted read reduced ≥ 40% (baseline ~0.16ms).

**Mixed-profile leg (SC-007)**: profile = 40% `get_state`, 40% `get_delta_since`, 20% `get_delta_proposals`, 128 closed-loop readers, 100s (profile authored as part of this feature's tasks). Run it twice on this machine — once against unmodified `main`, once against the changed server. Pass: authentication DB time per accepted read (SC-002 definition) drops ≥ 40%, and no endpoint's p95 regresses versus the `main` leg.

Report the SC-006 stretch marker either way: headroom = 1 − (2,000 / measured max throughput); ≥30% is the floor (equivalent to SC-001), ≥40% is the recorded aspiration.

**Escalation rule (pre-agreed in research.md)**: if SC-001/SC-002 is missed, stop — coarsening, external stores, and UNLOGGED tables are outside this feature by FR-001. Report the measured residual to the user; those options can only be pursued through a spec amendment or a successor feature.

## 6. SDK smoke flows (SC-005)

Run against the changed server with **unmodified** published/workspace clients:

- Rust: `smoke-test-rust-multisig-sdk` skill (`examples/demo` CLI flow).
- TypeScript: `smoke-test-ts-multisig-sdk` skill (`examples/smoke-web` browser flow).

Both must complete create → sync → propose → sign → execute with zero client-side changes.
