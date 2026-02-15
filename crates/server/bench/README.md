# Server Benchmarking

This folder contains a runnable benchmark harness for `private-state-manager-server`.

Scope:
- Compare Filesystem vs Postgres backend behavior.
- Measure scaling with users/accounts/signers/operations.
- Validate HTTP rate-limiting and body-size enforcement.

## Prerequisites

- Miden node already running on `localhost:57291`.
- Rust toolchain and `cargo`.
- Docker (required for Postgres benchmark path).
- Optional: `k6` (HTTP checks) and `jq` (summary formatting).

## Layout

- `config/`
  - `common.env`: shared server and workload defaults.
  - `fs.env`: Filesystem backend paths.
  - `postgres.env`: Postgres docker/database settings.
  - `profiles.toml`: small/medium/large profile presets.
- `loadgen/`: Rust load generator (`psm-server-bench-loadgen`).
- `k6/`: HTTP checks for body-limit and rate-limit behavior.
- `scripts/`: orchestration and metrics scripts.
- `sql/`: Postgres stats queries.
- `reports/`: curated benchmark reports.
- `results/`: run outputs.

## Metrics

The harness records:
- Throughput: `ops_per_sec`, `success_ops_per_sec`.
- Latency: `p50`, `p95`, `p99`, `max`.
- Reliability: total/success/failed operation counts and sample errors.
- Server resources: sampled `%CPU`, `RSS`, `VSZ`.
- Postgres query stats (Postgres runs only).
  `pg_stat_statements` is collected when available; otherwise `pg_stat_database` snapshots are still collected.
- HTTP middleware behavior (k6 scripts): status distribution for 429 and 413 checks.

## Workloads

Load generator scenarios:
- `state-read`: authenticated `get_state` workload.
- `state-write`: repeated `configure` workload.
- `mixed`: configurable mix of reads and writes.
- `state-sync`: deterministic `4 x get_state` + `1 x push_delta` cycle.
- `canonicalization`: push one delta per seeded account, then wait for terminal status.

Each run seeds accounts before measured operations:
- Accounts are multisig+PSM accounts generated locally.
- Signer count per account is configurable.
- Seeding time is reported separately from measured run time.

## Running

Run Filesystem benchmark:

```bash
./crates/server/bench/scripts/run_fs.sh
```

Run Postgres benchmark (Postgres via docker compose):

```bash
./crates/server/bench/scripts/run_postgres.sh
```

Run Postgres benchmark with canonicalization load:

```bash
./crates/server/bench/scripts/run_postgres_canonicalization.sh
```

Run Postgres state-sync benchmark (defaults to 1000 users, 1 push per 4 reads):

```bash
./crates/server/bench/scripts/run_postgres_state_sync.sh
```

Run Postgres Falcon vs ECDSA comparison:

```bash
./crates/server/bench/scripts/run_postgres_auth_compare.sh
```

`run_postgres_canonicalization.sh` defaults to `BENCH_SCENARIOS=canonicalization`.

Generate a markdown report from a suite run:

```bash
./crates/server/bench/scripts/generate_report.sh <suite_index_path>
```

Current workflow note:
- The canonical report for the current benchmark set is maintained in `crates/server/bench/reports/`.
- `generate_report.sh` is a helper for suite-index based runs and is optional.

Stop the Postgres container when done:

```bash
docker compose down
```

Run both:

```bash
./crates/server/bench/scripts/run_matrix.sh
```

## Benchmark Runtime Code Switch (Main Branch)

`crates/server/src/main.rs` defaults to:

- `NetworkType::MidenDevnet`
- `Some(CanonicalizationConfig::new(10, 24))`

If you want benchmark scripts to drive network/canonicalization through environment variables, change:

```rust
.network(NetworkType::MidenDevnet)
.with_canonicalization(Some(CanonicalizationConfig::new(10, 24)))
```

to:

```rust
.network(NetworkType::from_env("PSM_NETWORK_TYPE"))
.with_canonicalization({
    let canonicalization_enabled = std::env::var("PSM_CANONICALIZATION_ENABLED")
        .ok()
        .map(|value| !matches!(value.to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off"))
        .unwrap_or(true);
    if canonicalization_enabled {
        let check_interval_seconds = std::env::var("PSM_CANONICALIZATION_CHECK_INTERVAL_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(10);
        let max_retries = std::env::var("PSM_CANONICALIZATION_MAX_RETRIES")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(24);
        Some(CanonicalizationConfig::new(
            check_interval_seconds,
            max_retries,
        ))
    } else {
        None
    }
})
```

This keeps normal server defaults unchanged while allowing benchmark runs to tune those values.

## Configuration

Main knobs are in `config/common.env`:
- `BENCH_USERS`
- `BENCH_ACCOUNTS`
- `BENCH_SIGNERS_PER_ACCOUNT`
- `BENCH_AUTH_SCHEME` (`falcon` or `ecdsa`)
- `BENCH_TRANSPORT` (`grpc` or `http`)
- `BENCH_OPS_PER_USER`
- `BENCH_MIXED_WRITE_PERCENT`
- `BENCH_STATE_SYNC_READS_PER_PUSH`
- `PSM_SERVER_START_TIMEOUT_SECS`
- `BENCH_SKIP_PREBUILD`
- `BENCH_SERVER_LOG_LEVEL`
- `BENCH_SCENARIOS` (comma-separated, optional override)
- `BENCH_ENABLE_CANONICALIZATION`
- `BENCH_CANONICALIZATION_POLL_INTERVAL_MS`
- `BENCH_CANONICALIZATION_TIMEOUT_SECS`

Server knobs used for all benchmark runs:
- `PSM_NETWORK_TYPE=MidenLocal`
- `PSM_RATE_BURST_PER_SEC`
- `PSM_RATE_PER_MIN`
- `PSM_MAX_REQUEST_BYTES`
- `PSM_CANONICALIZATION_ENABLED`
- `PSM_CANONICALIZATION_CHECK_INTERVAL_SECS`
- `PSM_CANONICALIZATION_MAX_RETRIES`

Note: `PSM_NETWORK_TYPE` and canonicalization env knobs take effect only when the runtime code switch above is enabled.

Postgres-specific values are in `config/postgres.env`.
On macOS, set `PSM_PQ_LIB_DIR` if `libpq` is not on the default linker path.

## Outputs

Each run creates a timestamped folder in `results/`, containing:
- `server.log`
- `server_metrics.csv`
- `loadgen_state-read.json`
- `loadgen_state-write.json`
- `loadgen_mixed.json`
- optional `loadgen_state-sync.json`
- optional `loadgen_canonicalization.json`
- optional `k6_*.log`
- optional `pg_metrics.txt`
- `summary.txt`

## Methodology Notes

- Compare Filesystem and Postgres runs with the same values from `common.env`.
- Use at least 3 repetitions per profile and compare median values.
- Keep the node, machine, and background load consistent between runs.
- Treat 429/413 checks as correctness checks, not throughput tests.
