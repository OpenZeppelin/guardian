# Reference Results

These are the benchmark results from the April 8, 2026 benchmark round against the prod deployment.

Use them as comparison points or when a user asks what the previous benchmark established.

## Deployment Under Test

- endpoint: `https://guardian.openzeppelin.com`
- clients: distributed ECS/Fargate workers in `us-east-1`
- server shape: `1` ECS task, ARM64, `2 vCPU / 4 GB`, RDS Proxy enabled

## Worker Setup

- `16` ephemeral ECS/Fargate worker tasks
- `2 vCPU / 4 GB` each
- `4096` total users
- `1` account per user

## Throughput Summary

| Scenario | `push_delta/s` | `get_state/s` | `push_delta` p95 | Estimated GUARDIAN tasks for `500 TPS` with `30%` headroom | Notes |
| --- | ---: | ---: | ---: | ---: | --- |
| Pure write, ECDSA burst | `612.65` | `0.00` | `4156ms` | `2` | `4096` unique accounts |
| Mixed `1:4`, ECDSA only | `284.28` | `1137.14` | `2734ms` | `3` | recovered from `15/16` workers |
| Mixed `1:4`, Falcon only | `376.88` | `1507.51` | `3379ms` | `2` | recovered from `15/16` workers |
| Mixed `1:4`, Falcon+ECDSA | `352.42` | `1409.67` | `3935ms` | `3` | clean `16/16` worker run |

## Latency Reference Ladder

| Pressure | `get_state` p50 / p95 | `push_delta` p50 / p95 | Notes |
| --- | --- | --- | --- |
| Low | `37ms / 60ms` | `212ms / 228ms` | small distributed ECS smoke run |
| Medium | `227ms / 503ms` | `768ms / 1143ms` | ECDSA intermediate-pressure burst |
| High | `718ms / 926ms` | `2972ms / 3935ms` | mixed Falcon+ECDSA `1:4` run |

## Main Conclusions

- The current prod shape admitted more than `500 push_delta/s` in the pure write ECDSA burst.
- The current single-task prod shape did not reach `500 TPS` for the realistic mixed workload.
- The mixed Falcon+ECDSA run was the best planning signal and suggested `3` GUARDIAN tasks for a `500 TPS` reference target with `30%` headroom.
- Latency increased with load, especially on `push_delta`.
