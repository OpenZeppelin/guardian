# Prod Server Benchmark Report

Date: `2026-04-08`

Deployment under test:
- endpoint: `https://guardian.openzeppelin.com`
- clients: distributed ECS/Fargate workers in `us-east-1`

## Goal

Measure how much throughput the current prod Guardian deployment can admit, first in a pure write (`push_delta`) burst and then under a `1 push_delta : 4 get_state` mixed workload.

## Setup

Server under test:
- `1` ECS server task
- ARM64
- `2 vCPU / 4 GB`
- RDS Proxy enabled
- public HTTP/gRPC endpoint through `https://guardian.openzeppelin.com`

Benchmark client setup:
- `16` ephemeral ECS/Fargate worker tasks
- worker size: `2 vCPU / 4 GB` each
- workers launched in `us-east-1`

Workload setup:
- `4096` total users
- `1` account per user
- `4096` total benchmark-owned accounts per run

Signer setup:
- ECDSA-only mixed run: `4096` ECDSA accounts
- Falcon-only mixed run: `4096` Falcon accounts
- mixed-signer run: `2048` ECDSA accounts and `2048` Falcon accounts

## Reference Capacity Target

Treat `500 TPS` as a reference capacity target for sizing how many Guardian server tasks are needed, to cover the total theoretical Miden network throughput.

## Results Summary

The key planning result from this round is the mixed Falcon+ECDSA run:
- `352.42 push_delta/s` (352 TPS)
- `1409.67 get_state/s`

That implies: if `500 TPS` is the sizing target for the Guardian, the safer current answer is running 2 to 3 intances.

## Results

| Scenario | Run | `push_delta/s` | `get_state/s` | `push_delta` p95 | Estimated GUARDIAN tasks to cover a 500 TPS target with 30% headroom | Notes |
| --- | --- | ---: | ---: | ---: | ---: | --- |
| Pure write, ECDSA burst | [20260408T145050Z-b0c4a68d](https://github.com/OpenZeppelin/guardian/blob/benchmarks-against-prod/benchmarks/prod-server/reports/20260408T145050Z-b0c4a68d/run-report.json) | `612.65` | `0.00` | `4156ms` | `2` | `4096` unique accounts, burst mode |
| Mixed `1:4`, ECDSA only | [20260408T151157Z-7fdbac04](https://github.com/OpenZeppelin/guardian/blob/benchmarks-against-prod/benchmarks/prod-server/reports/20260408T151157Z-7fdbac04/run-report.json) | `284.28` | `1137.14` | `2734ms` | `3` | recovered from `15/16` worker shards |
| Mixed `1:4`, Falcon only | [20260408T155822Z-6b1f5622](https://github.com/OpenZeppelin/guardian/blob/benchmarks-against-prod/benchmarks/prod-server/reports/20260408T155822Z-6b1f5622/run-report.json) | `376.88` | `1507.51` | `3379ms` | `2` | recovered from `15/16` worker shards |
| Mixed `1:4`, Falcon+ECDSA | [20260408T161344Z-a78b2def](https://github.com/OpenZeppelin/guardian/blob/benchmarks-against-prod/benchmarks/prod-server/reports/20260408T161344Z-a78b2def/run-report.json) | `352.42` | `1409.67` | `3935ms` | `3` | full `16/16` worker run |

## Latency

`get_state` latency in that run:
- p50: `718ms`
- p95: `926ms`
- p99: `998ms`
- max: `1895ms`

`push_delta` latency in that run:
- p50: `2972ms`
- p95: `3935ms`
- p99: `4064ms`
- max: `6602ms`

Low-, medium-, and high-pressure references:

| Pressure | Run | `get_state` p50 / p95 | `push_delta` p50 / p95 | Notes |
| --- | --- | --- | --- | --- |
| Low | [20260408T135335Z-fa3463cd](https://github.com/OpenZeppelin/guardian/blob/benchmarks-against-prod/benchmarks/prod-server/reports/20260408T135335Z-fa3463cd/run-report.json) | `37ms / 60ms` | `212ms / 228ms` | small distributed ECS smoke run |
| Medium | [20260408T130128Z-cf025bd9](https://github.com/OpenZeppelin/guardian/blob/benchmarks-against-prod/benchmarks/prod-server/reports/20260408T130128Z-cf025bd9/run-report.json) | `227ms / 503ms` | `768ms / 1143ms` | ECDSA-only intermediate-pressure burst |
| High | [20260408T161344Z-a78b2def](https://github.com/OpenZeppelin/guardian/blob/benchmarks-against-prod/benchmarks/prod-server/reports/20260408T161344Z-a78b2def/run-report.json) | `718ms / 926ms` | `2972ms / 3935ms` | mixed Falcon+ECDSA `1:4` run |

What that means:
- Guardian can admit a lot of concurrent traffic, but write admission is still a multi second operation at high load levels
- read latency is under a second at p95
- compared across the low-, medium-, and high-pressure references above, latency rises with load, especially on `push_delta`

## Interpretation

Two separate conclusions came out of this round.

Throughput conclusion:
- the deployment can burst above `500 push_delta/s` without read operations.
- the deployment does not reach a `500 TPS` mixed workload on a single instance
- the current planning number for a realistic signer mix is 2 to 3 instances if `500 TPS` is the reference sizing target

Latency conclusion:
- the system is throughput capable, but request latency is already high at high load levels
- if Guardian is used as a network sidecar, it can likely improve the write latency once scaled horizontally to handle the write volume
