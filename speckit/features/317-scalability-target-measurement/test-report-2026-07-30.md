# Test Report — Issue #317 Scalability Target (day 2)

**Date**: 2026-07-30
**Branch**: `317-scalability-target-measurement`
**Tier**: Diagnostic (local, instrumented) — see [spec.md](./spec.md) FR-017a
**Supersedes**: [`test-report-2026-07-29.md`](./test-report-2026-07-29.md) on the write path only.
Yesterday's read-path findings and defect list stand as written.

> **This report carries no authoritative target verdicts.** Everything below was
> measured on a local stack against live Miden testnet, on a 10-core laptop.
> Where a number is not trustworthy, it says so.

---

## 1. What changed since yesterday

Yesterday's report attributed the write-path ceiling to "prover saturation" and
recommended a local prover as one of five next steps. Both were right in
direction and wrong in detail. The specific findings:

1. **The ceiling is serialisation, not load.** A Miden prover instance proves
   **one transaction at a time**; `--capacity` bounds queued requests, not
   parallelism. This is why testnet served 2 writers perfectly and collapsed at
   16 — a hard serialisation point, not aggregate pressure.
2. **Four client-side classifier defects** were manufacturing failures that were
   never real. Fixing them removed 306 of 645 failures from a single run without
   improving throughput by a single operation.
3. **A clean measurement now exists**: 4 writers against a self-hosted prover,
   **68 of 68 operations, canonicalization p95 8,084ms**, zero censoring, host
   not saturated.
4. **96 auth failures in one run were a local artifact** — Docker Desktop VM
   clock drift, not Guardian.

---

## 2. The headline measurement

| Criterion | Target | Measured | Status |
|---|---|---|---|
| Canonicalization p95 | ≤ 30,000ms | **8,084ms** at 4 concurrent writers | **PASS** (diagnostic tier) |
| Auth-window failures | 0 | **0** over 68 operations | **PASS** (diagnostic tier) |
| `get_state` p95 at 20k readers | ≤ 1,000ms | not measured | **NOT MEASURED** |

```
writers 4/4 | operations 68 (68 ok, 0 failed)
canonicalization ms: p50 5284  p95 8084  max 10086  (68 samples)
```

Every accepted delta reached canonical. **Zero censoring** — the first
multi-writer write-path run in this feature where the percentile is a settled
figure rather than a lower bound. Host load 5–7 on 10 cores; both provers at
their declared 2-CPU limits.

**The figure reproduces across four independent configurations:**

| Run | Writers | Prover | Canonicalization p95 |
|---|---|---|---|
| 2026-07-29 | 16 | testnet (shared) | 8,087ms |
| 2026-07-30 | 2 | in-process | 8,165ms |
| 2026-07-30 | 8 | in-process | 8,261ms |
| **2026-07-30** | **4** | **self-hosted** | **8,084ms** |

Different provers, different concurrency, four runs within 180ms of each other.
Canonicalization latency is a stable property of this system at these loads, and
it sits at roughly **27% of the 30s target**.

---

## 3. Why the write path stops where it does

### 3.1 One proof at a time

From the prover's own source (`miden-remote-prover 0.15.2`):

> *the prover only proves one request at a time; the rest are queued. This
> capacity is used to limit the number of requests that can be queued at any
> given time, and includes the one request that is currently being processed.*

Extra CPU makes a single proof finish sooner. It never makes two proofs run at
once. **Concurrency comes only from replicas.**

### 3.2 Raising capacity does not help — it changes the symptom

Measured, same hardware, same 8 writers:

| Capacity | Result | Failure mode |
|---|---|---|
| 4 | 20 of 159 | `ResourceExhausted` — rejected on arrival |
| 16 | 23 of 62 | proof timeouts — accepted, then queued past 120s |

The wall is identical; only its shape changes. Raising the timeout as well would
convert timeouts into completions at ever-growing latency, which lowers the
delta arrival rate — quietly reducing the load being applied rather than
surviving it.

One thing capacity does **not** corrupt: the criterion itself. The
canonicalization clock starts after `execution_ms` is recorded
(`scale.rs`), so prover queue time is charged to execution, not to
canonicalization.

### 3.3 Over-driving a serialised resource collapses throughput

| Writers | Completed ops/min (2 replicas) | Per replica |
|---|---|---|
| 4 | **13.6** | 6.8/min |
| 8 | 4.6 | 2.3/min |

Doubling writers cut completed throughput by **66%**: work that eventually timed
out still occupied a proof slot on the way. Congestion collapse, not degradation.

### 3.4 More slots at the same CPU makes it worse

Since a replica proves one at a time, more replicas look like more concurrency.
Tested at a constant 4-CPU budget, 8 writers:

| Layout | Slots | Result | Completed/min | execution p50 |
|---|---|---|---|---|
| **2 x 2 CPU** | 2 | **23 of 62** | **4.6** | 12,160ms |
| 4 x 1 CPU | 4 | 1 of 49 | 0.2 | 31,778ms |

Per-proof latency dominates slot count: halving cores per replica raised
execution p50 by 2.6x, worse than the 2x a linear model predicts, so four much
slower slots reach the timeout sooner than two fast ones. All four replicas were
pegged at ~100% of their 1-CPU limits, so the comparison is real rather than an
artefact of idle capacity. Keep replicas at 2 CPU or more.

An earlier attempt at this comparison returned 0 of 246 with **all four replicas
at 0.00% CPU** -- a compose port collision had left the proxy container created
with no network attached, and a subsequent `up` started the broken container
instead of recreating it. The run looked healthy from `docker compose ps`. Only
the per-replica CPU sampler distinguished "this layout is terrible" from "this
experiment never ran", which is the same class of error as the read-path legs
and the auth-window matcher: a measurement that cannot tell failure from
absence.

### 3.5 What 100 writers would cost

Two replicas sustain four writers. Linearly, **100 writers ≈ 50 prover replicas
≈ 100 CPUs**, before Guardian and the generator. This is not a laptop question;
it belongs to the authoritative tier. The diagnostic tier can deliver the
Guardian curve up to where the hardware runs out, which is what it is for.

---

## 4. Four classifier defects — one bug class, found four times

Each was a substring matcher pinned to a rendering that had drifted, guarded by
a test encoding the *old* rendering, so it stayed green while measuring nothing.

| # | Where | Missed | Cost |
|---|---|---|---|
| 1 | `prover.rs` `is_transient` | `connection error: i/o timeout` — no arm matched | 424 ops, zero retries |
| 2 | `error.rs` `From<ClientError>` | flattened the typed status to a string, so every `ResourceExhausted` read as permanent | the whole 429 storm |
| 3 | `scale.rs` `is_auth_window` | matched `outside allowed window`, which the server no longer puts on the wire | criterion reported PASS with 96 real failures |
| 4 | `error.rs` `is_transient_miden_rpc` | `Cancelled`, `Unknown`-with-transport, and note-transport errors carrying no status | 306 of 645 ops in one run |

Fixing #1 dropped its family from 158 to 10. Fixing #4 dropped its three
families from 306 to 3. **Neither improved throughput** — pressure simply moved
to the next real constraint. That is the point: the harness now reports
infrastructure limits that exist, instead of limits it invented.

Defect #3 is the most serious, because it inverted a verdict. Re-scoring all 19
historical runs under the fixed matcher: exactly one run was mis-scored (the
64-writer leg, 96 failures reported as 0), and all 18 others genuinely had zero.

### 4.1 The wire cannot distinguish expiry from a real auth failure

`GuardianError::AuthenticationFailed` carries the drift detail, but
`error.rs` maps every instance to one fixed message under one code
(`authentication_failed`). The harness therefore counts **all** authentication
failures — it cannot under-report, which is the right bias for a target of zero.
A distinct server-side code (`authentication_expired`) would make the criterion
meaningful for every client; filed as a follow-up.

---

## 5. The 96 auth failures were a local clock artifact

Server logs carry the measurement:

```
Request timestamp outside allowed skew window
  request_timestamp=1785365545594  server_now_ms=1785364623501
  time_diff_ms=922093   max_skew_ms=300000
```

The client timestamp was **922 seconds ahead of the server** — the Docker
Desktop VM clock running ~15 minutes behind the host. Timestamps are minted per
request (`crates/client/src/client.rs:90`) and these failed in ~260ms, so
nothing aged in a queue. Checked afterwards, the same clocks agreed to within 1
second, and a later run with 0s skew produced zero auth failures.

**Not a Guardian defect, and it would not occur against ECS.** It does invalidate
client-vs-server time comparisons for that window. A host-vs-container clock
assertion belongs in `run-diag.sh` alongside the saturation check — both are
"self-label the invalid run" gaps.

---

## 6. CPU budget: the trade-off is now explicit

| Setup | 8 writers | Host load (10 cores) | Verdict |
|---|---|---|---|
| In-process proving | 104 of 105 | **28.0** | fast, but a starved generator measuring a starved Guardian |
| Self-hosted, capped | 23 of 62 | 5–7 | honest accounting, fewer writers per host |
| Self-hosted, capped, **4 writers** | **68 of 68** | 5–7 | **the quotable number** |

In-process proving cannot be capped; a container takes a `cpus:` limit, which is
what turns proving capacity into a quantity you declare. Guidance and sizing:
[`docs/guides/local-prover/`](../../../docs/guides/local-prover/README.md).

---

## 7. Defects found (beyond the four classifiers)

- **Upstream `miden-client` panic**, `transaction/mod.rs:365`: an `expect` on
  note metadata in the already-consumed check fired and killed a writer thread
  under ordinary concurrent use. The harness correctly reported 7 of 8 writers
  and failed the concurrency criterion rather than summarising 8.
- **Wrong crate is easy to pick**: `miden-proving-service` (0.9.4) is the
  pre-rename package on the `miden-tx 0.9` line. The current one is
  `miden-remote-prover`; 0.15.2 matches `miden-client 0.15.x`. The wrong one
  starts cleanly and then fails to prove, which reads as a client bug.
- **Version note**: the benchmark links **miden-client 0.15.0**, while
  `miden-multisig-client` is versioned 0.16.0.

---

## 8. What is still not measured

| Target | Status | Blocked by |
|---|---|---|
| `get_state` p95 at 20k readers | not measured | generator-bound tooling — **not** the prover |
| 100 concurrent writers | not measured | ~50 prover replicas; authoritative tier |
| 100,000 guarded accounts | 100 provisioned | fixture scale |
| Authoritative replay | not run | AWS access |

**The read path needs no prover.** Reads generate no proofs, so nothing in this
report's write-path ceiling applies to the one criterion that remains completely
unmeasured. It is blocked only by the saturation check in `run-diag.sh`, which
samples container CPU and never host load or the generator — which is why all
five read legs were invalid. That remains the highest-value next fix.

---

## 9. Recommended next steps

1. **Fix the saturation check** (host load + generator process), then re-run the
   read path. Unblocks the only entirely unmeasured criterion, on hardware that
   already exists.
2. **File the server nonce-consumption bug** as its own issue. Fixed in-tree via
   a partial unique index; it is a correctness defect independent of
   benchmarking and deserves separate review.
3. **Add an `authentication_expired` code** so §4.1's criterion can stop
   over-reaching.
4. **Report the `miden-client` panic** upstream.
5. Authoritative replay once AWS access is available.

---

## 10. Artifacts

Runs referenced here (`benchmarks/multisig-e2e/reports/`):

| Stamp | Config | Result |
|---|---|---|
| `scale-20260730T080127Z` | 8 writers, 4 x 1 CPU provers | 1/49 — per-proof latency dominates |
| `scale-20260730T065445Z` | 4 writers, self-hosted prover | **68/68**, p95 8,084ms |
| `scale-20260730T064825Z` | 8 writers, self-hosted, capacity 16 | 23/62, proof timeouts |
| `scale-20260730T063645Z` | 8 writers, self-hosted, capacity 4 | 20/159, `ResourceExhausted` |
| `scale-20260730T061008Z` | 8 writers, in-process | 104/105, host load 28 |
| `scale-20260730T060341Z` | 2 writers, in-process | 20/20, p95 8,165ms |
| `scale-20260730T052746Z` | 64 writers, all classifiers fixed | 0/353, 96% prover queue |
| `scale-20260729T230836Z` | 64 writers, prover fix only | 1/645 |
| `scale-20260729T213814Z` | 64 writers, before fixes | 0/446 |

Stack: [`docs/guides/local-prover/`](../../../docs/guides/local-prover/README.md).
How to run: [`benchmarks/multisig-e2e/README.md`](../../../benchmarks/multisig-e2e/README.md).
