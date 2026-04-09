# Reporting and Interpretation

Use this when converting benchmark artifacts into conclusions.

## Required Inputs

Read these artifacts for the run:

- `run-report.json`
- `summary.md`
- `cleanup-manifest.json`

## Required Metrics

Always report:

- profile name
- worker count
- worker size
- user count
- account count
- signer distribution
- `push_delta/s`

For mixed runs also report:

- `get_state/s`

For latency, always include:

- `p50`
- `p95`
- `p99`
- `max`

## Caveats to Call Out

### Missing Worker Shards

If the run was recovered from fewer than the intended workers:

- state the exact fraction, for example `15/16`
- mark the result as slightly undercounted
- do not present it as a clean full-sample run

### `state_conflict` Failures

If most write failures are `state_conflict`, interpret that as:

- the workload is reusing accounts too aggressively
- the run is measuring account eligibility limits, not the full server write ceiling

Do not present that as the server's absolute `push_delta/s` limit.

### Burst vs Sustained

The kept profiles are burst-style admission tests.

- successful writes are tied to unique benchmark-owned accounts
- mixed profiles retire an account after its first successful `push_delta`
- these runs are good for admission capacity and planning
- these runs are not a sustained long-lived account reuse benchmark

## Sizing Rule

When the user asks how many GUARDIAN tasks are needed for a `500 TPS` reference target, use:

- the run's `push_delta/s`
- `30%` headroom

Formula:

`required_tasks = ceil(500 / (push_delta_per_task * 0.70))`

Frame this as a sizing reference, not as a requirement that one GUARDIAN task must absorb the full network TPS.

## Report Structure

Use this order:

1. Goal
2. Setup
3. Reference capacity target
4. Results summary
5. Results table
6. Latency
7. Interpretation

## Setup Section Checklist

Include:

- endpoint
- client region
- server task count
- server CPU and memory if known
- RDS Proxy status if known
- worker count
- worker CPU and memory
- total users
- accounts per user
- signer mix
- workload ratio

## Interpretation Rules

- Use mixed Falcon+ECDSA as the main planning signal when the user asks about realistic traffic.
- Use pure ECDSA burst as the best pure write-admission reference.
- If latency rises materially with load, say that explicitly; do not discuss throughput alone.
- If cleanup completed, say that benchmark-owned rows were purged and temporary ECS resources were torn down.
