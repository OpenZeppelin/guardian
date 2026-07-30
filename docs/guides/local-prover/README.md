# Self-hosted Miden prover

Run your own Miden transaction prover instead of the public one, so multisig
flows do not depend on shared infrastructure and proving CPU becomes a quantity
you declare rather than one you discover.

Use this when:

- a load or benchmark run needs more proving concurrency than the public prover
  serves (it collapses well below ten concurrent writers — see
  [Sizing](#sizing));
- you want proving CPU capped, so proving cannot starve whatever else shares the
  machine;
- you want local development that keeps working when the public prover is down.

The artifacts here are runnable as committed: [`docker-compose.yml`](./docker-compose.yml),
[`Dockerfile`](./Dockerfile), [`Caddyfile`](./Caddyfile).

## Two facts that shape everything below

**A prover instance proves one transaction at a time.** Extra CPU makes a single
proof finish sooner; it never makes two proofs run at once. Concurrency comes
from replicas, and replicas are only reachable through a balancer, because a
client is configured with exactly one prover URL.

**`--capacity` bounds requests in flight, including the one being proved.**
Beyond it the prover answers `ResourceExhausted` immediately instead of queueing
without bound. That is a feature — saturation surfaces as an error rather than
an ever-growing latency tail — but set it below your writer count and the prover
rejects work it has the CPU to do.

## Run it

```bash
cd docs/guides/local-prover
docker compose up -d --build
docker compose logs prover-a | tail -2
```

Expect:

```
INFO Proof server listening server_timeout=2m server_capacity=16 proof_kind=transaction server_port=50051
```

The first build compiles the prover from source and takes several minutes; it is
cached afterwards.

Point a client at the **proxy**, not at a replica:

```toml
# benchmarks/multisig-e2e/*.toml
prover = "http://127.0.0.1:50061"
```

The multisig SDK takes the same choice directly:

```rust
use miden_multisig_client::{MultisigClient, ProvingMode};

let client = MultisigClient::builder()
    .proving_mode(ProvingMode::Service("http://127.0.0.1:50061".to_string()))
    // ...
    .build()
    .await?;
```

`ProvingMode::Local` proves in-process with no container at all — fewer moving
parts and faster, but uncapped: see [Sizing](#sizing) for why that matters.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `PROVER_CAPACITY` | `16` | Requests in flight per replica, including the one being proved. At or above concurrent writers per replica. |
| `PROVER_TIMEOUT` | `120s` | A proof outliving this is abandoned. Above the slowest proof the host produces under load. |
| `PROVER_CPUS` | `2` | CPU limit per replica. |
| `PROXY_CPUS` | `1` | CPU limit for the balancer. |
| `PROVER_PORT` | `50061` | Host port for the proxy. |

## Sizing

Budget `replicas × PROVER_CPUS` against physical cores, leaving room for
everything else on the host. Measured on a 10-core machine with two replicas at
2 CPU each, driving the multisig benchmark against Miden testnet:

| Writers | Result | Binding constraint |
|---|---|---|
| **4** | **68 of 68**, canonicalization p95 8,084ms | none — headroom |
| 8 | 23 of 62 | proof timeouts: 2 proof slots cannot serve 8 writers |
| 8, `capacity=4` | 20 of 159 | `ResourceExhausted`: capacity below writer count |

Both provers sat at ~200% CPU (their limits) with host load 5–7, so in the
8-writer case the machine was healthy and *proving throughput* was the ceiling —
roughly one concurrent proof per replica. Two replicas sustained four writers
with room to spare and failed eight.

### Do not buy slots by shrinking replicas

Since a replica proves one transaction at a time, more replicas look like more
concurrency. Tested at a constant 4-CPU budget, 8 writers:

| Layout | Slots | Result | Completed/min | execution p50 |
|---|---|---|---|---|
| **2 × 2 CPU** | 2 | **23 of 62** | **4.6** | 12,160ms |
| 4 × 1 CPU | 4 | 1 of 49 | 0.2 | 31,778ms |

**Per-proof latency dominates.** Halving cores per replica raised execution p50
by 2.6×, worse than the 2× a linear model predicts, so four slots each far
slower is a net loss — the queue simply reaches the timeout sooner. Host load
also rose (5–7 → 8–9) from the extra containers. Keep replicas at **2 CPU or
more** and add replicas only when there are cores to give them; on a host
without spare cores, fewer writers is the answer, not thinner provers.

**The trade-off against `ProvingMode::Local`.** In-process proving served 8
writers at 104 of 105 operations, because it used every core it wanted: host
load hit 28 on 10 cores. That is faster, and it is also why the resulting
numbers are suspect — a starved Guardian is being measured by a starved
generator. Capped containers keep the accounting honest at the cost of
supporting fewer writers per host. Pick deliberately:

- **Measuring Guardian** → containers, and keep total container CPU under
  physical cores.
- **Just needing proofs** (development, smoke tests) → `ProvingMode::Local`.

## Troubleshooting

| Symptom | Cause |
|---|---|
| `GLIBC_2.38 not found`, container restart-loops | Runtime image older than the builder. The `rust:1.93` images are trixie; a `bookworm-slim` runtime fails at exec. Keep both on the same Debian release. |
| Proofs fail against a prover that starts fine | Version skew. The prover and client must agree on the `miden-tx` line — `miden-remote-prover 0.15.2` matches `miden-client 0.15.x`. The older `miden-proving-service` crate is the pre-rename package on the `miden-tx 0.9` line and cannot serve this client. |
| `Some resource has been exhausted` | `PROVER_CAPACITY` below concurrent writers. Raise it, or add replicas. |
| Proof timeouts under load, provers pinned at their CPU limit | Not enough proof slots. Add replicas — more CPU per replica will not help, since each proves one at a time. |
| One replica idle at 0% CPU | The client is pointed at a replica instead of the proxy. |
| Stack reports `Up`, every proof fails with `failed to connect to the remote prover`, all replicas at 0% CPU | A `docker compose up` that lost a port bind leaves the container *created* with no network attached, and a later `up` **starts** that broken container rather than recreating it. `docker port <container>` returns empty. Fix with `docker compose up -d --force-recreate proxy`. |

## See also

- [`MULTISIG_SDK.md`](../../MULTISIG_SDK.md) — the SDK surface, including `ProvingMode`.
- [`benchmarks/multisig-e2e/README.md`](../../../benchmarks/multisig-e2e/README.md) — the scale runner that consumes `prover = "<url>"`.
- [Horizontal scaling](../horizontal-scaling/README.md) — the same h2c round-robin pattern in front of Guardian replicas.
