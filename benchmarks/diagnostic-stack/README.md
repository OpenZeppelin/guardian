# Diagnostic Stack

Local, instrumented, multi-replica Guardian for **bottleneck attribution**.

Part of speckit feature
[`317-scalability-target-measurement`](../../speckit/features/317-scalability-target-measurement/spec.md).
This is the **diagnostic tier** of three (FR-017a):

| Tier | Produces | Needs | Carries a target verdict? |
|---|---|---|---|
| Authoritative — deployed, testnet-backed | The number the target is judged against | AWS | **Yes** |
| **Diagnostic — this stack** | **Why the number is what it is** | **Docker only** | **No** |
| Fast loop — `crates/server/bench` | Quick iteration signal | Local node + Postgres | No |

## Why this tier exists

Production runs with `GUARDIAN_METRICS_ENABLED=false`, and `infra/*.tf` never
sets it — no metrics listener, no recorder, no storage instrumentation. The
benchmark harness measures only client-side round trip.

So the authoritative tier can tell you a p95 is 900ms but **never which component
spent the time**. Issue #317's workstreams 1 and 2 — Postgres profiling and pool
tuning, the auth/replay-protection path — are single-variable questions that need
a stack where one thing changes at a time and the server reports on itself.

Production gives the verdict. This gives the reason.

## What it is

Assembled from two committed guide stacks rather than authored fresh:

- [`docs/guides/horizontal-scaling`](../../docs/guides/horizontal-scaling) —
  two replicas behind a Caddy round-robin proxy, one shared Postgres, one
  fleet-wide ACK identity, each replica also directly reachable.
- [`docs/guides/observability`](../../docs/guides/observability) — Prometheus and
  Grafana, whose dashboards this stack mounts rather than copies.

**Differs from both in one important way**: the image is built from this repo
with the **Postgres** backend. The horizontal-scaling guide pulls a published
image (which would measure whatever `latest` happens to be) and the observability
guide builds with the filesystem backend (which production does not run).

| Endpoint | Address |
|---|---|
| gRPC via proxy (harness default) | `127.0.0.1:50051` |
| HTTP via proxy | `127.0.0.1:8080` |
| Replica A direct — HTTP / gRPC | `127.0.0.1:3000` / `127.0.0.1:50052` |
| Replica B direct — HTTP / gRPC | `127.0.0.1:3010` / `127.0.0.1:50053` |
| Prometheus | `127.0.0.1:9090` |
| Grafana (anonymous admin, dev only) | `127.0.0.1:3001` |
| Postgres | `127.0.0.1:5432` |

Everything is bound to loopback. Target the proxy for fleet behaviour, or a
replica directly to isolate one instance from proxy effects.

## Setup

```sh
cp .env.example .env
# set POSTGRES_PASSWORD and GUARDIAN_DASHBOARD_CURSOR_SECRET (openssl rand -hex 32)

mkdir -p ack-keys
cargo run --quiet -p guardian-server --bin ack-keygen \
  | { read -r json; \
      jq -rj '.falcon_secret_key' <<<"$json" > ack-keys/ack-falcon-secret-key; \
      jq -rj '.ecdsa_secret_key'  <<<"$json" > ack-keys/ack-ecdsa-secret-key; }
chmod 600 ack-keys/ack-*

docker compose up -d --build   # first build compiles the server; later runs cache
```

`.env`, `ack-keys/`, and `results/` are gitignored. Treat `ack-keys/` as private
key material.

## Running a comparison

The point of this tier is **one variable at a time** (FR-017d). A leg that
changes two things attributes to neither.

```sh
docker compose --env-file .env --env-file variants/pool-16.env up -d
./run-diag.sh pool-16

docker compose --env-file .env --env-file variants/pool-64.env up -d
./run-diag.sh pool-64
```

Available variants:

| Sweep | Files | Question |
|---|---|---|
| DB pool size | `pool-16` (code default), `pool-32` (prod), `pool-64` | Is the pool the read-path constraint, and does the gain continue past prod's value? |
| Fast promotion | `fast-promotion-{on,off}` | What does the fast-track pass contribute to accepted→canonical wait? |
| Canonicalization concurrency | `canon-concurrency-{10,50}` | Code default vs the prod value. |
| Replica count | `replicas-{1,2}` | Does adding an instance help? The single-task production deployment cannot answer this. |

Each leg writes to `results/<label>-<timestamp>/`: build identity before and
after, the harness artifact, the effective server environment, Prometheus
snapshots, `pg_stat_statements`, and container stats.

## Reading a result

Check in this order — the first three can invalidate the fourth:

1. **`SPANNED_RESTART`** — if present, the server restarted mid-run and the leg
   measures more than one server instance. Discard it.
2. **`docker-stats-*.json`** — if the load generator or a container was pinned at
   its CPU limit, the leg is saturation-bound and supports no bottleneck claim.
3. **`prom-targets.json`** — both replicas must be `up`, or you measured one
   replica while attributing to two.
4. **`pg-stat-statements.csv` and `prom-*.json`** — the actual attribution.

## Things that will mislead you if you forget them

**Rate limiting does not apply to gRPC.** The rate-limit layer is attached to the
axum router only; the tonic server never sees it. Disabling rate limiting changes
nothing for a gRPC run — it is left **on** and at prod-equivalent values so this
stack stays comparable to production. Only an explicit HTTP leg is affected,
where prod-equivalent settings (5,000/min ≈ 83 req/s) would otherwise dominate
any load measurement; label such a leg non-prod-equivalent in its report.

**CPU budget is the whole ballgame.** The defaults total ~6.0 CPUs, leaving ~4 on
a 10-core host for a host-side load generator. February's benchmark numbers were
generator-bound because generator, server, and Postgres shared one laptop's
cores, and its own report attributes much of the observed latency to that. Raise
the limits only if you confirm the generator still has headroom.

**`users` here still means in-flight requests.** The load model is the existing
closed-loop one, not the paced definition FR-003 resolved. Paced load arrives in
Phase 3. For finding a bottleneck, saturation is the right shape anyway — just do
not read these numbers as target-shaped.

**This tier cannot produce a target verdict.** Different machine, different
network, different Postgres tuning, no RDS Proxy. Numbers from here are for
comparison between legs, never against the target.

## Teardown

```sh
docker compose down -v    # -v drops the database; this is the local cleanup path
```

There is no ECS-exec purge here and none is needed — the whole database is the
benchmark's, and `-v` removes it.
