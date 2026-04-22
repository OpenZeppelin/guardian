# Production Benchmark Suite

This workspace holds the new benchmark suite for the live production GUARDIAN
deployment.

Current scaffold status:
- `preflight` is implemented
- mixed `get_state` / `push_delta` workload execution is implemented
- push-only `push_delta` workload execution is implemented via `reads_per_push = 0`
- push-only burst runs retire an account after its first successful `push_delta`
- mixed burst runs can also retire an account after its first successful `push_delta`
- shardable `worker-run` execution is implemented
- local `aggregate` for distributed worker artifacts is implemented
- profile parsing, artifact layout, report models, and cleanup manifest models are implemented
- cleanup uses ECS exec against the live server task

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
