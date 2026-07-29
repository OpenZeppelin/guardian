# Phase 0 Research: Scalability Target Measurement

All findings below were read from the current tree or observed against the live
deployment on 2026-07-28. Each decision records what was chosen, why, and what
was rejected.

## 1. Load model: closed-loop today, paced needed

**Finding**: `runner.rs:142` runs `while Instant::now() < end_deadline` and issues
the next operation immediately after the previous returns. Each seeded user is
one `tokio` task spawned in `execute()` (`runner.rs:75-80`). There is no think
time anywhere in the loop.

Consequence: "users" currently means "permanently in-flight requests". Offered
load is an *output* of server latency — when the server slows, the harness
automatically backs off. That is the February reading of concurrency, and it is
why April's `push_delta` p95 of 3,935ms coexists with only 352 pushes/s.

**Decision**: Add a paced (open-loop) mode where each user sleeps to a schedule
between operations, with the per-user rate declared in the profile. Preserve the
existing saturating loop as an explicit mode for the stress ceiling (FR-003c).

**Rationale**: The resolved FR-003 defines a user as a client at a declared rate,
so offered load must be an input. Pacing also makes the target cheaper to reach —
a user at 0.1 reads/s is asleep ~99% of the time, so 20,000 of them need far
fewer hosts than 20,000 saturating ones.

**Alternatives rejected**: Keep closed-loop and pick a user count that happens to
produce 2,000 reads/s — rejected because the resulting rate drifts with server
latency, so the profile would no longer describe a fixed offered load and runs
would not be comparable. A separate rate-limiter task feeding a work queue —
rejected as more machinery than per-user sleep for the same result.

**Pacing schedule**: Fixed interval with a per-user random phase offset at start,
not a synchronised tick. Without the offset, 20,000 users would fire in lockstep
every 10 seconds and measure a thundering herd instead of a steady rate.

## 2. Account population is pinned to user count

**Finding**: `config.rs:95` — `accounts_per_user must be exactly 1 for phase 1`.
Seeding assigns exactly one account per user (`seed.rs:79-104`).

Consequence: the 100,000-account dimension is inexpressible. 20,000 readers imply
20,000 accounts and nothing else.

**Decision**: Introduce a population concept independent of `users`, with two
counts: a real-path subset (provisioned through `configure`, the same path a real
account takes) and a bulk subset. Load binds only to the real-path subset.

**Rationale**: FR-016a. It also matches the physical reality that only ~20,100
accounts can be touched by target load anyway.

**Alternatives rejected**: Raise `accounts_per_user` — rejected because it still
ties population to user count and would make each user round-robin accounts,
changing the workload's per-account concurrency in a way the target does not
describe.

## 3. Bulk provisioning channel

**Finding**: `seed.rs` provisions via the public `configure` call under a
32-permit semaphore (`ACCOUNT_CONFIGURE_CONCURRENCY`). Cleanup already reaches
into the deployment's database through `aws ecs execute-command` against the live
server task (`cleanup.rs:64-103`).

**Decision**: Bulk-provision the non-touched ~80,000 accounts through the same
ECS-exec SQL channel cleanup already uses, and verify a sample against real-path
accounts (FR-016b).

**Rationale**: The channel, its auth path, and its batching are already built and
already trusted for a destructive operation; reusing it for a constructive one
adds no new access surface. Provisioning 80,000 accounts through `configure` at
32-way concurrency would dominate the schedule for no measurement gain, since
load never touches them.

**Alternatives rejected**: All-real-path — rejected on time. A separate admin API
for bulk creation — rejected as a server contract change (Principle I) for a
benchmark's convenience.

**Risk accepted**: A bulk row that differs from a real row would make the
account-scale measurement meaningless. FR-016b's sample comparison is the
mitigation and must run before any account-dimension verdict.

## 4. Auth-window failures are conflated with generic auth failures

**Finding**: `error_classification.rs:6-11` returns `"auth"` for any message
containing `"auth"`. The auth-window failure message is
`"Request timestamp outside allowed window: {n}ms drift (max 300000ms)"` wrapped
in `GuardianError::AuthenticationFailed` (`services/mod.rs:133`), so it lands in
the same bucket as invalid signatures and unauthenticated calls.

**Decision**: Add an `auth_window` category matched on the `outside allowed
window` substring, ordered *before* the generic `auth` check.

**Rationale**: FR-008 makes this its own criterion, and it is the failure mode
February found dominant at scale (14.1% of requests at 10k users). It cannot be
its own criterion while it is invisible in the breakdown.

**Alternatives rejected**: Add a distinct server error code — more robust, and
the right long-term answer, but it is a wire-contract change requiring
propagation to both clients under Principle I. Out of scope for a measurement
feature; recorded as a follow-up.

**Known fragility**: Substring matching breaks silently if the server's message
wording changes. Mitigation: a harness test asserting the current message
classifies as `auth_window`, so a wording change fails a test rather than
quietly zeroing a criterion.

## 5. There is no unclassified bucket

**Finding**: `classify()` falls through to `"server"` (`error_classification.rs:40`),
and `OperationAccumulator::record` defaults a missing error to `"server"` too
(`runner.rs:32`).

Consequence: FR-009's "count of unclassified failures MUST be reported" is
unsatisfiable — unrecognised errors are silently labelled as server errors.

**Decision**: Add an explicit `unclassified` category as the fallthrough, keep
`server` for errors that positively match server-side failure, and report the
unclassified count as a first-class field.

**Rationale**: An unrecognised error masquerading as a known category is worse
than an honest unknown, especially in a report whose purpose is a pass/fail
verdict.

## 6. Canonicalization sampling blocks the sampling worker

**Finding**: `runner.rs:180-183` awaits `observe_canonicalization` inline in the
worker loop. That polls `get_delta` every `poll_interval_ms` until terminal or
`timeout_seconds` (180s in the April profiles). The worker issues no load while
polling. The harness README already documents this ("sampled workers pause load
generation while polling").

**Decision**: Move observation off the worker's critical path — spawn it as an
independent task and collect samples at join time.

**Rationale**: The spec lists this as an edge case: measurement must not perturb
the workload. Under pacing it becomes worse, not better — a paced worker that
stalls 180s silently drops its offered rate to zero, corrupting exactly the input
the paced model exists to control.

## 7. Canonicalization wait is already implemented — and postdates April

**Finding**: `CanonicalizationReport::from_samples` (`report.rs`) already computes
p50/p95/p99/max over `Canonical` sample waits, and separates `TimedOut` from
`ObservationFailed`. Sampling landed in `a30ffa4` (`perf: canonicalization
performance`, #326) — **after** the 2026-04-08 runs.

Consequence: April structurally could not report accepted→canonical wait; that is
why its report shows admission latency instead. The capability exists now and has
simply never been exercised at scale.

**Decision**: No new metric needed for FR-005 — wire the existing sample report
into the criterion evaluation and ensure the replay profiles enable sampling.

## 8. gRPC bypasses the HTTP rate limiter

**Finding**: `RateLimitLayer` is applied to the axum router only
(`builder/handle.rs:282-283`); the tonic server (`handle.rs:320-353`) gets no such
layer. Prod sets `GUARDIAN_RATE_PER_MIN = 5000` and burst `200/s`
(`infra/data.tf:141-142`) — 83 req/s sustained, which April's ~1,760 req/s would
have violated by 21× had it applied.

**Decision**: Record explicitly in every report that gRPC runs are not
rate-limited, and report a zero `rate_limited` count as a measured zero rather
than an absent field (FR-018). Any HTTP leg needs limits raised before it can
carry load, and that must be stated in its report.

**Rationale**: A reader seeing no 429s must be able to tell "none occurred" from
"the limiter does not apply here".

## 9. Build identity is available without privileged access

**Finding**: The deployment exposes `/status` unauthenticated, returning
`version`, `git_commit`, `environment`, `started_at`, `uptime_seconds`. Observed
live: `0.16.0` / `6e7c6263e9ac` / `testnet` / started 2026-07-24T12:33:15Z.
Neither the harness nor `guardian-client` reads it (no match in either tree).

**Decision**: New `status.rs` performing a plain HTTP GET, captured at run start
and run end. If `git_commit` or `started_at` differs between the two, flag the run
as spanning a restart and withhold its verdict (FR-A08).

**Rationale**: Cheap, unprivileged, and it turns "which build did we measure" from
an AWS question into a field in the report. `started_at` additionally gives a
restart detector April had no way to implement.

**Note**: `/status` is HTTP while the harness client is gRPC, so this is a
separate small request path, not a client-SDK change.

## 10. Deployed build is already current

**Finding**: Deployed commit `6e7c6263e9ac` is not an ancestor of `main`, but its
tree (`ea701c08…`) is byte-identical to `ce4c342` on main — it is the pre-squash
release commit. The only delta to main HEAD is
`packages/miden-multisig-client/package-lock.json`, a TypeScript lockfile with no
server impact. All four optimisations (`a30ffa4`, `cbf0f44`, `5bc7e06`, `e736e7f`)
are present.

**Decision**: The US1 replay can run against the current deployment with no
deploy step.

## 11. Sustained write load is bounded by canonicalization, not by the client

**Finding**: The April profiles set `retire_after_first_successful_push = true`,
and prior local work found sustained per-account push load unmeasurable because a
second push against an account with a non-canonical delta pending fails as
`state_conflict` (`error_classification.rs:16-21`).

Consequence: "100 concurrent transacting users" cannot mean 100 users pushing as
fast as possible. Per account, the achievable write rate is bounded by the
accepted→canonical cycle.

**Decision**: Define a transacting user as one account cycling
push → await terminal → push. Its rate is therefore an *observed* property, and
the write criterion measures the wait, not the throughput. Report offered vs
achieved write rate separately.

**Rationale**: This falls out of the append-only lifecycle rather than from a
harness limitation, so modelling it any other way would measure a workload
production cannot produce.

## 12. Worker fleet sizing

**Finding**: April used 16 Fargate tasks at 2 vCPU / 4 GB
(`scripts/run-prod-benchmark-ecs.sh` defaults: 4 workers, 2048 CPU / 4096 MB),
sharding users by `assigned_user_ids` (`distributed.rs:31`). Worker artifacts
return via a base64 CloudWatch log line (`WORKER_ARTIFACT_PREFIX`).

**Decision**: Keep the sharding model; size the fleet from the paced-load
arithmetic rather than by analogy to April, and record per-worker headroom so
FR-013's server-bound/generator-bound label has evidence behind it.

**Open item for Phase 2**: The artifact channel is a single base64 log line per
worker. At 20,000 users the per-worker artifact grows; whether it still fits the
CloudWatch line limit needs checking before the first target-scale run. Flagged,
not yet measured.

## 13. Production exposes no server-side metrics — the diagnostic tier is necessary, not merely convenient

**Finding**: `GUARDIAN_METRICS_ENABLED` defaults to `false`, and `infra/*.tf`
sets no metrics environment at all — production runs with the Prometheus
integration entirely off ("no metrics listener, no recorder, no storage
instrumentation, no background refresher"). The harness measures only client-side
round trip (`runner.rs:160`).

Consequence: on the authoritative tier, **server-side service time does not
exist**. FR-004's "both where available" resolves to client-side only on prod.
More importantly, no bottleneck claim can be substantiated there — pool
saturation, storage timings, and delta-status gauges are all unavailable, so
attributing latency to a component would be guesswork.

**Decision**: Add a **diagnostic tier**: a local multi-instance stack with
metrics enabled, positioned between the fast loop and the authoritative tier
(FR-017a). Bottleneck attribution happens there; target verdicts do not.

**Rationale**: This is what makes issue #317's workstreams 1 and 2 (Postgres
profiling and pool tuning, the auth/replay-protection path) actionable. Those are
single-variable questions — pool size 16 vs 32 vs 64, fast promotion on/off,
canonicalization concurrency — and answering them needs a stack where one thing
changes at a time and the server reports on itself. Prod gives the authoritative
number; the diagnostic tier gives the reason.

**Alternatives rejected**: Enable metrics in production — a live-deployment
configuration change, outside a measurement feature's remit and a decision for
the deployment's maintainers. Infer bottlenecks from client latency alone —
rejected; that is precisely the guesswork FR-023 exists to separate from measured
causes.

## 14. The diagnostic stack is assembly, not construction

**Finding**: Both halves already exist as committed, working compose assets.

- `docs/guides/horizontal-scaling/docker-compose.yml` — two Guardian replicas
  behind a Caddy round-robin proxy, sharing one Postgres and one fleet-wide ACK
  identity, with each replica also reachable directly (ports 3000 and 3010) to
  bypass the proxy.
- `docs/guides/observability/docker-compose.yml` — Guardian plus Prometheus
  (v3.1.0) and Grafana (11.4.0), with provisioned dashboards and a bearer-token
  scrape.

**Decision**: Compose the diagnostic tier from these two rather than authoring a
new stack: replicas and proxy from the first, metrics and dashboards from the
second, and the existing local harness's Postgres statistics collection
(`sql/pg_stat_snapshot.sql`, `scripts/collect_pg_metrics.sh`) for the database
side.

**Rationale**: Both assets are already tested and documented, and the
horizontal-scaling one additionally exercises the multi-replica shape that a
target-scale deployment would use — so the diagnostic tier answers "does adding
instances help" as a side effect, which the single-task production deployment
cannot.

**What the tier controls that production does not**: pool sizes, Postgres
settings, canonicalization concurrency and fast promotion, replica count, rate
limiting, and CPU allocation per container.

## 15. Turning rate limiting off is a no-op for gRPC

**Finding**: Per §8, the rate-limit layer is applied only to the axum HTTP
router. gRPC never traverses it.

Consequence: disabling rate limiting changes nothing for a gRPC run, on any tier.
It matters only for an HTTP leg, where the prod-equivalent settings (200/s burst,
5,000/min sustained → 83 req/s) would otherwise dominate any load measurement.

**Decision**: Keep rate limiting **on** for gRPC diagnostic runs, so the stack
stays comparable to production, and record that it did not apply. Disable it only
for an explicit HTTP leg, and label that leg as non-prod-equivalent in its report.

**Rationale**: Changing a variable that provably has no effect adds an unexplained
difference between the diagnostic and authoritative tiers for no measurement gain.

## 16. Guarding the diagnostic tier against February's failure mode

**Finding**: February's runs were generator-bound — load generator, server, and
Postgres shared one laptop's CPU, and the report attributes much of the observed
latency to that.

**Decision**: On the diagnostic tier, give the load generator, each replica, and
Postgres explicit, disjoint CPU allocations, and apply FR-013's saturation
attribution to the generator container exactly as on the authoritative tier. A
diagnostic run that is generator-bound is reported as such and produces no
bottleneck claim.

**Rationale**: The tier's whole value is attribution. A co-tenancy artefact
misread as a server bottleneck would send the optimisation work after the wrong
thing — which is the specific way February's numbers misled.

## 17. The server's metric catalogue already covers three otherwise-open questions

**Finding**: `crates/server/src/metrics/names.rs` defines a far richer set than
expected. Three entries resolve questions the plan had treated as open or as
authoritative-tier limitations:

| Metric | Resolves |
|---|---|
| `guardian_db_pool_pending_acquires` | Whether the connection pool is the constraint — the direct measurement of issue #317's bottleneck #1, rather than inference from latency |
| `guardian_grpc_request_duration_seconds` | FR-004's server-side service time, against the harness's client round trip |
| `guardian_miden_rpc_duration_seconds` | FR-005's chain-vs-Guardian split in the write path |

Also present and directly useful: `guardian_grpc_requests_in_flight` (a
server-side check on FR-003b's in-flight figure),
`guardian_canonicalization_candidate_age_seconds`,
`guardian_canonicalization_fast_runs_total` (measures what `cbf0f44` actually
does), `guardian_canonicalization_commitment_mismatches_total`,
`guardian_storage_operation_duration_seconds`, and
`guardian_rate_limit_rejections_total` (FR-018 from the server's own view).

**Decision**: Capture all of the above per diagnostic leg. FR-004 and FR-005
resolve to "both available on the diagnostic tier, client-side only on the
authoritative tier" — a tier property to state in each report, not a gap to
close.

**Consequence for FR-005**: The chain-vs-Guardian split is achievable, but only
where metrics are on. On the authoritative tier the canonicalization criterion is
necessarily measured end-to-end including testnet confirmation time, and reports
from that tier must say so.

## 18. The prod harness cannot express a read-only workload

**Finding**: `operation_for_index` (`workload.rs`) emits a `push_delta` every
`reads_per_push + 1` operations, and `reads_per_push = 0` means **push-only**,
not read-only. There is no value that yields reads alone — every profile shape
eventually pushes.

Consequence: the target's read dimension — 20,000 users issuing `get_state` — is
genuinely read-only and **not expressible in the authoritative harness**. The
February `state-read` scenario that produced the 10k-reader numbers belongs to
the *local* harness, which has a separate scenario vocabulary. This is why the
April round has no read-only leg.

**Decision**: Add a first-class read-only mode to the operation mix before the
authoritative read run. Do not rely on a very large `reads_per_push`: it works
only because a short leg ends before the first push, so the workload silently
changes character as duration grows — precisely the kind of drift that makes two
runs incomparable.

**Interim**: The diagnostic read profile uses `reads_per_push = 1000000` with the
limitation written into the profile, so a short diagnostic leg is read-only in
practice while the proper mode is built.

## 19. First diagnostic leg: the auth path is measurably ~33% of read query time

**Finding**: First working leg on the diagnostic stack — 8 saturating users,
read-only, two replicas, pool 16, 45s. Result: 68,512 `get_state`, 0 failures,
1,712 ops/s, client p95 6.69ms. Server-side mean was 2.29ms (87.72s over 38,348
requests on one replica), so roughly two thirds of the client-observed p95 is
outside server processing — the FR-004 split, working.

`pg_stat_statements` for the leg:

| Calls | Total ms | Mean ms | Query |
|---:|---:|---:|---|
| 76,695 | 7,421.2 | 0.10 | `SELECT states…` — the actual state read |
| 76,695 | 2,465.3 | 0.03 | `UPDATE account_metadata SET last_auth_timestamp…` |
| 76,703 | 1,263.1 | 0.02 | `SELECT account_metadata…` — auth metadata read |
| 230,188 | 275.0 | 0.00 | `SELECT $1` — pool health checks, 3× per operation |

The replay-protection CAS write plus the auth metadata read total **3,728ms
against 7,421ms for the state read itself** — about 33% of query time on the read
path, spent on authentication rather than on serving state.

**Significance**: This is issue #317's bottleneck #2, **measured rather than
hypothesised**, on the first leg. It is exactly what the authoritative tier cannot
produce, and it makes workstream 2 an evidence-backed priority.

**Caveats**: One leg, 8 users, one machine, no repetition — a signal, not a
result. It does not establish that the auth path is the *binding* constraint:
`guardian_db_pool_pending_acquires` was 0 on both pools of both replicas, so at
this load nothing was queueing on connections.

## 20. Saturation must be sampled during the window, and the first attempt was proxy-bound

**Finding**: Sampling container CPU once after the harness exits shows an idle
stack. The corrected in-window sampler immediately caught the real story on the
next leg: Caddy peaked at 52.56% against a 0.5-CPU limit — **pinned** — while
Postgres reached 122.74% of 2.0 and each replica ~61% of 1.5.

Consequence: that leg measured the proxy, not Guardian. Throughput read 1,623
ops/s and would have been reported as a Guardian figure.

**Decision**: Proxy budget raised to 1.5 CPUs, and peak-CPU-per-container is now
a first-class per-leg artifact (`cpu-peaks.json`) that the saturation call rests
on.

**Wider point**: This is FR-013 earning its place before a single real comparison
was run, and it is the same failure mode as February's generator-bound numbers —
just relocated to a different component. Any tier can produce a confident number
that describes its own infrastructure.

## 21. The canonicalization criterion has no measurement path today

**Finding**: The first write leg (16 users, ECDSA, 4:1 mix, retiring after first
push, `sample_rate = 1.0`, 300s timeout) produced:

- 16 `push_delta`, **all successful**, p50 84ms / p95 98ms admission latency
- 64 `get_state`, all successful
- **16 of 16 canonicalization samples `timed_out`** — zero canonical, zero
  discarded

This is not a tuning problem. Server-side evidence from the same leg:

| Signal | Value | Reading |
|---|---:|---|
| `guardian_canonicalization_runs_total` | 1,056 | Full passes ran constantly |
| `guardian_canonicalization_pass_accounts` | 16 | Every pass saw all 16 accounts |
| `guardian_canonicalization_deltas_fetched_total` | 672 | Candidates were fetched repeatedly |
| `guardian_canonicalization_retries_total` | 0 | Nothing was ever *retried* |
| `guardian_miden_rpc_requests_total{operation="get_account_commitment",outcome="ok"}` | 848 | Testnet reachable and answering |
| `worker_leases` | held, renewing | Coordination healthy |

Every delta sat at `status_kind = candidate` with `retry_count: 0`,
`divergence_count: 0`. The database shows why:

```
nonce | prev_commitment    | new_commitment     | state_commitment   | status
    1 | 0x6c82fde21d1a109f | 0x7009e1d21903b07e | 0x6c82fde21d1a109f | candidate
```

`prev_commitment == state_commitment`: Guardian holds the delta, and canonical
state is still at the previous commitment.

**Mechanism**: A delta becomes canonical when the **on-chain** account commitment
advances to its `new_commitment`. Guardian does not submit that transaction — the
client does, via the multisig SDK. The benchmark harness only calls Guardian's
`push_delta`; it never executes a Miden transaction. So the on-chain commitment
never moves, and the candidate waits forever, correctly.

**Consequence**: Issue #317's second acceptance criterion — *delta
canonicalization p95 ≤ 30s at 100 concurrent writers* — **cannot be measured by
this harness on any tier**, including the authoritative one. The sampling added
in `a30ffa4` will report `timed_out` for every harness-created delta by
construction.

This also reframes the April round: its `push_delta` figures measure **admission**
only, and no amount of replaying them produces the canonicalization number. The
gap is not that April forgot to enable sampling — it is that the workload has no
on-chain step to canonicalize against.

**Options, none cheap**:
1. Have the harness execute the real on-chain transaction per push (drives the
   multisig client flow) — highest fidelity, large lift, and its throughput would
   then be bounded by Miden, not Guardian.
2. Measure the criterion against real traffic on a deployment where clients do
   submit, rather than against synthetic benchmark load.
3. Re-scope the criterion to what the harness can observe — e.g. canonicalization
   *pass* latency and candidate age under load, which are already instrumented —
   and state plainly that end-to-end accepted→canonical is out of scope.

**Recommendation superseded** — see §22. PR #348 already implements option 1, so
re-scoping the criterion away is no longer the right call.

**Caveat**: One leg, 16 accounts, local stack. The mechanism is confirmed by the
commitment comparison and the zero-retry counters, but the conclusion that *no*
harness-created delta can canonicalize should be re-checked against a prod replay
before it is treated as settled.

## 22. PR #348 supplies the missing workload — and independently corroborates §21

**Finding**: Open PR #348 (`multisig-e2e-benchmark`) adds
`benchmarks/multisig-e2e`: two persistent 1-of-1 accounts alternating real P2ID
transfers through Guardian, **with the Miden transactions proved and submitted**.
It carries `fixture.rs`, `canonicalization.rs`, and a
`prepare` → fund → `bootstrap` → `preflight` → `run` command split.

Its README states the split directly:

> It is intentionally separate from `benchmarks/prod-server`: that harness
> measures Guardian API capacity with **synthetic deltas**, while this one
> measures the full Rust SDK, Guardian, prover, and Miden network path.

That is §21's conclusion reached independently by the PR author, which raises it
from "one diagnostic leg suggests" to a known property of the two harnesses.

**Decision**: Measure the canonicalization criterion on the real-account
workload; keep the scale dimensions on the synthetic harness. Three workloads,
three jobs:

| Target dimension | Workload | Status |
|---|---|---|
| 20,000 concurrent readers | `prod-server`, synthetic | Works today; needs paced load and a read-only mode (§18) |
| 100 concurrent transacting users (push admission) | `prod-server`, synthetic | Works today |
| Canonicalization p95 ≤ 30s | `multisig-e2e`, real funded accounts | Needs fixture parameterised beyond 2 accounts |

**What to reuse**: the fixture pattern is the valuable part and is already
well-built — persist the key **before** registering with Guardian, refuse to
overwrite an existing fixture, `0600`, keep partial fixtures on interruption
(they may hold keys for funded accounts). That reasoning is now FR-005d.

**The gap**: `Fixture::load` hard-fails unless `accounts.len() == 2`, and
`prepare` iterates a literal `["alice", "bob"]`. The criterion says *100
concurrent writers*, so the fixture must become N-account. Funding ~100 testnet
accounts is plausible subject to faucet limits; funding 20,000 is not — which is
exactly why reads must stay synthetic (FR-005e).

**Cost caveat**: On the real path every operation carries proving and submission,
so per-account throughput is low and the ceiling may be the prover and the
network rather than Guardian. That makes the real-account workload the right tool
for the *latency* criterion and the wrong one for the *scale* dimensions.

## 23. Local harness README is stale

**Finding**: `crates/server/bench/README.md:112-157` instructs the reader to
hand-edit `main.rs` to make network type and canonicalization env-driven.
`crates/server/src/main.rs:35-46` already reads all of it from env, including
fast promotion and max-concurrent-accounts.

**Decision**: Delete that README section as part of the local-leg work.
