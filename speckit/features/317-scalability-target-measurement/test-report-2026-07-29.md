# Test Report — Issue #317 Scalability Target

**Date**: 2026-07-29
**Branch**: `317-scalability-target-measurement`
**Tier**: Diagnostic (local, instrumented) — see [spec.md](./spec.md) FR-017a

> **This report carries no authoritative target verdicts.** Only the deployed,
> production-shaped deployment measured by the distributed harness may do that.
> Everything below was measured on a local stack against live Miden testnet, on a
> 10-core laptop. Where a number is not trustworthy, it says so.

---

## 1. Summary

Two of the three issue #317 acceptance criteria now have real measurements. The
third does not, and the reason is a measurement limitation rather than a Guardian
one.

| Criterion | Target | Measured | Status |
|---|---|---|---|
| Canonicalization p95 | ≤ 30,000ms | **8,087ms** at 16 concurrent writers | **PASS** (diagnostic tier) |
| Auth-window failures | 0 | **0** across ~2,500 operations | **PASS** (diagnostic tier) |
| `get_state` p95 at 20k readers | ≤ 1,000ms | not measured | **NOT MEASURED** |

| Dimension | Target | Reached | Notes |
|---|---|---|---|
| Concurrent transacting users | 100 | **64** (56% success) | bounded by public testnet infra |
| Concurrent readers | 20,000 | not attempted | needs off-box load generation |
| Guarded accounts | 100,000 | 100 provisioned | funded fixture, reusable |

**Headline**: Guardian canonicalizes a real multisig transaction in **~6–8s at up
to 16 concurrent writers, flat from 2 writers upward**, with zero auth-window
failures. Guardian was never the bottleneck in any run — its CPU peaked at ~60%
of allocation while everything above it saturated.

---

## 2. Write path — the measurable result

Workload: `benchmarks/multisig-e2e scale-run`. Every writer is a funded testnet
account in a ring, looping propose → execute → await-canonical, with real
transactions proved and submitted.

### 2.1 The clean run

16 writers, 240s window, after the fixes in §4:

```
writers 16/16 | operations 243 (243 ok, 0 failed)
canonicalization  p50 6,055ms   p95 8,087ms   max 15,155ms
243 accepted deltas, all reached canonical
auth_window_failures 0
```

All three criteria the write path can address **pass**, with a 3.7× margin under
the canonicalization target and no censoring.

### 2.2 Scaling

| Writers | Ops | Succeeded | Stranded | Canon p50 | Limiting factor |
|---:|---:|---:|---:|---:|---|
| 2 | 41 | 22 | 9 | 7,077ms | remote prover |
| 8 | 137 | 74 | 62 | 7,091ms | remote prover (45%) |
| 16 | 278 | 153 | 125 | 6,066ms | remote prover (45%) |
| **16** (post-retry) | **243** | **243** | **0** | **6,055ms** | — |
| 32 | 1,061 | 199 | 125 | 8,074ms | prover + RPC rate limit |
| 64 | — | — | — | — | aborted during writer init |
| **64** (post-retry) | **571** | **318** | **30** | **12,114ms** | prover + chain inclusion |

Canonicalization is **flat from 2 to 16 writers** (~6–7s) and degrades to ~12s at
64 — consistent with longer time-to-inclusion under more submitted transactions,
not with Guardian slowing down. Container CPU during the 64-writer run: server-a
4–22%, server-b 2–9%, host load ~3.6 on 10 cores.

### 2.3 Why 100 writers was not reached

Two public-infrastructure limits, neither Guardian's:

- **Shared remote prover.** `miden-client`'s `for_testnet()` preset delegates
  proving to `tx-prover.testnet.miden.io`. Failure rate was **45% at both 8 and
  16 writers** — an identical proportion at double the load, i.e. a saturated
  queue. Bounded retry (§4.6) took this to 0% at 16 and ~13% at 64.
- **Node RPC rate limit.** `x-ratelimit-limit: 128`. At 32 writers, 437
  operations were lost to `"Too Many Requests!"`; at 64 the run aborted during
  *initialisation*, because N concurrent client syncs trip the limit before any
  load begins. Backoff (§4.6) smooths bursts but cannot raise the node's ceiling.

Reaching 100 writers requires removing both: a local prover, and a node without
the 128-request cap.

---

## 3. Read path — measured, then invalidated

Workload: `benchmarks/prod-server` synthetic harness against the diagnostic stack.
No prover, no funded accounts.

| Leg | Users | reads/s | p50 | p95 |
|---|---:|---:|---:|---:|
| prod shape, 1 replica | 64 | 2,643 | 22ms | 39ms |
| prod shape, 2 replicas | 64 | 2,426 | 22ms | 49ms |
| prod shape, 2 replicas, Postgres 6 CPU | 64 | 2,477 | 23ms | 46ms |
| saturating, 1 replica | 768 | 2,613 | 277ms | 412ms |
| saturating, 2 replicas | 768 | 2,561 | 276ms | 507ms |

**These numbers do not support any conclusion about Guardian read capacity, and
the 1-vs-2 replica comparison is invalid.**

Throughput is pinned at ~2,600 reads/s across every configuration — 12× more
offered concurrency, double the replicas, triple the Postgres CPU — and does not
move, while latency scales almost exactly 12× with concurrency (22ms → 277ms).
That is queueing against a fixed ceiling. But nothing server-side was saturated:
Guardian ~59% of allocation, Postgres 266% of a 600% limit, pool
`pending_acquires` ≈ 0.

The ceiling was the host: **12 CPU of containers allocated on 10 physical cores**,
with the load generator running unlimited on top, at host load **15.9**. Every leg
is generator-bound.

Two errors produced this, both mine:

1. The first three legs were **under-loaded**. At 64 in-flight readers with 22ms
   latency, Little's law caps throughput at ~2,900/s regardless of server
   capacity, so two configurations were compared while neither worked hard. It
   only surfaced when tripling Postgres CPU changed nothing.
2. `run-diag.sh`'s saturation check samples **containers only**. It never looks at
   host load or the generator process, so these runs would have passed as valid.
   That is the February failure mode — measuring the harness and attributing it to
   Guardian — and the tooling does not yet catch it.

**Consequence**: capacity comparisons need the generator off the server's box.
That is exactly why the spec makes the deployed distributed harness the
authoritative tier. The diagnostic tier is sound for *attribution* of a single
configuration and unsound for capacity.

### 3.1 One read-path finding that does hold

From `pg_stat_statements`, which `run-diag.sh` resets per leg (so this is not
affected by the Prometheus contamination in §4.2):

| Calls | Total ms | Query |
|---:|---:|---|
| 76,695 | 7,421 | `SELECT states…` — the state read itself |
| 76,695 | 2,465 | `UPDATE account_metadata SET last_auth_timestamp…` |
| 76,703 | 1,263 | `SELECT account_metadata…` |
| 230,188 | 275 | `SELECT $1` — pool health checks, 3× per operation |

The replay-protection CAS write plus the auth metadata read total **3,728ms
against 7,421ms for the state read** — roughly **33% of read-path query time spent
on authentication**. This is issue #317's hypothesised bottleneck #2, measured.

Caveat: one leg, 8 users, no repetition. A signal, not a result.
`db_pool_pending_acquires` was 0 throughout, so bottleneck #1 (the 16-connection
pool) was not queueing at this load.

---

## 4. Defects found and fixed

### 4.1 Server — a client-abandoned delta consumed its nonce

**The most serious finding.** After a prover timeout stranded a candidate and the
client abandoned it, the account became **permanently unable to submit anything**
— 1,389 consecutive rejections in one run, reported as *"There's already a pending
change"* while nothing was pending.

`UNIQUE(account_id, nonce)` treated any row at a nonce as settled. Correct for a
**canonical** delta; wrong for a **discarded** one, whose transaction never landed
and therefore never consumed the nonce. Since the on-chain nonce never advanced,
every later attempt targeted the same blocked nonce.

Fixed with a partial unique index — `WHERE status_kind <> 'discarded'` — encoding
the real invariant: at most one *live* delta per (account, nonce). Same canary
afterwards: 0 successful operations → 22, nonce advancing past each abandonment.

Follow-on obligation of that change: `pull_delta` used `first` with no ordering,
so once a nonce could carry several rows the answer became arbitrary. Live rows
now sort ahead of discarded ones.

**Why it was never seen before**: the pre-existing lifecycle benchmark is
sequential and aborts on first failure, so it never abandons a candidate and
retries. Only a concurrent runner that continues past failures reaches that state.

### 4.2 Benchmark — Prometheus snapshots included earlier runs

Counters are cumulative over the server process lifetime and were sampled only
after each leg: the first write leg's snapshot reported ~171,000 `GetState` calls
for a leg that issued **64**. Now captured before and after with per-leg deltas —
verified 74,894 delta against 245,937 cumulative for a leg issuing ~69k.

### 4.3 Benchmark — canonicalization p95 excluded unsettled deltas

Percentiles covered only deltas that reached canonical, so 60 fast and 40
timed-out deltas would have reported p95 = 1s and **passed**. Every accepted delta
now enters the population, with unsettled ones right-censored at the poll bound —
a lower bound on the true p95, so it can understate delay but never manufacture a
pass.

### 4.4 Benchmark — measurement started before writers were ready

The clock started at spawn, before per-writer client load and sync, so slow
writers got shorter windows while results were attributed to full concurrency.
Writers now signal readiness and park on a start gate; the run aborts if any
writer fails to initialise rather than silently measuring fewer. The concurrency
verdict counts writers that produced at least one operation.

### 4.5 Fixture — resume trusted the file over Guardian

A 100-account fixture reported all 100 registered while Guardian held **98**. The
two missing entries predated the run and had been registered against a *different*
Guardian instance; `bootstrap` then failed with `NOT_FOUND`.

Recording a `created`/`registered` state was not enough, because fixtures written
before that field defaulted to `registered` — a claim, not a fact. `prepare` now
reconciles every entry against Guardian, which is the authority. Only a genuine
`NOT_FOUND` counts as unregistered; transport errors propagate, because misreading
a blip as absence would mark good accounts for discard.

Also fixed: label collision after discarding entries, which would have produced
two accounts named `account-0098`.

### 4.6 Retries — two layers, and one that must never be retried

| Layer | Idempotent? | Retry |
|---|---|---|
| Proof generation | yes — delta already pushed | **yes**: 4 attempts, 500ms→8s, ±25% jitter |
| RPC sync / reads | yes | **yes**: backoff + jitter on 429 |
| `execute` (propose→push→prove→submit) | **no** | **never** |
| Proposal on conflict | no — creates a row per attempt | no; clear the blocker instead |

The `execute` row is evidence-backed: retrying it re-pushes a delta Guardian
already holds, and turned **one failed bootstrap account into 25**, leaving 22
stuck candidates. The delta is pushed *before* proving, to obtain the ack
signature, so `execute` is not idempotent. Retrying only the proof is.

**The prover retry silently did nothing on first implementation.**
`TransactionProverError` carries its cause via thiserror's `#[source]`, so
`to_string()` yields only `"failed to prove transaction"` while the
`"Timeout expired"` being matched sits further down the chain. Every failure was
classified permanent. Caught not by reading the code but by **failed-operation
durations being unchanged** (21,006ms vs 20,920ms p50). The classifier now walks
the source chain, and a regression test asserts the top message does *not* carry
the signal, so it cannot pass for the wrong reason.

### 4.7 Workload — the transfer ring drained itself

Writers only sent, so every vault emptied: at 8 writers, **1,003 of 1,126**
operations failed with *"the amount of the asset in the vault is less than the
amount to remove"*. Writers now consume what the ring delivers before sending
onward. Vault-exhaustion errors 1,003 → **0**.

### 4.8 Diagnostic stack — three setup defects

- `pg_stat_statements` was preloaded but the **extension was never created**, so
  query attribution silently degraded to a warning.
- Container CPU was sampled only *after* the run, showing an idle stack. Sampling
  in-window immediately caught the proxy **pinned at its 0.5-CPU limit** while
  relaying 1,623 reads/s — that leg measured Caddy, not Guardian.
- Both replicas were building **separate images** from one context.

### 4.9 Configuration added

`GUARDIAN_CANONICALIZATION_ABANDON_QUARANTINE_SECONDS` / `_CHECKS` as environment
overrides, following the pattern of the two existing canonicalization knobs.
Defaults unchanged (15s / 2 checks). The account accepts no new deltas while the
quarantine runs, so at the default a single prover failure idles a writer for that
window; the diagnostic stack uses 2s / 1 check. Documented with the trade-off —
shortening it narrows the window in which a late-landing transaction is still
recognised, so it is for local benchmarking only.

---

## 5. Infrastructure notes

**Production is already running current code.** `/status` reported `0.16.0` /
`6e7c6263e9ac`, whose tree is byte-identical to `ce4c342` on main — the only
difference to main HEAD being a TypeScript lockfile. All four canonicalization
optimisations are deployed. A replay needs no deploy.

**Production exposes no server-side metrics.** `GUARDIAN_METRICS_ENABLED` defaults
to `false` and `infra/*.tf` never sets it, so the authoritative tier can report a
p95 but never which component spent the time. That is why the diagnostic tier
exists.

**gRPC bypasses the HTTP rate limiter.** `RateLimitLayer` is attached to the axum
router only; the tonic server never sees it. Disabling rate limiting changes
nothing for a gRPC run, and prod's 5,000/min (≈83 req/s) never applied to the
April measurements.

**Deployment shapes differ.** Terraform's default is `server_cpu = 512` (0.5 vCPU)
/ 1 GB with `desired_count = 2` in prod. The April benchmark overrode this to
2 vCPU / 4 GB with a single task. "Prod shape" is therefore ambiguous; the April
shape is the one existing numbers describe.

**Issue #317's bottleneck #3 is stale.** The serialized `network_client` lock in
`push_delta` is no longer on `main` and should come off the issue's list.

**Canonicalization sampling postdates April.** It landed in `a30ffa4`, after the
2026-04-08 runs, which is why that report shows `push_delta` *admission* latency
and no accepted→canonical figure. Replaying those profiles cannot produce the
canonicalization number.

---

## 6. What is not yet measured

| Gap | Blocker |
|---|---|
| `get_state` p95 at 20,000 readers | Needs off-box load generation; local runs are generator-bound |
| 100,000-account population | 100 provisioned; bulk provisioning path not built |
| 100 concurrent writers | Public testnet prover + 128-request RPC cap |
| Authoritative (deployed) replay | AWS session, `dev` profile, Session Manager plugin |
| Read-path 1-vs-N replica scaling | Same off-box requirement as above |

---

## 7. Recommended next steps

1. **Fix the saturation check** to include host load and the generator, so a
   generator-bound run self-labels instead of looking valid. This is the highest
   priority: without it, every future read measurement is suspect.
2. **Containerize the load generator** (the `Dockerfile` already has a
   `benchmark-runner` stage) and budget total container CPU ≤ physical cores. To
   make replica scaling measurable, size servers at Terraform's real 0.5 vCPU
   default rather than the April 2 vCPU override — small enough that a
   containerized generator can saturate them. Report **ratios**, not absolutes.
3. **Run the authoritative replay** of the April profiles once AWS access is
   available. No deploy needed (§5), and it is the only tier that can carry a
   verdict.
4. **Decide on the prover.** A local prover would likely take the residual 13%
   failure rate at 64 writers to near zero. Without it, 100 writers against public
   testnet is not reachable.
5. **File the nonce-consumption bug** (§4.1) as its own issue — it is a server
   correctness defect independent of this benchmarking work, and any client that
   abandons a candidate hits it.

---

## 8. Commits

Branch `317-scalability-target-measurement`, 14 commits, 54 files, ~7,600 lines.
Nothing pushed.

```
57be84b feat: retry transient proving failures and rate-limited reads
2340fc9 fix(multisig-e2e): consume received notes so the transfer ring sustains itself
5d3999e fix(server): return the live delta when a nonce carries several rows
7948619 fix(server): a client-abandoned delta must not consume its nonce
6cf931f fix(benchmarks): confirm abandons, reuse proposals; make abandon quarantine tunable
abab7ee fix(multisig-e2e): recover stranded candidates; stop crediting rejected pushes
ecfa276 fix(multisig-e2e): verify fixture state against Guardian; survive prover timeouts
956c823 fix(benchmarks): address #317 review findings
81f82e2 feat(multisig-e2e): add N-writer scale runner for the #317 write target
8842892 feat(multisig-e2e): provision N accounts, resumably (#317)
3b0b892 Merge remote-tracking branch 'origin/multisig-e2e-benchmark'
8e83e85 feat(benchmarks): add diagnostic stack and scalability measurement spec (#317)
f957fba chore: Apply PR suggestions          (from #348)
7e07e7a feat: Add multisig e2e benchmark test (from #348)
```

Validation at each step: 44 benchmark tests, 120 multisig-client tests, 776 server
tests, `clippy --all-targets --locked -- -D warnings` clean, `fmt --check` clean.
One Postgres regression test (`a_discarded_delta_does_not_consume_its_nonce`) runs
against a live database and is `#[ignore]` by default.

## 9. Artifacts

- Spec and requirements: [`spec.md`](./spec.md), [`plan.md`](./plan.md)
- Findings with evidence: [`research.md`](./research.md) §17–§22
- Diagnostic stack: `benchmarks/diagnostic-stack/`
- Write-path runs: `benchmarks/multisig-e2e/reports/scale-*`
- Read-path legs: `benchmarks/diagnostic-stack/results/` (gitignored)
- Account fixture: `.guardian/bench/multisig-e2e-accounts.json` (gitignored, 0600)
