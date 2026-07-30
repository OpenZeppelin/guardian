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
5. **The read path is no longer unmeasured.** `get_state` p95 is **≤ 101ms** at
   2,314 reads/s — above the 2,000 reads/s the target actually asks for
   (FR-003a) — but the margin is thin and the constraint is Postgres, where
   **43.5% of database time is authentication**. Removing the per-read auth
   write measured **+71% throughput**, and it is what stops `get_state` from
   being served by a read replica (§7).
6. **Reads and writes were measured together for the first time**, and did not
   disturb each other — at 640 reads/s against 4 writers, which is a third of
   the target read rate and a twenty-fifth of the writer count. Getting there
   required a paced load model, without which the generator alone saturated the
   host. These are also the **only four legs in this report that passed the
   saturation check** (§8).

---

## 2. The headline measurement

| Criterion | Target | Measured | Status |
|---|---|---|---|
| Canonicalization p95 | ≤ 30,000ms | **8,084ms** at 4 concurrent writers | **PASS** (diagnostic tier) |
| Auth-window failures | 0 | **0** over 68 operations | **PASS** (diagnostic tier) |
| `get_state` p95 at 20k readers (= 2,000 reads/s, FR-003a) | ≤ 1,000ms | **≤ 101ms** at 2,314 reads/s | **BOUNDED, NOT VERDICT** (§7) |

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

## 7. Read path — measured, bounded, and Postgres-bound

Reads generate no proofs, so none of the write-path ceiling above applies here.
This is the criterion that was completely unmeasured yesterday.

### 7.1 What it ran against

Load generator (native macOS process, release build) -> Caddy proxy (h2c
round-robin) -> two GUARDIAN replicas -> shared Postgres, all on one 10-core
laptop with the server side inside Docker Desktop's VM.

| Component | CPU limit |
|---|---|
| server-a, server-b | 1.0 each |
| postgres | 1.5 |
| proxy | 1.0 |
| prometheus, grafana | 0.25 each |
| **total containers** | **5.0 of 10 cores** |

Build `0.16.0 / 5d3999eadbf0`, Postgres backend, state and metadata pools 16
each, `GUARDIAN_MAX_REPLICAS=2`. Workload: closed-loop `get_state`, 100% ECDSA,
effectively read-only. This is **not** the production shape (Terraform's real
default is 0.5 vCPU per task against RDS), so the absolute throughput describes
this stack only; what transfers is where the limit sits.

### 7.2 Results

| Readers | Throughput | p50 | p95 | p99 |
|---|---|---|---|---|
| 768 | 2,090/s | 371ms | 760ms | 1,429ms |
| 256 | 2,151/s | 109ms | 221ms | 344ms |
| **128** | **2,314/s** | **52ms** | **101ms** | 165ms |

Throughput is flat-to-falling as concurrency rises -- the definition of a
saturated system. Past the knee, extra readers queue rather than get served:
6x the readers buys 7.5x the p95 and no additional work. **The read ceiling of
this shape is ~2,300/s.**

Zero read failures across 655,000+ operations in the three legs.

Because contention inflates latency rather than reducing it, the latency figures
are valid **one-sided bounds**: `get_state` p95 at 128 concurrent readers is
**<= 101ms**, an order of magnitude inside the 1,000ms target. A host with spare
cores can only do better.

### 7.3 The bottleneck is authentication, and a third of it is a write

Postgres pegged at ~165% of its 150% cap in every leg while both servers idled at
65-75% of theirs. **GUARDIAN was never the constraint.** From
`pg_stat_statements` on the 128-reader leg:

| Query | Calls | Total ms | Share of DB time |
|---|---|---|---|
| `SELECT states` (the actual read) | 280,100 | 59,687 | 56.5% |
| **`UPDATE account_metadata SET last_auth_timestamp`** | 280,100 | 35,368 | **33.5%** |
| `SELECT account_metadata` | 280,264 | 10,583 | 10.0% |

**43.5% of database time on the read path is authentication, and 33.5% is a
durable write issued on every single read.** The write is the replay-protection
compare-and-set at `crates/server/src/services/mod.rs:163` -- a correctness
feature, not an oversight: it is what makes a captured request unusable twice.
But it turns a read-only workload into 280,100 UPDATEs and is why Postgres
saturates while the servers idle.

This confirms issue #317's bottleneck #2 with far stronger evidence than the
~33% estimate in yesterday's report.

### 7.4 Distance to the target: closer than it first appears

**Correction.** An earlier draft of this section applied Little's law to 20,000
*in-flight* requests and concluded the target needed ~20,000 req/s. That is the
closed-loop reading FR-003b explicitly forbids: "peak in-flight request
concurrency MUST be measured and reported as an observed output of every run,
never used as the definition of the user count."

The target's own arithmetic (FR-003a): one `get_state` per user per 10 seconds,
so 20,000 concurrent readers is **2,000 reads/s sustained** -- an order of
magnitude below what the closed-loop reading implies. At ~100ms latency that is
roughly **200 requests in flight**, not 20,000.

Against that number this shape is not short at all. It sustained **2,314/s with
the auth write and 3,957/s without**, on 1.5 CPU of Postgres. For reference, the
spec records April sustaining 1,409 reads/s at p95 926ms on a single task.

The honest reading is therefore *thin margin*, not an order-of-magnitude gap:
2,000/s against a measured ceiling of ~2,300/s is ~86% utilisation, on a
read-only workload with nothing else competing. Production shares the same
database with `push_delta`, the canonicalization workers, and dashboard queries.
Reducing the per-read auth write moves that utilisation to roughly 50% (§7.7).

### 7.5 The per-read write pins reads to the primary

The consequence that outweighs the throughput number: because `get_state`
performs a durable `UPDATE`, it **cannot be served by a read replica**. Adding
read replicas -- the standard, cheap way to scale a read path on RDS -- does
nothing for the read path as it stands.

Removing or relocating that write makes `get_state` replica-eligible, which is a
larger structural unlock than any single-instance tuning. On the production shape
(`db.r6g.large`, 2 vCPU, behind RDS Proxy) this matters more than instance size:
the proxy already handles the connection side comfortably, since 2,000 reads/s at
~100ms is ~200 concurrent database-side requests, not 20,000.

### 7.6 No leg passed validity

Every leg was flagged `OVERSUBSCRIBED` (peak host load 12.4-17.4 on 10 cores),
including one deliberately budgeted to 5 of 10 cores and one with the generator
bounded to 4 tokio worker threads. On Docker Desktop the host load also counts
the VM threads doing container work, so this machine cannot both drive ~2,300
req/s and stay under its core count.

So §7.2 and §7.3 are **bounds and attribution, never verdicts**. A clean read
measurement needs the generator off-box, which is what the authoritative tier's
distributed harness exists for. That is now a specific, evidenced requirement
rather than a preference.

### 7.7 Measured: removing the write buys 71%

§7.3 argues from attribution that the auth write is the constraint. To
demonstrate causation rather than correlation, the CAS was patched out locally,
the server rebuilt (tagged `NOCAS-EXPERIMENT` in `/status` so the artifacts can
never be mistaken for a real leg), and the identical 128-reader profile re-run.
The patch was reverted immediately afterwards; it is a measurement device, not a
candidate change -- it disables replay protection.

| | With CAS | Without CAS | Change |
|---|---|---|---|
| Throughput | 2,314/s | **3,957/s** | **+71%** |
| p50 | 52ms | 27ms | -48% |
| p95 | 101ms | 66ms | -35% |
| p99 | 165ms | 101ms | -39% |
| Postgres peak CPU | 165.7% | 158.9% | ~unchanged |

| | Total DB time | Reads served | DB time per read |
|---|---|---|---|
| With CAS | 107,951ms | 280,100 | **0.385ms** |
| Without CAS | 102,538ms | 476,860 | **0.214ms** |

Near-identical total database time doing **70% more work**: a 44% drop in DB cost
per read. That exceeds the 33.5% the `UPDATE` consumed directly, because removing
it also removes the MVCC churn it caused -- the state `SELECT` itself sped up
from 0.213ms to 0.178ms mean on reduced bloat alone.

Two caveats on how far this generalises:

- This is the **upper bound** on the opportunity. The recommended fix keeps the
  write and only makes it cheaper, so it recovers a fraction of this, not all of
  it. Quoting +71% as the expected result of the migration would overclaim.
- **Postgres stayed pegged** (158.9% of its 150% cap) with the write gone, which
  refutes the prediction that it would drop below cap. Removing the write does
  not relocate the bottleneck; it lets the same bottleneck serve 71% more reads.
  The read path needs database capacity either way.

### 7.8 Three harness defects fixed to get here

- **The generator was a debug build.** `run-diag.sh` called `cargo run` with no
  `--release`, and the workspace sets no `[profile.dev]` overrides. Every read
  leg ever run drove load with an unoptimized binary -- plausibly the largest
  single reason the generator kept being the bottleneck.
- **The saturation check sampled containers only.** It now samples host load in
  the same tick and writes `saturation.json` with an explicit verdict, printing
  a `WARNING` and touching a `SATURATED` marker file. This is what made all five
  earlier read legs look valid while the host ran at load 15.9.
- **The verdict could silently vanish.** The sampler's records span two lines
  (`docker stats` emits its own newline), so the file was never line-delimited
  JSON, and killing the sampler truncates the final record -- which made `jq -s`
  reject the entire file, costing both the CPU peaks and the verdict. Now parsed
  with streaming `jq -c .`, keeping every complete record and stopping at the
  tail.

---

## 8. Read and write together — no interference at these rates

Every measurement above isolates one path. The #317 target is both at once
against one deployment, and reads and writes share a single Postgres, so
isolation is exactly the assumption that needed testing.

### 8.1 Pacing, and why it was required first

The first attempt failed validity before it began. A closed-loop 64-reader leg
produced 2,318/s — the same 2,314/s as 128 readers in §7.2, confirming the
ceiling is server-side — at **peak host load 16.65 on 10 cores**, with no
writers running. Containers accounted for only ~4.4 of that; the generator was
the rest.

Reducing reader count would not have helped. Past the knee, latency falls and
the rate holds, so generator cost tracks **throughput**, not concurrency. The
harness had no way to ask for less load than the server's ceiling: `run_worker`
looped with no think time, so `users` was the only knob and past saturation it
stopped being one.

`LoadModel` now makes the load model explicit in every profile — `closed_loop`
(the §7 in-flight-saturation model, FR-003c) or `paced` (the target's model,
FR-003/FR-003a). Three details decide whether a paced generator is honest:

- **Sleep to the next scheduled instant**, not for `interval` after the response.
  Sleeping after the response decays the offered rate as latency rises, so a run
  silently offers least load exactly when the server is struggling —
  coordinated omission.
- **Phase-stagger each user** across one interval, or the population shares a
  schedule and offers `users`-wide spikes once per interval instead of a rate.
- **Count slipped ticks**, so a generator-bound run declares itself rather than
  passing as a server that kept up.

The same 64 readers, paced at one read per 100ms, offered **640/s at peak host
load 5.68** — a third of the CPU for a third of the load, which is what left
room for the write harness on the same machine.

*(First cut of the slip counter flagged any lateness, including sub-millisecond
timer jitter, and reported 1.22% on a leg whose offered rate exactly matched its
declared rate. A validity flag that fails healthy runs trains you to ignore it.
A slip is now a tick late by more than a whole interval — real overload falls
behind a fixed schedule monotonically.)*

### 8.2 What it ran against

Same `read-budgeted` stack as §7 — 5.0 of 10 cores, two replicas, Postgres at
1.5 — build `0.16.0 / 79efe93237d9`.

Two deliberate choices:

- **Remote prover, not self-hosted.** Two prover replicas plus the proxy is
  5.0 CPU, which with the stack leaves nothing for two generators. Dropping them
  bought back the write concurrency. The trade is that shared prover capacity
  drifts, which §8.4 tests rather than assumes.
- **4 writers, not 8.** Eight failed 39 of 62 on proof timeouts standalone
  (§3.2), so a leg built on it would measure proof-timeout variance rather than
  read contention. Four is the count already shown to run clean (68/68 against a
  self-hosted prover, §2), and it held here on the remote one too — all three
  write legs completed 100% of their operations, which is what makes any
  degradation unambiguous.

Four legs, A-B-A ordered so prover drift is detectable rather than absorbed:
reads alone, writes alone, both, writes alone again.

### 8.3 Results

**Read side** — 76,800 operations per leg, zero failures in both:

| | Reads alone | Reads + 4 writers |
|---|---|---|
| Offered rate | 640/s | **640/s** |
| p50 / p95 / p99 / max | 1 / 3 / 33 / 506ms | **1 / 2 / 6 / 47ms** |
| Slipped ticks | 0.68% | **0%** |

Reads were not degraded. They were marginally *better*, almost certainly warm
buffers from the preceding legs rather than an effect of the writers — but the
direction rules out read-side harm.

**Write side** — 4 writers, all legs 100% success (53/53, 51/51, 50/50), zero
auth-window failures:

| Stage | Leg 2 alone | Leg 3 + reads | Leg 4 alone | Spread |
|---|---|---|---|---|
| `proposal` p50 | 310ms | 280ms | 312ms | — |
| **`execution` mean** | **18,923ms** | **18,905ms** | **18,917ms** | **0.1%** |
| `canonicalization` mean | 4,603ms | 5,047ms | **5,346ms** | — |

**`execution` is the control** — the stage the prover owns, which reads cannot
touch. A 0.1% spread across three legs says the shared prover held steady, so
nothing in the other stages is prover drift.

**Server side**, peak container CPU:

| | Reads alone | Writes alone | Combined |
|---|---|---|---|
| postgres (cap 150%) | 33.8% | 6.2% | **49.1%** |
| peak host load | 5.68 | 4.74 | **4.60** |

All four legs carry `saturation: ok`. Postgres load is close to additive with no
contention penalty, and stays a third of its cap.

### 8.4 The one number that moved, and why it is not read load

Canonicalization p95 rose 7,066 → 8,099ms in the combined leg, +15%. Leg 4
settles it, and against the initial reading:

| Leg | Config | canon mean | share ≥ 6s |
|---|---|---|---|
| 2 (22:24) | writes alone | 4,603ms | 13.2% |
| 3 (22:34) | writes + 640 reads/s | 5,047ms | 37.3% |
| 4 (22:43) | **writes alone** | **5,346ms** | **36.0%** |

**Leg 4 has no readers and is the slowest of the three.** Were reads the cause,
the combined leg would exceed the write-only legs; instead the write-only leg
run last is worse on both mean and tail share. Legs 3 and 4, adjacent in time,
agree at 37.3% vs 36.0% while Leg 2 sits apart at 13.2%. The variation is
chronological — consistent with testnet chain conditions — not with read load.

Comparing percentiles was the wrong instrument here. Canonicalization is
measured by polling every `poll_interval_ms = 1000`, so values are **quantized
to 1s buckets**, and at n≈50 a percentile rests on two or three observations.
The distributions are the honest view and show Legs 3 and 4 as one population.

### 8.5 What this establishes, and what it does not

**Establishes:** at 640 reads/s against 4 concurrent writers on a shared
Postgres, neither workload measurably degrades the other. Reads unaffected on
throughput and latency; writes unaffected on all three stages once chronological
drift is accounted for; database load additive with the primary at a third of
its cap.

**Does not establish** that the combined dimension is met, for three reasons:

1. **Rate.** 640 reads/s is under a third of the 2,000/s target (§7.4), and
   4 writers is far below 100. Contention appears at saturation, and both sides
   ran well below it.
2. **Asymmetry.** The write path issued roughly 2.5 queries/s against the read
   path's ~1,900/s — 250:1. Writes at this scale have too little database work
   to contend with anything, so "writes do not degrade reads" was close to
   structurally guaranteed. Only the read→write direction was genuinely tested.
3. **Tier.** Diagnostic, as everything in this report. No target verdict.

The measurement that would close it needs ~2,000 reads/s and ~100 writers
simultaneously, which one 10-core host cannot generate while also hosting the
stack. That is an off-box generator or the deployed environment — the same
blocker as §7.6, now reached from the other direction.

*(Incidental: `pg_stat_statements` shows `SELECT $1` running 273,000 times in
the combined leg, ~3 per `get_state` — a pool liveness check on every checkout.
It cost 188.9ms across the leg, so it is noise at this scale, but it is a fixed
per-query tax that scales with read volume. Worth a look alongside #365, not
before.)*

---

## 9. Defects found (beyond the four classifiers)

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

## 10. What is still not measured

| Target | Status | Blocked by |
|---|---|---|
| `get_state` p95 at 20k readers | **bounded, not verdicted** (§7) | no leg passed the saturation check; needs an off-box generator |
| Reads and writes together, at target rates | **null result at 640 reads/s + 4 writers** (§8) | one host cannot generate 2,000 reads/s and 100 writers while hosting the stack |
| 100 concurrent writers | not measured | ~50 prover replicas; authoritative tier |
| 100,000 guarded accounts | 100 provisioned | fixture scale |
| Authoritative replay | not run | AWS access |

**The read path needed no prover**, and §7 now measures it: reads generate no
proofs, so nothing in this report's write-path ceiling applies to it. What
remains missing is not a measurement but a *clean* one — every leg was flagged
`OVERSUBSCRIBED`, because one laptop cannot both generate ~2,300 req/s and host
the stack being measured. Only an off-box generator closes that.

§8 narrows the combined gap without closing it. Reads and writes were measured
together and did not disturb each other, but at a third of the target read rate
and a twenty-fifth of the writer count — and with the write path issuing so
little database work (2.5 queries/s against ~1,900/s) that one of the two
directions could not have shown an effect. The same off-box generator closes
this row and the one above it.

---

## 11. Recommended next steps

1. **Reduce the per-read auth write.** 33.5% of read-path database time is the
   replay-protection CAS; removing it measured **+71% throughput** (§7.7).
   Two independent reasons it is the highest-leverage change here: it converts
   ~86% utilisation of the measured ceiling into roughly 50%, and it is what
   makes `get_state` ineligible for a read replica (§7.5) — which is the larger
   structural unlock on RDS.
2. **Move the load generator off-box** so a read leg can pass validity, and so
   reads and writes can be driven together at target rates rather than at the
   third-of-target the host allows (§8.5). The saturation check now refuses to
   certify a contended run (§7.6) and the paced load model can now express the
   target's own shape (§8.1); the host is the remaining blocker, not the tooling.
3. **File the server nonce-consumption bug** as its own issue. Fixed in-tree via
   a partial unique index; it is a correctness defect independent of
   benchmarking and deserves separate review.
4. **Add an `authentication_expired` code** so §4.1's criterion can stop
   over-reaching.
5. **Report the `miden-client` panic** upstream.
6. Authoritative replay once AWS access is available.

---

## 12. Artifacts

Runs referenced here (`benchmarks/multisig-e2e/reports/`):

| Stamp | Config | Result |
|---|---|---|
| `scale-20260730T224842Z` | **§8 leg 4** — 4 writers alone, repeat | 50/50, canon mean 5,346ms |
| `scale-20260730T224009Z` | **§8 leg 3** — 4 writers + 640 reads/s | 51/51, canon mean 5,047ms |
| `scale-20260730T222939Z` | **§8 leg 2** — 4 writers alone | 53/53, canon mean 4,603ms |
| `scale-20260730T080127Z` | 8 writers, 4 x 1 CPU provers | 1/49 — per-proof latency dominates |
| `scale-20260730T065445Z` | 4 writers, self-hosted prover | **68/68**, p95 8,084ms |
| `scale-20260730T064825Z` | 8 writers, self-hosted, capacity 16 | 23/62, proof timeouts |
| `scale-20260730T063645Z` | 8 writers, self-hosted, capacity 4 | 20/159, `ResourceExhausted` |
| `scale-20260730T061008Z` | 8 writers, in-process | 104/105, host load 28 |
| `scale-20260730T060341Z` | 2 writers, in-process | 20/20, p95 8,165ms |
| `scale-20260730T052746Z` | 64 writers, all classifiers fixed | 0/353, 96% prover queue |
| `scale-20260729T230836Z` | 64 writers, prover fix only | 1/645 |
| `scale-20260729T213814Z` | 64 writers, before fixes | 0/446 |

Read legs (`benchmarks/diagnostic-stack/results/`):

| Leg | Readers | Result | Peak host load |
|---|---|---|---|
| `leg4-write-only-repeat-20260730T224321Z` | 0 | §8 drift check, observe-only | **3.99** |
| `leg3-combined-20260730T223447Z` | 64 paced | **§8 combined leg** — 640/s, p95 2ms | **4.60** |
| `leg2-write-only-20260730T222409Z` | 0 | §8 write baseline, observe-only | 4.74 |
| `leg1-read-only-20260730T222031Z` | 64 paced | **§8 read baseline** — 640/s, p95 3ms | **5.68** |
| `read-128-t4-20260730T083554Z` | 128 | **2,314/s, p95 101ms** — the tightest bound | 12.79 |
| `nocas-128-t4-20260730T092323Z` | 128 | 3,957/s, p95 66ms — **CAS patched out, experiment only** | 13.25 |
| `read-256-t4-20260730T083138Z` | 256 | 2,175/s, p95 214ms | 14.90 (recomputed) |
| `read-256-20260730T082856Z` | 256 | 2,151/s, p95 221ms | 12.39 |
| `read-budgeted-20260730T082554Z` | 768 | 2,090/s, p95 760ms | 17.39 |

The four `leg*` runs are the §8 combined-load legs and are the **only legs in
this report that passed the saturation check** — all four `ok`, peak host load
3.99-5.68 of 10 cores, which paced load bought (§8.1). The five read legs below
them all exceeded the 10 physical cores. Three of those carry `saturation.json`; the
`read-256-t4` leg is the one whose truncated sampler tail exposed the parsing
defect in §7.6, so its verdict was recomputed by hand rather than written by the
harness.

Stack: [`docs/guides/local-prover/`](../../../docs/guides/local-prover/README.md).
How to run: [`benchmarks/multisig-e2e/README.md`](../../../benchmarks/multisig-e2e/README.md).
