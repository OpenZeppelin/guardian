# Phase 1 Data Model: Scalability Target Measurement

Entities are benchmark-side. None of them live in the Guardian server or in any
client SDK, and none change a wire contract.

## ScalabilityTarget

The versioned statement a run is judged against (FR-001). Stored as
`benchmarks/prod-server/targets/scalability-317.toml`.

| Field | Type | Notes |
|---|---|---|
| `target_id` | string | `scalability-317` |
| `target_version` | string | Bumped whenever any dimension, criterion, or declared rate changes |
| `dimensions` | TargetDimension[] | Exactly three for this target |
| `criteria` | AcceptanceCriterion[] | Exactly three for this target |
| `declared_read_rate_per_sec` | float | `0.1` — one `get_state` per user per 10s (FR-003a) |

**Rule**: A report cites `target_version`. Changing the declared rate is a version
bump, never a silent profile edit.

### TargetDimension

| Field | Type | Notes |
|---|---|---|
| `name` | enum | `guarded_accounts` \| `transacting_users` \| `reading_users` |
| `target_value` | u64 | `100000` \| `100` \| `20000` |
| `unit` | string | `accounts` \| `users` \| `users` |

### AcceptanceCriterion

| Field | Type | Notes |
|---|---|---|
| `name` | enum | `read_p95` \| `canonicalization_p95` \| `auth_window_failures` |
| `metric` | string | Which reported metric it reads |
| `comparator` | enum | `lte` \| `eq` |
| `threshold` | float | `1000.0` ms \| `30000.0` ms \| `0` |
| `applies_to` | enum | `single_dimension` \| `combined_run` (FR-006) |

## MeasurementDefinition

Prose plus structure, one per dimension and per criterion (FR-002). Lives beside
the target file and is cited by reports.

| Field | Type | Notes |
|---|---|---|
| `subject` | string | Dimension or criterion name |
| `counted_population` | string | What is counted |
| `observation_point` | enum | `client_round_trip` \| `server_service_time` \| `both` (FR-004) |
| `timing_start` / `timing_stop` | string | Where the clock starts and stops |
| `window` | string | Steady-state window, warmup and provisioning excluded |
| `aggregation` | string | e.g. p95 over successful operations |
| `disqualifiers` | string[] | Conditions voiding the run |

## LoadProfile

The TOML a run executes. Extends the current `RunConfig` — see
`contracts/profile-schema.md` for the full contract.

| Field | Type | Notes |
|---|---|---|
| `load_model` | enum | **new** — `paced` \| `saturating` (FR-003 / FR-003c) |
| `read_rate_per_user_per_sec` | float | **new** — required when `paced` |
| `users` | u32 | Concurrent clients, not in-flight requests |
| `population` | PopulationRef | **new** — replaces the `accounts_per_user == 1` pin |
| `operation_mix` | OperationMix | Existing; `reads_per_push`, retire flag |
| `scheme_distribution` | SchemeDistribution | Existing; must sum to 100 |
| `canonicalization` | CanonicalizationConfig | Existing; sampling now off the critical path |
| `duration_seconds` / `warmup_seconds` | u64 | Measured window ≥ 300s for target runs |

**Rules**
- `paced` requires a declared rate; `saturating` forbids one.
- A profile carrying a target verdict must set `duration_seconds - warmup_seconds ≥ 300`.

## AccountPopulation

A provisioned, reusable, resumable set of guarded accounts (FR-015, FR-016).
Manifest persisted so later runs bind to it without re-provisioning.

| Field | Type | Notes |
|---|---|---|
| `population_id` | string | Stable identifier referenced by profiles |
| `total_accounts` | u64 | e.g. `100000` |
| `real_path_accounts` | u64 | Provisioned via `configure` |
| `bulk_accounts` | u64 | Provisioned via the privileged channel |
| `provisioning_state` | enum | `partial` \| `complete` |
| `provisioned_count` | u64 | Resumption cursor |
| `equivalence_check` | EquivalenceCheck? | Required before an account-dimension verdict |
| `provisioning_seconds` | f64 | Reported separately from measured time (FR-010) |

**State transitions**: `absent → partial → complete`. A `partial` population may
be resumed but may never be reported as its `total_accounts` (spec edge case).

### EquivalenceCheck

| Field | Type | Notes |
|---|---|---|
| `sampled_real` / `sampled_bulk` | u64 | Sample sizes compared |
| `fields_compared` | string[] | Stored-state fields |
| `result` | enum | `equivalent` \| `divergent` |
| `divergences` | string[] | Empty when equivalent |

**Rule (FR-016b)**: `divergent` or absent → no account-dimension verdict.

## BenchmarkRun

One execution of one profile against one environment.

| Field | Type | Notes |
|---|---|---|
| `run_id` | string | Existing convention `<timestamp>-<hash>` |
| `target_version` | string | **new** — what it is judged against |
| `profile` | LoadProfile | Resolved profile |
| `population_id` | string | **new** |
| `build_identity_start` / `build_identity_end` | BuildIdentity | **new** (FR-A08) |
| `phase_durations` | PhaseDurations | **new** — provisioning / warmup / measured / teardown |
| `shards_expected` / `shards_recovered` | u32 | **new** (FR-A06) |
| `clock_samples` | ClockSample[] | **new** (FR-014) |

### BuildIdentity

Captured from `/status`. `version`, `git_commit`, `environment`, `started_at`.

**Rule**: `git_commit` or `started_at` differing between start and end → run flagged
`spanned_restart`, verdicts withheld.

### ClockSample

Per generator host: `host_id`, `offset_ms`, `measured_at`. Max absolute offset is
reported; the auth window is 300,000ms, so a material fraction of that voids the
auth-window criterion's attribution.

### PhaseDurations

`provisioning_seconds`, `warmup_seconds`, `measured_seconds`, `teardown_seconds`.
Headline metrics derive from the measured window only.

## OperationOutcome

Per-operation record (aggregated, not persisted individually at target scale).

| Field | Type | Notes |
|---|---|---|
| `operation` | enum | `get_state` \| `push_delta` |
| `scheme` | enum | `falcon` \| `ecdsa` |
| `succeeded` | bool | |
| `latency_ms` | f64 | Client round trip |
| `failure_category` | FailureCategory? | Present iff failed |

### FailureCategory

`auth_window` (**new**, matched before `auth`), `auth`, `rate_limited` (**new**),
`state_conflict`, `transport`, `timeout`, `upstream_miden`, `not_found`,
`server`, `unclassified` (**new** fallthrough).

**Rules**
- Exactly one category per failed operation (FR-009).
- `auth_window` must be evaluated before `auth`, or it is absorbed by it.
- `unclassified` replaces the current silent fallthrough to `server`.

## RunReport

Derived output. Full contract in `contracts/run-report-schema.md`.

Adds to the existing report: `target_version`, `build_identity_*`,
`concurrency` (declared users, declared rate, offered rate, **achieved** rate,
observed peak in-flight), `saturation` (`server_bound` \| `generator_bound`, with
per-worker evidence), `verdicts`, `phase_durations`, `shard_recovery`,
`population` summary, `clock_divergence_ms`.

### CriterionVerdict

| Field | Type | Notes |
|---|---|---|
| `criterion` | string | |
| `measured_value` | f64? | Absent when not measured |
| `target_value` | f64 | |
| `distance` | f64? | Signed gap (FR-022) |
| `verdict` | enum | `pass` \| `fail` \| `not_measured` |
| `withheld_reason` | string? | e.g. `spanned_restart`, `generator_bound` |

**Rule**: A run flagged `spanned_restart`, `generator_bound`, or lacking an
equivalence check emits `not_measured` with a reason — never a silent `pass`.

## Baseline

The `RunReport` designated as the current position, published under
`benchmarks/prod-server/results/`. Adds `gap_table` (one row per dimension and
criterion) and `causes` — each tagged `measured` or `hypothesised` with citing
artifacts (FR-023).

## ComparisonReport

Two runs judged against each other (FR-A03, FR-024, FR-025).

| Field | Type | Notes |
|---|---|---|
| `baseline_run_id` / `candidate_run_id` | string | For a replay, the historical report is the baseline |
| `metric_deltas` | MetricDelta[] | Per metric |
| `config_differences` | string[] | Everything differing besides code (FR-A05) |
| `detection_threshold` | map | Smallest distinguishable change per metric (FR-024) |

### MetricDelta

`metric`, `baseline_value`, `candidate_value`, `judgement` ∈
`improved` \| `regressed` \| `within_noise`.

**Rule**: `within_noise` when the delta is below that metric's detection
threshold — never asserted without one.
