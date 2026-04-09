---
name: run-guardian-prod-benchmarks
description: Run, evaluate, and report production GUARDIAN benchmarks with the repository's distributed ECS client harness. Use when Codex needs to prepare benchmark profiles, verify prod benchmark readiness, execute ECDSA/Falcon/mixed gRPC runs against GUARDIAN, clean up benchmark-owned data, interpret throughput or latency results, or write the benchmark report from the generated artifacts.
---

# Run Guardian Prod Benchmarks

Read the current source of truth at the start of every task:

- `benchmarks/prod-server/README.md`
- `scripts/run-prod-benchmark-ecs.sh`
- `benchmarks/prod-server/profiles/*.toml`
- `benchmarks/prod-server/src/config.rs`
- `benchmarks/prod-server/src/cleanup.rs`
- `benchmarks/prod-server/reports/20260408-prod-benchmark-report.md`

If the task involves changing the live GUARDIAN deployment or verifying the active AWS shape before benchmarking, also use [`deploy-guardian-aws`](../deploy-guardian-aws/SKILL.md).

Trust these sources in this order:

1. `scripts/run-prod-benchmark-ecs.sh` for the real execution and teardown flow
2. `benchmarks/prod-server/profiles/*.toml` for supported scenarios
3. `benchmarks/prod-server/src/*.rs` for benchmark behavior and cleanup semantics
4. `benchmarks/prod-server/reports/20260408-prod-benchmark-report.md` for the reporting style and reference numbers

## Scope

This skill is only for the current distributed ECS benchmark path:

- gRPC only
- prod GUARDIAN endpoint
- ECS/Fargate worker clients in `us-east-1`
- benchmark-owned logical cleanup through ECS exec
- ECDSA burst
- ECDSA mixed `1 push_delta : 4 get_state`
- Falcon mixed `1 push_delta : 4 get_state`
- Falcon+ECDSA mixed `1 push_delta : 4 get_state`

Do not revive or recommend:

- local single-process benchmark runs
- local direct-SQL cleanup
- file-backed seed caches
- deleted smoke or baseline profiles

## Workflow

### 1. Confirm Target Readiness

Before the first benchmark in a session:

1. Verify the live endpoint and benchmark assumptions.
2. If the deployment shape matters, inspect it first with [`deploy-guardian-aws`](../deploy-guardian-aws/SKILL.md).
3. Verify public health at minimum:
   ```bash
   curl -fsS https://guardian.openzeppelin.com/
   curl -fsS https://guardian.openzeppelin.com/pubkey
   ```
4. If reproducing the April 2026 reference runs, confirm the deployment is still close to:
   - `1` ECS server task
   - ARM64
   - `2 vCPU / 4 GB`
   - RDS Proxy enabled

### 2. Run Local Preflight

Run the toolchain and auth checks before any benchmark run:

```bash
set -a && source .env && set +a
aws sts get-caller-identity
docker info
command -v jq
command -v session-manager-plugin
```

Then run the benchmark preflight for the intended profile:

```bash
cargo run --manifest-path benchmarks/prod-server/Cargo.toml -- \
  preflight --profile benchmarks/prod-server/profiles/<profile>.toml
```

If preflight fails on AWS auth, refresh SSO before continuing.

### 3. Choose the Scenario

Read [`references/scenario-matrix.md`](references/scenario-matrix.md) when selecting or explaining a run.

Use these profiles:

- `ecdsa-burst-scale.toml`
- `ecdsa-mixed-burst-scale.toml`
- `falcon-mixed-burst-scale.toml`
- `falcon-ecdsa-mixed-burst-scale.toml`

Default worker count for the reference runs is `16`.

### 4. Execute the Distributed ECS Run

Use the repository script, not ad hoc ECS commands:

```bash
./scripts/run-prod-benchmark-ecs.sh \
  --profile benchmarks/prod-server/profiles/<profile>.toml \
  --workers 16
```

Important rules:

- Let the script build the `benchmark-runner` image unless you are intentionally reusing an existing image with `--image-uri`.
- Only use `--no-cleanup` for diagnosis. If you use it, run cleanup immediately afterward.
- Only use `--keep-image` or `--keep-task-definition` when you explicitly need post-run inspection.
- Treat missing worker artifacts or `502` shard failures as benchmark caveats that must be reported.

The script already:

- builds and pushes the benchmark image
- creates a temporary task definition
- launches ephemeral Fargate workers
- collects worker artifacts from CloudWatch logs
- aggregates results locally
- runs ECS-exec SQL purge
- tears down temporary ECS and ECR resources on exit

### 5. Validate Run Outputs and Cleanup

After aggregation, inspect the generated artifacts in `benchmarks/prod-server/reports/<run-id>/`:

- `run-report.json`
- `summary.md`
- `cleanup-manifest.json`

Confirm all of these before finishing:

1. `cleanup-manifest.json` shows `complete`
2. the report has `all`, `ecdsa`, and/or `falcon` scopes as expected
3. the run notes any missing shard artifacts
4. the public endpoint still passes `/` and `/pubkey`

### 6. Interpret the Results

Read [`references/reporting.md`](references/reporting.md) before writing conclusions.

Use these rules:

- `push_delta/s` is the main metric
- mixed runs should report both `push_delta/s` and `get_state/s`
- latency should always include at least `p50`, `p95`, `p99`, and `max`
- if failures are mostly `state_conflict`, the account pool shape is capping admitted writes, not necessarily the server
- if a run recovered from fewer than all shards, call that out explicitly
- treat `500 TPS` as a sizing reference for required GUARDIAN tasks, not as a requirement that one task must absorb the entire network

For comparison against the benchmark we already ran, read [`references/reference-results.md`](references/reference-results.md).

## Output Shape

When asked to run or summarize benchmarks, report:

- the exact profile used
- the number and size of ECS worker tasks
- the server shape under test
- the number of users/accounts
- signer distribution
- throughput results
- latency results
- cleanup status
- any shard-loss, conflict, or measurement caveats
- the implied GUARDIAN task count for a `500 TPS` reference target when relevant

When writing a benchmark report, follow the same structure as `benchmarks/prod-server/reports/20260408-prod-benchmark-report.md`:

1. goal
2. setup
3. reference capacity target
4. results summary
5. results table
6. latency section
7. interpretation
