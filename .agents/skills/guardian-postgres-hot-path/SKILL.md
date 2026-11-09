---
name: guardian-postgres-hot-path
description: Write, review and measure Guardian server Postgres code on hot request paths (per-request auth, session reads and the canonical nonce poll, state reads, delta pushes). Use when a change adds or edits a Diesel query, a raw `sql_query`, a SQL function or migration, pool or connection settings, a lock, a commit, or request logging on a path every request takes, or when a benchmark points at the database.
---

# Guardian Postgres hot path

Rules learned from measuring issue #317 on AWS and locally. The evidence is in
`benchmarks/prod-server/results/2026-11-04-devnet-317-bench.md`; the earlier
local Docker ramps are archived on issue #317.

## What counts as hot

Anything that runs once per request at read rates (thousands per second):

| Path | Where | Database call today |
|---|---|---|
| session nonce poll | `services/get_canonical_nonce.rs` → `resolve_unique_session_read` | `guardian_serve_session_read` (read-only, one call) |
| other session reads | `services/mod.rs` `resolve_unique_session_read` | `guardian_admit_session_read` (one call, records the read) |
| wallet-signed requests | `services/mod.rs` `consume_auth_timestamp` | `guardian_advance_wallet_floor` |
| session writes | `metadata/postgres.rs` `advance_session_floor` | `guardian_advance_session_floor` |
| metadata for auth | `metadata/postgres.rs` `get_for_read` / `get_for_read_at` | in-process cache validated by `auth_version` |
| state reads, pushes | `storage/postgres.rs` | Diesel DSL |

Background work (dashboard stats, read-record and floor maintenance,
migrations, execution pruning, leases) is not hot; the rules below are
optional there.

## Rules

1. **Prepare once.** A raw `diesel::sql_query` is not in Diesel's statement
   cache: it is parsed, planned and executed as an unnamed statement on every
   call, which doubled round trips and transactions per read on Nov 4. On a
   hot path use the Diesel DSL or call SQL functions through
   `diesel::define_sql_function!` (see `coordination/postgres/read_records.rs`
   and `metadata/postgres.rs` for `guardian_serve_session_read`,
   `guardian_admit_session_read`, `guardian_advance_wallet_floor`).
2. **One round trip per request.** Combine the lookups a request needs (account,
   session, record, state head) into one SQL function that returns everything,
   instead of several calls from Rust. Every statement on RDS costs TLS,
   protocol and transaction overhead: about 0.2 ms per poll on RDS against
   0.06 ms of function body locally.
3. **Do not commit on a read.** A durable commit per read put a WAL-flush
   ceiling of about 4,100 to 4,850 reads/s on a 2 vCPU `db.m7g.large`
   (`LWLock:WALWrite`, `IO:WalSync`). A read path that must write needs a
   security decision (see `decision-nonce-read-window.md`), not only a
   performance change.
4. **No per-request locks.** A shared advisory lock taken by every read
   contends on one lock-table entry (`LWLock:LockManager`). Keep locks on
   maintenance paths; refuse at the fence instead of locking per request.
5. **Keep PL/pgSQL for multi-step functions.** Small statements in a PL/pgSQL
   function keep cached plans. On Postgres 17 a `LANGUAGE sql` set-returning
   function is planned on every call (4.5x slower in pgbench), and one big
   joined query was about 8% slower than the current four small statements.
   Measure before rewriting.
6. **Index every hot lookup.** Each statement must hit a primary key or unique
   index (`EXPLAIN` it). The slowest statement seen under load was the
   JSONB cosigner lookup (`find_by_cosigner_commitment`, about 2 ms): fine for
   grants and key lookups, never for a per-poll path.
7. **Read the clock where you check it.** `now()` is the transaction start;
   after any lock or I/O wait use `clock_timestamp()` (`guardian_db_now_ms()`),
   and re-read anything an authorization depends on after a wait (see
   `guardian_admit_session_read`).
8. **Keep pool checkout cheap.** The pools use `RecyclingMethod::Fast`
   (`storage/postgres.rs`); the default `Verified` ran `SELECT 1` on every
   checkout, half of all statements under read load. A 64-connection pool was
   slower than 32 on a 2 vCPU database: size the database, not the pool.
9. **No INFO line per hot request.** The request span closes with one INFO
   line by default; at 4,000 polls/s that cost about 15% of server CPU and
   2 to 3 times the p99. Hot handlers instrument at `debug`
   (`services/get_canonical_nonce.rs`).
10. **Migrations.** A new or changed SQL function goes in a migration. Never
    edit a migration that any database has applied (released or deployed);
    use a new dated migration with `CREATE OR REPLACE FUNCTION`.

## Measuring a change

Work from cheap to expensive, and stop when the answer is clear.

1. **Statement level.** `pg_stat_statements` locally, or Performance Insights
   "Top SQL" on RDS (calls/s, average latency, load by wait). Choose the time
   range to match the measured window: setup (seeding users) shows up as WAL
   waits and inserts that are not the steady load.
2. **Function A/B with pgbench.** Postgres 17 in Docker capped at 2 CPUs,
   pgbench in a separate container, `-M prepared`, 32 clients, 30 s rounds,
   variants alternated over two rounds, seeded data at realistic size.
   First check every variant returns identical rows for every outcome
   (served, missing account, unknown, revoked and expired session, both window
   edges). Seed with the server's own migrations: let one Postgres test
   migrate `guardian_test`, then `CREATE DATABASE x TEMPLATE guardian_test`
   (some migrations do not apply through plain `psql`).
3. **Server A/B, locally.** Two server images that differ only in the change
   (`docker build --target server-runner --build-arg GUARDIAN_SERVER_FEATURES=postgres`,
   with `.with_canonicalization(Some(` in `crates/server/src/main.rs` patched
   to skip canonicalization when `GUARDIAN_BENCH_NO_CANONICALIZATION` is set,
   as `scripts/guardian-bench-env.sh` does), one Docker network, and:
   - Postgres 17 with `--cpus 2 --memory 2g`, `max_connections=300`,
     `shared_buffers=512MB`, a fresh database per leg;
   - one or two servers with `--cpus 2 --memory 2g`,
     `GUARDIAN_DB_POOL_MAX_SIZE=32`, rate limits raised out of the way,
     `GUARDIAN_METRICS_ENABLED=true`, fixed ACK keys from `ack-keygen`;
   - the load generator (`--target benchmark-runner`) as two
     `worker-run --shard-index i --shard-count 2` containers, then
     `aggregate` over their artifacts;
   - `docker stats` sampled every 5 s for CPU, and each server's `/metrics`
     scraped at the end (per-poll server time, pool acquire wait).

   Alternate legs; a single round can mislead. The laptop saturates around
   10,000 polls/s in total, before Postgres does. The release images use fat
   LTO: on an 8 GB Docker VM stop idle containers and cap the build at a few
   jobs or it runs out of memory.
4. **AWS.** The devnet bench environment (`scripts/guardian-bench-env.sh`,
   #588) and `run-guardian-prod-benchmarks`.
   Read database CPU and Performance Insights per run, and server CPU per
   task, not only the average.

Report latency honestly: the distributed harness averages per-worker
percentiles (not a fleet percentile), and a closed-loop load generator turns
overload into lower throughput and higher latency rather than failures.

## Sizing reference

About 0.2 vCPU of database (Graviton3) and 0.5 vCPU of Guardian server per
1,000 nonce polls/s; keep peak database CPU at or below about 70%. Table and
caveats: `docs/SERVER_AWS_DEPLOY.md`, "Sizing for session reads".

## Before calling a hot-path change done

- [ ] No raw `sql_query` on a hot path; statements prepared once.
- [ ] One database call per request on the path; no new commit on a read.
- [ ] No per-request lock; every lookup on an index (`EXPLAIN`).
- [ ] Time checks after waits use `clock_timestamp()`.
- [ ] Hot handler spans at `debug`.
- [ ] Postgres tests under a `postgres` module path, `#[ignore]`d, run with
  `--test-threads=1` against a recreated `guardian_test`.
- [ ] A measurement at step 2 or 3 above when the change could move the cost,
  recorded in `benchmarks/prod-server/results/`.
- [ ] If a replay or auth admission function changed: `verify-auth-replay`.
