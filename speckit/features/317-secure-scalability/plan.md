# Implementation Plan: Security-Preserving Scalability for Reads, Replay Protection and Canonicalization

**Feature Key**: `317-secure-scalability` | **Date**: 2026-10-30 | **Spec**: [spec.md](./spec.md)
**Branch**: `317-secure-scalability` (not yet created; branch from `main`)
**Input**: [spec.md](./spec.md), [gap analysis](../../../docs/architecture/scalability/gap-analysis.md), [session-auth RFC](../../../docs/architecture/scalability/session-auth-rfc.md)
**Release**: breaking session-request change ships in a 0.19.0 release candidate after rc.1; 0.19.0 final waits for security review and the F2 closure.

## Summary

Reach the #317 target without weakening any security property:

1. **Operation-bound sessions (`session-v2`)**: every session request signs
   a fixed envelope with transport and operation ID. Breaking for session
   requests only; grants and keys keep working.
2. **Reads reject exact duplicates, not reorderings**: the per-session
   timestamp floor for reads is replaced by a fleet-wide, Postgres-backed
   record of accepted request digests. A captured request still works at
   most once. Writes keep the strict floor.
3. **Stalled requests rejected two ways**: at admission, after any lock or
   pool wait, the request's timestamp is re-checked against the database
   clock (freshness). Separately, the purge advances a watermark under an
   exclusive advisory lock while each record/CAS holds the shared lock, so
   a purged request can never be readmitted (fence, closes F2). Both apply
   to the read record and the write floor.
4. **Cheaper reads**: one validation lookup on the primary (auth version and
   live session) plus a metadata cache that is used only when the version
   matches; verification off the async runtime if measurements support it;
   direct DB route.
5. **Throughput elsewhere**: CPU-aware canonicalization admission (parallel
   only on leaders with 4+ vCPUs), 100k-account background jobs, per-route
   percentiles and auth rejection metrics, and a target-shaped benchmark.

**Revision 2026-10-30: the standalone RDS gate is replaced.** A local
same-machine comparison (the replay-store gate harness, a prototype kept
outside this repository since 2026-11-02) showed the proposed read path costs less database work per read than
today's (about 2x throughput, a third less CPU at 10,000/s), with about
1 GB of live records at 10,000 reads/s as the main cost. `session-v2`
starts now; the read duplicate record was first planned behind a server
read-policy setting (`strict` or `unique`; later removed, `unique` is the
only behavior), and the end-to-end benchmark on a
prod-like stack runs once per setting to give the absolute RDS answer.
The text below is the original gating rationale.

**The replay store is gated, not assumed.** It still writes once per read
(about 864 million records per day at 10,000 reads/s). A standalone
capacity experiment and the formal checks run before any protocol or SDK
work commits to it (phase 2 below).

Research and decisions: [research.md](./research.md).

## Technical Context

**Language/Version**: Rust (workspace toolchain, edition 2024), TypeScript (ES2022, browser and Node)
**Primary Dependencies**: axum, tonic 0.14, diesel + tokio-postgres, p256, `@noble/curves` (tests), WebCrypto; new: `moka` (future feature) for the metadata cache
**Storage**: Postgres (prod; new partitioned `session_read_seen`, `replay_fence`, `account_metadata.auth_version`), filesystem (dev; durable per-5-minute bucket files for the read record, same checks as Postgres)
**Testing**: `cargo test` per crate (ignored live-Postgres tests), `npm test -w` per package, TLA+ skills (`verify-auth-replay` deep tier, `verify-candidate-queue`, `verify-worker-lease`), `benchmarks/prod-server`, examples smoke
**Target Platform**: Linux server on AWS ECS (Fargate) with RDS Postgres; browser and native SDK clients
**Project Type**: multi-crate web service + two SDK families + benchmark tooling + Terraform
**Performance Goals**: state reads p95 <= 1 s at 10,000 reads/s design target (pass floor 8,000 reads/s) with 20,000 readers; accepted-to-canonical p95 <= 30 s at 100 writers; zero first-attempt replay rejections for valid traffic
**Constraints**: no window-only reads, no stale authorization, no cached state, backend parity, HTTP/gRPC parity, Rust/TS parity, fail closed on any auth-store error
**Scale/Scope**: 100,000 accounts; about 7 M live duplicate-record rows at 10 k reads/s; 13 session operations; metadata cache <= 256 MB per replica

## Constitution Check

*Pre-research gate and post-design re-check.*

| Principle | Assessment | Status |
|---|---|---|
| I. Bottom-up propagation | Order: shared envelope, server, Rust client, TS client, multisig SDKs, examples, docs (see Phases). | Pass |
| II. Transport and cross-language parity | HTTP and gRPC have the same semantics; their signatures intentionally differ by the transport byte (documented in spec and contract). Rust and TS share vectors and the `WriteSequencer` contract. | Pass (documented divergence) |
| III. Append-only and explicit lifecycles | No change to state, delta or proposal lifecycles. Canonicalization concurrency change is verified by existing models. | Pass |
| IV. Explicit auth, stable boundary errors | Auth remains explicit and replay-protected (stronger: F2 closed). New typed error `unsupported_auth_version`; existing codes keep meaning. Tests in server plus both SDKs. | Pass |
| V. Evidence-driven delivery | Independent stories, quickstart per story, TLA+ deep tier, benchmark. | Pass |
| Invariant: backend parity | Filesystem persists the read record to bucket files and applies the same freshness and fence checks, so restart plus grant re-registration cannot readmit a request. Tested on both backends. | Pass |
| Invariant: per-account auth replay-protected | Reads: exact-duplicate rejection fleet-wide; writes: strict floor. | Pass |

No unjustified violations. Complexity notes below.

## Project Structure

### Documentation (this feature)

```text
speckit/features/317-secure-scalability/
├── spec.md
├── plan.md                     # this file
├── research.md
├── data-model.md
├── quickstart.md
├── contracts/
│   ├── session-request-v2.md
│   ├── session-policy-api.md
│   ├── sdk-api.md
│   └── observability.md
├── checklists/requirements.md
└── tasks.md                    # /speckit-tasks
```

### Source Code (touched areas)

```text
crates/shared/src/
├── session_request.rs          # NEW: SessionOperation, Transport, ReplayPolicy, envelope
├── session_key.rs              # byte-level sign/verify
└── lib.rs
crates/shared/tests/
├── fixtures/session_request_vectors.json   # NEW
└── session_request_vectors.rs              # NEW

crates/server/
├── proto/guardian.proto                    # CreateSessionResponse fields 3-5
├── migrations/2026-11-xx-000001_session_read_seen/   # partitioned table, replay_fence, functions
├── migrations/2026-11-xx-000002_account_auth_version/
└── src/
    ├── metadata/auth/credentials.rs        # SessionV2, transport, legacy -> UnsupportedAuthVersion
    ├── services/mod.rs                     # resolve_* take SessionOperation; record vs fenced CAS
    ├── services/*.rs                       # 13 session-eligible callers pass their operation
    ├── api/http.rs, api/grpc.rs            # operation per handler; session response fields
    ├── services/status.rs                  # SessionsStatus fields
    ├── session.rs                          # read record key/digest, fence cutoffs
    ├── replay/                             # NEW: ReadRecordStore trait, Postgres + filesystem, fence
    ├── metadata/{mod,postgres,filesystem}.rs  # fenced CAS, auth_version bump, read auth lookup
    ├── auth_cache/                         # NEW: ReadAuthorizer + moka cache
    ├── auth_verify.rs                      # NEW: VerifyPool (gated by benchmark)
    ├── builder/handle.rs                   # sweep: fence advance, partition maintenance
    ├── network/mod.rs, builder/canonicalization.rs  # configurable admission
    ├── jobs/release_sweep/, builder/release_sweep.rs  # adaptive rate
    ├── dashboard/stats.rs, metrics/refresher.rs       # SQL aggregates, counts
    ├── metrics/{names,grpc,http}.rs        # auth rejection counters, gRPC empty-body mapping
    └── error.rs                            # UnsupportedAuthVersion

crates/client/src/client.rs, error.rs, session*.rs   # v2 signing, WriteSequencer, policy validation
crates/miden-multisig-client/src/client/              # shared session slot (sequencer travels with it)

packages/guardian-client/src/
├── session-request.ts          # NEW: operation enum, envelope encoder
├── write-sequencer.ts          # NEW
├── types.ts, http.ts, server-types.ts, conversion, error-codes.ts
packages/miden-multisig-client/src/session/key.ts, manager.ts
packages/miden-multisig-client/tests/session-request-vectors.test.ts  # NEW

.claude/skills/verify-auth-replay/ (+ .agents mirror)   # model: read record, fence, split policy
docs/architecture/scalability/tla/                      # add "unique" read variant
benchmarks/prod-server/src/                             # reader/writer roles, nonce poll, rejection counts
infra/observability.tf, infra/*.tfvars                  # route dimension, percentiles, direct DB
spec/api.md, docs/MULTISIG_SDK.md, docs/MIDEN_COMPATIBILITY.md, docs/TROUBLESHOOTING.md,
docs/CONFIGURATION.md, READMEs of shared, client, guardian-client, both multisig SDKs
```

**Structure Decision**: Existing multi-crate layout. New server modules
`replay/` and `auth_cache/` keep the new storage and cache behind traits
consumed only by `services/mod.rs`, so write services cannot reach the
cache.

## Phases

Ordered so that measurement and the riskiest design assumption come first,
independent performance work proceeds in parallel, and the breaking
protocol change starts only on a validated replay design. Phase 1 starts
now; phases 3 and 2 can run side by side.

| Phase | Work | Stories | Release coupling |
|---|---|---|---|
| 1 | Metrics, current SDK workload baseline, proxy vs direct DB | US6 | None |
| 2 | Replay-store capacity experiment and formal checks (gate) | US1, US3 (design) | None |
| 3 | Independent read-path and background-job optimizations | US4, US7 | Any release |
| 4 | Operation-bound auth and SDK rollout on the validated replay design | US1, US2, US3 | 0.19.0 rc (breaking) |
| 5 | CPU-aware canonicalization tuning and full sustained qualification | US5, SC-001 to SC-009 | Before claiming #317 |

### Phase 1. Measurement and baseline (US6)

- Per-route percentiles and health-check exclusion in CloudWatch, Grafana
  panels, `guardian_auth_rejections_total`, empty gRPC body mapping.
- Pool metrics (size, in use, pending acquires) for both Postgres pools;
  pool sizes set against RDS `max_connections` for the maximum task count
  (research R17).
- Rate limiter: measure lock wait at target rate; shard it if it shows in
  p99; benchmark and prod limits sized for 20,000 readers behind few IPs;
  rate-limit rejections counted separately (research R17).
- Benchmark: reader/writer roles, nonce poll, writer rate and account
  distribution, sustained mode, rejection counts, 100k seeding, client vs
  server-span vs dashboard latency, accepted-to-canonical vs
  chain-inclusion-to-canonical.
- Baseline of the current SDK workload (v1 strict) on a prod-like stack,
  run once through RDS Proxy and once direct, to measure the proxy cost
  under load. Then switch devnet and prod to the direct route (Terraform).

### Phase 2. Replay-store gate (no protocol change)

- Standalone harness for `session_read_seen` (40-byte key), `replay_fence`
  and `guardian_authorize_and_record_session_read` (research R4, R16):
  12,000 calls/s from 3 processes, 1 h with partition rollover and purge
  under load, duplicates across processes, forced stalls, pool
  pending-acquire metrics, compared with the current UPSERT plus separate
  lookups.
- `verify-auth-replay`: add admission freshness, purge fence and
  filesystem restart plus re-registration as three separate properties;
  read-record and split read/write policies; deep tier.
- Exit: R16 pass criteria met and model properties pass. On failure, stop
  and take the decision back to review before phase 4.

### Phase 3. Independent optimizations (US4, US7)

- `auth_version` migration and bumps; combined read-auth lookup on the
  primary; `moka` `ReadAuthorizer`; tests for cross-replica signer removal
  and revocation, fail closed, pause readable. Works with today's v1 auth,
  so it does not wait for phase 4.
- `VerifyPool`, kept only if the benchmark shows a p99 improvement.
- Adaptive release-sweep rate; count query in the metrics refresher;
  dashboard stats reads commitments first and fetches state only for
  changed accounts, SQL aggregates for column-backed counts.

### Phase 4. Operation-bound auth and SDK rollout (US1, US2, US3; breaking)

1. `crates/shared`: `session_request.rs`, byte-level P-256 API, vectors
   fixture generated from Rust; TS vector test.
2. Server: `SessionV2` credentials with transport; `SessionOperation` passed
   by every session-eligible handler (HTTP and gRPC); legacy `session` to
   `UnsupportedAuthVersion`; policy fields in session creation and status.
3. Server: production version of the phase 2 replay store; freshness and
   fence on the write CAS; `ReadRecordStore` (Postgres partitioned,
   filesystem durable buckets); read path records the digest after
   authorization; sweep advances fences and maintains partitions.
4. Model: `mirrored.txt` and fingerprints updated to the real code; deep
   tier passes. Add the "unique" variant to the design model.
5. Rust client and TS client: v2 signing, operation per call, session
   timestamp source, `WriteSequencer`, typed error, policy validation.
6. Multisig SDKs: session adapters (`GuardianSession`), Rust session slot.
7. Examples smoke (`demo`, `web`, `smoke-web`), docs, OpenAPI regen,
   `MIDEN_COMPATIBILITY.md` cutover notes, security review.

### Phase 5. Canonicalization and full qualification (US5, all SCs)

- Configurable admission gate, effective `min(admission, cores/2)`;
  measured on 4 and 8 vCPU at 1, 2, `cores/2` with foreground push p95;
  candidate-queue and worker-lease models; leader size documented.
- Three sustained benchmark legs (v1 strict, v2 without record, v2 with
  record) on a prod-like stack with 2+ replicas; report under
  `benchmarks/prod-server/results/`.

## Validation Matrix

| Change | Minimum checks |
|---|---|
| Envelope, registry | `cargo test -p guardian-shared`; TS vectors in both packages |
| Server auth and replay | `cargo test -p guardian-server`; Postgres ignored tests; `verify-auth-replay --deep` |
| Rust client / multisig | `cargo test -p guardian-client -p miden-multisig-client`; `examples/demo` smoke |
| TS client / multisig | `npm run build -w @openzeppelin/guardian-client`; `npm test -w` both; `examples/smoke-web` |
| Read path cache | server tests with two `AppState`s on one Postgres (cross-replica freshness) |
| Canonicalization | `verify-candidate-queue`, `verify-worker-lease`; benchmark leg |
| Contract | OpenAPI regen drift check; proto compiles in both clients |
| Release | qualification suite (deterministic + live) against devnet before the rc |

## Risks

| Risk | Mitigation |
|---|---|
| 10 k record inserts/s too heavy for the RDS instance | Phase 2 gate before any protocol work; on failure, a reviewed decision (instance size, plain-table fallback, other design), never silent window-only reads |
| Stalled request admitted before the next purge | Admission freshness against the DB clock after lock acquisition, modeled separately from the fence |
| Filesystem restart plus grant re-registration readmits a read | Durable bucket files; explicit test of that sequence |
| Parallel rebuilds impossible on 2-vCPU leaders | Effective concurrency `min(admission, cores/2)`; leader size documented from phase 5 measurements |
| Dashboard latency judged against client latency | Compare with server spans (same boundary); client latency reported separately |
| Missing partition stops all session reads | Create 1 h ahead on every replica, alarm on partition horizon < 30 min, test |
| Advisory lock key collision | Fixed documented keys in one module, test asserting uniqueness |
| Old browser apps lose session reads at cutover | Release notes, `MIDEN_COMPATIBILITY.md`, coordinated SDK publish |
| Mixed v1/v2 fleet | Homogeneous blue/green cutover runbook |
| Identical reads from independent processes sharing a key in the same ms | Documented; SDK retry re-signs; separate grants recommended |

## Complexity Tracking

| Addition | Why Needed | Simpler Alternative Rejected Because |
|---|---|---|
| Partitioned `session_read_seen` | 10 k inserts/s; partition drop avoids delete and vacuum churn | Plain table with `DELETE` purge: viable fallback, kept as plan B |
| Advisory-lock fence + `plpgsql` record function | Closes F2 without assuming bounded handler time, one round trip | Freshness predicate in `INSERT`: evaluated at statement start, races purge; request deadline alone: not a proof |
| `moka` dependency | Single-flight bounded cache | Hand-rolled map: reimplements eviction and single flight |
| `WriteSequencer` in both SDKs | Per-instance counters are not shared across clients of one session | Server-side tolerance for write reordering: would weaken the write floor |
