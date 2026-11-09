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

Profiles live in `profiles/`.
Run artifacts live under `reports/<run-id>/`.

A write (`push_delta` in the report) is what a multisig client does: store
the proposal, approve it with the user's key (accounts are 1-of-N), then push
the delta, since GUARDIAN admits a multisig push only against a pending
proposal holding the threshold of verified approvals. Its latency covers all
three calls. Accounts are built with the signers' scheme, so ECDSA users get
ECDSA accounts.

Every profile names its `[credential]`: `kind = "wallet"`, or
`kind = "session"` with the Guardian's `network`, in which case every
active user starts a session before warmup and signs reads and pushes with
it. Session runs must fit in one session (run duration plus 30 minutes, at
most 8 hours). The report records the credential: compare wallet and
session runs separately, since they exercise different replay protection
(each session read except the canonical nonce poll is admitted once through
a record of accepted reads; the nonce poll is checked against its timestamp
window only).
A user whose session ended mid-run (expired, revoked, or forgotten by the
server) keeps going signed by its wallet; the report counts those measured
operations (a write once, for its three requests) as
`wallet_signed_operations` and the summary warns when it is not
zero, since such a run is not a clean session run. Both fields are required
in every profile: a profile without `[credential]` (or, with `[roles]`,
without `cosigners_per_writer`) fails to load.

Profiles with `[roles]` set `cosigners_per_writer`: that many readers per
writer are seeded as extra cosigners of the writer's account and poll it,
so their nonce polls see new state; the remaining readers poll accounts of
their own, which never change, so their polls stay nonce-only. In the
`target-317-*` profiles that is 300 of 20,000 readers (3 per writer, 100
writers): most read traffic is the cheap nonce poll, and only the watching
readers fetch state after each push. Raise `cosigners_per_writer` to put
more of the read load on changing accounts. Readers only watch writers run by the same worker shard, and take
their writer's signature scheme. `target-317-mix` (session) and
`target-317-mix-wallet` are the same load under the two credentials.

## Rate limiting applies to gRPC

The harness speaks gRPC, and the server now meters it (the gRPC surface used
to be unthrottled): the sustained per-minute limit is keyed per IP and is
therefore shared with any HTTP traffic from the same worker, while the burst
limit is keyed per IP and method. Over-budget calls
fail with `ResourceExhausted`, `code: rate_limit_exceeded`, and a
`retry-after` metadata hint. Before a run, check the target's
`GUARDIAN_RATE_BURST_PER_SEC` / `GUARDIAN_RATE_PER_MIN` (divided per
replica by `GUARDIAN_MAX_REPLICAS`) against the profile's intended
request rate: a throughput number measured while the limiter is rejecting
is a limiter benchmark, not a server benchmark. Worker source IPs share
one sustained budget per IP, so distributed workers behind one NAT
address collapse into a single budget.

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
