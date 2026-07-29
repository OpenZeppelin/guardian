# Feature Specification: Guardian Scalability Target — Measurement Definition

**Feature Branch**: `317-scalability-target-measurement`
**Created**: 2026-07-28
**Status**: Draft
**Input**: User description: "https://github.com/OpenZeppelin/guardian/issues/317 — define how to get required numbers"

## Context

Issue #317 states a scalability target for the Guardian server:

| Dimension | Target |
|---|---|
| Guarded accounts | 100,000 |
| Concurrent users transacting (push / canonicalize deltas) | up to 100 |
| Concurrent users reading state (`get_state`) | up to 20,000 |

with acceptance criteria at target load: read p95 ≤ 1s, canonicalization p95 ≤ 30s,
and zero auth-window (replay-protection) failures.

Two rounds of evidence exist, and neither answers the target.

The February round (`crates/server/bench/reports/2026-02-15.md`) tops out at
10,000 readers and 10,000 accounts on one developer laptop with mostly default
Postgres settings. Its headline latency mixes client-side queueing delay with
server service time, so its multi-minute figures describe that setup as much as
they describe Guardian.

The April round (`benchmarks/prod-server/results/20260408-prod-benchmark-report.md`)
is the more relevant precedent: 16 distributed ECS workers against the deployed
testnet-backed Guardian on a single 2 vCPU / 4 GB ARM64 task with RDS Proxy. At
4,096 users it recorded `get_state` p95 of 926ms — inside the 1s criterion — with
1,409 `get_state`/s and 352 `push_delta`/s. So the read criterion is not a
distant aspiration; the open question is what happens between 4,096 and 20,000.

But the April round cannot be read as a verdict against the target, for reasons
that are about measurement rather than performance:

- it reports `push_delta` **admission** latency (p95 3,935ms), not the
  accepted→canonical wait the 30s criterion is about;
- it contains no auth-window failure accounting at all, so the "zero
  auth-window failures" criterion is simply unmeasured;
- two of its four headline runs were salvaged from 15 of 16 worker shards after
  transient `502`s, with no statement of what the missing shard did to the
  aggregate;
- it ran 4,096 accounts, leaving the 100,000-account dimension untouched.

Meanwhile the code has moved: canonicalization promotion and gating work has
landed, and the serialized `network_client` lock that issue #317 lists as
bottleneck #3 is no longer in `push_delta` on `main`. The April numbers therefore
describe a server that no longer exists.

**This feature is the measurement layer, not the optimisation work.** It closes
the measurement gaps above, re-runs the existing February and April profiles
against current code to establish where the optimisations actually left us, then
defines and reaches the target load shape. Making the numbers *good* — pool
tuning, the auth/replay-protection path, an alternative storage backend — is
follow-up work that this feature makes measurable and therefore reviewable.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Re-baseline current code with the existing profiles (Priority: P1)

A maintainer needs to know where the optimisations that landed since April
actually left the server, measured the same way April measured it, before any new
profile or target-load capability is designed.

**Why this priority**: It is the cheapest way to get real numbers and it is a
prerequisite for judging everything else. The April profiles already exist, the
deployed environment they targeted is already testnet-backed, and the distributed
harness already runs them — so this is a replay, not a build. Without it, the
current position is unknown and any new measurement has nothing to be compared
against; with it, the follow-up workstreams can be ranked against a current
bottleneck ordering instead of a February one that is already partly stale.

**Independent Test**: Re-run the April prod profiles unchanged against the
current deployment and produce a side-by-side comparison with the April report,
per metric, plus the same for the February local profiles. Delivers value on its
own — it either shows the optimisations moved the numbers or shows they did not.

**Acceptance Scenarios**:

1. **Given** the four April prod profiles and the February local profiles,
   **When** they are re-run against current code, **Then** each produces a report
   comparable to its historical counterpart on every metric that historical
   report published.
2. **Given** a re-run and its historical counterpart, **When** they are compared,
   **Then** the comparison names per metric whether it improved, regressed, or is
   within noise, and states which configuration differences besides code could
   account for the change.
3. **Given** the re-run reports, **When** they are published, **Then** each
   records the build under test and which previously-identified bottlenecks are
   present or absent in it — specifically that the serialized delta-path lock is
   absent on current `main`.
4. **Given** a re-run loses a worker shard as two April runs did, **When** the
   report is produced, **Then** it states the shard loss, the resulting reduction
   in offered load, and whether the aggregate remains usable — rather than
   presenting a partial run's throughput as a full-fleet result.
5. **Given** the re-run is against a live testnet-backed deployment, **When**
   write-path numbers are reported, **Then** chain-side wait is separated from
   Guardian-side wait, or the report states that it is not and that its write
   numbers therefore include testnet confirmation behaviour.

---

### User Story 2 - Agree what each target number means (Priority: P1)

A maintainer, a reviewer, and a benchmark author need to read "`get_state` p95 ≤ 1s
at 20k concurrent readers" and agree, without discussion, on what would have to be
observed for that to be true: who is counted, where the clock starts and stops,
over which window, and what disqualifies a run.

**Why this priority**: Every other part of this feature is worthless without it.
Two runs that disagree on what "concurrent" means produce numbers that cannot be
compared to each other, to the February report, or to the target — which is the
present situation. This story is independently valuable: it ships as a written
measurement definition and immediately lets existing reports be re-interpreted.

**Independent Test**: Give the definition and an existing run's raw artifacts to
someone who did not run the benchmark; they can recompute every headline number
and reach the same value, and can state which acceptance criteria the run does and
does not address.

**Acceptance Scenarios**:

1. **Given** the published measurement definition, **When** a reviewer reads the
   read-latency criterion, **Then** it states the counted population, the
   observation point (client-observed round trip, server-side service time, or
   both), the steady-state window excluding warmup and provisioning, and the
   aggregation used for p95.
2. **Given** a benchmark run's artifacts, **When** a reviewer recomputes the
   headline read p95 from the raw per-operation records, **Then** the recomputed
   value matches the reported value.
3. **Given** the February 2026 report, **When** it is re-read against the new
   definition, **Then** each of its runs is labelled with which target dimensions
   it does and does not measure, and which of its numbers are not comparable to
   the target.
4. **Given** a run in which some requests fail, **When** the report is produced,
   **Then** every failed operation carries exactly one named failure category and
   the count of unclassified failures is reported as zero.

---

### User Story 3 - Generate load at the target shape (Priority: P1)

A benchmark author needs to place the defined load on a single Guardian
deployment — 20,000 concurrent readers, 100 concurrent transacting users, and a
population of 100,000 guarded accounts — and needs to know whether the resulting
numbers describe the server or merely the load generator.

**Why this priority**: This is the "how to get the numbers" capability. Without
it the target dimensions are unreachable regardless of how well they are defined:
the highest reader count ever exercised is 10,000, from one machine, where the
client's own queueing dominated the measurement (p50 ≈ 4.4 min for a single
`get_state` per user).

**Independent Test**: Run the target-load profile against a deployment and obtain
a report whose per-criterion verdicts are populated and which states, with
evidence, whether the load generators had headroom. This delivers value even if
every verdict is FAIL — a trustworthy FAIL with a number attached is the
deliverable.

**Acceptance Scenarios**:

1. **Given** the target-load profile, **When** it runs, **Then** the reported
   concurrency actually sustained equals or exceeds 20,000 readers and 100
   transacting users under the agreed definition, or the report declares the
   shortfall and its cause.
2. **Given** load spread over multiple generator hosts, **When** the run
   completes, **Then** the report includes generator-side saturation evidence
   sufficient to attribute observed latency to the server rather than the
   generator.
3. **Given** a population of 100,000 guarded accounts is required, **When**
   provisioning runs, **Then** provisioning time is reported separately from the
   measured window, the population is resumable after interruption, and it is
   reusable by later runs without full re-provisioning.
4. **Given** the auth window depends on request timestamps and load is
   distributed, **When** a run executes, **Then** clock divergence across
   generator hosts is measured and reported, so timestamp-window failures can be
   attributed to server-side queueing rather than to generator clock skew.
5. **Given** server-side rate limiting is active, **When** the run completes,
   **Then** rate-limit rejections are reported as their own category with the
   configured limits recorded, and are not counted as saturation.

---

### User Story 4 - Publish the gap to the target (Priority: P2)

A maintainer planning the scalability workstreams needs one document stating, per
dimension and per acceptance criterion, the current measured value, the target
value, and the numeric distance between them, with ranked evidence for what is in
the way.

**Why this priority**: Turns the target from an aspiration into a tracked gap and
lets the follow-up workstreams be prioritised by measured contribution rather
than by the current list of suspected bottlenecks — one of which (the serialized
`network_client` lock in the delta path) is already absent from `main` and would
otherwise be optimised twice.

**Independent Test**: A reader who has not followed the work can name, from the
baseline document alone, which dimension is furthest from target and what the next
experiment should be.

**Acceptance Scenarios**:

1. **Given** a completed target-load run, **When** the baseline is published,
   **Then** it contains a gap table with one row per dimension and acceptance
   criterion: measured value, target value, distance, and PASS/FAIL.
2. **Given** the baseline, **When** a criterion is FAIL, **Then** the document
   cites the run artifacts supporting the attributed cause and marks each cause
   as measured or hypothesised.
3. **Given** the deployment under test, **When** the baseline is published,
   **Then** it records the full environment description — server version, storage
   and metadata pool sizes, database shape and settings, instance sizing,
   signature scheme, transport, rate limits — so the run can be reproduced.

---

### User Story 5 - Re-measure to show progress (Priority: P3)

An engineer who has changed the auth path or the connection pool needs to re-run
the same measurement and see whether the change moved the number, in a form a
reviewer can trust.

**Why this priority**: Makes the follow-up optimisation work verifiable rather
than plausible. Lower priority because the first baseline already unblocks
planning; comparison support only becomes load-bearing once changes start landing.

**Independent Test**: Run the same profile twice against an unchanged deployment,
then compare: the run-to-run spread is reported, and the report states the
smallest change in each headline metric that this spread would let a reviewer
distinguish from noise.

**Acceptance Scenarios**:

1. **Given** two runs of the same profile against the same unchanged deployment,
   **When** they are compared, **Then** the report quantifies run-to-run variance
   per headline metric and states the resulting detection threshold.
2. **Given** a baseline and a later run, **When** they are compared, **Then** the
   comparison names each metric that improved, regressed, or is within noise.
3. **Given** any completed run, **When** it finishes, **Then** benchmark-created
   accounts and data are identifiable as benchmark-owned and removable, and the
   report states what was left behind.

---

### Edge Cases

- **Client-bound results**: the load generators saturate before the server does.
  The run must be reported as generator-bound and its latency marked as not
  attributable to the server, rather than published as a server measurement.
- **Auth-window expiry as a measurement artefact**: at 10k readers the February
  mixed run lost 14.1% of requests (7,053/50,000) to the 5-minute
  replay-protection window. Requests that expire while queued must count as
  failures against the "zero auth-window failures" criterion, and must never be
  silently retried, re-timestamped, or dropped from the latency distribution —
  either would turn a target violation into a clean-looking result.
- **Provisioning failure partway through 100k accounts**: the population must be
  resumable; a partial population must be reported as partial and never presented
  as a 100k-account run.
- **Chain-dependent waiting**: canonicalization waits on an external chain. The
  report must separate Guardian-side wait from chain-side wait, or state
  explicitly that it does not and that the criterion is measured end-to-end
  including chain time.
- **Measurement perturbing the workload**: sampling a canonicalization wait
  currently pauses the sampling worker's load generation. Sampling must either not
  reduce offered load, or the report must state the reduction.
- **Target dimensions in isolation vs together**: reads at target concurrency,
  writes at target concurrency, and a 100k-account population are three runs plus
  one combined run. The definition must state which combination the acceptance
  criteria apply to, since February's evidence shows reads alone behave very
  differently from a 4:1 read/write mix.
- **Zero-failure criteria with a nonzero failure floor**: infrastructure-level
  errors (connection resets, load-balancer timeouts) must be classified
  separately so "zero auth-window failures" stays checkable in a run that is not
  otherwise perfectly clean.
- **Population present but idle**: 100,000 accounts that exist but are never
  touched exercises storage size, not lookup behaviour under load. The definition
  must state how much of the population the measured operations reach.

## Requirements *(mandatory)*

### Functional Requirements

**Re-baselining on current code**

- **FR-A01**: The existing April production profiles MUST be re-runnable unchanged
  against the current deployment, so that a difference in results is attributable
  to the build and environment rather than to a changed profile.
- **FR-A02**: The existing February local profiles MUST likewise be re-runnable
  unchanged, giving a same-machine-class comparison independent of the deployed
  environment.
- **FR-A03**: Each re-run report MUST be comparable to its historical counterpart
  on every metric that counterpart published, and MUST name per metric whether it
  improved, regressed, or is within noise.
- **FR-A04**: Each re-run MUST record the build under test and the status of each
  previously-identified bottleneck in it — present, absent, or unknown — so a
  fixed bottleneck is not optimised twice.
- **FR-A05**: A re-run report MUST state every configuration difference from its
  historical counterpart (instance sizing, pool sizes, rate limits, database
  settings, network, scheme, transport, user count), so code-attributable change
  is separable from environment-attributable change.
- **FR-A06**: When a run loses worker shards, the report MUST state the loss, the
  resulting reduction in offered load, and whether the aggregate remains usable;
  a partial-fleet aggregate MUST NOT be presented as a full-fleet result.
- **FR-A08**: Each run MUST capture the server's self-reported build identity at
  run start and again at run end, and MUST record it in the report. If the
  reported commit or process start time changes between the two captures, the run
  MUST be flagged as spanning a restart or redeploy and MUST NOT carry a target
  verdict — its measured window covered more than one server instance.
- **FR-A07**: The re-baseline MUST close the three coverage gaps in the April
  round before its results are treated as target evidence: accepted→canonical
  wait must be measured rather than admission latency alone, auth-window failures
  must be counted, and per-criterion verdicts must be emitted.

**Defining the numbers**

- **FR-001**: The scalability target MUST be recorded as a named, versioned set
  of dimensions and acceptance criteria, each with exactly one unit, so a report
  can cite the target version it was measured against.
- **FR-002**: Each dimension and each acceptance criterion MUST have a written
  measurement definition stating: the population counted, the observation point,
  the start and stop of any timing, the steady-state window, the aggregation, and
  the conditions that disqualify a run.
- **FR-003**: A "concurrent user" MUST be defined as a connected client issuing
  operations at a stated per-user rate, not as a simultaneously in-flight request.
  A concurrent reader is one client issuing `get_state` at the target's declared
  read rate; a concurrent transacting user is one client issuing `push_delta`
  followed by canonicalization at the target's declared write rate. Every report
  MUST restate both rates alongside the user counts, because the user count alone
  does not determine offered load.
- **FR-003a**: The target's declared read rate MUST be recorded as a first-class,
  reviewable parameter of the target version (FR-001), so that changing it is a
  visible change to the target rather than a silent change to a benchmark script.
  The initial value is one `get_state` per user per 10 seconds, giving 2,000
  reads/s sustained at 20,000 concurrent readers.
- **FR-003b**: Peak in-flight request concurrency MUST be measured and reported as
  an observed output of every run, never used as the definition of the user count.
  The two readings differ by more than an order of magnitude at the same user
  count, so conflating them silently changes the target.
- **FR-003c**: In-flight-saturation runs — the February reading, where N users
  means N outstanding requests — MUST remain available as a separate stress
  ceiling and MUST be reported as a ceiling, not as a target verdict.
- **FR-004**: The measurement definition MUST state, for the read criterion,
  whether p95 ≤ 1s applies to client-observed round-trip latency or to
  server-side service time, and reports MUST carry both where available — the
  February runs show these diverge by orders of magnitude once client-side
  queueing dominates.
- **FR-005**: The canonicalization criterion MUST be defined as the wait from
  delta acceptance to a terminal outcome, MUST state how it is sampled, and MUST
  state whether external chain time is included.
- **FR-005a**: The measurement definition MUST state that end-to-end
  accepted→canonical wait is **not observable from synthetic benchmark load**. A
  delta becomes canonical only when the on-chain account commitment advances to
  its `new_commitment`, and that transaction is submitted by the client, not by
  Guardian — the harness never performs it. Measured on the diagnostic tier: 16
  of 16 sampled pushes timed out at 300s with 1,056 canonicalization passes
  running, zero retries, and healthy chain connectivity.
- **FR-005b**: A run whose workload does not perform the on-chain step MUST
  report the canonicalization criterion as `not_measured` with that reason, and
  MUST NOT report it as a timeout, a failure, or a passing zero. The observable
  substitutes — canonicalization pass latency, candidate age under load, and push
  admission latency — MUST be labelled as substitutes rather than as the
  criterion.
- **FR-005c**: The canonicalization criterion MUST be measured by a real-account
  workload that proves and submits Miden transactions, not by the synthetic-delta
  harness. Such a workload MUST be driven by a fixture of pre-provisioned, funded
  accounts established before the measured window, and the fixture size MUST be
  configurable to the concurrency the criterion states.
- **FR-005d**: Account fixtures for real-account workloads MUST persist each
  account's identity and key material **before** registering it with Guardian, and
  MUST refuse to overwrite an existing fixture. A partially-provisioned fixture
  MUST be preserved rather than discarded, because it may already contain keys for
  registered and funded accounts whose loss would strand funds.
- **FR-005e**: The three target dimensions MUST NOT be forced onto a single
  workload. Reads at target scale MUST remain synthetic — a funded-account
  prerequisite would cap the read dimension at whatever can be faucet-funded,
  which is orders of magnitude below 20,000. Only the write and canonicalization
  measurements require funded accounts.
- **FR-006**: The definition MUST state which acceptance criteria apply to
  single-dimension runs and which apply to a combined run at full target load.
- **FR-007**: The definition MUST fix the signature scheme and transport used for
  headline numbers, and MUST require per-scheme reporting where schemes differ
  materially — February measured Falcon writes at roughly 2.8 ops/s against ECDSA
  at 108.5 ops/s under the same profile.
- **FR-008**: Auth-window (replay-protection) failures MUST be counted and
  reported as their own category; any nonzero count MUST fail the corresponding
  criterion.
- **FR-009**: Every failed operation in a report MUST be assigned exactly one
  named failure category, and the count of unclassified failures MUST be reported.
- **FR-010**: Provisioning, warmup, measured, and teardown phases MUST be reported
  as separate durations; headline metrics MUST derive from the measured window
  only.

**Obtaining the numbers**

- **FR-011**: The benchmark capability MUST sustain at least 20,000 concurrent
  readers and at least 100 concurrent transacting users, as defined in FR-003,
  against a single Guardian deployment.
- **FR-012**: Load generation MUST be able to span multiple generator hosts, with
  a run's results aggregated into one report.
- **FR-013**: Each run MUST report generator-side saturation evidence sufficient
  to decide whether the result is server-bound or generator-bound, and MUST label
  the run accordingly.
- **FR-014**: Each run MUST measure and report clock divergence across generator
  hosts, since acceptance depends on request timestamps against a 5-minute
  server-side window.
- **FR-015**: The capability MUST be able to establish a population of 100,000
  guarded accounts; provisioning MUST be resumable after interruption and MUST
  report progress.
- **FR-016**: A provisioned account population MUST be reusable across runs
  without full re-provisioning, and each run MUST record which population it used
  and how many distinct accounts the measured operations reached.
- **FR-016a**: The account population MUST be split by whether load reaches it.
  Every account the measured operations touch MUST be provisioned through the same
  externally-visible path a real account takes. The remaining accounts, which exist
  to create population scale rather than to be operated on, MAY be bulk-provisioned
  by a faster path. Each run MUST report the split as two counts.
- **FR-016b**: Bulk-provisioned accounts MUST be verified equivalent to real-path
  accounts in stored state, by comparing a sample of each against the other, and
  the verification result MUST be recorded with the population. An unverified bulk
  population MUST NOT be used for a target verdict — otherwise a measurement of
  100,000 accounts may be measuring 100,000 rows that no real account resembles.
- **FR-016c**: A run MUST NOT report a target verdict for the account dimension if
  its real-path subset is smaller than the number of accounts its own load reaches.
- **FR-017**: Each run MUST record the environment under test: server version or
  commit, storage and metadata pool sizes, database shape and non-default
  settings, instance sizing, rate-limit configuration, signature scheme, and
  transport.
- **FR-017a**: Measurement MUST be organised into three labelled tiers, and every
  report MUST state which tier produced it:
  - **Authoritative** — the deployed, production-shaped, testnet-backed Guardian
    measured by the distributed harness. Only this tier may carry a target
    verdict.
  - **Diagnostic** — a local multi-instance deployment under container
    orchestration, with server-side instrumentation enabled and each variable
    under direct control. This tier MUST NOT carry a target verdict, and is where
    bottleneck attribution and single-variable comparisons are produced.
  - **Fast loop** — the local single-process harness, for quick iteration. Also
    non-authoritative; the February report attributes much of its own observed
    latency to that setup.
- **FR-017c**: Any claim that a specific component is a bottleneck MUST cite
  server-side instrumentation from the diagnostic tier, or be marked
  hypothesised. Client-observed latency alone cannot establish which component is
  responsible.
- **FR-017d**: A diagnostic-tier comparison MUST vary one configuration variable
  at a time against an otherwise identical stack, and MUST record the variable and
  both values, so the result attributes to that variable rather than to drift.
- **FR-017b**: A target verdict MUST state the deployment shape it was measured
  against, including task count and size, and MUST NOT be generalised to a
  different shape without re-measurement — the April round measured a single
  2 vCPU / 4 GB task and concluded on that basis that a 500 TPS mixed target
  would need 2 to 3 instances.
- **FR-018**: Server-side rate-limit rejections MUST be reported as their own
  category with the configured limits recorded, and MUST NOT be conflated with
  saturation or with auth-window failures.
- **FR-019**: Benchmark-created accounts and data MUST be identifiable as
  benchmark-owned and removable, and each run MUST report what it removed and what
  it left behind.
- **FR-020**: A run MUST NOT retry, re-timestamp, or discard operations in a way
  that removes a target violation from the reported result.

**Publishing and re-using the numbers**

- **FR-021**: Each run MUST emit a per-criterion PASS/FAIL verdict against the
  cited target version, including verdicts of "not measured" where a criterion was
  out of the run's scope.
- **FR-022**: A baseline report MUST be published in the repository containing the
  gap table: measured value, target value, numeric distance, and verdict per
  dimension and criterion.
- **FR-023**: The baseline MUST distinguish measured causes from hypothesised
  causes, and MUST cite run artifacts for each measured cause.
- **FR-024**: Two runs of the same profile against an unchanged deployment MUST be
  comparable, and the comparison MUST report run-to-run variance and the resulting
  smallest detectable change per headline metric.
- **FR-025**: A later run MUST be comparable against the published baseline,
  naming per metric whether it improved, regressed, or is within noise.
- **FR-026**: Operator-facing documentation MUST state how to run a target-load
  measurement and how to read its report, including the meaning of each failure
  category.

### Key Entities

- **Scalability Target**: a named, versioned set of dimensions and acceptance
  criteria; the thing a run is judged against.
- **Target Dimension**: one axis of scale (guarded accounts, concurrent
  transacting users, concurrent readers) with a target value and a unit.
- **Acceptance Criterion**: a threshold on an observed metric at target load
  (read p95, canonicalization p95, auth-window failure count).
- **Measurement Definition**: for one dimension or criterion, the counted
  population, observation point, window, aggregation, and disqualifying
  conditions.
- **Load Profile**: a reusable description of one run's offered load — scenario
  mix, user counts, per-user operation rates, duration, warmup, scheme, transport,
  account population.
- **Per-User Rate**: the declared operation rate of one concurrent user; together
  with the user count it determines offered load, which the user count alone does
  not.
- **Account Population**: a provisioned set of guarded accounts of stated size,
  split into a real-path subset that load reaches and a bulk subset that provides
  scale, reusable across runs, resumable while provisioning, identifiable as
  benchmark-owned.
- **Benchmark Run**: one execution of a profile against one environment, producing
  raw per-operation records, phase durations, resource samples, and an environment
  description.
- **Failure Category**: the named class of a failed operation (auth-window expiry,
  rate-limited, state conflict, transport/infrastructure, unclassified).
- **Run Report**: the derived, human-readable result of a run — headline metrics,
  failure breakdown, saturation attribution, per-criterion verdicts.
- **Baseline**: the published run report designated as the current position
  against the target, carrying the gap table.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-000**: The February and April profiles have been re-run against current
  code and each historical metric has a current counterpart with an
  improved / regressed / within-noise judgement; no historical metric is left
  without one, and no re-run result is published without its configuration
  differences stated.
- **SC-001**: All three target dimensions and all three acceptance criteria have a
  written measurement definition; a reviewer who did not run the benchmark can
  recompute 100% of a report's headline numbers from that report's raw artifacts
  and reach the same values.
- **SC-002**: A target-load measurement can be started from a single documented
  command sequence, and a first-time operator following only the documentation
  reaches a completed report without consulting its author.
- **SC-003**: A run at full target load — 20,000 concurrent readers at the declared
  read rate (2,000 reads/s), 100 concurrent transacting users, 100,000 guarded
  accounts — completes and returns a populated verdict for every acceptance
  criterion, with no criterion left unmeasured and unexplained.
- **SC-003a**: Every published run states its user counts, its per-user rates, and
  its observed peak in-flight concurrency as three separate figures; no run reports
  a user count without the rate that gives it meaning.
- **SC-003b**: For any run carrying an account-dimension verdict, the real-path
  subset of its population is at least as large as the number of accounts its load
  reached, and its bulk subset has a recorded equivalence-verification result.
- **SC-004**: 100% of failed operations in every published run carry exactly one
  named failure category; unclassified failures are zero.
- **SC-005**: Every published run states whether it was server-bound or
  generator-bound, with supporting evidence; no run's latency figures are
  presented as a server measurement without that attribution.
- **SC-006**: Run-to-run variance of each headline metric on an unchanged
  deployment is quantified, and the resulting detection threshold is small enough
  that a 25% change in a headline metric is distinguishable from noise.
- **SC-007**: An account population of 100,000 can be established and is reusable
  by subsequent runs without re-provisioning; provisioning is resumable, and its
  duration is reported separately from measured time rather than inflating it —
  February's runs spent most wall time provisioning (20.7 minutes of seeding
  against 9.1 minutes of measurement at 10k accounts).
- **SC-008**: The published baseline states a numeric distance to target for every
  dimension and criterion, such that a reader can name the furthest-from-target
  dimension without further analysis.
- **SC-009**: Every run cleans up or explicitly accounts for the benchmark data it
  created; no published run leaves unaccounted-for benchmark accounts behind.
- **SC-010**: A subsequent measurement after an optimisation change can be
  compared to the baseline and produce a per-metric improved / regressed /
  within-noise judgement.

## Assumptions

- **Scope is measurement, per the request "define how to get required numbers."**
  Closing the performance gap — connection-pool and Postgres tuning, the
  per-request replay-protection write, evaluating a non-relational backend
  (issue #317 workstreams 1, 2, and 4) — is out of scope here and becomes
  measurable and reviewable because of this work.
- Existing benchmark assets are extended rather than replaced: the local harness
  under `crates/server/bench` and the distributed harness under
  `benchmarks/prod-server`, which already provides multi-host worker runs, local
  aggregation of worker artifacts, canonicalization sampling, and benchmark-owned
  data cleanup.
- The measured deployment is testnet-backed, which the deployed production
  configuration already is by default; no network change is needed to reproduce
  the April setup, and write-path numbers therefore carry live testnet
  confirmation behaviour that varies independently of Guardian.
- The server reports its own build identity — version, commit, environment, and
  process start time — on an unauthenticated status endpoint. Build
  identification therefore needs no privileged access, and the start time also
  makes a mid-run restart detectable (FR-A08). Neither the benchmark harness nor
  the client currently captures this, so wiring it up is part of the re-baseline
  rather than an assumption that it already happens.
- The re-baseline runs precede any new profile work. Its results may change the
  priority of the remaining stories — if reads at 4,096 users still sit inside the
  1s criterion, the read question becomes purely about scaling to 20,000, not
  about read latency as such.
- The initial read rate of one `get_state` per user per 10 seconds models a client
  polling for state changes at human-perceptible latency. It puts offered read load
  at 2,000 reads/s, the same order as the 1,409 reads/s April already sustained at
  p95 926ms on a single task — so the target is neither trivially met nor absurd.
  The rate is a parameter of the target, not a fact about clients; if real client
  behaviour is known to differ, changing it is a target revision under FR-003a.
- The real-path share of the 100,000-account population is bounded by what the
  load reaches — roughly 20,100 accounts at target load (20,000 readers plus 100
  transacting users, one account each) — so about a fifth of the population needs
  real provisioning. Distributed provisioning makes that share feasible in a way
  February's single-machine seeding rate would not have.
- The April profiles' 4,096-user shape is a starting point, not the target shape;
  reaching 20,000 concurrent readers requires more offered load than that
  16-worker fleet produced, and the required fleet size is an output of this work.
- "Users reading state" means `get_state`; "users transacting" means `push_delta`
  followed by canonicalization to a terminal outcome. These match the existing
  scenario vocabulary.
- The measured steady-state window is at least 5 minutes, so behaviour relative to
  the 5-minute replay-protection window is observable rather than inferred.
- "Zero auth-window failures" means zero across all operations in the measured
  window, not a small permitted fraction.
- Headline write numbers are reported per signature scheme; the target is judged
  against the scheme a production deployment actually defaults to, because the
  Falcon/ECDSA gap on writes is large enough to change the verdict.
- gRPC is the headline transport, matching the existing production benchmark
  profiles, with HTTP reported where the two differ materially.
- No production customer data is used; all accounts under measurement are
  benchmark-owned and removed by the harness's existing cleanup path.
- The April round measured the live production endpoint directly. A replay at
  April's scale inherits that precedent, but target-scale runs — 20,000 concurrent
  readers — amount to a deliberate load event against production and MUST be
  either scheduled and announced or directed at a separate production-shaped
  stack. Which of the two is a decision for the maintainers of that deployment,
  and the choice MUST be recorded in the run's environment description, since a
  dedicated stack and the live endpoint are not interchangeable evidence.
- Issue #317's third listed bottleneck — a serialized `network_client` lock in
  `push_delta` — is no longer present on `main`; the write-path measurement is
  defined against current behaviour, not against that description.

## Dependencies

- A deployed, testnet-backed Guardian to measure, and the access needed to
  describe and reconfigure it (pool sizes, instance shape, rate limits), plus the
  AWS access the distributed harness needs to launch worker tasks and to run its
  benchmark-owned data cleanup.
- The February and April profiles and their published reports, as the comparison
  baseline for the re-run.
- Enough load-generation capacity to reach 20,000 concurrent readers under the
  definition chosen in FR-003, which is beyond a single developer machine.
- An available Miden network for the write path; chain-side latency is outside
  Guardian's control and must be separated or explicitly included per FR-005.
- Sufficient time and capacity to provision a 100,000-account population under the
  fidelity decision in FR-016.

## Out of Scope

- Implementing the performance improvements needed to pass the acceptance
  criteria.
- Making the target-load measurement part of routine continuous integration.
- Multi-instance horizontal-scaling correctness (issue #242).
- Decoupling canonicalization from API instances (issue #190).
- Changing the replay-protection window or the auth path (measured here, changed
  elsewhere).
