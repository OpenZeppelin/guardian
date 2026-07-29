# Quickstart: Running a Scalability Measurement

Three tiers. Pick by what you need, not by what is convenient — only one of them
can produce a target verdict, and only one of them can tell you why a number is
what it is.

| Tier | Produces | Needs | Can carry a verdict? |
|---|---|---|---|
| **Authoritative** | The number the target is judged against | AWS, live deployment | Yes |
| **Diagnostic** | Why the number is what it is | Docker only | No |
| **Fast loop** | Quick iteration signal | Local node + Postgres | No |

## Tier 1 — Authoritative (deployed, testnet-backed)

### One-time setup

```bash
aws login                                   # interactive; session expires
brew install --cask session-manager-plugin  # required: cleanup runs via ECS exec
aws configure list-profiles                 # profiles reference `dev`
```

All four April profiles set `cleanup.enabled = true`, so `preflight` asserts the
Session Manager plugin before anything runs. Without it you fail at the gate, not
mid-run.

### Confirm what you are about to measure

```bash
curl -s https://guardian.openzeppelin.com/status | jq .
```

Returns `version`, `git_commit`, `environment`, `started_at`. Compare `git_commit`
against the build you intend to measure **before** spending the run. As of
2026-07-28 the deployment served `0.16.0` / `6e7c6263e9ac`, whose tree is
identical to `ce4c342` on `main`.

### Preflight, then run

```bash
cargo run --manifest-path benchmarks/prod-server/Cargo.toml -- \
  preflight --profile benchmarks/prod-server/profiles/replay-falcon-ecdsa-mixed.toml

./scripts/run-prod-benchmark-ecs.sh \
  --profile benchmarks/prod-server/profiles/replay-falcon-ecdsa-mixed.toml \
  --workers 16
```

Start with the low-pressure profile as a smoke run before the headline four.

**This places real load on the live production endpoint and creates
benchmark-owned accounts on it.** Cleanup is part of the run; do not pass
`--no-cleanup` unless you intend to leave accounts behind and say so in the
report. At target scale (20,000 readers) this is a deliberate load event and needs
scheduling, or a separate production-shaped stack.

### Reading the result

```bash
jq '.verdicts, .saturation, .build_identity.spanned_restart' \
  benchmarks/prod-server/reports/<run-id>/run-report.json
```

Check in this order:

1. `build_identity.spanned_restart` — `true` means the server restarted mid-run
   and every verdict is withheld.
2. `saturation.attribution` — `generator_bound` means the number describes the
   load fleet, not Guardian.
3. `shard_recovery.aggregate_usable` — two of April's four headline runs were
   salvaged from 15 of 16 shards.
4. Only then, `verdicts`.

## Tier 2 — Diagnostic (local multi-instance, instrumented)

This is where bottleneck evidence comes from. Production runs with
`GUARDIAN_METRICS_ENABLED=false` and `infra/*.tf` never sets it, so the
authoritative tier has **no server-side metrics at all** — it can tell you a p95
is 900ms, never which component spent the time.

### Bring the stack up

```bash
cd benchmarks/diagnostic-stack
cp .env.example .env          # set POSTGRES_PASSWORD, GUARDIAN_VERSION
docker compose up -d
```

Assembled from two committed guide stacks:
`docs/guides/horizontal-scaling/docker-compose.yml` (N replicas behind a Caddy
round-robin proxy, one shared Postgres, one fleet-wide ACK identity, each replica
also directly reachable) and `docs/guides/observability/docker-compose.yml`
(Prometheus + Grafana with provisioned dashboards).

### Point the same harness at it

```bash
cargo run --manifest-path benchmarks/prod-server/Cargo.toml -- \
  worker-run --profile benchmarks/diagnostic-stack/profiles/diag-paced-read.toml \
             --run-id local-$(date +%s) --shard-index 0 --shard-count 1
```

Same binary, same profile schema, `guardian_endpoint` pointing at the proxy.
Target the proxy for fleet behaviour, or a replica directly (`:3000`, `:3010`) to
isolate one instance from proxy effects.

### Compare one variable at a time

```bash
docker compose --env-file variants/pool-16.env up -d && ./run-diag.sh pool-16
docker compose --env-file variants/pool-64.env up -d && ./run-diag.sh pool-64
```

A comparison that changes two things attributes to neither. Variants cover pool
size, fast promotion, canonicalization concurrency, and replica count.

**Rate limiting**: leave it **on**. gRPC never traverses the rate-limit layer —
it is applied to the axum router only — so turning it off changes nothing for a
gRPC run and introduces an unexplained difference from production for no gain.
Disable it only for an explicit HTTP leg, where the prod-equivalent 5,000/min
(83 req/s) would otherwise dominate the measurement, and label that leg
non-prod-equivalent.

**Give each container its own CPU.** February's numbers were generator-bound
because generator, server, and Postgres shared one laptop's cores. A
generator-bound diagnostic run produces no bottleneck claim.

## Tier 3 — Fast loop (single-process local)

```bash
miden-node start --config <local>              # localhost:57291
./crates/server/bench/scripts/run_postgres.sh
```

Ignore `crates/server/bench/README.md`'s "Benchmark Runtime Code Switch"
section — it tells you to hand-edit `main.rs`, but `crates/server/src/main.rs`
already reads network type and canonicalization settings from the environment,
including fast promotion and max-concurrent-accounts.

## What every published run must state

Regardless of tier: which tier it ran on, the build identity at start and end,
declared users **and** the declared per-user rate **and** observed peak in-flight
concurrency as three separate figures, phase durations with provisioning
separated from measurement, the failure breakdown including `auth_window` and
`unclassified`, and what was cleaned up.

A user count without its rate is not a measurement — under pacing, 20,000 users
at 0.1 reads/s and 20,000 saturating users differ by more than an order of
magnitude in offered load.
