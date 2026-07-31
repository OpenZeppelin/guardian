# Feature Specification: Reduce Per-Read Cost of Replay-Protection Auth Writes

**Feature Branch**: `365-read-path-auth-write`
**Created**: 2026-07-31
**Status**: Draft
**Input**: GitHub issue [#365](https://github.com/OpenZeppelin/guardian/issues/365) — "Read path: every authenticated read performs a durable auth write". User direction: this is a security-sensitive change; the approach must be selected after a careful, documented comparison of alternatives, and the options proposed in the issue are candidates — not mandates.

## Problem Statement

Every signature-authenticated read request to Guardian performs a durable write to shared storage as part of replay protection (recording the last accepted request timestamp per account). Measurement (issue #365) shows this write consumes 33.5% of read-path database time (authentication overall: 43.5%), and that the database — not the Guardian servers — is the throughput ceiling: removing the write in a controlled experiment increased read throughput by 71% while server replicas remained idle. Because the write lives in the shared authentication path, all five authenticated read endpoints pay it, including the two that SDK clients poll continuously.

The replay-protection guarantee itself is correct and required. What must change is the **cost** of maintaining it per read — not the guarantee.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Operator serves read-heavy production load without database saturation (Priority: P1)

An operator running Guardian for a large deployment (e.g. 100k accounts, the #317 scalability target of 2,000 paced reads/s) needs the read path to run within database capacity with comfortable headroom, because the same database also serves writes, canonicalization workers, and dashboard queries. Today the replay-protection write pushes read-only traffic to ~86% database utilisation at target load, leaving no room for the rest of the system.

**Why this priority**: This is the reason the issue exists — the database saturates while server replicas idle, so read throughput cannot be scaled by adding replicas. Every other benefit follows from fixing this.

**Independent Test**: Re-run the reference read benchmark (128-reader closed-loop leg from issue #365) against the changed system and compare throughput, latency, and per-read database cost against the recorded baseline (2,314/s, p95 101ms, 0.385ms DB time/read).

**Acceptance Scenarios**:

1. **Given** the reference benchmark environment from issue #365, **When** the 128-reader read leg is re-run on the changed system, **Then** sustained throughput improves by at least 25% over the recorded baseline and p95 latency does not regress.
2. **Given** sustained read-only traffic at the #317 paced target rate (2,000 reads/s), **When** database utilisation is measured, **Then** the read workload consumes materially less database capacity than baseline, leaving headroom for concurrent write and background workloads.
3. **Given** a mixed read profile (state reads plus the two polled sync/proposal-discovery endpoints), **When** the benchmark is run, **Then** all authenticated read endpoints show the reduced per-call authentication cost — not just the benchmark-headline endpoint.

---

### User Story 2 - Replay protection remains intact across replicas (Priority: P1)

A security-conscious operator (and Guardian's own threat model) requires that a captured authenticated request cannot be replayed: within the timestamp skew window, each accepted request timestamp is usable exactly once per account, enforced consistently across all server replicas behind a load balancer. This guarantee was introduced as a security fix and must survive the cost optimisation unchanged — or be relaxed only by an explicit, documented, security-reviewed decision, never as a side effect.

**Why this priority**: Equal-P1 with performance. A faster read path that silently weakens replay protection is a regression, not a win. The user has flagged this issue as sensitive precisely because the optimisation touches a security control.

**Independent Test**: Replay a previously accepted authenticated request (same timestamp, same signature) against each replica of a multi-replica deployment and confirm rejection; run concurrent same-timestamp requests and confirm exactly one acceptance.

**Acceptance Scenarios**:

1. **Given** an authenticated read request that was accepted by replica A, **When** the identical request is replayed to replica A or replica B within the skew window, **Then** it is rejected with the same client-visible error as today.
2. **Given** two concurrent identical requests for the same account arriving at different replicas, **When** both attempt to record the same timestamp, **Then** exactly one succeeds and one is rejected.
3. **Given** a request with a timestamp older than the account's last accepted timestamp, **When** it is presented to any replica, **Then** it is rejected.
4. **Given** a server crash and restart (or failover), **When** a captured request from before the crash is replayed after recovery, **Then** it is rejected exactly as it would be today: replay state survives crash and failover with the same durability as the current implementation. Any approach that weakens this is outside this feature entirely and requires a user-approved amendment to this specification or a successor feature (see FR-001).

---

### User Story 3 - The fix lands once and covers every authenticated read endpoint (Priority: P2)

SDK clients poll `get_delta_since` (sync loop) and `get_delta_proposals` (cosigner work discovery) continuously; `get_delta`, `get_delta_proposal`, and `get_state` complete the set. All five pay the identical per-call authentication write via the shared resolution path. The improvement must apply at the shared call site so all five endpoints — and any future authenticated endpoint — benefit identically, with no per-endpoint divergence.

**Why this priority**: Production call volume is dominated by the polled endpoints, which the headline benchmark does not exercise. Fixing only the benchmark endpoint would understate and under-deliver the real-world saving.

**Independent Test**: Verify the cost reduction applies at the shared authentication path, and exercise each of the five endpoints confirming identical replay-protection behavior and reduced per-call storage cost.

**Acceptance Scenarios**:

1. **Given** any of the five authenticated read endpoints, **When** a request is authenticated, **Then** the per-call storage cost of replay protection is the reduced cost (identical mechanism for all five).
2. **Given** the authenticated write endpoints (`push_delta`, `push_delta_proposal`, `sign_delta_proposal`, `abandon_candidate`), **When** they authenticate, **Then** their behavior and correctness are unchanged.

---

### User Story 4 - Operators see storage behavior consistent with a read-only workload (Priority: P3)

An operator watching database health today sees continuous garbage generation, background cleanup churn, and write-ahead-log volume on a deployment that is nominally serving only reads — and sees the account "last updated" timestamp advance on every authenticated read, destroying its value as a record of configuration change. After the fix, each authenticated read still performs one small, durable replay-state write — that write *is* the guarantee (FR-001) and is retained by design — but it stops rewriting the large configuration record: churn against configuration records disappears, write volume per read shrinks by orders of magnitude, and configuration-change tracking means what it says.

**Why this priority**: Real operational value (monitoring, capacity planning, storage-throughput sizing) but secondary to the throughput ceiling and the security guarantee.

**Independent Test**: Run sustained read-only traffic and observe storage-maintenance activity and per-account "last updated" timestamps.

**Acceptance Scenarios**:

1. **Given** sustained authenticated read-only traffic, **When** storage maintenance activity is observed over the run, **Then** garbage/cleanup churn attributable to account configuration records is eliminated or reduced to a level consistent with the chosen design (and the residual is documented).
2. **Given** an account whose configuration has not changed, **When** it serves authenticated reads, **Then** its configuration-change timestamp does not advance.

---

### Edge Cases

- **Migration/upgrade window**: When an existing deployment upgrades, previously recorded replay state must not be lost or reset in a way that lets a captured pre-upgrade request replay post-upgrade. The upgrade must fail closed with respect to replay protection.
- **Mixed-version fleet during rolling deploy**: With one old-version and one new-version replica live simultaneously, both must consult replay state such that a request accepted by one cannot be replayed to the other. If the deployment model cannot guarantee this, the constraint (e.g. "not safe for rolling deploy; restart both replicas together") must be documented for operators.
- **Concurrent same-timestamp requests across replicas**: exactly-once acceptance must hold under race (see US2 scenario 2).
- **Account with no prior replay state** (newly registered, or first request post-migration): first valid request accepted, second identical request rejected.
- **Timestamp at the skew-window boundary**: behavior at the window edges is unchanged from today.
- **Crash/restart and failover**: the durability of replay state across process and storage failures matches today's behavior exactly (see US2 scenario 4 and FR-001).
- **Unauthenticated and session-authenticated paths** (`/pubkey`, `/status`, `/`, dashboard session reads, `/state/lookup`): unchanged — they never paid this cost and must not start paying it.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST preserve the per-account replay-protection guarantee exactly as it stands today: within the timestamp skew window, an accepted request timestamp for an account MUST NOT be accepted again — atomically, with unchanged durability across crash and failover, and consistently across all server replicas. Approaches that bound or relax this guarantee (e.g. coarsened persistence granularity, or volatile state that can be lost on failure) are **not available within this feature at all**: they would conflict with FR-004 and the Out of Scope list, so pursuing one requires a user-approved amendment to this specification or a successor feature — a design-review sign-off is not sufficient. "All server replicas" refers to deployments on the multi-process-capable backend (Postgres); the filesystem backend is single-process by design across all of its operations — a pre-existing, documented backend limitation that this feature inherits and does not change.
- **FR-002**: The system MUST materially reduce the per-request storage cost that replay protection imposes on authenticated reads. The reduction target is defined by the Success Criteria; the measured no-write experiment (+71% throughput) is the known upper bound no approach can exceed.
- **FR-003**: The improvement MUST apply at the shared authentication path so that all five signature-authenticated read endpoints (`get_state`, `get_delta_since`, `get_delta_proposals`, `get_delta_proposal`, `get_delta`) receive it identically, and authenticated write endpoints are not regressed.
- **FR-004**: The change MUST be invisible to clients: no wire-contract change, no change to which requests are accepted or rejected, and identical client-visible error behavior for rejected (replayed or stale) requests. Existing SDK clients (Rust and TypeScript) MUST work unmodified.
- **FR-005**: The approach MUST be chosen through a documented comparison of candidate designs — including but not limited to the three options sketched in issue #365 — evaluating each on: security posture (durability and atomicity of replay state, failure-mode behavior), performance recovered (fraction of the +71% upper bound), operational complexity (new runtime dependencies, migration risk), and deployment-model constraints (multi-replica operation, rolling upgrade). The comparison and its conclusion MUST be recorded in the planning artifacts before implementation begins. Only guarantee-preserving candidates are eligible for selection within this feature (FR-001); weaker candidates appear in the comparison to document why they were not taken and what a future amendment would trade.
- **FR-006**: Upgrading an existing deployment MUST NOT create a replay opportunity: replay state recorded before the upgrade MUST remain effective (or the system must fail closed) during and after migration, including for deployments running multiple replicas.
- **FR-007**: The performance claim MUST be validated by re-running the reference benchmark from issue #365 (same profile, same environment class) and recording an A/B comparison against the baseline measurements recorded in that issue (result directories `read-128-t4-20260730T083554Z` with the write; `nocas-128-t4-20260730T092323Z` without it), plus a mixed-profile A/B per SC-007. The benchmark harness (`benchmarks/diagnostic-stack/`) lands via a separate pending PR that this feature's verification depends on; it is a dependency, not part of this feature's scope. If the recorded baseline artifacts are not part of the merged harness, the baseline legs MUST be regenerated by running the same profiles against unmodified `main` on the same machine before the A/B — every headline comparison in this feature is same-machine A/B, never a comparison against numbers measured elsewhere.
- **FR-008**: Replay-protection activity MUST NOT distort account change tracking: the account "last updated" timestamp MUST advance only on non-authentication metadata mutations — configuration changes and explicit lifecycle operations (pause/release transitions, pending-candidate flag changes) — and MUST never advance as a side effect of authenticating a request.
- **FR-009**: Replay-protection behavior MUST be verifiably covered by automated tests: same-timestamp replay rejection, older-timestamp rejection, concurrent-acceptance race (exactly one winner), and first-request acceptance for accounts without prior state.

### Key Entities

- **Account replay state**: the per-account record of the most recently accepted authenticated-request timestamp. Written on every successful authentication, read on every authentication. Tiny (an identifier and a timestamp), extremely hot, and security-critical: must be shared, atomic, and durable to the degree the approved design decision specifies.
- **Account configuration metadata**: the per-account record of signer public keys and network configuration. Large (multisig key material), read on every authentication, but changed only by explicit configuration operations. Its change-tracking timestamp must reflect configuration changes only.
- **The distinction between them is the heart of this feature**: today the two share a lifecycle coupling that makes every read rewrite the large record to update the tiny one. The feature exists to decouple their cost — by whatever mechanism the design comparison selects.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: On the reference 128-reader read benchmark, sustained throughput improves by ≥25% over the recorded with-write baseline (≥ ~2,900/s vs 2,314/s) and p95 latency is no worse than baseline. (The no-write experiment's 3,957/s is the ceiling; ≥25% means recovering at least roughly a third of the available headroom.)
- **SC-002**: Authentication database cost drops on both a relative and an absolute measure, on the reference read benchmark. "Authentication" is defined as exactly two statements: the replay-state write and the account-configuration lookup performed per authenticated request. Relative: their share of total database time drops from the measured 43.5% to at most 25%. Absolute (immune to denominator shifts): their combined mean database time per accepted read drops by at least 40% from the baseline's ~0.16ms (0.126ms write + 0.038ms lookup).
- **SC-003**: Replay-protection verification passes in a two-replica deployment on the multi-process-capable backend (Postgres): replayed requests are rejected on both replicas, concurrent same-timestamp requests admit exactly one winner, and all existing authentication test suites pass unchanged. The filesystem backend — single-process by design — is verified with in-process concurrency tests instead.
- **SC-004**: A sustained read-only workload no longer produces continuous storage-garbage churn against account configuration records (cleanup activity on those records during a read-only benchmark run is zero, or attributable only to the documented residual of the chosen design).
- **SC-005**: Existing Rust and TypeScript SDK clients complete their full smoke flows against the changed server with no client-side changes.
- **SC-006**: Headroom at the #317 paced target improves to at least 30%. The observable is concrete: headroom = 1 − (2,000 / maximum sustained read throughput measured on the reference leg — a 100-second closed-loop read-only run on the reference environment, no concurrent background traffic). The ≥30% floor is arithmetically equivalent to SC-001's ≥25% throughput gain (≥ ~2,860/s vs the 2,314/s baseline, which gave ~14% headroom); the two criteria are one gate expressed two ways, and pass or fail together. A production aspiration of ≥40% headroom (≥ ~3,333/s, i.e. +44%) is recorded as a stretch marker only: missing it while passing SC-001 does not fail this feature, but the measured residual MUST be reported to the user (see Assumptions) to inform whether a successor feature is warranted. This is an A/B proxy on the reference environment, not a production-utilisation prediction.
- **SC-007**: Mixed-profile A/B passes. Profile: 40% `get_state`, 40% `get_delta_since`, 20% `get_delta_proposals`, 128 closed-loop readers, 100 seconds — run against both unmodified `main` and the changed server on the same machine. Pass: authentication database time per accepted read (as defined in SC-002) drops by at least 40%, and no individual endpoint's p95 latency regresses versus the `main` leg.

## Assumptions

- The evidence in issue #365 (measured 2026-07-30 against `0.16.0` / `5d3999ead`) is accepted as the baseline; the diagnosis is not re-litigated before design. The reproduction path is the `benchmarks/diagnostic-stack/` harness, which lands via a separate pending PR — an external dependency of this feature's verification (FR-007), not part of its scope. Baseline legs are regenerated on unmodified `main` if the recorded result directories do not ship with the merged harness.
- The exactly-once-per-timestamp guarantee is the default requirement. The issue notes one authenticated path (`/state/lookup`) already accepts a bounded replay window as documented precedent; this spec treats any extension of that precedent as an explicit security decision (FR-001, FR-005), never a default.
- "Materially reduce" is quantified by SC-001/SC-002 rather than by mandating a mechanism. If measurement shows those thresholds (or the SC-006 stretch marker) are reachable only via a guarantee-weakening approach, that finding returns to the user as a **spec-level decision**: per FR-001, options such as an external volatile store, unlogged storage, or coarsened persistence can only be pursued through an amendment to this specification or a successor feature — never resolved inside this one.
- Benchmark numbers were gathered on developer hardware (VM on Apple Silicon) and are order-of-magnitude checks; success criteria are evaluated as A/B deltas on the same environment class, not as absolute production predictions.
- Confirming the live production storage configuration (storage type, allocated size, pool size) is an operational follow-up flagged in the issue, not a blocker for this feature.

## Out of Scope

- **Read scaling via read-only database replicas** — blocked on this feature (a read that writes cannot run on a read replica) and deserving its own issue afterwards.
- **Dashboard read cost** (per-row summary computation) — different mechanism, different path, no replay write involved.
- **Production storage-tier migration** (gp2 → gp3) — standalone infrastructure work.
- **Changing the timestamp skew-window semantics or the signature scheme** — the authentication protocol itself is untouched.
- **Choosing the implementation approach within this document** — deliberately deferred to the planning phase, where FR-005's weighed comparison happens with full codebase context.
