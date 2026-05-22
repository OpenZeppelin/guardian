# Feature Specification: Operator-Initiated Per-Account Pause

**Feature Key**: `001-account-pausing`
**Suggested Branch**: `001-account-pausing` (or continue on current `account-pausing`)
**Created**: 2026-05-19
**Status**: Draft
**Input**: User description: "Operator-initiated per-account pause — server-side kill switch that rejects state-mutating actions for a paused account and returns `GUARDIAN_ACCOUNT_PAUSED`, with operator-authenticated pause/unpause endpoints, persisted `paused_at` / `paused_reason`, and `admin_actions` audit trail. Implemented as a self-contained flag at a single chokepoint; the chokepoint is designed so it can be transparently swapped for the future `PolicyEngine` (#182) without changing the external contract."

**Related**:
- Implements [#181](https://github.com/OpenZeppelin/guardian/issues/181) (Operator-Initiated Account Pause).
- Builds on [`006-operator-authz`](../006-operator-authz/spec.md) — consumes the existing `Permission::AccountsPause` (`"accounts:pause"`) gate and the `admin_actions` audit writer.
- Soft prerequisite for [#179](https://github.com/OpenZeppelin/guardian/issues/179) (Guardian error model) — introduces one new code, `GUARDIAN_ACCOUNT_PAUSED`, in that family.
- **Forward dependency**: [#182](https://github.com/OpenZeppelin/guardian/issues/182) (Policy Evaluation). This feature ships the pause chokepoint as a self-contained helper, not as a `Policy` impl. Once #182 lands, the chokepoint is refactored into a built-in `AccountPaused` policy without changing this feature's external contract (pause/unpause API, persisted fields, error code, audit shape).

## Context

Guardian's multisig server today exposes three state-mutating service
entry points — `services::push_delta` (apply a committed delta),
`services::push_delta_proposal` (submit a new proposal), and
`services::sign_delta_proposal` (operator cosign) — reached from both
the gRPC surface (`crates/server/src/api/grpc.rs`) and the HTTP surface
(`crates/server/src/api/http.rs`). None of these paths consult any
per-account operational gate; once an account is configured and its
signer set is satisfied, mutation proceeds unconditionally.

Operators today have **no in-band way to stop activity on a single
account**. If a key is suspected compromised, a misbehaving cosigner
is observed, or a recipient address is later flagged, the only options
are global (take the server offline) or contractual (instruct
cosigners offline). Both are coarse and neither leaves a forensic
trail that ties the action to the operator who took it.

Account-level metadata already persists in
`crates/server/migrations/2026-03-12-000002_account_metadata` — the
`account_metadata` table keyed by `account_id VARCHAR(128)` with
auth + network_config JSONB plus timestamps and pending-candidate
state. Adding pause fields here keeps the durable surface contained.
The operator authz foundation ([`006-operator-authz`](../006-operator-authz/spec.md))
has already landed a permission enum that includes
`Permission::AccountsPause` (`accounts:pause`, in
`crates/server/src/dashboard/permissions.rs`), an enforcement
middleware that rejects unauthorized routes with
`GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`, and an append-only
`admin_actions` table (`2026-05-16-000001_admin_actions` migration)
written from the same enforcement layer. The pause endpoints can
therefore be added as routes under the existing operator surface
without inventing new authz or audit machinery.

What is missing is the runtime gate itself: a single decision point
on every mutating path that says "this account is paused — reject"
with a stable error code, a persisted reason and timestamp the
dashboard can show, and an idempotent operator API to flip the state
either direction.

The architecture document attached to this feature (§2.4) frames
pause as a built-in policy on a future `PolicyEngine` (§2.5). That
framing is correct in the long run, but the engine itself is a
separate, larger feature (#182). To unblock #181 without taking on
the engine's scope, this feature ships pause as a **self-contained
flag with a single chokepoint helper** whose external contract is
identical to the policy-engine implementation. When #182 lands, the
chokepoint is refactored from `ensure_account_active(account_id)`
to `policy_engine.evaluate_all(..)` internally; clients, audit
records, and persisted fields do not change.

## Goals

1. Give operators a **per-account kill switch**: any mutating action
   on a paused account is rejected at the server with a stable,
   machine-readable error code and a human-readable reason.
2. Make pause/unpause **operator-authenticated, permissioned, and
   audited** — same surface as the rest of `006-operator-authz`,
   no parallel auth path.
3. Make pause state **durable and observable** — survives restart,
   is exposed on the existing account-detail read endpoint, and is
   not derived from logs.
4. Keep the **read surface unaffected** — dashboards and SDK
   consumers can continue to fetch state, deltas, and proposals on
   a paused account so the operator can investigate.
5. Ship a **drop-in seam** for the future `PolicyEngine` (#182): the
   chokepoint is a single internal helper that can be replaced
   without API or storage migration.

## Non-Goals

- **System-wide pause / global kill switch.** Out of scope; deferred
  with the rest of the system-policy layer (architecture doc §2.5,
  "no operator-facing system-pause control in M3").
- **The `PolicyEngine` itself, the `Policy` trait, the
  `AllowedRecipients` policy, or any runtime policy CRUD endpoints.**
  All deferred to #182.
- **On-chain or protocol-level pause.** Pause is a Guardian
  operational control, not a multisig-account primitive. Already-
  submitted proofs/txs in flight at the sequencer are not rolled
  back.
- **Operator self-service revocation of compromised keys.** Pause
  blocks new mutating work on the affected account; it does not
  rotate or revoke keys. Key-rotation is a separate workflow.
- **Bulk pause** (pause N accounts in one call). Each pause is a
  single per-account action; bulk operations can be added later if
  needed.
- **Gating account configuration and auth reconfiguration.**
  Pause covers the per-account mutating *proposal/delta/signature
  pipeline* (FR-008). Admin/setup paths
  (`services::configure_account`,
  `evm::service::register_account`) are NOT gated by pause:
  recovery from a suspected key compromise commonly requires
  rotating the auth config, and forcing an unpause first would
  invert that flow. A future feature can revisit gating these
  paths separately if operators ask for it.
- **Time-bounded auto-unpause.** Pause is sticky until an operator
  with `accounts:pause` explicitly unpauses. No TTL.
- **Dashboard UI changes.** The TypeScript operator client gains
  the pause/unpause/status methods; rendering those in the dashboard
  is a separate UI task tracked elsewhere.

## User Scenarios & Testing *(mandatory)*

### User Story 1 — Pause an account during incident response (Priority: P1)

A security on-call operator observes that a specific account is
behaving anomalously (e.g., a cosigner key may be compromised, a
recipient address appears on a fraud feed). They need to stop new
state-mutating work on that account immediately, with their
identity recorded, while they investigate.

**Why this priority**: This is the entire reason the feature
exists. Pause without operator-driven invocation is meaningless.
Ships the API + chokepoint + audit row that makes the kill switch
real. Everything else is read-side polish.

**Independent Test**: Stand up Guardian with one account active.
Authenticate an operator that holds `accounts:pause`. POST to the
pause endpoint with a reason. Attempt any mutating action against
that account — it is rejected with `GUARDIAN_ACCOUNT_PAUSED`.
Inspect `admin_actions` — there is a row with the operator's
identity, the route, the account ID, and the reason. No other
operator action was required.

**Acceptance Scenarios**:

1. **Given** an active account `acct_X` and an authenticated
   operator session that holds `accounts:pause`, **When** the
   operator POSTs `/dashboard/accounts/acct_X/pause` with
   `{ "reason": "suspected cosigner compromise" }`, **Then** the
   server returns 200, the account's stored
   `paused_at` is set to a UTC timestamp not before the request
   was received, and `paused_reason` is the supplied string.
2. **Given** the account is paused, **When** any client (gRPC or
   HTTP, multisig or operator) calls `push_delta`,
   `push_delta_proposal`, or `sign_delta_proposal` against
   `acct_X`, **Then** the server rejects the call with the
   `GUARDIAN_ACCOUNT_PAUSED` error code, the response includes
   `paused_at` and `paused_reason` in `details`, and no state
   change is persisted.
3. **Given** the operator that issued the pause holds
   `accounts:pause`, **When** the pause response is returned,
   **Then** an `admin_actions` row exists recording the
   operator's identity, the route `POST /dashboard/accounts/{account_id}/pause`,
   the account ID, the reason, and the request timestamp.
4. **Given** an authenticated operator session that does **not**
   hold `accounts:pause`, **When** that operator attempts to pause
   any account, **Then** the existing operator-authz middleware
   rejects the request with `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`
   *before* the pause handler runs, and no `admin_actions` pause
   row is written.

---

### User Story 2 — Unpause once the incident is resolved (Priority: P1)

After investigation concludes that the account is safe, an operator
with `accounts:pause` needs to restore mutating activity, with the
same audit guarantees.

**Why this priority**: Pause without unpause is a one-way door.
Co-equal P1 with US1: shipping pause without a way out is not
acceptable for production operations.

**Independent Test**: Continuing from US1, POST the unpause
endpoint. The mutating call that was previously rejected now
succeeds. Inspect `admin_actions` — a second row records the
unpause with the operator's identity and reason.

**Acceptance Scenarios**:

1. **Given** `acct_X` is paused and the calling operator holds
   `accounts:pause`, **When** the operator POSTs
   `/dashboard/accounts/acct_X/unpause` with
   `{ "reason": "investigation closed, no compromise" }`,
   **Then** the server returns 200, `paused_at` is cleared
   (NULL), `paused_reason` is cleared (NULL), and the account is
   active.
2. **Given** the account is now active, **When** a previously
   blocked mutating call is retried, **Then** the server accepts
   it on the normal path, with no residual pause-related state.
3. **Given** the unpause succeeded, **When** `admin_actions` is
   inspected, **Then** a row exists with route
   `POST /dashboard/accounts/{account_id}/unpause`, operator identity,
   account ID, the unpause reason, and the timestamp.

---

### User Story 3 — Operator and dashboard can see pause state (Priority: P2)

Operators (and the dashboard UI in a follow-up) need to read pause
status without parsing logs. The existing account-detail read
endpoint should surface `paused_at` and `paused_reason` so the
operator can confirm the state and the reason it was paused.

**Why this priority**: P2 because the writers (US1, US2) carry the
critical security capability; the read endpoint is a usability and
correctness affordance — without it, operators have no
in-band way to confirm pause state took effect.

**Independent Test**: After pausing `acct_X` per US1, call the
existing operator account-detail read endpoint. The response body
includes `paused_at` (RFC 3339 UTC timestamp) and `paused_reason`
(non-empty string matching the supplied reason; never null while
the account is paused via this feature's API — see FR-007). After
US2 unpauses, both fields are null.

**Acceptance Scenarios**:

1. **Given** `acct_X` is paused, **When** an operator with
   `dashboard:read` calls `GET /dashboard/accounts/acct_X`,
   **Then** the response contains `paused_at: "<RFC 3339 UTC>"`
   and `paused_reason: "<non-empty string matching the stored
   reason>"`. (Because pause requires a `reason` (FR-007), a
   paused account reached through this feature's API always has
   a non-null `paused_reason`.)
2. **Given** `acct_X` is active, **When** the same GET is made,
   **Then** `paused_at` and `paused_reason` are both `null` (or
   omitted in a way the client treats as equivalent to null,
   chosen consistently across the response schema).
3. **Given** a typed operator client (`@openzeppelin/guardian-operator-client`),
   **When** it deserializes the account-detail response, **Then**
   it surfaces the two new fields with the same nullable
   semantics as the server.

---

### Edge Cases

- **Pause of an already-paused account.** Idempotent and
  observable: see FR-013. The server returns success and the
  caller can inspect whether the timestamp/reason updated.
- **Unpause of an already-active account.** Idempotent no-op; see
  FR-014. Audit still records the operator action.
- **Pause of a non-existent `account_id`.** Reject with the
  existing "account not found" error before any pause state is
  touched or audited.
- **Pause issued while a mutating request is mid-flight.** Two
  concurrent operations: the in-flight delta either completes
  (state was already mutating before the pause check fired) or
  fails the chokepoint (the pause flip won the race). Outcome
  is deterministic but order-dependent on transaction
  serialization; no half-applied state.
- **Audit-writer failure during pause/unpause.** Pause state
  must still flip — durability of pause is the security
  invariant. Audit-writer failures fall back to the structured
  log path already established by `006-operator-authz` (the
  existing `Auditor` trait's `LogAuditor` fallback; see this
  spec's FR-021) with a loud warning. Pause is **not** rolled
  back on audit failure.
- **Pause request with a `reason` containing user-controlled
  text** (newlines, control chars, very large payload). Server
  validates length (see FR-007); rendering is up to the consumer
  but the stored string is escaped/safe-by-construction.
- **Server restart while paused.** Pause survives — `paused_at`
  and `paused_reason` are durable columns. On warm boot the
  chokepoint reads them as part of normal account-metadata load.
- **Pause/unpause endpoint called without an authenticated
  session.** Rejected by the existing session middleware before
  authz runs; no audit row, no pause flip.
- **Operator without `accounts:pause` calls pause.** Rejected
  with `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` by the existing
  authz middleware; pause state is untouched.
- **gRPC mutating call vs HTTP mutating call.** Both share the
  same `services::*` chokepoint — pause is enforced once,
  centrally; surface choice does not change the answer.

## Requirements *(mandatory)*

### Functional Requirements — API surface

- **FR-001**: The server MUST expose an operator endpoint
  `POST /dashboard/accounts/{account_id}/pause` that accepts
  a **required** `{ "reason": string }` body (see FR-007 for
  validation rules) and transitions the account into the paused
  state.
- **FR-002**: The server MUST expose an operator endpoint
  `POST /dashboard/accounts/{account_id}/unpause` that accepts
  an **optional** `{ "reason": string }` body (see FR-007 for
  validation rules) and transitions the account out of the
  paused state.
- **FR-003**: Both pause and unpause endpoints MUST be gated by
  `Permission::AccountsPause` (`"accounts:pause"`) via the
  existing authz middleware. The authz layer's existing failure
  mode (`GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`) applies
  unchanged.
- **FR-004**: Both endpoints MUST require an authenticated
  operator session (existing session middleware); pause is not
  reachable from unauthenticated callers.
- **FR-005**: The existing operator account-detail read endpoint
  MUST include `paused_at` (nullable RFC 3339 UTC string) and
  `paused_reason` (nullable string) in the response body for
  every account, paused or not.
- **FR-006**: Read endpoints (account detail, dashboard feeds,
  state, deltas, proposals — both gRPC and HTTP, both operator
  and multisig surfaces) MUST continue to work unchanged for
  paused accounts. Pause MUST NOT add a read-side gate.
- **FR-007**: The server MUST validate the `reason` field as
  follows: on **pause**, `reason` is **required** and MUST be a
  non-empty string of ≤ 512 UTF-8 characters; missing, empty,
  or oversized values are rejected with HTTP 400 / gRPC
  `INVALID_ARGUMENT` before any pause flip. On **unpause**,
  `reason` is **optional** but if supplied MUST also satisfy
  the ≤ 512 UTF-8 character cap. (Rationale: pause is the
  security-significant transition that should always carry
  forensic context; unpause is a recovery action where context
  is helpful but not required.)

### Functional Requirements — Enforcement chokepoint

- **FR-008**: Pause scope is the per-account mutating
  proposal / delta / signature pipeline only. Read endpoints
  (FR-006) and admin/setup writers (`configure_account`,
  `register_account`; see Non-Goals) are explicitly NOT
  gated. Concretely, the server's per-account mutating
  pipeline MUST consult a single account-status check before
  performing any state mutation or before persisting any side
  effect (proposal record, signed artifact, durable delta).
  Every one of the following service entry points MUST call
  the chokepoint helper (FR-012) as its first non-validation
  step:
  - **Multisig pipeline**: `services::push_delta`,
    `services::push_delta_proposal`,
    `services::sign_delta_proposal`.
  - **EVM pipeline (feature-gated)**: `evm::service::create_proposal`,
    `evm::service::approve_proposal`,
    `evm::service::cancel_proposal`. When the EVM Cargo feature
    is disabled these entry points do not compile and the
    requirement is trivially satisfied. When enabled, a paused
    account rejects EVM proposal creation, approval, and
    cancellation with the same `GUARDIAN_ACCOUNT_PAUSED`
    contract.
  Admin/setup paths (`services::configure_account`,
  `evm::service::register_account`) are **out of scope** for
  this feature and are listed under Non-Goals; pause does not
  block initial account configuration or auth reconfiguration.
  See spec rationale in Non-Goals.
- **FR-009**: The account-status check MUST return a deny
  outcome for an account whose `paused_at` is non-null,
  carrying the persisted `paused_at` and `paused_reason`.
- **FR-010**: On a deny outcome, the mutating path MUST surface a
  new error `GUARDIAN_ACCOUNT_PAUSED` to the caller, with
  response details including `paused_at` and `paused_reason`.
- **FR-011**: `GUARDIAN_ACCOUNT_PAUSED` MUST be surfaced
  consistently across gRPC and HTTP — same code string, same
  details schema. HTTP transport status is **409 Conflict**
  (account is in a state that does not permit the requested
  mutation, semantically distinct from
  `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`'s 403). gRPC
  status code is `FAILED_PRECONDITION`.
- **FR-012**: The account-status check MUST be implemented as a
  single internal helper (working name `ensure_account_active`)
  exposed as the only call site, so future replacement by
  `PolicyEngine::evaluate_all` (#182) is a strictly internal
  refactor.

### Functional Requirements — State semantics

- **FR-013**: A pause request against an already-paused account
  MUST return success and MUST NOT change the original
  `paused_at` timestamp. The `paused_reason` field is left
  unchanged. (Rationale: the original timestamp/reason carries
  forensic value; re-pausing must not overwrite it. Operators
  who want to amend the reason can unpause + re-pause and the
  audit trail captures both transitions.)
- **FR-014**: An unpause request against an already-active
  account MUST return success as a no-op (no state change). The
  call MUST still write an `admin_actions` row, so attempts are
  attributable.
- **FR-015**: Pause MUST persist across server restarts.
  `paused_at` and `paused_reason` are durable account-metadata
  columns; no in-memory-only state.
- **FR-016**: Pause MUST NOT roll back, cancel, or otherwise
  alter mutations that have already been accepted and persisted
  before the pause flip. Pause prevents new entries into the
  mutating pipeline; it is not a transactional undo.
- **FR-017**: Pause state MUST be readable consistently — once
  the pause endpoint has returned success, subsequent mutating
  calls and reads MUST observe the paused state without
  user-visible eventual-consistency lag.

### Functional Requirements — Audit

- **FR-018**: Every successful pause and unpause transition MUST
  write a row to the existing `admin_actions` table with at
  minimum: operator identity (commitment + optional human
  identifier), route path, HTTP method, account ID,
  `before_state` ("active" / "paused"), `after_state`, supplied
  `reason` (nullable), and request-received timestamp.
- **FR-019**: Idempotent pause/unpause (FR-013, FR-014) MUST
  still write an `admin_actions` row, with `before_state ==
  after_state`, so attempts are auditable even when they did
  not change state.
- **FR-020**: Failed pause/unpause requests that pass session
  auth but fail at the authz layer MUST be audited per the
  existing `006-operator-authz` rejection semantics — the
  authz middleware writes a row with kind `auth.denied`
  carrying the route, HTTP method, and `required_permissions`.
  This feature MUST NOT write an `accounts.pause` or
  `accounts.unpause` row for requests rejected at authz
  (consistent with US1 scenario 4: the pause handler never
  runs, so no pause-transition row is emitted). No new audit
  shape is introduced for pause-specific rejections.
- **FR-021**: An audit-writer failure MUST NOT roll back a
  successful pause flip. The fallback structured-log path
  established by `006-operator-authz` MUST be used so the
  transition remains observable even when Postgres is
  unavailable.

### Functional Requirements — Client

- **FR-022**: `@openzeppelin/guardian-operator-client` MUST gain
  typed methods for pause and unpause, matching the server
  contract (account ID, optional reason, server response
  including `paused_at`/`paused_reason`/error details).
- **FR-023**: The account-detail TypeScript type returned by the
  operator client MUST expose `pausedAt` and `pausedReason` as
  optional/nullable fields, matching the server schema.
- **FR-024**: The operator client MUST surface
  `GUARDIAN_ACCOUNT_PAUSED` as a typed error so dashboard code
  can branch on it without string-matching the code field.

### Functional Requirements — Forward compatibility

- **FR-025**: The chokepoint helper MUST be the only call site
  that interrogates pause state; mutating handlers MUST NOT
  read `paused_at` directly. (Rationale: when #182 lands, this
  module is replaced wholesale by `PolicyEngine::evaluate_all`
  — scattering pause checks across handlers would block that
  refactor.)
- **FR-026**: The error code `GUARDIAN_ACCOUNT_PAUSED` and the
  shape of its `details` (carrying `paused_at`, `paused_reason`)
  MUST be picked so the future `Policy::evaluate` reject
  variant `Reject { code: GUARDIAN_ACCOUNT_PAUSED, reason }` can
  emit the same wire payload without a client-visible change.

### Key Entities

- **Account pause state**: per-account record carrying
  `paused_at` (nullable UTC timestamp; non-null means paused)
  and `paused_reason` (nullable string). Lives on the same
  account-metadata row as `account_id`, `auth`,
  `network_config`. No new primary table is required.
- **Admin-action audit row** (existing): each pause/unpause
  transition writes one row to the existing `admin_actions`
  table. No new columns are required; pause uses existing
  fields (route, method, operator identity, account ID,
  before/after state, reason, timestamps).
- **Pause-error response payload**: structured `details` block
  carried by `GUARDIAN_ACCOUNT_PAUSED` responses, with
  `paused_at` and `paused_reason`. Same shape across gRPC and
  HTTP. Same shape future `PolicyEngine`-generated rejects will
  emit.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: An on-call operator with `accounts:pause` can
  pause an account end-to-end (form intent → API call →
  observed enforcement on the next mutating attempt) in under
  60 seconds of wall time, without consulting documentation
  beyond the endpoint URL.
- **SC-002**: 100% of mutating proposal/delta/signature
  pipeline actions against a paused account are rejected with
  `GUARDIAN_ACCOUNT_PAUSED`, measured by an end-to-end test
  that exercises every entry point named in FR-008 — multisig
  (`push_delta`, `push_delta_proposal`,
  `sign_delta_proposal`) over both gRPC and HTTP, and
  feature-gated EVM (`create_proposal`, `approve_proposal`,
  `cancel_proposal`) over HTTP — and zero state changes are
  persisted on a rejected call.
- **SC-003**: For every successful pause or unpause transition
  observed in `admin_actions`, the row carries operator
  identity, account ID, route, before/after state, reason, and
  timestamp — verifiable from the DB without joining logs.
  Coverage target: 100% of transitions in integration tests
  and 100% of transitions in production audit (validated by
  reconciliation between API success responses and audit
  rows).
- **SC-004**: An operator without `accounts:pause` who attempts
  to pause receives `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`
  with the missing permission set named, and no pause state
  changes — verified by an integration test.
- **SC-005**: After a server restart, pause state survives:
  an account paused before the restart remains paused
  afterward without operator intervention, and the next
  mutating attempt is still rejected with
  `GUARDIAN_ACCOUNT_PAUSED` carrying the original
  `paused_at` and `paused_reason`.
- **SC-006**: Read endpoints exercised against a paused account
  (account detail, dashboard feeds, state/delta/proposal
  fetches over gRPC and HTTP) succeed in 100% of cases in
  integration tests. The chokepoint helper MUST NOT be invoked
  on any read path (FR-006); verified structurally by a code-
  review checklist plus a unit test that asserts the helper is
  not called by any handler whose route is registered as `GET`
  on the dashboard router.
- **SC-007**: When `PolicyEngine` (#182) lands, swapping the
  chokepoint to a built-in policy is a localized change to the
  helper module only — zero changes to mutating handlers,
  audit shape, error code, response details, persisted
  fields, or operator-client types. Verified by review at the
  time of #182's merge.

## Assumptions

- The persisted-pause representation extends the existing
  `account_metadata` table with two nullable columns
  (`paused_at TIMESTAMPTZ`, `paused_reason TEXT`). Adding a
  separate `account_pause` table is rejected because (a) pause
  is a per-account property naturally co-located with
  configuration, and (b) the chokepoint must be a cheap read
  on the hot path — colocating avoids an extra join.
- The maximum stored length of `paused_reason` is 512 UTF-8
  characters (FR-007). Storage uses `TEXT` for flexibility,
  with length validation at the handler. `reason` is required
  on pause and optional on unpause.
- Pause idempotency follows "first writer wins for the
  forensic timestamp" (FR-013). The architecture document is
  silent on this; the rationale is in FR-013.
- The HTTP transport status for `GUARDIAN_ACCOUNT_PAUSED` is
  409 Conflict (FR-011), distinguishing pause (a resource-
  state condition) from authz failures (403). gRPC is
  `FAILED_PRECONDITION`.
- The operator-client (TypeScript) update ships in the same
  release as the server; the existing release-coordination
  workflow (`release-guardian-sdk-packages` skill) handles
  version alignment.
- Multisig gRPC mutating callers (the multisig SDKs) reach
  the same `services::push_delta` / `push_delta_proposal` /
  `sign_delta_proposal` entry points the HTTP surface uses, so
  the chokepoint catches them without per-SDK changes. EVM
  mutating callers (the EVM client, HTTP-only today) reach
  `evm::service::create_proposal` / `approve_proposal` /
  `cancel_proposal`, which each get their own chokepoint call
  under `#[cfg(feature = "evm")]` per FR-008. No SDK
  protocol-level field is added for pause.
- The future `PolicyEngine` will surface
  `PolicyDecision::Reject { code, reason }` with the same
  payload shape Guardian errors already use, so the
  `details` block this feature defines is the long-term
  contract for any reject reason.

## Dependencies

- **Hard prerequisite (already landed)**: `006-operator-authz`
  — provides `Permission::AccountsPause`, the authz middleware
  that emits `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`, and
  the `admin_actions` audit writer with Postgres + structured-
  log fallback.
- **Hard prerequisite (already landed)**: the operator session
  middleware (`002-operator-auth`) — every pause/unpause call
  flows through session auth before authz.
- **Soft prerequisite**: [#179](https://github.com/OpenZeppelin/guardian/issues/179)
  (Guardian error model) — this feature introduces one new
  stable code (`GUARDIAN_ACCOUNT_PAUSED`); when #179 lands the
  code is registered in the central catalog without semantic
  change.
- **Migration**: one new schema migration adds
  `paused_at TIMESTAMPTZ NULL` and `paused_reason TEXT NULL`
  to `account_metadata`. Backfill is trivial (NULL = active);
  no data migration needed.
- **Operator-client update**: typed methods + types land in
  `@openzeppelin/guardian-operator-client` and are surfaced
  through whatever HTTP client wrapper the dashboard uses.

## Out of Scope

- Global / system-wide pause.
- The `PolicyEngine`, `Policy` trait, `AllowedRecipients`
  policy, and any runtime policy CRUD endpoints (#182).
- Bulk pause / multi-account pause endpoints.
- Auto-expiring pause (TTL-based unpause).
- Dashboard UI changes — the operator-client gains the methods,
  but rendering pause status and pause/unpause controls in the
  dashboard is a separate UI task.
- On-chain pause primitives.
- Key rotation / compromised-cosigner recovery flows.

## Design decisions captured in this spec

The user-resolved scope questions for this feature, with their
chosen options:

| Question | Choice | Where it shows up |
|----------|--------|-------------------|
| Pause architecture | Self-contained flag with explicit policy-engine seam | FR-012, FR-025, FR-026, Goals #5 |
| System-wide pause | Per-account only; no global pause in this feature | Non-Goals, Out of Scope |
| Permission gate | Existing `accounts:pause`, same gate for pause and unpause | FR-003 |

## Resolved clarifications

The two open spec-phase clarifications were resolved with the
operator before plan-phase begins:

| # | Topic | FR | Resolution |
|---|-------|----|------------|
| C1 | `reason` field | FR-007 | Required on pause, optional on unpause, both capped at ≤ 512 UTF-8 characters. Pause without `reason` is rejected with 400 / `INVALID_ARGUMENT`. |
| C2 | HTTP transport status | FR-011 | `GUARDIAN_ACCOUNT_PAUSED` → HTTP **409 Conflict**, gRPC `FAILED_PRECONDITION`. Distinct from authz failures (403). |
