# Performance and Capacity

How to size a Guardian deployment, what throughput and latency to expect, and
how to measure it yourself.

## How to read the numbers in this document

Every figure here is tagged with the tier it came from, because the two are not
interchangeable:

| Tier | Where it runs | What it can claim |
|---|---|---|
| **Diagnostic** | Local stack, instrumented, load generated on the same host | Bottleneck attribution and bounds. **Never** a target verdict. |
| **Authoritative** | Deployed environment, load generated off-box from sharded workers | Verdicts against a stated target. |

Diagnostic numbers describe the shape they were measured on. They are useful for
*where the limit is* and *how to size relative to it*, not as capacity promises.
Sizing rules survive the shape change; absolute throughput does not.

Figures below are marked `[D]` diagnostic or `[A]` authoritative, with the shape
they came from. Anything marked **TODO(authoritative)** has no verdict yet.

## Where the bottlenecks are

Measured across two days of write- and read-path runs:

| Path | Binding constraint | Guardian server CPU at the ceiling |
|---|---|---|
| Write (propose → execute → canonical) | **The prover** | ≤ 60% of allocation `[D]` |
| Read (`get_state`) | **The database** | 65–75% of allocation `[D]` |

**The Guardian server was not the constraint in any measurement taken.** Both
ceilings belong to components either side of it. Size those first; adding server
replicas addresses neither, and on the read path it makes the database load
worse.

## Write path

### The prover is a serialisation point

A Miden prover instance **proves one transaction at a time**. Extra CPU makes a
single proof finish sooner; it never makes two proofs run at once. Concurrency
comes from replicas, reached through a load balancer, because a client is
configured with exactly one prover URL.

`--capacity` bounds requests *queued*, not proved in parallel. Raising it does
not add throughput — it changes the failure from immediate rejection to queued
timeout:

| Capacity | 8 writers, 2 replicas `[D]` | Failure mode |
|---|---|---|
| 4 | 20 of 159 | `ResourceExhausted` — rejected on arrival |
| 16 | 23 of 62 | proof timeouts — accepted, then queued past 120s |

### Sizing rule

**Roughly two writers per prover replica**, with replicas at **2 CPU or more**.

| Layout `[D]`, 4 CPU total | Writers | Result |
|---|---|---|
| 2 × 2 CPU | 4 | **68 of 68** |
| 2 × 2 CPU | 8 | 23 of 62 |
| 4 × 1 CPU | 8 | 1 of 49 |

Do **not** buy concurrency by shrinking replicas. At a constant CPU budget, four
1-CPU replicas performed far worse than two 2-CPU replicas: per-proof latency
rose 2.6× (worse than the 2× a linear model predicts), so more slots each much
slower reach the timeout sooner.

Over-driving a serialised resource does not degrade gracefully — it collapses.
Completed throughput *fell* from 13.6 to 4.6 operations/min when writers went
4 → 8, because work that eventually timed out still occupied a proof slot.

**Consequence for large targets:** 100 concurrent writers implies roughly
**50 prover replicas / ~100 CPUs** of proving, before Guardian or the load
generator. That is a hardware question, not a tuning question.

### The public prover is not a capacity plan

The shared network prover served **2 writers at 17/17** and **16 writers at
5/273** `[D]`, and its capacity changed by an order of magnitude across a few
days. Fine for development and low concurrency; unusable as the basis of a
measurement or a production concurrency guarantee. Run your own:
[Self-hosted Miden prover](./guides/local-prover/README.md).

### What to expect

Canonicalization latency (accepted → canonical) is **stable and well inside the
30s design target**, and flat from 2 to 16 concurrent writers:

| Writers | Prover | Canonicalization p95 `[D]` |
|---|---|---|
| 2 | in-process | 8,165ms |
| **4** | **self-hosted, 2 × 2 CPU** | **8,084ms** |
| 8 | in-process | 8,261ms |
| 16 | shared network | 8,087ms |

Four independent configurations within 180ms of each other. Treat **~8s p95** as
the expected canonicalization latency at low-to-moderate writer counts on a
testnet-backed deployment; the dominant term is chain inclusion, not Guardian.

**TODO(authoritative)**: canonicalization p95 at 100 writers.

## Read path

### Reads are database-bound

`get_state` never touches Miden. On a stack with 2 × 1 CPU servers and a
1.5 CPU Postgres `[D]`:

| Concurrent readers | Throughput | p50 | p95 | p99 |
|---|---|---|---|---|
| 128 | **2,314/s** | 52ms | **101ms** | 165ms |
| 256 | 2,151/s | 109ms | 221ms | 344ms |
| 768 | 2,090/s | 371ms | 760ms | 1,429ms |

Throughput is flat-to-falling as concurrency rises — the system is past its knee,
and additional readers queue rather than get served. Postgres pegged at ~165% of
its cap in every leg while both servers idled at 65–75%.

**Latency is not the scarce resource; database throughput is.** Read latency has
roughly an order of magnitude of headroom against a 1s target at these rates.

### Authentication is 43.5% of read-path database time

Every authenticated read performs a durable `UPDATE` — the replay-protection
compare-and-set that makes each request timestamp usable exactly once `[D]`:

| Query | Share of DB time |
|---|---|
| `SELECT states` (the actual read) | 56.5% |
| `UPDATE account_metadata SET last_auth_timestamp` | **33.5%** |
| `SELECT account_metadata` | 10.0% |

Two consequences when sizing:

1. **The read path is write-sensitive at the storage layer.** It consumes IOPS
   and generates WAL and autovacuum churn in proportion to *read* volume. Size
   storage for writes, not for a read-mostly workload.
2. **`get_state` cannot be served by a read replica**, because it writes. The
   standard way to scale reads on RDS is unavailable while this holds.

Removing the write entirely measured **+71% throughput** (2,314 → 3,957/s) and
cut database time per read from 0.385ms to 0.214ms `[D]`. That is the upper
bound of the opportunity, not a promise — and Postgres remained at its CPU cap
afterwards, so this buys headroom rather than moving the bottleneck.

### Concurrent readers ≠ requests in flight

A concurrent reader is a client issuing `get_state` **at a declared rate**, not a
permanently in-flight request. At the default rate of one read per user per 10s,
20,000 readers is **2,000 reads/s**, which at ~100ms latency is about **200
requests in flight** — not 20,000.

Always state the rate alongside the user count. The two readings differ by more
than an order of magnitude and quietly change what is being claimed.

**TODO(authoritative)**: `get_state` p95 at 20,000 concurrent client
connections, as opposed to at the equivalent offered rate.

## Guardian server sizing

Server CPU was never the binding constraint in any measurement. Replicas exist
for availability and for spreading connection load, not for read throughput —
every replica shares one database, so adding them increases database load.

Scale server replicas when: connection counts or TLS/gRPC handling saturate a
task, or availability requires it. Not because reads are slow.

## Database sizing

The database is the read path's ceiling, so this is where sizing effort belongs.

### Storage: use gp3, not gp2

gp2 provisions **3 IOPS per GiB**. A 100 GiB volume gets **300 baseline IOPS**,
burstable to 3,000 by spending a 5.4M-credit bucket — roughly **33 minutes** at
full burst, after which it throttles to 300 until credits rebuild.

That is a cliff, not a slope, and it is invisible to any load test shorter than
the burst window. Given the read path issues a durable write per read, this is
the configuration most likely to bind before CPU does.

**gp3 gives 3,000 IOPS baseline and 125 MiB/s at any volume size**, with no
credit model, and is typically cheaper per GiB. Storage autoscaling does not
substitute — it triggers on free space, not IOPS.

Watch `BurstBalance` in CloudWatch on any gp2 volume. Below 100% under normal
load means credits are already being spent.

### Instance and connections

`db.r6g.large` (2 vCPU / 16 GiB) is a reasonable starting point for the current
target: a 1.5 CPU Postgres sustained 2,314 reads/s `[D]`, above the 2,000/s that
20,000 readers at the default rate implies — but that is **~86% utilisation of a
measured ceiling**, on a read-only workload with nothing else competing.
Production shares the instance with `push_delta`, canonicalization workers, and
dashboard queries.

Connections are not the constraint at these rates. ~200 in-flight requests
against pools of 64 per replica, fronted by RDS Proxy, is comfortable. Size pools
for concurrency actually in flight, not for client count.

Working set is small: ~20,100 accounts at target load fits easily in 16 GiB, so
reads should be cache hits.

### Observability

Enable **Performance Insights** (7-day retention is free). Every diagnosis in
this document came from per-query attribution. Without it, a slow read path can
only be investigated by reproducing it elsewhere.

## How to measure

### Local, for attribution

```bash
cd benchmarks/diagnostic-stack
docker compose --env-file .env --env-file variants/read-budgeted.env up -d
./run-diag.sh <label> profiles/diag-read-128.toml
```

Captures build identity, before/after Prometheus deltas, `pg_stat_statements`,
per-container CPU peaks, and a saturation verdict. See
[`benchmarks/diagnostic-stack/README.md`](../benchmarks/diagnostic-stack/README.md).

### Deployed, for verdicts

```bash
./scripts/run-prod-benchmark-ecs.sh \
  --profile benchmarks/prod-server/profiles/<profile>.toml \
  --workers 16
```

Generates load from sharded ECS workers — **off-box**, which is what makes a
verdict possible.

### Check the run is valid before believing it

A load generator sharing a host with the system under test measures the host.
Each leg writes `saturation.json`; a run whose peak host load exceeded physical
cores is marked `OVERSUBSCRIBED` and carries no verdict.

Container CPU alone will not tell you this — a generator-bound run looks healthy
from inside the containers, because nothing is pushing them.

Two traps worth knowing:

- **Build the generator with `--release`.** A debug build burns several times the
  CPU per request and becomes the bottleneck itself.
- **Latency measured under contention is an upper bound**, not a point estimate.
  Contention inflates latency, so a saturated run's p95 can be quoted as "no
  worse than X" but never as the settled figure.

## What these numbers do not say

- **No verdicts.** Every figure marked `[D]` is diagnostic tier, measured on a
  10-core laptop against Miden testnet. They locate bottlenecks; they do not
  certify a target.
- **Absolute throughput does not transfer** across shapes. The sizing *rules* do.
- **Nothing here measures 100k accounts, 100 concurrent writers, or 20,000
  concurrent client connections.** Those remain open.

## See also

- [`CONFIGURATION.md`](./CONFIGURATION.md) — every environment variable.
- [`PRODUCTION.md`](./PRODUCTION.md) — the supported production shape.
- [Self-hosted Miden prover](./guides/local-prover/README.md) — prover stack and sizing.
- `speckit/features/317-scalability-target-measurement/` — the measurement
  reports these figures come from, including method and caveats.
