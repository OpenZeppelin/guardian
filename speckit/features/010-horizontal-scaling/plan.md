# Implementation Plan: Horizontal Scaling Correctness Across Multiple Guardian Instances

**Branch**: `010-horizontal-scaling` | **Date**: 2026-06-20 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `speckit/features/010-horizontal-scaling/spec.md`
**Tracking issue**: [#242](https://github.com/OpenZeppelin/guardian/issues/242) (subsumes [#190](https://github.com/OpenZeppelin/guardian/issues/190))

## Summary

Make `crates/server` correct when run as 2-6 replicas behind a round-robin load
balancer. Five subsystems currently assume a single process: operator auth
challenges/sessions, EVM challenges/sessions, the canonicalization worker, the
pagination cursor secret, and the rate limiter. The approach is to introduce
**trait-backed coordination** (`SessionStore`, `ChallengeStore`, `LeaderElector`)
with two implementations each — an in-memory implementation (filesystem/dev,
single replica) and a Postgres implementation (the prod backend) — selected
alongside the existing storage-backend choice. The canonicalization worker
acquires a Postgres **lease** (heartbeat + TTL) so exactly one replica runs it.
Cursor-secret and filesystem-backend misconfigurations become **fail-fast in the
prod stage** (gated by the existing `GUARDIAN_ENV=prod` signal) and warnings in
dev. Rate limiting stays per-process but partitions the configured budget by the
deployment's **autoscaling max capacity** (defaulted from Terraform), so aggregate
enforcement stays at or below the configured global limit — conservative
(stricter, never looser) when running below max capacity. No client-facing wire
contract changes.

## Technical Context

**Language/Version**: Rust (workspace edition; `crates/server`)
**Primary Dependencies**: axum (HTTP), tonic (gRPC), diesel + diesel_async (deadpool pool), tokio
**Storage**: Postgres (prod, `feature = "postgres"`) via Diesel embedded migrations (`crates/server/migrations/`); filesystem backend (dev default, no DB)
**Testing**: `cargo test` (unit + integration in `crates/server`); multi-replica integration via two server instances against one Postgres
**Target Platform**: Linux server (AWS ECS Fargate, 2-6 tasks behind ALB)
**Project Type**: Multi-crate Rust web service + Rust/TS client SDKs (server-only change here)
**Performance Goals**: No added latency on the request hot path beyond a single shared-store lookup for session/challenge verification; canonicalization interval unchanged (10s default)
**Constraints**: No new infrastructure component (reuse Postgres; no Redis/queue); single-replica/dev path must require zero new infra; no client wire-contract drift; auth error surfaces unchanged
**Scale/Scope**: 2-6 replicas; one shared Postgres; pool sizing must respect `max_connections` (pool size x replica count)

### Resolved decisions (see research.md)

- **Shared store** = existing Postgres backend. No Redis/queue introduced. (research R1)
- **Sessions + challenges** (operator and EVM) move behind `SessionStore` / `ChallengeStore` traits: in-memory impl for filesystem/dev, Postgres impl for prod. (R2)
- **Canonicalization** uses a Postgres **lease row** (`worker_leases`) with holder id + heartbeat/TTL, not session-level advisory locks (incompatible with pooled connections). Renewal runs on its **own timer concurrent with the pass**; the pass becomes **cooperatively cancellable**; and every state-mutating submission/promotion is gated by a **mandatory fence check** (`verify_held`) so a superseded holder can never commit during the cancellation window. In-memory `LeaderElector` always grants for single-replica/dev. (R3)
- **Concurrent migrations** at multi-replica startup are serialized with a Postgres **advisory lock** around `run_pending_migrations` (`storage/postgres.rs`); no manual migrate-then-start step. (data-model.md / db-schema.md)
- **Auth availability/perf**: with sessions in Postgres, auth **fails closed** on a store outage (was always-available in-memory) and every authenticated request does one indexed `SELECT` (immediate revocation chosen over a local cache, per FR-003). Documented trade-offs, not surprises. (coordination-traits.md)
- **Rate limiting** stays per-process; global limits are divided by `GUARDIAN_RATE_LIMIT_PARTITIONS` (= autoscaling max capacity, defaulted from Terraform `effective_server_autoscaling_max_capacity`), so aggregate stays at or below the global limit and is stricter below max capacity; unset/`1` => current behavior. (R4)
- **Stage** reuses `GUARDIAN_ENV=prod` (already set from Terraform `deployment_stage`); the existing `is_prod_environment()` check is promoted to a shared helper. (R5)
- **Cursor secret** + **filesystem backend** become fail-fast in prod, warn in dev. (R6)

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Assessment | Status |
|---|---|---|
| I. Bottom-Up Change Propagation | Change is confined to `crates/server`. No proto/HTTP/JSON contract changes, so base clients, multisig SDKs, and examples are provably unaffected — to be confirmed by an explicit "no wire diff" check + OpenAPI drift gate. | PASS |
| II. Transport & Cross-Language Parity | No HTTP/gRPC surface change; same status/error semantics for auth. Rust/TS clients unchanged. Any divergence would be documented; none introduced. | PASS |
| III. Append-Only Integrity & Explicit Lifecycles | Leader election changes **who** runs canonicalization, not the pending->candidate->canonical/discarded transitions. No implicit fallback or silent rewrite. Lease state is separate from delta lineage. Exactly-once must not double-count retries. | PASS (verify in tests) |
| IV. Explicit Auth & Stable Boundary Errors | Sessions/challenges move stores but MUST return identical auth outcomes and the same 401/boundary errors. High-risk: requires updated tests in the changed layer; no upstream consumer behavior changes (validated by client smoke). | PASS (high-risk, tests mandatory) |
| V. Evidence-Driven Delivery | Independently testable user stories already defined; validation plan includes a multi-replica integration harness + single-replica no-regression. | PASS |

**System-invariant note (storage backend parity)**: The invariant "filesystem and
Postgres preserve the same externally observable semantics unless a documented
backend-specific limitation is explicitly accepted" is engaged. We **explicitly
accept** a documented limitation: the filesystem backend supports only
single-replica deployments (no shared sessions/lease), which is why the prod
stage refuses it (US5/FR-012). This is recorded here and in operator docs, as the
constitution requires.

No violations requiring Complexity Tracking.

## Project Structure

### Documentation (this feature)

```text
speckit/features/010-horizontal-scaling/
├── plan.md              # This file
├── spec.md              # Feature spec
├── research.md          # Phase 0 decisions
├── data-model.md        # New tables + entities
├── quickstart.md        # Local 2-replica validation
├── contracts/
│   ├── coordination-traits.md   # SessionStore / ChallengeStore / LeaderElector contracts
│   ├── db-schema.md             # New migration tables + columns
│   └── config-contract.md       # Env-var + startup-guard contract
└── checklists/
    └── requirements.md
```

### Source Code (repository root)

```text
crates/server/
├── migrations/
│   ├── <date>_auth_sessions/{up,down}.sql        # NEW: shared session store
│   ├── <date>_auth_challenges/{up,down}.sql      # NEW: shared challenge store
│   └── <date>_worker_leases/{up,down}.sql        # NEW: canonicalization lease
├── src/
│   ├── coordination/                             # NEW module
│   │   ├── mod.rs
│   │   ├── session_store.rs        # SessionStore trait + InMemory impl
│   │   ├── challenge_store.rs      # ChallengeStore trait + InMemory impl
│   │   ├── leader.rs               # LeaderElector trait + AlwaysLeader impl
│   │   └── postgres/               # Postgres impls (cfg feature = "postgres")
│   │       ├── session_store.rs
│   │       ├── challenge_store.rs
│   │       └── lease.rs
│   ├── dashboard/state.rs          # use SessionStore/ChallengeStore (operator)
│   ├── evm/session.rs              # use SessionStore/ChallengeStore (evm)
│   ├── dashboard/config.rs         # prod fail-fast on unset cursor secret
│   ├── builder/storage.rs          # prod fail-fast on filesystem backend
│   ├── builder/state.rs            # AppState gains coordination handles
│   ├── builder/mod.rs / handle.rs  # wire coordination impls by backend
│   ├── storage/postgres.rs         # advisory-lock guard around run_pending_migrations
│   ├── jobs/canonicalization/worker.rs    # concurrent renewal task + cancellation signal
│   ├── jobs/canonicalization/processor.rs # cooperative cancel check + fence verify_held before submit
│   ├── middleware/rate_limit.rs    # GUARDIAN_RATE_LIMIT_PARTITIONS budget split
│   └── config/stage.rs             # NEW: shared GUARDIAN_ENV stage helper
└── tests/
    ├── multi_replica_*.rs          # NEW: two-instance integration tests
    └── (existing single-replica tests unchanged)

infra/
├── data.tf   # effective_guardian_rate_limit_partitions (= max capacity default)
└── ecs.tf    # inject GUARDIAN_RATE_LIMIT_PARTITIONS env var

docs/
├── runbooks/horizontal-scaling.md  # NEW: HA runbook (US6)
├── CONFIGURATION.md                # GUARDIAN_RATE_LIMIT_PARTITIONS, stage guards
└── SERVER_AWS_DEPLOY.md            # HA notes, pool-sizing guidance
```

**Structure Decision**: Add a single new `coordination` module that owns the
trait abstractions and both implementation families. The storage builder selects
the implementation family in lockstep with the storage backend (Postgres backend
=> Postgres coordination; filesystem backend => in-memory coordination), so
coordination availability can never diverge from where shared state actually
lives. `AppState` carries the chosen handles; `DashboardState`, `EvmSessionState`,
and the canonicalization worker consume them through the traits.

## Phased Delivery (maps to prioritized user stories)

The user stories are independently shippable; deliver in priority order, each as
its own PR with its own tests and docs:

1. **P1 — US1 Sessions+challenges (operator)**: `SessionStore`/`ChallengeStore`
   traits + in-memory + Postgres impls; wire into `DashboardState`. Plus EVM
   (US1 scope) in `evm/session.rs`.
2. **P1 — US2 Canonicalization lease**: `LeaderElector` + `worker_leases` +
   worker integration; exactly-once + automatic failover tests.
3. **P2 — US3 Cursor secret enforcement**: prod fail-fast; dev warn (mostly
   config + a startup guard; small).
4. **P2 — US4 Rate-limit partitioning**: `GUARDIAN_RATE_LIMIT_PARTITIONS` budget
   split (read env in `rate_limit.rs`) **and** the Terraform wiring in
   `infra/data.tf` + `infra/ecs.tf` so the default ships from autoscaling max
   capacity; documented under-enforcement tolerance.
5. **P3 — US5 Filesystem-in-prod guard**: startup refusal in `builder/storage.rs`.
6. **P3 — US6 HA runbook + config docs**: `docs/runbooks/horizontal-scaling.md`,
   `CONFIGURATION.md`, AWS deploy notes.

Shared prerequisite for 3/5: the `config/stage.rs` helper (extract
`is_prod_environment()`), landed with whichever story merges first.

## Risks & Mitigations

- **Auth regression when moving stores** (high): keep the trait surface identical
  to today's method shapes; port existing unit tests onto the in-memory impl
  first (no behavior change), then add a Postgres impl behind the same tests.
- **Lease split-brain / double-processing** (high): the worker today is a single
  awaited `process_all_accounts()` per tick with no cancellation hook — this is a
  real refactor, not a wrapper. Mitigation has three mandatory parts: (1) renewal
  on its own timer concurrent with the pass (TTL sized for renew/failover only,
  independent of the submission grace); (2) a cooperative cancellation signal the
  processor polls between accounts; (3) a **mandatory** fence check (`verify_held`)
  immediately before every on-chain submission/promotion, skipping the write if
  superseded. (3) is the hard guarantee that closes the cancellation-window race;
  TTL + voluntary abort alone is insufficient. Covered by an explicit failover
  integration test (SC-002/SC-003).
- **Concurrent first-deploy migrations** (high): all replicas run embedded
  migrations against one DB at boot; without serialization they can race/deadlock.
  Mitigation: a Postgres advisory lock around `run_pending_migrations`
  (`storage/postgres.rs`); first replica migrates, others wait then no-op. No
  manual migrate-first step (FR-017).
- **Auth fails closed on store outage** (medium, accepted): a Postgres blip now
  rejects auth where the in-memory map never did. This is the safe choice for
  custody and is documented (FR-018, runbook); recovery is automatic.
- **Per-request DB load** (medium, accepted): immediate cross-replica revocation
  (FR-003) rules out a local session cache, so each authenticated request does one
  indexed `SELECT`. Deliberate trade-off (revocation correctness over latency);
  reinforces the pool-sizing risk below.
- **DB connection exhaustion**: pool size x replica count (plus the new
  per-request session lookups) can exceed Postgres `max_connections`; document and
  recommend lowering `GUARDIAN_DB_POOL_MAX_SIZE` per replica in the runbook, and
  note RDS Proxy is already enabled in prod (`infra/data.tf`).
- **Rate-limit under-enforcement when below max capacity**: partitioning by the
  fixed autoscaling max capacity means a fleet running below max enforces stricter
  than the global limit; this is the intended conservative trade-off (never
  silently looser), documented as the tolerance band (SC-005). No external
  coordination dependency exists on the request path (FR-010).
- **Clock skew on lease/expiry**: all expiry computed from DB `now()` (single
  clock) for the Postgres impls, not per-replica wall clock.

## Complexity Tracking

No constitution violations requiring justification. The one accepted
backend-specific limitation (filesystem = single-replica only) is documented in
the Constitution Check above and enforced by US5.
