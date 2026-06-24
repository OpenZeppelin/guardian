# Feature Specification: Horizontal Scaling Correctness Across Multiple Guardian Instances

**Feature Branch**: `010-horizontal-scaling`
**Created**: 2026-06-20
**Status**: Draft
**Input**: User description: "Ensure horizontal scaling works correctly across multiple Guardian instances (issue #242)"
**Tracking issue**: [#242](https://github.com/OpenZeppelin/guardian/issues/242)

## Overview

The production deployment runs the Guardian server as 2-6 ECS tasks behind a
round-robin load balancer. Several subsystems were written under an implicit
single-instance assumption, so a request that begins on one replica and
continues on another can fail, and background work runs redundantly on every
replica. This feature makes the server correct under horizontal scaling: any
request may land on any replica, replicas may be added or removed at any time,
and operators have a documented configuration for a highly-available (HA)
deployment.

The scope is **correctness and operability under multiple replicas**, not new
end-user functionality. Each subsystem below is independently testable and
independently shippable.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Operator login succeeds with multiple replicas (Priority: P1)

An operator authenticates to the dashboard while the load balancer routes the
challenge request and the verification request to different replicas. The login
must complete successfully regardless of which replica handles each step, and an
established session must be honored by every replica.

**Why this priority**: Authentication is the entry point to all operator
functionality. Today the auth challenge and the session record live in
per-process memory, so a login or any subsequent authenticated call that lands
on a different replica than the one that issued the challenge/session fails.
This makes the dashboard effectively unusable with more than one replica - the
highest-impact breakage in the issue.

**Independent Test**: Run 2+ replicas behind the load balancer and complete the
full challenge -> sign -> verify -> authenticated-request flow, forcing each step
onto a different replica. Login completes and the session is accepted on every
replica.

**Acceptance Scenarios**:

1. **Given** 2+ replicas behind the load balancer, **When** an operator requests
   a login challenge from replica A and submits the signed response to replica B,
   **Then** verification succeeds and a session is established.
2. **Given** an established operator session, **When** an authenticated request
   is routed to any replica, **Then** the session is recognized and the request
   is authorized without re-login.
3. **Given** a pending challenge issued by one replica, **When** the operator
   never completes it, **Then** the challenge expires consistently and cannot be
   replayed on any replica after expiry.
4. **Given** an operator logs out on one replica, **When** a subsequent request
   with the same session token reaches any other replica, **Then** the session
   is rejected.

---

### User Story 2 - A delta is canonicalized exactly once (Priority: P1)

The background canonicalization worker promotes pending candidate deltas to
canonical state after verifying them against on-chain state. With multiple
replicas, each pending candidate must be processed exactly once, regardless of
how many replicas are running.

**Why this priority**: The canonicalization worker currently runs on every
replica with no leader election or shared lock, so every replica independently
re-processes the same candidates. This causes duplicate work and races on state
transitions (promote/discard/retry-budget), which can corrupt the proposal
nonce sequence and lead to permanent state-commitment mismatches. Correctness of
custody state is paramount.

**Independent Test**: Run 2+ replicas, create pending candidates, and confirm
each candidate transitions exactly once (one promotion or one discard), with no
duplicate submissions or double-counted retries, across the full replica set.

**Acceptance Scenarios**:

1. **Given** N replicas running and a pending candidate delta, **When** the
   canonicalization interval elapses, **Then** exactly one replica processes the
   candidate and it is promoted or discarded exactly once.
2. **Given** the replica currently performing canonicalization stops or crashes,
   **When** the next interval elapses, **Then** another replica takes over
   canonicalization with no manual intervention.
3. **Given** a candidate's retry budget, **When** processing fails, **Then** the
   retry count is incremented exactly once per interval across the whole fleet
   (not once per replica).
4. **Given** only a single replica is running, **When** canonicalization runs,
   **Then** behavior is unchanged from today (no regression).

---

### User Story 3 - Pagination cursors are valid across all replicas (Priority: P2)

An operator pages through dashboard list results (e.g. accounts, deltas) where
successive page requests are routed to different replicas. Cursors returned by
one replica remain valid on every other replica.

**Why this priority**: Cursors are signed/verified with a secret that, when
unset, is generated randomly per process. Across replicas this silently breaks
pagination (a cursor from replica A fails verification on replica B). It is
high-frequency operator pain but degrades to "start over" rather than corrupting
state, so it ranks below auth and canonicalization.

**Independent Test**: With 2+ replicas and a shared cursor secret configured,
request page 1 from one replica and page 2 (using the returned cursor) from
another; the second page returns the correct continuation. With the secret
unset in a multi-replica configuration, startup surfaces the misconfiguration.

**Acceptance Scenarios**:

1. **Given** a shared cursor secret configured on all replicas, **When** a cursor
   issued by one replica is submitted to another, **Then** it verifies and
   returns the correct next page.
2. **Given** a multi-replica configuration with no shared cursor secret, **When**
   the server starts, **Then** the operator is clearly warned that pagination
   will break across replicas (and, per FR-013, startup fails in the prod stage).
3. **Given** a tampered or expired cursor, **When** it is submitted to any
   replica, **Then** it is rejected consistently.

---

### User Story 4 - Rate limits are enforced consistently across replicas (Priority: P2)

A client making requests that are spread across replicas by the load balancer is
subject to rate limits that reflect total traffic, within a documented
tolerance - not per-replica limits that multiply with replica count.

**Why this priority**: The rate limiter is per-process, so the effective limit
scales with replica count (e.g. 2 replicas ~ 2x the configured burst). This
weakens an abuse-prevention control, but it fails open (more lenient) rather
than blocking legitimate traffic, so it ranks below correctness-critical items.

**Independent Test**: With 2+ replicas and `GUARDIAN_MAX_REPLICAS` set to the
autoscaling max capacity, drive traffic exceeding the global limit through the
load balancer and confirm the aggregate accepted rate stays at or below the
global limit (stricter when running below max capacity), rather than scaling by
replica count.

**Acceptance Scenarios**:

1. **Given** a configured global request limit and 2+ replicas, **When** a client
   exceeds the limit across replicas, **Then** excess requests are throttled so
   the aggregate accepted rate stays at or below the global limit, regardless of
   how the load balancer distributes them.
2. **Given** rate limiting is disabled by configuration, **When** running with
   multiple replicas, **Then** no throttling occurs (no regression).
3. **Given** `GUARDIAN_MAX_REPLICAS` is set to the autoscaling max capacity,
   **When** fewer than that many replicas are running, **Then** aggregate
   enforcement is stricter than the global limit (never looser), and no request
   depends on an external coordination service.

---

### User Story 5 - Filesystem backend is refused in the prod stage (Priority: P3)

When the server is configured for the production stage, it refuses to start with
the filesystem storage backend, because filesystem storage is local to a single
task and cannot be shared across replicas. The filesystem backend remains fully
supported for local development.

**Why this priority**: The filesystem backend cannot back a multi-replica
deployment (each replica would have divergent local state and audit events are
not persisted). Refusing it in prod prevents a silent, dangerous
misconfiguration. It is a guardrail rather than core multi-replica plumbing, and
the published prod image is already built with the Postgres backend, so it ranks
P3.

**Independent Test**: Start the server in the prod stage with the filesystem
backend selected and confirm it fails fast with a clear, actionable error. Start
the same configuration in a non-prod stage and confirm it starts (dev-only path
preserved).

**Acceptance Scenarios**:

1. **Given** the server is configured for the prod stage, **When** it would use
   the filesystem storage backend, **Then** startup fails with an error
   identifying the misconfiguration and the required remedy (use a shared
   database backend).
2. **Given** the server is configured for a non-prod stage, **When** it uses the
   filesystem backend, **Then** it starts normally (development workflow
   unaffected).
3. **Given** the prod stage with a shared database backend, **When** the server
   starts, **Then** there is no filesystem-related failure.

---

### User Story 6 - Operators have an HA configuration runbook (Priority: P3)

An operator deploying multiple replicas can follow a single runbook listing
every environment variable and external state-store dependency required for a
correct HA deployment, and understands what breaks if each is omitted.

**Why this priority**: Several of the fixes above depend on operator
configuration (shared secrets, shared state stores, stage selection). Without
documentation the feature is not safely usable, but it depends on the other
stories being defined first, so it is sequenced last.

**Independent Test**: A reviewer follows only the runbook to configure a 2+
replica deployment and all P1/P2 acceptance scenarios pass without consulting
source code.

**Acceptance Scenarios**:

1. **Given** the operator runbook, **When** an operator configures an HA
   deployment using only the runbook, **Then** all required environment
   variables and state-store dependencies are covered.
2. **Given** the runbook, **When** an operator reads it, **Then** each HA-related
   setting documents the consequence of omitting it.
3. **Given** the runbook, **When** an operator reviews stage guidance, **Then**
   the dev-only status of the filesystem backend is clearly stated.

### Edge Cases

- **Replica added mid-session**: A newly started replica must immediately honor
  existing sessions, challenges, cursors, and the elected canonicalization owner
  without restart of the fleet.
- **Replica removed mid-flight**: When the replica that holds canonicalization
  leadership disappears, leadership must transfer within a bounded time so
  canonicalization is not stalled.
- **Clock skew between replicas**: Challenge/session expiry and lease/lock
  timing must remain correct (or fail safe) when replica clocks differ within a
  reasonable bound.
- **Concurrent migrations on simultaneous startup**: When 2-6 replicas boot at
  once (rolling deploy / cold start) they all run schema migrations against the
  one shared store at the same time. Applying migrations MUST be serialized (one
  replica migrates, the rest wait then proceed) so first-deploy startup cannot
  race or deadlock. See FR-017.
- **Shared store outage**: If the shared coordination/state store is temporarily
  unavailable, each affected subsystem MUST have a defined, documented behavior
  rather than undefined behavior or a crash loop. Specifically: authenticated
  requests and login **fail closed** (auth rejected, never bypassed) and the
  canonicalization leader **steps down** (work stalls, never double-processes),
  both recovering automatically when the store returns. See FR-018.
- **Split brain during leadership handoff**: Two replicas must never both believe
  they own canonicalization long enough to double-process a candidate.
- **Mixed configuration across replicas**: Replicas configured with different
  shared secrets (e.g. one missing the cursor secret) - the failure mode must be
  detectable rather than silent.
- **Single-replica deployments**: All changes must preserve current behavior when
  exactly one replica runs (no new mandatory infrastructure for dev/local).

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: Operator dashboard auth challenges MUST be resolvable by any
  replica, so a challenge issued by one replica can be verified by another.
- **FR-002**: Operator (and EVM, where applicable) sessions MUST be recognized by
  any replica, so an authenticated request succeeds on any replica without
  re-login.
- **FR-003**: Session and challenge lifecycle events (issuance, consumption,
  expiry, logout/revocation) MUST be consistent across replicas; a logged-out or
  expired session/challenge MUST be rejected on every replica.
- **FR-004**: Canonicalization of any pending candidate MUST occur exactly once
  across the entire fleet per processing interval, regardless of replica count.
- **FR-005**: The system MUST elect, or otherwise coordinate, a single owner for
  canonicalization at any given time, and MUST transfer ownership automatically
  when the current owner becomes unavailable. Ownership renewal MUST run
  concurrently with (not gated on) the canonicalization pass, the pass MUST be
  cooperatively cancellable so a lost owner can stop promptly, and every
  state-mutating write (canonical promotion **and** retry/discard) MUST be gated
  by an advisory fencing check so a superseded owner is prevented from committing
  during the cancellation window. (The fence is a pre-write ownership re-check;
  combined with idempotent writes — same delta ⇒ identical bytes — a brief
  two-leader overlap can at most re-apply the same transition, never corrupt
  state.)
- **FR-006**: Canonicalization retry budgets and state transitions
  (promote/discard) MUST be counted once per interval across the fleet, never
  once per replica.
- **FR-007**: Pagination cursors MUST be issued and verified using a shared
  secret so a cursor issued by one replica is valid on all replicas.
- **FR-008**: When a shared cursor secret is not configured, the system MUST
  surface the misconfiguration at startup (warning in non-prod, see FR-013 for
  prod behavior) rather than silently generating a per-process secret without
  notice.
- **FR-009**: The aggregate request rate enforced across all replicas MUST NOT
  exceed the configured global limit. This is achieved by dividing the global
  limit by the deployment's **maximum replica capacity** (`GUARDIAN_MAX_REPLICAS`),
  so each replica enforces `global_limit / GUARDIAN_MAX_REPLICAS`. When fewer than
  the maximum number of replicas are running, aggregate enforcement is stricter
  than the global limit (never looser); the resulting tolerance band MUST be
  documented. `GUARDIAN_MAX_REPLICAS` MUST default from the deployment's
  autoscaling max capacity (set by infrastructure), not from a manually maintained
  value, and MUST remain operator-overridable.
- **FR-010**: Rate limiting MUST NOT introduce any external coordination
  dependency on the request hot path; enforcement is per-process arithmetic over
  the partitioned budget and therefore has no shared-store failure mode. Any
  future shared/global limiter would have to define and document its
  fail-open/fail-closed behavior; none is introduced by this feature.
- **FR-011**: The system MUST provide a single configuration value that
  identifies the deployment stage (at minimum distinguishing "prod" from
  non-prod) usable by HA guardrails.
- **FR-012**: In the prod stage, the system MUST refuse to start with a storage
  backend that cannot be shared across replicas (the filesystem backend),
  failing fast with an actionable error.
- **FR-013**: In the prod stage, the system MUST refuse to start (or fail fast)
  when a required HA setting is missing where its absence would cause
  cross-replica incorrectness (e.g. an unset shared cursor secret); in non-prod
  the same condition MUST warn but allow startup.
- **FR-014**: All HA behaviors MUST preserve existing single-replica behavior;
  running exactly one replica MUST NOT require new external infrastructure for
  local/dev use.
- **FR-015**: The Rust and TypeScript clients MUST observe no behavior drift as a
  result of these changes; the wire contract for clients MUST remain unchanged
  unless an explicit, documented contract change is made.
- **FR-016**: Operator-facing documentation MUST enumerate every environment
  variable and external state-store dependency required for a correct HA
  deployment, including the consequence of omitting each, and MUST mark the
  filesystem backend as dev-only.
- **FR-017**: Schema migrations MUST be safe under concurrent execution by
  multiple replicas starting simultaneously; migration application MUST be
  serialized across the fleet so a first deploy cannot race or deadlock, with no
  manual "migrate first, then start" operator step required.
- **FR-018**: When the shared state store is briefly unavailable, authentication
  (login and authenticated requests) MUST fail closed (rejected, never bypassed)
  and the canonicalization owner MUST step down rather than risk double-processing;
  both MUST recover automatically when the store returns. This fail-closed auth
  behavior is an accepted, documented change from the previous always-available
  in-memory behavior.
- **FR-019**: At startup the server MUST emit a single, unambiguous log line
  stating which coordination mode is active — "shared" (backed by the external
  store, replica-safe) or "single-process" (in-memory, single-replica only) —
  together with the effective HA-relevant settings it derives from configuration:
  the storage backend, the deployment stage, the maximum replica capacity, and
  whether the pagination cursor secret was supplied or generated. This makes the
  active mode explicit and diagnosable without inferring it from other logs, and
  is the discoverable signal that replaces an explicit mode toggle (coordination
  capability is determined by resolved configuration, not a separate flag). The
  line MUST reflect the actual resolved state, never operator intent.
- **FR-020**: The coordination mode MUST be determined by the **storage backend
  alone**: the Postgres backend MUST use shared coordination (sessions,
  challenges, leadership) and the filesystem backend MUST use in-memory
  coordination. Shared coordination MUST be the default whenever Postgres is
  active and MUST NOT be disabled by any tunable — a missing, mis-overridden, or
  low `GUARDIAN_MAX_REPLICAS` (or any other knob) MUST NEVER silently reintroduce
  per-process auth/canonicalization state on a Postgres deployment. (Skipping the
  per-request session lookup for a deployment known to be single-instance is a
  possible future optimization behind an explicit, guarded opt-in; it is out of
  scope here and MUST NOT be inferred from a rate-limit signal.)

### Key Entities

- **Auth Challenge**: A short-lived, one-time login challenge bound to an
  operator identity; must be readable and consumable by any replica until it
  expires or is consumed.
- **Operator Session**: An authenticated session with an issue and expiry time
  and a revocation (logout) state; must be authoritative across replicas.
- **Canonicalization Lease / Leadership**: The right, held by at most one replica
  at a time, to run the canonicalization worker; has a holder identity, an
  expiry/heartbeat so it can be reclaimed, and a fencing token (advancing on each
  steal) re-checked before every state-mutating write so a superseded holder is
  prevented from committing (advisory check, made safe by idempotent writes).
- **Pagination Cursor**: An opaque, integrity-protected continuation token whose
  validity depends on a secret shared by all replicas.
- **Maximum Replica Capacity** (`GUARDIAN_MAX_REPLICAS`): The
  infrastructure-derived signal for how many replicas the deployment can scale to.
  It feeds **rate-limit partitioning only** (`global_limit / GUARDIAN_MAX_REPLICAS`).
  It MUST NOT influence the coordination mode (which is backend-derived, FR-020).
- **Effective Rate-Limit Budget**: The per-replica share of the global limit,
  computed as `global_limit / GUARDIAN_MAX_REPLICAS`. Per-client burst/sustained
  counters remain per-process; they are partitioned, not aggregated, so total
  enforcement stays at or below the global limit.
- **Deployment Stage**: A configuration value identifying the environment (prod
  vs. non-prod) that gates HA guardrails.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: With 2+ replicas behind the load balancer, an operator completes
  the full login flow with a 100% success rate across 20 consecutive attempts,
  including attempts where challenge and verification are forced onto different
  replicas.
- **SC-002**: With 2+ replicas, every pending candidate is canonicalized exactly
  once - zero duplicate promotions, discards, or submissions - across a test of
  at least 50 candidates.
- **SC-003**: When the replica holding canonicalization leadership is terminated,
  canonicalization resumes on another replica within the configured lease TTL
  (the failover bound, independent of the delta submission grace period), with no
  manual intervention.
- **SC-004**: With 2+ replicas and a shared cursor secret, 100% of pagination
  cursors issued by one replica are accepted by other replicas across a paging
  test of at least 100 page transitions.
- **SC-005**: With N replicas, the aggregate accepted request rate for a client
  exceeding the configured limit stays at or below the configured global limit
  (rather than ~ Nx the limit). The documented tolerance band MUST also state the
  two-sided imprecision: (a) running below the autoscaling max capacity enforces
  stricter than the global limit, and (b) HTTP keep-alive can pin a single client
  to one replica, so that client may be throttled at
  `global_limit / GUARDIAN_MAX_REPLICAS` (e.g. 1/6) — an over-strict, fail-closed
  outcome for that client. Both are accepted trade-offs of partitioning without
  shared hot-path state.
- **SC-006**: A prod-stage server configured with the filesystem backend (or with
  a required HA setting missing) fails to start 100% of the time with an error
  that names the misconfiguration and the remedy.
- **SC-007**: A reviewer who has never seen the code can stand up a correct 2+
  replica deployment using only the operator runbook, and all P1/P2 acceptance
  scenarios pass.
- **SC-008**: All existing single-replica test suites pass unchanged, confirming
  no regression for dev/local deployments.
- **SC-009**: On startup, the server logs exactly one coordination-mode line that
  correctly reports "shared" when backed by the external store and
  "single-process" otherwise, including the resolved backend, stage, max replica
  capacity, and cursor-secret source; an operator can determine the active mode
  from that single line alone (mode follows the storage backend).

## Assumptions

- The shipped production image is built with the Postgres storage backend, so a
  shared relational database is available to replicas and is the natural shared
  coordination/state store for sessions, challenges, leadership, cursors, and
  rate-limit counters. No new infrastructure component (e.g. a separate cache or
  queue) is assumed to be mandatory; if one is proposed it will be justified in
  planning.
- "Prod stage" is represented by the existing `GUARDIAN_ENV=prod` signal (today
  used only for ACK secret sourcing), extended to gate HA guardrails. Confirming
  this versus introducing a dedicated stage variable is a planning decision.
- The cursor secret environment variable already exists
  (`GUARDIAN_DASHBOARD_CURSOR_SECRET`); this feature changes its enforcement, not
  its format.
- The load balancer does not provide sticky sessions; correctness must not depend
  on session affinity.
- Replica clocks are synchronized within a few seconds (standard for the ECS
  environment); expiry/lease logic must tolerate small skew.
- Rate limiting is partitioned conservatively against the autoscaling **max**
  capacity (not the current replica count), so it is never silently looser than
  the global limit during scale-out and over-throttles (conservatively stricter)
  when running below max capacity. A documented tolerance band for this
  over-throttling is acceptable, consistent with the issue's "within some
  documented tolerance".
- The infrastructure already computes the autoscaling max capacity
  (`infra/data.tf` `effective_server_autoscaling_max_capacity`, prod =
  `max(desired, 6)`); `GUARDIAN_MAX_REPLICAS` defaults from it via Terraform
  rather than a manually maintained value. It drives **rate-limit partitioning
  only**; the coordination mode is backend-derived (FR-020).

## Dependencies

- Issue [#190](https://github.com/OpenZeppelin/guardian/issues/190) (single
  canonicalization owner / no leader election) is subsumed by User Story 2.
- Existing configuration surface: `GUARDIAN_DASHBOARD_CURSOR_SECRET`,
  `GUARDIAN_ENV`, `GUARDIAN_RATE_LIMIT_ENABLED`, `GUARDIAN_RATE_BURST_PER_SEC`,
  `GUARDIAN_RATE_PER_MIN`, `DATABASE_URL`, `GUARDIAN_STORAGE_PATH`,
  `GUARDIAN_METADATA_PATH`.
- New configuration: `GUARDIAN_MAX_REPLICAS` (maximum replica capacity; drives
  **rate-limit partitioning only**; defaults from
  `effective_server_autoscaling_max_capacity`).
- Infrastructure wiring (in scope): `infra/data.tf`
  (`effective_server_autoscaling_max_capacity`) and `infra/ecs.tf` (server env
  block) must set `GUARDIAN_MAX_REPLICAS` so the correct default ships without
  operator action.
- Operator documentation set (`docs/CONFIGURATION.md`, AWS deploy docs, runbooks)
  must be updated per the contributor docs table.

## Out of Scope

- Autoscaling policy, ALB/ECS provisioning, or Terraform changes beyond the
  `GUARDIAN_MAX_REPLICAS` env-var wiring (in scope above) and documenting required
  configuration.
- Skipping shared coordination for a known single-instance Postgres deployment
  (a per-request-lookup optimization); if pursued later it MUST be an explicit,
  guarded opt-in, never inferred from `GUARDIAN_MAX_REPLICAS` or another tunable.
- Changing the storage backend selection from a compile-time feature to a runtime
  switch.
- Multi-region or active/active cross-region deployment.
- End-user (custody client) facing feature changes; this work is server-side
  correctness and operability only.
