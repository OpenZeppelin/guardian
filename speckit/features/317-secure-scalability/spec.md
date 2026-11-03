# Feature Specification: Security-Preserving Scalability for Reads, Replay Protection and Canonicalization

**Feature Branch**: `317-secure-scalability`
**Created**: 2026-10-30
**Status**: Draft
**Input**: User description: "Based on gap-analysis.md and on session-auth-rfc.md start working on specification for perf improvements. We should go with options that are okay for security."
**Tracking issue**: [#317](https://github.com/OpenZeppelin/guardian/issues/317)
**Design inputs**: [gap analysis](../../../docs/architecture/scalability/gap-analysis.md), [session-auth RFC](../../../docs/architecture/scalability/session-auth-rfc.md), [session replay design model](../../../docs/architecture/scalability/tla/README.md)

## Context

Issue #317 sets a target: 100,000 guarded accounts, up to 100 users
transacting at once, and up to 20,000 users reading state at once, with
`get_state` p95 <= 1 s, accepted-to-canonical p95 <= 30 s and zero
replay-protection failures for legitimate traffic.

The gap analysis found that Guardian misses this target in four places:

1. **Replay protection rejects legitimate traffic.** Every per-account
   request must carry a timestamp strictly greater than the last accepted
   one for that signer. Parallel or out-of-order requests from one client
   fail, so "zero failures" is unreachable by tuning.
2. **Reads are expensive.** A session `get_state` costs three database
   reads and one database write, plus a fixed per-request cost from the
   connection proxy. One task saturated at about 1,400 reads/s in April;
   the target is 4,000 to 10,000 reads/s.
3. **Canonicalization is serialized.** One rebuild runs at a time, so 100
   simultaneous candidates can take up to about 100 s.
4. **Whole-account jobs and measurement do not scale.** The release sweep
   nearly fills its rotation at 100,000 accounts, and the dashboard cannot
   show per-route latency percentiles, so no acceptance criterion can be
   checked today.

### Security posture of this feature

The design documents list several ways to close these gaps. This
specification adopts **only options that keep or strengthen today's
security properties**. In particular:

- A captured signed request stays usable **at most once**, fleet-wide,
  exactly as today. This specification does **not** adopt the RFC's
  window-only session reads, under which a captured read could be replayed
  repeatedly for its whole window and return private state created after
  capture.
- Authorization is always decided against current, authoritative data.
  Signer removal, session revocation and expiry take effect on the next
  request on every replica.
- Decrypted account state is never held in long-lived memory.
- Every request is bound to the operation it was signed for, closing the
  latent hazard where reads (and potentially writes) share signed payloads.

The adopted replay design is the RFC's named security-preserving
alternative: replace the per-signer timestamp floor for session reads with
a fleet-wide record of request digests already accepted, and reject only
exact repeats.

## Scope *(mandatory)*

### In Scope

- Operation-bound session request authentication (version 2 envelope,
  operation registry), as specified in the RFC, with strict replay checks.
- Replacing the session read timestamp floor with fleet-wide exact-duplicate
  rejection, so unique parallel and out-of-order session reads succeed.
- Keeping the strict per-signer floor for session writes, and SDK write
  sequencing that makes legitimate writes from one client never trip it.
- A server-side account metadata cache that is validated against the
  authoritative store on every request.
- Keeping signature verification from stalling request handling during
  bursts.
- More than one canonicalization rebuild at a time.
- Whole-account background jobs that complete within their rotation at
  100,000 accounts.
- Per-route latency percentiles and replay-rejection metrics.
- Routing the database connection directly (without the pinned connection
  proxy) while keeping the connection encrypted and verified.
- A target-shaped benchmark that proves or disproves the acceptance
  criteria.
- Both SDKs (Rust and TypeScript), both storage backends (filesystem and
  Postgres), both transports (HTTP and gRPC).

### Out of Scope

- **Window-only session reads** (RFC "read window" policy). Rejected for
  this feature on confidentiality grounds; may be revisited only through a
  separate, explicitly reviewed security decision.
- Any account-type exemption from replay protection. Public accounts also
  return off-chain proposals and deltas.
- Response encryption. Needs a separate encryption key authenticated by
  the grant; a separate proposal.
- Authenticated streams replacing polling. A separate, later proposal.
- Per-replica replay caches, TTL-only authorization caches, caching of
  account state, and an external cache service.
- Changes to wallet-signed request formats (raw and EIP-712), wallet-signed
  replay protection, session grant format and lifetime, transaction
  approvals, ACK signatures, operator dashboard sessions and EVM sessions.
- Sharding the canonicalization leader across replicas (only if measurements
  after this feature show one leader is the limit).
- Read replicas for authorization decisions.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Parallel session reads never fail replay checks (Priority: P1)

A cosigner's browser or Rust app holds an active session and syncs several
accounts at once: it polls the canonical nonce, fetches state and lists
proposals in parallel, sometimes with identical timestamps and sometimes
arriving out of order through different replicas. Every one of these
requests is a fresh, legitimately signed request, and every one succeeds.

**Why this priority**: This is the hardest acceptance criterion (zero
replay failures) and cannot be met by tuning. It is also the change with
the most security sensitivity, so it must land with its protections intact.

**Independent Test**: Run a client that issues many concurrent session
reads for one account, with equal and reversed timestamps, across at least
two server replicas. Count replay rejections (expect zero). Then resend one
captured request (expect rejection) on the same and on another replica.

**Acceptance Scenarios**:

1. **Given** an active session, **When** the client sends 50 distinct reads
   for one account with the same timestamp in parallel, **Then** all 50
   succeed.
2. **Given** an active session, **When** reads arrive in the reverse of
   signing order on different replicas, **Then** all succeed.
3. **Given** a session read that was accepted, **When** the exact same
   signed request is sent again to any replica within its time window,
   **Then** it is rejected as a replay and returns no account data.
4. **Given** a session read whose signature is re-encoded in its alternate
   valid form (high-S/low-S), **When** it is sent after the original was
   accepted, **Then** it is rejected as a replay.
5. **Given** a session read signed for "get state", **When** the same
   credentials are presented on "list proposals" (identical payload),
   **Then** the request fails authentication.
6. **Given** a session read timestamped outside the time window, **When**
   it arrives, **Then** it fails authentication with a timestamp-specific
   (non-replay) classification.

---

### User Story 2 - Operation-bound session requests (Priority: P1)

Every session-signed per-account request identifies the operation and
transport it was signed for. A request signed for one operation is never
accepted for another, even when the request payloads are byte-identical.

**Why this priority**: It is the prerequisite for any relaxation of the
read replay floor (shown by the design model) and closes a latent
colliding-payload hazard on its own. It is the breaking part of this
feature and must land in a coordinated release.

**Independent Test**: Using shared cross-language test vectors, sign one
payload for each registered operation and each transport, and confirm each
signature verifies only for its own operation and transport, in both SDKs
and on the server.

**Acceptance Scenarios**:

1. **Given** the shared test vectors, **When** the Rust and TypeScript SDKs
   sign the same inputs, **Then** they produce byte-identical messages and
   both verify on the server.
2. **Given** a request signed for HTTP, **When** it is replayed over gRPC
   (or the reverse), **Then** it fails authentication.
3. **Given** an old-format session request on a per-account route after
   cutover, **When** it arrives, **Then** it is rejected with a distinct,
   non-retryable "unsupported auth version" error, the SDK does not clear
   the grant and does not fall back to wallet signing.
4. **Given** a route that is wallet-only today, **When** a version 2
   session request targets it, **Then** it is refused exactly as session
   requests are refused there today.
5. **Given** an existing live grant and stored session key from before the
   upgrade, **When** the user upgrades the SDK, **Then** the same key signs
   version 2 requests without a new wallet prompt.

---

### User Story 3 - Session writes stay strictly replay-protected without spurious failures (Priority: P1)

A cosigner pushes deltas, creates and signs proposals through a session.
Every write is protected by the strict per-signer floor, as today. The SDK
orders that client's writes so a legitimate write never fails replay checks
because of parallelism inside the same client.

**Why this priority**: Writes must keep their strongest protection; the
read change must not weaken them or let reads interfere with them.

**Independent Test**: From one client, issue concurrent writes for one
account and session. Expect zero replay rejections and correct
serialization. Resend any accepted write; expect rejection before any
business effect, on any replica.

**Acceptance Scenarios**:

1. **Given** one client issuing parallel writes for one account and
   session, **When** they are sent, **Then** the SDK orders them with
   strictly increasing timestamps and none is rejected as a replay.
2. **Given** an accepted write, **When** its exact signed request is
   resent to any replica, **Then** it is rejected before any business
   effect.
3. **Given** many reads interleaved with writes, **When** they run, **Then**
   reads neither consume nor depend on the write floor, and writes neither
   consume nor depend on the read duplicate record.
4. **Given** a write that times out ambiguously, **When** the caller
   retries, **Then** the SDK does not silently re-sign and resend it; the
   caller reconciles first.
5. **Given** two independent processes sharing one persisted session key,
   **When** both write concurrently, **Then** occasional replay rejections
   are possible and documented, and are retried within the existing
   bounded retry policy.

---

### User Story 4 - Reads are fast at target load without stale authorization (Priority: P2)

20,000 readers poll their accounts. Reads return within the latency target,
while a removed signer or a revoked session is refused on its very next
request on every replica.

**Why this priority**: Throughput is the remaining gap after replay is
fixed, but it is pure performance and can follow the protocol change.

**Independent Test**: Under sustained read load across several replicas,
remove a signer through one replica and revoke a session through another,
then confirm the next request from each is refused on all replicas, while
read latency stays within target.

**Acceptance Scenarios**:

1. **Given** a signer removed by a committed configuration change on
   replica A, **When** that signer's next request reaches replica B,
   **Then** it is refused as "not a cosigner".
2. **Given** a session revoked or expired, **When** its next request
   reaches any replica, **Then** it is refused with the existing expired or
   revoked classification.
3. **Given** the authoritative store is unavailable during authorization,
   **When** a read arrives, **Then** it fails closed.
4. **Given** a paused account, **When** a reader polls it, **Then** reads
   succeed and mutations are refused, as today.
5. **Given** all replicas restarted at once with empty caches, **When**
   20,000 readers reconnect, **Then** concurrent misses for one account
   trigger one metadata load, not one per request.

---

### User Story 5 - Canonicalization keeps up with 100 concurrent writers (Priority: P2)

100 users transact at once. Each accepted delta becomes canonical within
30 s at p95, bounded below by Miden block inclusion.

**Why this priority**: Required by the target but independent from the
auth changes.

**Independent Test**: Push 100 concurrent candidates on distinct accounts
and measure accepted-to-canonical latency; run the existing candidate-queue
and worker-lease models against the changed concurrency.

**Acceptance Scenarios**:

1. **Given** 100 candidates pending at once on a leader with at least 4
   vCPUs, **When** the worker runs, **Then** more than one rebuild proceeds
   at a time, within a bounded CPU budget, and foreground `push_delta` p95
   stays within 20% of its value without canonicalization load.
2. **Given** a leader with fewer than 4 vCPUs, **When** the worker runs,
   **Then** rebuilds stay at one at a time, and the deployment guide states
   the leader size needed for the canonicalization target.
3. **Given** the concurrency change, **When** the existing candidate-queue
   and worker-lease models run, **Then** they pass without weakening any
   property.

---

### User Story 6 - Operators can measure every acceptance criterion (Priority: P2)

An operator opens the dashboard and sees per-route p50/p95/p99 latency,
replay rejections split by cause, and accepted-to-canonical latency. A
benchmark shaped like the target reports pass or fail for each criterion.

**Why this priority**: Without it, no other story can be shown to work.

**Independent Test**: Run the target-shaped benchmark against a prod-like
stack and compare the dashboard numbers with the benchmark's own
client-side measurements.

**Acceptance Scenarios**:

1. **Given** traffic with known latencies, **When** the operator views the
   dashboard, **Then** per-route percentiles match the server's own
   request-span measurements (same measurement boundary) within 10%.
   Client-side latency is reported separately and is expected to be higher
   (network, load balancer, response transfer).
2. **Given** replay rejections, timestamp-window rejections and invalid
   signatures, **When** they occur, **Then** each is counted under its own
   bounded label, with no account IDs, keys, signatures or payloads in
   labels.
3. **Given** a gRPC request with an empty body, **When** it arrives,
   **Then** it is classified as invalid input, not as a server fault.

---

### User Story 7 - Background jobs complete at 100,000 accounts (Priority: P3)

With 100,000 accounts, the release sweep, dashboard statistics and metric
gauges complete within their schedules without degrading request latency.

**Why this priority**: Not yet failing, but the sweep uses about 5.6 h of
its 6 h rotation at that size.

**Independent Test**: Seed 100,000 accounts and measure each whole-account
job's duration and its effect on concurrent request latency.

**Acceptance Scenarios**:

1. **Given** 100,000 accounts, **When** the release sweep runs, **Then** a
   full walk completes within its rotation and the rate cap leaves at
   least 2x headroom.
2. **Given** 100,000 accounts, **When** dashboard statistics refresh,
   **Then** they finish within their refresh interval and read p95 stays
   within target during the refresh.

---

### Edge Cases

- **Duplicate inside a concurrent race**: the original and a captured copy
  arrive at two replicas at the same instant. Exactly one is accepted.
- **Stalled handler, no purge**: a request passes the initial time check,
  stalls beyond its window, then reaches admission before any purge. It
  must be rejected: freshness is re-checked at admission, after any lock
  or pool wait.
- **Stalled handler across a purge (model finding F2)**: a request passes
  the time check, stalls while its duplicate record is purged, then
  resumes. It must be rejected, both by the admission freshness check and
  by the purge fence. The two properties are modeled separately.
- **Filesystem restart with a future-dated request**: a read timestamped
  up to the allowed skew ahead of server time is accepted, the server
  restarts, and the same grant is registered again. A resend of that read
  must be rejected, because the duplicate record survives the restart.
- **Captured request never delivered by the client**: an attacker who
  holds a signed request the legitimate client never delivered can use it
  once within its window. This is equivalent to winning a race today and
  is accepted.
- **Clock skew**: a client clock off by up to the allowance still succeeds;
  beyond it, requests fail with the timestamp-specific error
  `timestamp_out_of_window`, and the client keeps its session. Session reads
  allow 60 seconds (decision 2026-11-02: a shorter window bounds how long a
  captured read stays usable and how many read records are kept); wallet
  requests and session writes keep 5 minutes.
- **Duplicate-record growth**: records older than the window are purged on
  a schedule; purging never readmits a request (see F2).
- **Duplicate store unavailable**: reads fail closed rather than skipping
  the duplicate check.
- **Mixed fleet during cutover**: v1 and v2 servers behind one endpoint
  would cause intermittent failures; cutover is homogeneous (drained or
  blue/green).
- **Rollback to a v1 server**: the v2 SDK fails explicitly and does not
  downgrade signatures.
- **Legacy nonce backfill on a read**: remains an internal guarded repair
  and is measured separately from replay writes.
- **Metadata version bumped mid-request**: a request authorized before a
  configuration commit may complete; one authorized after it must fail.
- **Cache memory**: a burst of distinct accounts cannot grow the metadata
  cache beyond its configured bound.

## Requirements *(mandatory)*

### Functional Requirements

#### Operation-bound session authentication

- **FR-001**: Every session-signed per-account request MUST be signed over
  a version 2 message that binds a fixed domain, the version, the
  transport, a registered operation ID, the account, the timestamp and the
  request payload, with the exact byte layout defined in the RFC.
- **FR-002**: The server MUST derive the operation from the matched route
  or RPC, never from a client-supplied field.
- **FR-003**: The operation registry MUST assign each session-enabled
  operation a permanent ID and a replay policy (read or write) as listed
  in the RFC, plus the execution operations that accept sessions today
  (execute proposal, get proposal execution, get current execution).
  Unknown IDs and unregistered session routes MUST fail closed. IDs MUST
  NOT be reused.
- **FR-004**: After cutover the server MUST reject the version 1 session
  format on per-account routes with a stable `unsupported_auth_version`
  error (non-retryable). Logout MUST keep its existing dedicated format.
- **FR-005**: Session creation responses (both transports) and the HTTP
  status endpoint MUST advertise the request auth version (version 2 fixes
  the read replay policy and windows, so they are not advertised
  separately). SDKs MUST treat a missing or unknown version as
  unsupported and MUST NOT fall back to version 1 or to wallet signing.
- **FR-006**: Rust and TypeScript SDKs MUST produce byte-identical
  messages for identical inputs, pinned by shared cross-language test
  vectors covering every operation, both transports, and same-payload
  operation pairs.
- **FR-007**: Existing grants and stored session keys MUST keep working
  after an SDK upgrade, without a new wallet prompt or grant migration.

#### Replay protection

- **FR-008**: Session reads MUST be accepted only if the timestamp is
  within 60 seconds of server time (wallet requests and session writes keep
  5 minutes) and the request's signed message digest
  has not been accepted before by any replica for that session.
- **FR-009**: The duplicate record MUST be keyed on the signed message
  digest together with the signer identity, never on signature bytes, so
  alternate encodings of one signature are the same request.
- **FR-010**: Duplicate detection MUST be atomic across all replicas: of
  two identical requests arriving concurrently, exactly one is accepted.
- **FR-011**: Freshness MUST be re-checked at admission, after acquiring
  any lock and after any pool or storage wait, against one authoritative
  clock, so a request delayed beyond its window is rejected whether or not
  any purge has run.
- **FR-011a**: A request whose duplicate record (or write floor) was purged
  MUST never be admitted again (purge fence; closes model finding F2).
  FR-011 and FR-011a are separate properties and MUST be modeled and tested
  separately.
- **FR-012**: Duplicate records MUST be purged only after the window has
  passed for them, and storage for them MUST stay bounded by request rate
  times window.
- **FR-013**: Session writes MUST keep the strict per-account, per-session
  timestamp floor with its existing key; floors MUST NOT be reset at
  cutover. Reads MUST NOT read or advance the write floor, and writes MUST
  NOT read or advance the read duplicate record.
- **FR-014**: Model finding F2 MUST also be closed for writes, or an
  explicit, enforceable execution bound MUST be approved, before this
  feature ships.
- **FR-015**: Both SDKs MUST serialize session writes per account and
  session within one client, holding order across bounded replay retries,
  and MUST NOT serialize reads or unrelated accounts.
- **FR-015a**: Both SDKs MUST mint session timestamps from a strictly
  increasing source per (session, account), shared by every client
  instance using that session, so two otherwise identical reads are never
  byte-identical. The source MUST NOT run more than 1,000 ms ahead of the
  local clock; if it would, the call waits.
- **FR-016**: SDKs MUST NOT automatically re-sign and resend a write after
  an ambiguous timeout.
- **FR-017**: Wallet-signed requests MUST keep today's replay behavior
  unchanged.
- **FR-018**: Filesystem and Postgres backends MUST implement identical
  replay semantics. On both, the duplicate record MUST survive a process
  restart for as long as any recorded request is still within its window.
- **FR-019**: A failure to consult the duplicate record or the floor MUST
  fail closed.

#### Read path

- **FR-020**: The server MAY keep account metadata in a size-bounded
  in-memory cache, but every request MUST first confirm against the
  authoritative primary store that the cached metadata is current and, for
  sessions, that the session is live (not expired, not revoked).
- **FR-021**: The cache MUST hold metadata only, never account state or
  decrypted data.
- **FR-022**: Concurrent cache misses for one account MUST share a single
  load.
- **FR-023**: Every metadata change MUST make all cached copies stale on
  their next validation.
- **FR-024**: Write paths (push gate, conditional updates,
  canonicalization) MUST keep reading authoritative data, never the cache.
- **FR-025**: Authorization errors MUST keep their current distinctions
  (expired, revoked, not a cosigner, network or Guardian mismatch).
- **FR-026**: Pause MUST NOT block reads; no cached pause flag may deny
  reads.
- **FR-027**: Signature verification bursts MUST NOT stall unrelated
  request handling; verification runs with bounded CPU concurrency.
- **FR-028**: Deployments MUST connect to the database directly rather
  than through a connection proxy that pins every connection, while
  keeping TLS with certificate verification.
- **FR-028a**: At the target read rate, p99 wait to acquire a database
  connection MUST stay <= 5 ms on each pool, and p99 wait on the request
  rate limiter MUST stay <= 1 ms. Pool sizes MUST keep the maximum task
  count within 80% of the database's connection limit. If either bound is
  exceeded, fixing it (pool sizing, sharded limiter) is in scope.

#### Canonicalization and background jobs

- **FR-029**: The canonicalization worker MUST run more than one rebuild
  at a time when the host's CPU budget allows it (at least 4 vCPUs), with
  a configurable bound that never exceeds the host-derived CPU limit, and
  without degrading foreground `push_delta` latency by more than 20%. On
  smaller hosts it runs one at a time, and the deployment guide MUST state
  the leader size required for SC-004.
- **FR-030**: The candidate-queue and worker-lease formal models MUST be
  updated as needed and pass for the new concurrency.
- **FR-031**: At 100,000 accounts, the release sweep's rate cap MUST be
  able to walk the fleet at least twice per rotation (2x headroom), and
  the sweep MUST warn when a growing fleet erodes that headroom. A walk is
  spread over the whole rotation by design.
- **FR-032**: Whole-account aggregate queries (dashboard statistics,
  metric gauges) MUST complete within their intervals at 100,000 accounts.

#### Observability and proof

- **FR-033**: The system MUST expose per-route p50, p95 and p99 latency
  for both transports.
- **FR-034**: The system MUST count, per route class, replay rejections,
  timestamp-window rejections, invalid signatures and unsupported auth
  versions separately, with bounded labels only.
- **FR-035**: A gRPC request with an empty body MUST be classified as
  invalid input, not a server fault.
- **FR-036**: The benchmark MUST support 100,000 seeded accounts, 20,000
  reader identities polling once every 2 s (10,000 reads/s), and 100
  writers on 100 distinct accounts, each pushing one delta every 10 s
  (10 pushes/s sustained), plus a burst leg (100 simultaneous pushes) and a
  hot-account leg (5 writers on one account). It MUST include the
  nonce-precheck polling flow, changed-state fetches, multi-replica traffic
  and a sustained run of at least 30 minutes.
- **FR-037**: The benchmark MUST report route percentiles, offered and
  completed rates, first-attempt errors and errors hidden by SDK retries,
  replay rejection count, database reads and writes per request,
  accepted-to-canonical latency and chain-inclusion-to-canonical latency
  as separate measurements, comparing v1 strict, v2 strict, and v2
  with the duplicate record.

#### Release and documentation

- **FR-038**: The version 2 envelope MUST ship as a coordinated breaking
  release of the server and both SDKs, in a later 0.19.0 release candidate
  (after 0.19.0-rc.1). 0.19.0 final MUST NOT be released until the security
  review (FR-041) passes and F2 is closed (FR-011, FR-014).
- **FR-039**: Release notes and `docs/MIDEN_COMPATIBILITY.md` MUST state
  that browser apps on old SDKs lose session requests at cutover until
  upgraded, and that the server fleet must be cut over homogeneously.
- **FR-040**: The protocol spec, session spec, SDK guides and READMEs
  MUST describe the version 2 envelope, the operation registry, the read
  duplicate policy and the new error code.
- **FR-041**: The change MUST pass a security review before release.

### Key Entities

- **Session request envelope (v2)**: The signed message for a session
  request: domain, version, transport, operation ID, and the existing
  account/timestamp/payload word.
- **Operation registry**: Permanent mapping from operation ID to route,
  RPC and replay policy (read or write).
- **Read duplicate record**: Fleet-wide set of accepted session read
  digests per signer, each kept until its window passes.
- **Write floor**: Existing per-account, per-session strictly increasing
  timestamp for writes.
- **Account metadata version**: A value that changes with every metadata
  write, used to validate cached metadata.
- **Metadata cache entry**: Per-replica copy of account metadata and its
  version; never contains state.
- **Server auth policy advertisement**: The session request version,
  returned at session creation and in status. Version 2 implies the read
  policy and windows above, so nothing else is advertised.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Zero replay rejections of valid, unique, in-window session
  requests during the target-shaped benchmark (first attempt, not hidden
  by retries).
- **SC-002**: 100% of resent captured requests (reads and writes, either
  signature encoding, any replica) are rejected, and 100% of requests
  presented on a different operation or transport fail.
- **SC-003**: Two legs, each with p95 <= 1 s at a sustained rate of at
  least 8,000 requests/s (design target 10,000/s, one poll every 2 s from
  20,000 readers): (a) the realistic mix, nonce poll with full state fetched
  only on change; (b) full `get_state` on every poll. The benchmark also
  reports the highest rate at which each leg stays <= 1 s.
- **SC-004**: Accepted-to-canonical latency p95 <= 30 s under the FR-036
  writer load (end to end, as #317 states), on a canonicalization leader of
  the size documented under FR-029 (at least 4 vCPUs if one rebuild at a
  time is not enough). Chain-inclusion-to-canonical
  p95 is reported separately, so a miss can be attributed to Guardian or to
  block inclusion.
- **SC-005**: A removed signer or revoked session is refused on its next
  request on every replica in 100% of tests.
- **SC-006**: Database round trips per session read drop from four (3
  reads + 1 contended write) to two: one combined authorization and
  duplicate-record call, and one state or state-head read.
- **SC-007**: Every whole-account job completes within its schedule at
  100,000 accounts.
- **SC-008**: Every acceptance number above can be read from the
  dashboard and the benchmark report, with dashboard percentiles within
  10% of the server's request-span measurements for the same routes and
  interval.
- **SC-009**: All existing formal models plus the updated auth-replay
  model pass at their deep tier, with F2 closed.

## Assumptions

- Target readers use sessions. Wallet-signed readers keep today's strict
  floor and are not covered by the zero-failures criterion.
- The existing timestamp allowance (±5 min) stays. A shorter window would
  shrink duplicate-record storage and copy lifetime, but needs SDK clock
  correction; it is a possible follow-up.
- The duplicate record replaces a contended update with an insert; it does
  not remove the write per read (at 10,000 reads/s, about 864 million
  records per day, with a bounded live set). Whether this is affordable is
  an **early gate**: a focused replay-store experiment (sustained inserts,
  partition rollover, multiple replicas) must pass before protocol and SDK
  work commits to this design. If it fails, the next step is a separate
  decision, never silently adopting window-only reads.
- Running prod as two smaller instances (for availability) and keeping
  storage encryption and KMS signing are operational choices outside this
  specification.
- Miden block inclusion and Miden RPC capacity are outside Guardian's
  control and bound canonicalization latency.

## Dependencies

- Security review of the v2 envelope, operation registry and duplicate
  record design (RFC decision 2).
- Updated `verify-auth-replay` model with separate read and write
  policies and F2 closed (RFC decision 3).
- Prod and devnet security groups allowing direct database access.
- Phases 1 and 3 of the plan (measurement, read-path cache, background
  jobs) do not depend on the v2 envelope and can ship in any release, even
  if the 0.19.0 gates slip.
- Coordinated SDK releases and browser application upgrades, landing in a
  0.19.0 release candidate after rc.1; 0.19.0 final waits for this feature's
  security gates.

## Clarifications

### Session 2026-10-30

- Q: Which release carries the breaking v2 session envelope? → A: A later
  0.19.0 release candidate; 0.19.0 final waits for the security review and
  F2 closure.
- Q: Reader poll interval? → A: Every 2 s (10,000 reads/s) as the design
  target; slightly lower is acceptable, set as a pass floor of 8,000
  reads/s (every 2.5 s).
