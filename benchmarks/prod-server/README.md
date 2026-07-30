# Production Benchmark Suite

This workspace holds the new benchmark suite for the live production GUARDIAN
deployment.

Current scaffold status:
- `preflight` is implemented
- shardable `worker-run` execution is implemented
- local `aggregate` for distributed worker artifacts is implemented
- profile parsing, artifact layout, report models, and cleanup manifest models are implemented
- cleanup uses ECS exec against the live server task
- canonicalization sampling is implemented: after a successful `push_delta`, a
  worker samples the push with probability `canonicalization.sample_rate` and
  polls `get_delta` every `poll_interval_ms` until the delta reports
  `canonical_at` or `discarded_at` (bounded by `timeout_seconds`), recording
  the accepted→canonical wait; polling failures are recorded separately from
  canonicalization timeouts; sampled workers pause load generation while
  polling, so keep `sample_rate` low on profiles that do not retire accounts
  after their first push
- sampled waits land in `reports/<run-id>/canonicalization-samples.json` and
  as a `canonicalization` section (p50/p95/p99/max wait) in the run report
  and summary

## Workload modes

`[operation_mix]` selects one of three shapes:

| `mode` | Operations issued | Extra keys |
|---|---|---|
| `read_only` | `get_state` only | none |
| `push_only` | `push_delta` only | `retire_after_first_successful_push` |
| `mixed` | `reads_per_push` reads then one push, repeating | `reads_per_push` (> 0), `retire_after_first_successful_push` |

`retire_after_first_successful_push` stops a user after its first accepted
push, which keeps a burst run on unique accounts instead of walking one
account's nonce sequence. It is only meaningful where pushes happen.

Read-only runs still create one account per user through the real `configure`
path, so cleanup matters as much as it does for a write run.

**A read-only run is closed-loop**: users issue the next `get_state` as soon as
the previous returns, so `users` is the number of requests that may be in flight,
not a paced reader population. That produces an in-flight-saturation ceiling
(spec FR-003c), which must be reported as a ceiling and never as a target
verdict.

Profiles live in `profiles/`.
Run artifacts live under `reports/<run-id>/`.

Initial commands:

```bash
cargo run --manifest-path benchmarks/prod-server/Cargo.toml -- \
  preflight --profile benchmarks/prod-server/profiles/falcon-ecdsa-mixed-burst-scale.toml
```

Distributed ECS execution:

```bash
./scripts/run-prod-benchmark-ecs.sh \
  --profile benchmarks/prod-server/profiles/read-only-ramp.toml \
  --workers 16

./scripts/run-prod-benchmark-ecs.sh \
  --profile benchmarks/prod-server/profiles/ecdsa-burst-scale.toml \
  --workers 16

./scripts/run-prod-benchmark-ecs.sh \
  --profile benchmarks/prod-server/profiles/ecdsa-mixed-burst-scale.toml \
  --workers 16

./scripts/run-prod-benchmark-ecs.sh \
  --profile benchmarks/prod-server/profiles/falcon-mixed-burst-scale.toml \
  --workers 16

./scripts/run-prod-benchmark-ecs.sh \
  --profile benchmarks/prod-server/profiles/falcon-ecdsa-mixed-burst-scale.toml \
  --workers 16
```

This flow:
- builds a temporary `benchmark-runner` container image
- launches ephemeral Fargate tasks against the existing ECS cluster
- collects `worker-run` artifacts from CloudWatch logs
- aggregates them locally into the normal `reports/<run-id>/` directory
- runs cleanup through the existing ECS-exec SQL purge path
- deregisters the temporary task definition and deletes the temporary image tag on exit
