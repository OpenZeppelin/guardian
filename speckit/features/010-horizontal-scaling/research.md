# Phase 0 Research: Horizontal Scaling Correctness

**Feature**: 010-horizontal-scaling | **Date**: 2026-06-20

This document records the design decisions that resolve the planning unknowns
left open by the spec. Each is grounded in the current codebase (citations
inline).

## R1 — Shared coordination store

**Decision**: Reuse the existing Postgres backend as the single shared store for
sessions, challenges, and the canonicalization lease. Do not introduce Redis, a
message queue, or any new infrastructure component.

**Rationale**: The published prod image is built with `feature = "postgres"` and
the prod ECS task always provides `DATABASE_URL`; Postgres is already the system
of record (`crates/server/src/storage/postgres.rs`, `crates/server/migrations/`).
Adding Redis/queue would create new operational surface, new failure modes, and
new secrets for zero capability the database cannot provide at this scale (2-6
replicas, low coordination volume).

**Alternatives considered**:
- *Redis for sessions + rate limiting*: rejected — new infra, new secret, new
  outage mode; unjustified at this scale.
- *Sticky sessions at the ALB*: rejected — masks rather than fixes the
  single-instance assumptions; breaks canonicalization and cursors; the spec
  states correctness must not depend on session affinity.

## R2 — Sessions and challenges: trait-backed stores with in-memory + Postgres impls

**Decision**: Introduce `SessionStore` and `ChallengeStore` traits in a new
`crates/server/src/coordination` module, each with (a) an in-memory
implementation that preserves today's behavior exactly and (b) a Postgres
implementation. The implementation family is chosen alongside the storage backend
(Postgres backend => Postgres stores; filesystem backend => in-memory stores).
Both the operator stores (`dashboard/state.rs:24-35`) and the EVM stores
(`evm/session.rs:18-22`) consume the traits.

**Rationale**: The filesystem/dev deployment has no database
(`builder/storage.rs:89-149`), so coordination must degrade to in-memory there;
that is also exactly what a single-replica deployment needs (FR-014). Tying the
coordination family to the storage backend guarantees shared state and its
coordination can never diverge. The trait surface mirrors the existing method
shapes (`issue_challenge`, `verify`, `authenticate_session`,
`dashboard/state.rs:108-332`) so the in-memory port is behavior-preserving and
auth tests carry over unchanged (Constitution IV).

**Why challenges must be shared too**: a challenge is issued on replica A
(`issue_challenge`) and verified on replica B (`verify`); per-process storage
guarantees verification failure. Both challenge and session storage must be
shared.

**Schema**: see data-model.md — `auth_sessions` and `auth_challenges` tables with
a `realm` discriminator (`operator` | `evm`) rather than four tables, keyed by the
SHA-256 token digest already used in code (`[u8; 32]`), with `expires_at` for TTL
sweeps. Tokens are never stored in plaintext (matches current digest-keying).

**Alternatives considered**:
- *Always-Postgres (no in-memory impl)*: rejected — would force a database on
  local/dev and violate FR-014 and the "filesystem default for dev" invariant.
- *Four separate tables*: rejected — needless duplication; a `realm` column with
  the same shape is simpler and keeps one sweep path.

## R3 — Canonicalization: Postgres lease (heartbeat + TTL), not advisory locks

**Decision**: Add a `LeaderElector` trait with an `AlwaysLeader` in-memory impl
(single replica/dev) and a Postgres lease impl backed by a `worker_leases` row
(`lease_name`, `holder_id`, `acquired_at`, `renewed_at`, `expires_at`). The worker
(`jobs/canonicalization/worker.rs:7-45`) attempts to acquire/renew the lease each
tick; only the holder runs `process_all_accounts()`. Lease renewal interval is
well below the TTL. The TTL is sized purely for renew/failover behavior — it must
comfortably exceed one renew interval (so a healthy holder never loses its lease)
and it sets the failover bound (another replica may claim only after the TTL
elapses without a renew). The TTL is **not** tied to the canonicalization
`submission_grace_period_seconds`; that grace governs delta promotion timing, not
lease ownership. A replica that fails to renew MUST abort its in-flight pass
before the TTL lets another replica claim the lease. All time comparisons use the
database clock (`now()`), not per-replica wall clocks. (See data-model.md for the
exact timing constraints — this resolves the earlier inconsistency between the two
documents in favor of the renew/failover sizing.)

**Rationale**: The worker already receives the full `AppState` and already
touches Postgres through the storage trait (`builder/handle.rs:128-135`), so a
lease table is a natural, observable, debuggable fit. Diesel uses pooled
connections (`diesel_async` deadpool), so a **session-level** `pg_advisory_lock`
is fragile: the lock lives with a connection that returns to the pool between
calls, making "who holds it" non-deterministic. A lease row with explicit
heartbeat/TTL survives connection churn, is inspectable by operators, and gives a
clean, bounded failover window (SC-003). There is no existing coordination
primitive in the repo to reuse (grep for `advisory_lock` / `SKIP LOCKED` /
`lease` / leader election returns nothing).

**Alternatives considered**:
- *Session-level advisory locks*: rejected — connection-pool lifetime mismatch.
- *Transaction-level advisory lock held for the whole pass*: rejected — holds a
  pooled connection for the entire (potentially long) canonicalization pass,
  starving the pool.
- *Per-account `SELECT ... FOR UPDATE SKIP LOCKED`* (cooperative, all replicas
  work different accounts): viable and more scalable, but more complex and
  changes the worker's structure significantly. Recorded as a **future
  optimization** if single-leader throughput becomes a bottleneck; the issue
  (#190) explicitly asks for leader election, which the lease delivers directly.

## R4 — Rate limiting: per-process budget partitioning by autoscaling max capacity

**Decision**: Keep the rate limiter per-process (`middleware/rate_limit.rs:125-220`)
but divide the configured global limits (`GUARDIAN_RATE_BURST_PER_SEC`,
`GUARDIAN_RATE_PER_MIN`) by a new partition count `GUARDIAN_RATE_LIMIT_PARTITIONS`,
so each replica enforces `global_limit / partitions`. The partition count is set
to the deployment's **autoscaling max capacity**, not the current replica count.
If `GUARDIAN_RATE_LIMIT_PARTITIONS` is unset or `1`, behavior is identical to
today.

**Why max capacity, not current count**: partitioning by the live replica count
would require the server to know how many replicas are running right now (it does
not, without coupling to AWS APIs) and would silently become up to Nx too loose
during scale-out before the count propagated. Partitioning by the fixed max
capacity is conservative: when only 2 of a max-6 fleet are running, enforcement is
stricter than the global limit (each replica caps at limit/6), and it can never
silently exceed the global limit. The documented tolerance band is therefore an
**under-enforcement** band (stricter below max capacity), not an over-enforcement
risk.

**Wired in Terraform, not runbook-only**: the default value ships from
infrastructure. `infra/data.tf` already computes
`effective_server_autoscaling_max_capacity` (prod = `max(desired, 6)`);
`infra/ecs.tf` already injects the rate-limit env block (after
`GUARDIAN_RATE_PER_MIN`). The new env var
`GUARDIAN_RATE_LIMIT_PARTITIONS = tostring(local.effective_server_autoscaling_max_capacity)`
is added to that block (optionally fronted by a `var.guardian_rate_limit_partitions`
override that falls back to the effective max). Operators can override, but the
default is correct out of the box — no manual runbook value to keep in sync.

**Fallback (FR-010)**: the limiter is purely in-process arithmetic over the
partitioned budget, so there is no external coordination dependency and no
shared-store failure mode on the request path.

**Rationale**: The issue accepts enforcement "within some documented tolerance,"
which makes an exact global counter unnecessary. A shared counter on the request
hot path (Postgres or Redis) would add latency and write amplification to every
request for a control the issue treats as approximate. Budget partitioning needs
no shared state and adds no hot-path I/O.

**Alternatives considered**:
- *Partition by current/desired replica count*: rejected — server cannot know the
  live count without AWS coupling, and it goes silently loose during scale-out.
- *Postgres token bucket per client*: rejected — write on every request; hot-path
  latency and contention.
- *Redis counters*: rejected — new infra (see R1).
- *GCRA/sliding window in a shared store*: rejected — same hot-path cost; the
  approximate requirement does not warrant it.
- *Runbook-only manual partition value*: rejected — drifts from the actual
  autoscaling config; the correct value already exists in Terraform.

## R5 — Deployment stage signal: reuse `GUARDIAN_ENV=prod`

**Decision**: Reuse the existing `GUARDIAN_ENV` variable as the stage signal.
Extract the existing prod check (`ack/mod.rs:139-145` `is_prod_environment()`)
into a shared helper `config/stage.rs` used by the cursor-secret guard, the
filesystem-backend guard, and any future HA guardrail. Do not introduce a new
stage variable.

**Rationale**: `GUARDIAN_ENV` is already set by Terraform from
`var.deployment_stage` on the prod ECS task (`infra/ecs.tf:128-129`), and
`local.is_prod` already drives the prod profile (desired_count 2, max 6,
autoscaling) in `infra/data.tf`. Reusing it avoids a second, redundant stage
concept and keeps a single source of truth that operators already set.

**Alternatives considered**:
- *New `GUARDIAN_STAGE` variable*: rejected — duplicates an existing, already
  wired signal; more to document and keep consistent.

## R6 — Cursor secret + filesystem backend: fail-fast in prod, warn in dev

**Decision**:
- *Cursor secret*: in the prod stage, refuse to start if
  `GUARDIAN_DASHBOARD_CURSOR_SECRET` is unset (today it silently generates an
  ephemeral per-process secret with only a warning,
  `dashboard/state.rs:359-370`). In non-prod, keep the warning + ephemeral
  fallback (dev convenience).
- *Filesystem backend*: in the prod stage, refuse to start when the active
  storage backend is the filesystem backend (`builder/storage.rs:89-149`), with
  an actionable error pointing to a shared database backend. In non-prod it
  remains the default.

**Rationale**: Both are silent, dangerous multi-replica misconfigurations today.
Failing fast in prod converts a hard-to-diagnose runtime breakage (broken
pagination; divergent per-replica state; unpersisted audit) into an immediate,
actionable startup error, while preserving the zero-friction dev path
(Constitution: "Local development defaults to the filesystem backend").

**Alternatives considered**:
- *Always fail (even dev)*: rejected — breaks the documented dev default and
  FR-014.
- *Warn only (even prod)*: rejected — this is the status quo that the issue calls
  out as silently broken.

## R7 — Concurrent migrations at multi-replica startup

**Decision**: Guard `run_pending_migrations` (`storage/postgres.rs:32-47`) with a
Postgres **session-level advisory lock** on a fixed key: acquire ->
`run_pending_migrations` -> release. The first replica to boot migrates; the
others block on the lock, then find no pending migrations and proceed. No manual
"migrate first, then start" deploy step.

**Rationale**: With 2-6 replicas booting together (rolling deploy / cold start),
every replica runs `embed_migrations!` against the one shared Postgres
simultaneously. Diesel's embedded runner does not serialize concurrent runners
safely by default, so identical concurrent migrations can race or deadlock on
first deploy. An advisory lock is the standard, minimal fix and — unlike for the
canonicalization lease — is appropriate here because the critical section is
short, single-connection, and bounded (held only across the migration call, not
across request/pool churn).

**Alternatives considered**:
- *Separate one-shot migration job/step in ECS before serving*: rejected — adds
  deploy orchestration and an operator step; the in-process advisory lock keeps
  "just start the image" semantics (FR-017).
- *Rely on Diesel default behavior*: rejected — not safe under concurrency.

## R8 — Auth availability & per-request cost trade-offs

**Decision**: With sessions/challenges in Postgres, (a) auth **fails closed** when
the store is briefly unavailable (login and authenticated requests are rejected,
never bypassed; the lease holder steps down rather than risk double-processing),
and (b) every authenticated request performs **one indexed `SELECT`** by
`token_digest` rather than an in-memory map hit — **no local session cache** — so
logout/expiry are honored on every replica immediately (FR-003).

**Rationale**: For a custody system, a DB blip must never grant access, so
fail-closed is the only safe choice; it is nonetheless a behavior change from
today's always-available in-memory map and must be stated, not assumed. A local
cache would reduce per-request DB load but would serve revoked sessions until its
TTL, violating the "immediate revocation on every replica" requirement — so
immediate revocation is deliberately chosen over caching. Challenges are touched
only during login (low volume), so the recurring per-request cost is the single
session lookup, which reinforces (not creates) the pool-sizing concern below.

**Alternatives considered**:
- *Local per-replica session cache (with TTL)*: rejected — breaks immediate
  cross-replica revocation (FR-003).
- *Fail-open auth on store outage*: rejected — would grant access during a DB
  outage; unacceptable for custody.

## Cross-cutting: no client wire-contract change

**Decision**: This feature changes only server-internal storage of auth state,
background-work coordination, startup guards, and rate-limit arithmetic. No
proto, HTTP/JSON payload, status enum, or error surface changes. Auth endpoints
must return identical outcomes and identical boundary errors.

**Validation**: OpenAPI drift gate must show no diff; Rust/TS multisig and
operator smoke flows must pass unchanged (Constitution I, II). A "no wire diff"
check is part of each PR.

## DB connection sizing note

`GUARDIAN_DB_POOL_MAX_SIZE` (default 16) is per replica; with N replicas the
total can reach 16*N + metadata pool, which may exceed Postgres
`max_connections`. Not a code change, but the runbook (US6) must instruct
operators to size the pool against `max_connections / (replicas * pools)`.
