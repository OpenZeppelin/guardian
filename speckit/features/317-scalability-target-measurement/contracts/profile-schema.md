# Contract: Benchmark Profile TOML

Consumed by `RunConfig::load_from_path` (`benchmarks/prod-server/src/config.rs`).
This contract is between profile authors and the harness. Marked **new** where it
extends today's schema.

## Full shape

```toml
profile_name = "target-read-20k"
guardian_endpoint = "https://guardian.openzeppelin.com"
transport = "grpc"
duration_seconds = 600
warmup_seconds = 60
deployment_shape = "prod-single-task-arm64-rds-proxy"
artifacts_dir = "benchmarks/prod-server/reports"

target_id = "scalability-317"          # new — which target this run is judged against
target_version = "1.0.0"               # new — pinned; mismatch is a hard error

[load]                                  # new section
model = "paced"                         # "paced" | "saturating"
users = 20000
read_rate_per_user_per_sec = 0.1        # required when model = "paced"
phase_offset = "random"                 # "random" | "none"; guards thundering herd

[population]                            # new section
population_id = "pop-100k-v1"
total_accounts = 100000
require_equivalence_check = true

[operation_mix]
read_operation = "get_state"
reads_per_push = 4
retire_after_first_successful_push = false

[scheme_distribution]
falcon_percent = 50
ecdsa_percent = 50

[canonicalization]
sample_rate = 0.05
poll_interval_ms = 1000
timeout_seconds = 180
off_critical_path = true                # new — sampling must not stall offered load

[cleanup]
enabled = true

[aws]
profile = "dev"
region = "us-east-1"
ecs_cluster = "guardian-prod-cluster"
ecs_service = "guardian-prod-server"
```

## Validation rules

Existing rules retained: non-empty `profile_name`, `guardian_endpoint`,
`aws.region`, `aws.ecs_cluster`, `aws.ecs_service`; `duration_seconds > 0`;
`users > 0`; scheme percentages sum to 100; `sample_rate ∈ [0,1]`;
`poll_interval_ms > 0`; `timeout_seconds > 0`.

**Removed**: `accounts_per_user must be exactly 1 for phase 1` — replaced by the
`[population]` section. This is the change that makes the 100k dimension
expressible.

**Added**:

| Rule | Rationale |
|---|---|
| `model = "paced"` requires `read_rate_per_user_per_sec > 0` | Offered load must be an input, not an output |
| `model = "saturating"` forbids `read_rate_per_user_per_sec` | The two models are mutually exclusive readings of "user" |
| A profile carrying a target verdict requires `duration_seconds - warmup_seconds >= 300` | The auth window is 300s; a shorter window cannot observe behaviour against it |
| `target_version` must match the referenced target file | A silently-drifted target makes reports incomparable |
| `population.total_accounts >= users` | Load cannot reach more accounts than exist |
| `require_equivalence_check = true` when the population has a bulk subset and the run carries an account-dimension verdict | FR-016b |
| `off_critical_path = true` required for `paced` runs | A stalled paced worker silently drops its offered rate to zero |

## Compatibility

The four April profiles are pinned unchanged as `replay-*.toml` for US1. They
omit the new sections, so absent `[load]` defaults to
`model = "saturating"` with `users` read from the top level — preserving today's
semantics exactly. A replay must reproduce April's behaviour, so it must not
silently acquire pacing.

`accounts_per_user = 1` in the pinned profiles is accepted and mapped to a
population of `users` real-path accounts with no bulk subset.
