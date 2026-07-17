# Local Canonicalization A/B: v0.15.1 vs perf/canonicalization-performance

2026-07-17. Local single-host A/B measuring the canonicalization worker's
per-pass throughput before and after the Phase 0–3 performance work
(`2851cac`..`79b572b`).

## Goal

Quantify how fast the background canonicalization worker processes a
backlog of candidate accounts, comparing the v0.15.1 baseline (what prod
runs) against the perf branch (candidate-only reads, network-client
de-mutex, bounded per-account concurrency). The client-facing
`push_delta` latency was already known to be flat across this work
(issue #320); the pass duration is the number the branch changes.

## Setup

- Host: local macOS (Apple Silicon), release builds, filesystem storage
  backend, rate limiting off, metrics on.
- Network: `GUARDIAN_NETWORK_TYPE=MidenTestnet` — every candidate costs
  one real `get_account_commitment` RPC to `rpc.testnet.miden.io` per
  pass, matching prod's RPC dependency. (Devnet avoided: mid-update and
  unstable at time of testing.)
- Workload: `guardian-prod-benchmarks worker-run`, local profile —
  128 users × 1 account, ECDSA, push-only burst (retire after first
  successful push), i.e. a standing backlog of 128 candidate accounts.
- Baseline leg: `8222510` (v0.15.1 release, merge-base with main, the
  commit prod runs). Branch leg: `79b572b` with default
  `GUARDIAN_CANONICALIZATION_MAX_CONCURRENT_ACCOUNTS=4`.
- Fresh storage/metadata/keystore dirs per leg; both legs pushed
  128/128 deltas successfully at ~1.07 push/s (single local client).

## Results

| | v0.15.1 baseline | perf branch |
|---|---|---|
| Full passes observed | 8 (4 × 128-account backlogs) | 17 (1 standing backlog) |
| Pass duration (avg) | **~12.1 s** | **~4.2 s** |
| Pass duration (histogram) | all 8 in the 10–30 s bucket | all 17 in the 1–10 s bucket (p50 ≈ 4.0 s, min 2.4 s, max 6.7 s) |
| Miden RPC per pass | 128 | 128 |
| Miden RPC latency (avg) | 85.2 ms | 125.6 ms |
| Effective account concurrency¹ | ~0.90 (serial) | ~3.9 (≈ configured 4) |

¹ (128 × avg RPC latency) / pass duration — how many accounts the worker
overlaps in flight.

RPC-normalized comparison (branch leg happened to see ~47% slower
testnet RPCs): at equal RPC latency the branch processes the same
backlog ≈ **4.3× faster**; measured raw wall-clock improvement is ≈ 3×.
With `max_concurrent_accounts` raised, headroom scales further — the
pass is RPC-bound, not CPU-bound.

## Postgres repeat

Same A/B repeated with both servers built with `--features postgres`,
each leg on a fresh dedicated database (`guardian_bench_leg_a`/`_b`) on
the local Postgres 16 instance, dropped after the run. RPC latencies
this time were nearly identical between legs, making it the cleanest
comparison:

| | v0.15.1 + postgres | perf branch + postgres |
|---|---|---|
| Full passes observed | 8 (4 × 128-account backlogs) | 17 (standing backlog) |
| Pass duration (avg) | **~12.2 s** (all in 10–30 s bucket) | **~2.6 s** |
| Miden RPC latency (avg) | 85.9 ms | 78.1 ms |
| Effective account concurrency | ~0.90 (serial) | ~3.86 |

Raw improvement ≈ **4.7×** (≈ 4.3× RPC-normalized). Baseline pass times
match the filesystem run (~12.1 s vs ~12.2 s) — the pass is RPC-bound,
not storage-bound, at this scale; the storage backend has no visible
effect on either leg. The baseline's diverged-discard behavior
reproduced identically on Postgres (all 512 candidates across 4 rounds
discarded after 2 passes each).

## Concurrency scaling: `max_concurrent_accounts = 16`

Third leg: perf branch + postgres with
`GUARDIAN_CANONICALIZATION_MAX_CONCURRENT_ACCOUNTS=16` and
`GUARDIAN_DB_POOL_MAX_SIZE=32` (the prod pool size, keeping the
worker at ≤ half the pool per the config guidance):

| concurrency | pass duration (avg) | RPC latency (avg) | effective concurrency |
|---|---|---|---|
| serial (v0.15.1) | ~12.2 s | 85.9 ms | 0.90 |
| 4 (branch default) | ~2.6 s | 78.1 ms | 3.86 |
| 16 | **~0.74 s** | 76.4 ms | **~13.2** |

15 of 16 full passes landed in the 0.5–1 s bucket. Theoretical floor at
concurrency 16 is ~0.61 s (8 waves × 76 ms), so the worker runs at ~82%
of ideal — scaling is close to linear through 16 with no sign of DB-pool
or RPC push-back at this depth. Overall: **~16× faster than the v0.15.1
baseline** for the same 128-account backlog.

## Behavioral finding (not just performance)

v0.15.1 classified all 128 not-yet-on-chain candidates as **diverged and
discarded the entire backlog after 2 passes (~20 s)** — the
unknown-account commitment matches neither the expected nor the previous
commitment, so the divergence path fires. The branch's corrected
classifier grace-defers these candidates instead
(`grace_deferred`, 600 s grace + retry budget). For real accounts whose
deployment/tx has not landed on chain yet, the baseline behavior
destroys valid candidates; the branch retains them.

## Caveats

- Synthetic accounts never exist on-chain, so no candidate can reach
  `canonical`; the client-observed accepted→canonical sampling (added to
  the harness this week) correctly reported its sampled pushes as
  `timed_out` on both legs. End-to-end time-to-canonical needs
  on-chain-backed accounts (real proposal flow) — this A/B measures
  worker pass throughput instead, which is the direct target of
  Phases 2–3.
- Single local client, no concurrent API load; prod passes contend with
  request traffic and Postgres instead of the filesystem backend.
- Testnet RPC latency differed between legs (85 vs 126 ms avg); the
  effective-concurrency figures normalize this out.
- Baseline pass samples are fewer (8) because each seeded backlog
  survives only 2 baseline passes before being discarded (see above).

## Interpretation

With a 128-account candidate backlog, the baseline worker is fully
serial: one pass ≈ 128 × RPC latency ≈ 11–13 s, during which no other
account's candidate progresses. The branch overlaps 4 accounts and cuts
the pass to ~4 s at higher RPC latency; the improvement is the
mutex removal (Phase 2) making the concurrency cap (Phase 3) effective.
Against issue #320's observation of a ~10–12 s canonicalization cycle
(#316) overlapping each send's handshake, this directly shrinks the
per-cycle window in proportion to the configured concurrency.
