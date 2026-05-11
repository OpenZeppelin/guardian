# Feature Specification: Operator Dashboard Metrics — Pagination, Info, and Activity

**Feature Key**: `005-operator-dashboard-metrics`
**Suggested Branch**: `005-operator-dashboard-metrics` (manual creation optional)
**Created**: 2026-05-07
**Status**: Draft
**Input**: User description: "Operator dashboard read-only metrics: account list pagination with cursor and account_id search, dashboard info endpoint with health and account/delta counts, and a per-account activity endpoint exposing candidate/canonical/discarded status and pending candidate details, all behind operator session auth." (Note: the standalone account-id lookup has since been dropped from scope — the existing `GET /dashboard/accounts/{account_id}` detail endpoint already serves this need; see Out of Scope.)

## Context

Guardian already exposes a minimal read-only operator dashboard surface
(`002-operator-auth`, `003-operator-account-apis`, `004-operator-http-client`):
operators can log in with a challenge/sign flow, list every configured
account, and inspect one account's current summary.

That surface is enough for a single-screen MVP, but it cannot back even a
basic operations dashboard once an operator manages more than a few hundred
accounts or wants to answer questions like "is the service healthy", "how
many accounts have pending candidates right now", or "what activity has
happened on this account in the last week". The list endpoint returns every
account in one response with no cursor; there is no aggregate health/inventory
endpoint; and per-account history (deltas / proposed state changes) is not
exposed at all on the dashboard surface.

This feature adds the next read-only data slice on the existing operator
dashboard surface: cursor-based pagination on the account list, a new
dashboard info endpoint that summarizes Guardian status and inventory, two
new per-account history endpoints — one for deltas (the recorded state-change
lifecycle) and one for proposals (the in-flight multisig signature-collection
lifecycle) — and two cross-account feeds that aggregate the same data
globally for the dashboard landing page. All new behavior remains
operator-scoped, read-only, derived from existing persisted records, and
backed by the operator session model already in place.

The change affects the dashboard HTTP surface only. It must reuse the
operator session model defined in `002-operator-auth`, preserve the
existing multisig protocol and per-account authenticated APIs, and
behave consistently across supported metadata/state/delta backends.

**v1 is Miden-oriented.** A Guardian instance is gated to one network
family in practice: the default build serves Miden accounts, and EVM
support sits behind the `evm` server feature flag with its own
separate proposal storage path. v1 of this feature targets the Miden
deployment pattern: the per-account proposal queue and the global
in-flight proposal feed surface only the Miden multisig signing
lifecycle (`DeltaStatus::Pending` rows in `delta_proposals`), and EVM
accounts return empty results on those endpoints. EVM-specific
dashboard surfaces (e.g. EVM proposal queues) are deferred to a
follow-up feature paired with the EVM-specific dashboard work.

## Scope *(mandatory)*

### In Scope

- Add cursor-based pagination as the **only** mode for the dashboard account
  list endpoint. This is a breaking change relative to
  `003-operator-account-apis`: the existing internal dashboard consumer is
  updated as part of this feature to use the paged contract; no
  full-inventory fallback mode is preserved.
- Add a new authenticated dashboard endpoint that returns service-level
  inventory and lifecycle counts (total accounts, deltas grouped by
  lifecycle status, count of in-flight proposals) plus a Guardian
  self-status summary (service status indicator, deployment environment
  identifier, latest activity timestamp). The response does not
  enumerate networks; v1 is Miden-oriented (see Context).
- Add a new authenticated dashboard endpoint that returns the per-account
  delta history, exposing each persisted delta record with its
  per-account `nonce`, lifecycle status (`candidate`, `canonical`,
  `discarded`), status timestamp, prior and resulting state
  commitments, and the relevant per-status detail (`retry_count` on
  candidate entries).
- Add a new authenticated dashboard endpoint that returns the per-account
  in-flight proposal queue — the multisig proposals still collecting cosigner
  signatures (corresponding to `DeltaStatus::Pending` rows in the
  `delta_proposals` storage) — exposing the proposer identifier, signatures
  collected vs. required, and originating timestamp so operators can triage
  stuck signing.
- Add two new authenticated dashboard endpoints that return cross-account
  feeds — one global delta feed (paginated, optionally filtered by lifecycle
  status) and one global in-flight proposal feed — so the dashboard landing
  page can surface "what's happening across the whole Guardian right now"
  without fanning out per-account requests. Treated as the lowest-priority
  slice of this feature.
- Define stable, technology-agnostic dashboard read models for pagination
  envelopes, inventory/health summary, delta entries, and proposal entries.
- Pin a documented HTTP error taxonomy and status-code mapping for all new
  endpoints so dashboard consumers and integration tests share one contract.
- Preserve parity across supported filesystem and Postgres
  metadata/state/delta backends, with explicit allowance for the filesystem
  backend to return a degraded marker on cross-account aggregates above an
  inventory threshold rather than perform a full scan.
- Provide deterministic ordering, explicit empty/missing/unauthenticated
  outcomes, and explicit data-unavailable outcomes consistent with prior
  dashboard endpoints.

### Out of Scope

- Account creation, update, deletion, signing, or any other mutating dashboard
  action.
- An account-id lookup parameter on the list endpoint. Locating one account
  by id is already served by the existing
  `GET /dashboard/accounts/{account_id}` detail endpoint from
  `003-operator-account-apis`, which also returns richer per-account fields
  than a list summary; a redundant filtered-list shortcut is not added in v1.
- Per-account asset, balance, token-amount, or "TVL" data of any kind. Today
  Guardian persists account state as an opaque `state_json` blob whose schema
  is client-defined; there is no normalized asset/vault read surface to
  expose. Asset/TVL views are explicitly deferred to a follow-up feature that
  must first define either a documented `state_json` schema convention or a
  network-specific account-inspector extension (e.g. Miden `AssetVault`
  enumeration). USD/fiat conversion, oracles, and pricing logic remain out
  of scope for the foreseeable future, to be re-evaluated only after a
  normalized state schema or account-inspector extension exists. Any
  future asset surface MUST expose raw token-identifier and amount data
  only and leave value derivation to the client.
- Time-series charts, trendlines, alerting, or aggregation windows.
  Aggregates are point-in-time snapshots derived from current persisted
  records.
- Free text search, tag/label search, prefix/substring/fuzzy matching on
  account ids, or proposal/signer search.
- Dashboard endpoints for cosigner activity, operator audit logs, or service
  configuration changes.
- Dashboard-side rate limiting and read-side audit logging for the new
  endpoints. The operator session is treated as trusted for v1; no per-IP or
  per-session throttle is introduced, and reads are not written to an audit
  log.
- A delta `transaction_type` / category descriptor field on delta entries.
  Deferred until a stable cross-network derivation rule exists; v1 entries
  expose only the identifier, status, and per-status detail listed below.
- Cursor TTL / rotation policy. Cursors are validated for tampering and for
  reference to data that still exists; they are not given an explicit
  expiry, and any operational signing-secret rotation is left to the plan
  phase.
- Changes to existing per-account authenticated HTTP or gRPC APIs, the Rust
  base client, the TypeScript base client, multisig SDKs, or example apps.
- Dashboard UI design and frontend interaction details.

## User Scenarios & Testing *(mandatory)*

These endpoints sit on the dashboard HTTP surface only and rely on the
operator session established by `002-operator-auth`. Existing low-level
Guardian HTTP/gRPC clients are unchanged. Validation is primarily through
server integration tests and the `guardian-operator-client` typed wrappers
once they are extended.

### User Story 1 - Page Through Many Accounts (Priority: P1)

As an authenticated operator, I can page through a large account inventory
using cursors so that the dashboard remains usable as the account count grows
beyond what fits comfortably in a single response.

**Why this priority**: Without pagination, the only existing dashboard list
endpoint becomes unusable past a few thousand accounts. This is the blocker
that converts the dashboard MVP into something that can be deployed against
real operator data sets.

**Independent Test**: Establish a valid operator session, seed enough
accounts to require multiple pages, request the first page with an explicit
`limit`, follow the returned cursor for subsequent pages until the end, and
verify each account appears exactly once across the full traversal under
quiescent inventory.

**Acceptance Scenarios**:

1. **Given** a valid operator session and more configured accounts than the
   chosen `limit`, **When** the operator requests the first page with that
   `limit`, **Then** the server returns at most `limit` account summaries
   plus a cursor token that can be used to fetch the next page.
2. **Given** a valid cursor returned by a previous page, **When** the
   operator requests the next page using that cursor, **Then** the server
   returns the following accounts in the same deterministic ordering with no
   overlap and no gap relative to the previous page under quiescent
   inventory.
3. **Given** the operator has paged to the end of the inventory, **When** the
   operator requests a further page, **Then** the server returns an empty
   page and a clear end-of-list indicator rather than wrapping or erroring.
4. **Given** a tampered, malformed, or no-longer-valid cursor token, **When**
   the operator submits it, **Then** the server returns an explicit
   `400 InvalidCursor` error and does not silently fall back to "first page".
5. **Given** new accounts are inserted between two paged requests, **When**
   the operator continues paging with the previously issued cursor, **Then**
   already-seen accounts are not reshuffled into a later page, inserts may
   appear on a future page or not at all, and no entry already returned is
   duplicated on a later page.
6. **Given** an account's `updated_at` is bumped between two paged
   requests, **When** the operator continues paging with the previously
   issued cursor, **Then** that account MAY be skipped or MAY reappear
   on a later page; this is documented expected behavior of cursor
   pagination over the mutable `updated_at` sort key on the account
   list endpoint, and is not treated as a contract violation. (Other
   endpoints sort by an immutable record identifier and do not have
   this caveat.)
7. **Given** a valid operator session and no `limit` parameter, **When** the
   operator requests the list, **Then** the server returns the first page
   using the documented default `limit` of 50, with a `next_cursor` if more
   pages exist. There is no full-inventory fallback mode.
8. **Given** a valid operator session, a previously issued cursor, and no
   `limit` parameter, **When** the operator submits the cursor alone,
   **Then** the server applies the default `limit` of 50 to the resumed
   page rather than rejecting the request or treating `cursor` as a
   no-op.

---

### User Story 2 - See Guardian Inventory And Lifecycle Health At A Glance (Priority: P1)

As an authenticated operator, I can request a single dashboard info response
that reports Guardian's self-described status, the deployment environment
identifier, the total account count, the latest activity timestamp, and
counts of recorded deltas grouped by lifecycle status, so the dashboard
can show a top-level health and inventory tile without issuing
per-account calls.

**Why this priority**: This is the dashboard landing-tile data. Without it,
operators must derive aggregates client-side from paged list responses and
per-account reads, which is both slow and incorrect once data sets are
large.

**Independent Test**: Establish a valid operator session against a Guardian
seeded with a known set of accounts and a known mix of recorded delta
records, request the info endpoint, and verify the response totals and
lifecycle-status distribution exactly match the seeded inventory at the
time of the call.

**Acceptance Scenarios**:

1. **Given** a valid operator session and a Guardian with seeded accounts
   and recorded delta records, **When** the operator requests the info
   endpoint, **Then** the response includes a Guardian service status
   indicator, the deployment environment identifier (e.g.
   `mainnet`/`testnet`), the total configured account count, a
   latest-activity timestamp (or an explicit "no activity" marker if the
   inventory has produced none), per-status counts of recorded deltas
   (`candidate`, `canonical`, `discarded`) plus the count of in-flight
   proposals, build identity (`version`, `git_commit`, `profile`,
   `started_at`) for the responding binary, backend configuration
   (`storage`, `supported_ack_schemes`, and the canonicalization-worker
   config which is `null` in optimistic-commit mode), and a per-auth-method
   account breakdown (`accounts_by_auth_method`).
2. **Given** a valid operator session and a Guardian with no configured
   accounts and no recorded deltas or proposals, **When** the operator
   requests the info endpoint, **Then** the response returns explicit zero
   counts and an explicit "no activity" marker rather than failing.
3. **Given** no valid operator session, **When** the operator requests the
   info endpoint, **Then** the server returns `401` and reveals nothing
   about inventory, environment, or activity.

---

### User Story 3 - Inspect One Account's Delta History (Priority: P2)

As an authenticated operator, I can request the delta history for one
account and see each persisted delta record with its per-account
`nonce`, lifecycle status, status timestamp, prior and resulting
state commitments, and retry count where applicable, so I can triage
stuck or rejected changes without dropping to raw storage.

**Why this priority**: This is the per-account drill-down that makes the
dashboard useful for actual operations. The list+detail surface from
`003-operator-account-apis` only exposes "has pending candidate" as a
boolean; operators need to see which candidate is still pending, when it
became a candidate, and how many retries it has accumulated.

**Independent Test**: Seed an account with a known sequence of delta
records spanning candidate, canonical, and discarded statuses (including a
retrying candidate), establish a valid operator session, request that
account's delta history, and verify the response contains each record
exactly once with the correct status, status timestamp, and (for
candidate entries) `retry_count`.

**Acceptance Scenarios**:

1. **Given** a valid operator session and a known account ID with
   recorded delta history, **When** the operator requests that
   account's delta history, **Then** the server returns the delta
   records ordered newest-first by creation, with each entry carrying
   at minimum the per-account `nonce`, the lifecycle status
   (`candidate`, `canonical`, or `discarded`), the status timestamp at
   which the record entered that status, `prev_commitment`, and
   `new_commitment` (nullable). Candidate entries additionally carry
   `retry_count`.
2. **Given** a valid operator session and an unknown account ID, **When**
   the operator requests that account's delta history, **Then** the server
   returns `404`.
3. **Given** a valid operator session and a known account ID with no
   recorded deltas yet, **When** the operator requests that account's delta
   history, **Then** the server returns `200` with an empty page rather
   than `404`.
4. **Given** the delta history for one account spans more entries than fit
   in one response, **When** the operator requests that account's delta
   history, **Then** the response uses the same cursor envelope shape as
   the account list so the dashboard can page through history with one
   cursor convention.

---

### User Story 4 - Inspect One Account's In-Flight Proposals (Priority: P2)

As an authenticated operator, I can request the in-flight proposal queue
for one account and see each proposal that is still collecting cosigner
signatures, including who proposed it, when, and how many of the required
signatures have been collected, so I can identify and triage stuck multisig
signing.

**Why this priority**: For multisig accounts, "the candidate is pending"
collapses two questions: "is the proposal still collecting signatures" and
"is the candidate awaiting canonicalization". Operators need a direct view
of the signature-collection state to answer the first question. Single-key
accounts simply return an empty queue here.

**Independent Test**: Seed a multisig account with one proposal in
`DeltaStatus::Pending` (i.e. a row in `delta_proposals`) that has some but
not all cosigner signatures, establish a valid operator session, request
that account's proposal queue, and verify the response includes that
proposal with the correct proposer identifier, originating timestamp,
signatures-collected count, and signatures-required count.

**Acceptance Scenarios**:

1. **Given** a valid operator session and a known multisig account ID
   with at least one in-flight proposal, **When** the operator requests
   that account's proposal queue, **Then** the response returns each
   in-flight proposal with the proposal `commitment`, the per-account
   `nonce`, the proposer identifier, the originating timestamp, the
   count of cosigner signatures collected, the count of cosigner
   signatures required, `prev_commitment`, and `new_commitment`
   (nullable).
2. **Given** a single-key (non-multisig) account or a multisig account
   with no in-flight proposals, **When** the operator requests that
   account's proposal queue, **Then** the server returns `200` with an
   empty page rather than `404`.
3. **Given** a valid operator session and an unknown account ID, **When**
   the operator requests that account's proposal queue, **Then** the
   server returns `404`.
4. **Given** the proposal queue for one account spans more entries than
   fit in one response, **When** the operator requests that account's
   proposal queue, **Then** the response uses the same cursor envelope
   shape as the account list and the delta history.

---

### User Story 5 - Receive Explicit Read Outcomes (Priority: P3)

As an operator, I receive explicit, code-pinned outcomes when dashboard
data is unavailable, when a request is unauthenticated, or when a cursor
is invalid, so the dashboard does not silently hide failures or invent
fallback behavior.

**Why this priority**: Read-only operator surfaces still need trustworthy
error semantics; ambiguous failures would make the dashboard misleading.
This extends the explicit-outcome discipline already established by
`003-operator-account-apis` to the new endpoints introduced here.

**Independent Test**: Exercise each new endpoint with an unauthenticated
request, an invalid-cursor request, and a request whose underlying records
exist in metadata but cannot be loaded from storage, and verify each
produces the documented HTTP status code and error body.

**Acceptance Scenarios**:

1. **Given** any new endpoint, **When** it is called without a valid
   operator session, **Then** the server returns `401 Unauthorized` and
   reveals nothing about inventory, accounts, deltas, or proposals.
2. **Given** any paginated endpoint, **When** the cursor is malformed,
   tampered, or no-longer-valid, **Then** the server returns
   `400 InvalidCursor` rather than silently restarting from the first page.
3. **Given** the delta-history or proposal-queue endpoint for a known
   account, **When** the underlying records cannot be loaded for that
   account, **Then** the server returns `503 DataUnavailable` distinct
   from `404 AccountNotFound`.
4. **Given** the info endpoint, **When** one underlying source (e.g. a
   delta store) cannot be read, **Then** the response either returns an
   explicit degraded-status indicator with the partial data clearly marked,
   or returns `503 DataUnavailable` — never a silent zero count that looks
   healthy.

---

### User Story 6 - See A Global Feed Of Recent Deltas Across All Accounts (Priority: P3, smallest)

As an authenticated operator, I can request a single cross-account feed of
delta records sorted by most recent first, optionally filtered to one or
more lifecycle statuses, so the dashboard landing page can show "what's
been happening across the whole Guardian" without requesting each account
separately.

**Why this priority**: This is genuinely useful for ops triage but is not
on the critical path. The per-account history (US3) plus the info
aggregates (US2) already cover account-scoped operations and the top-level
health tile. The global feed becomes valuable once an operator wants to
drill into "show me every candidate that's currently stuck" across the
whole inventory in one view, but the dashboard remains usable without it.

**Independent Test**: Seed multiple accounts with deltas in various
lifecycle states, establish a valid operator session, request the global
delta feed with no filter and verify entries from multiple accounts appear
in deterministic time order with each entry tagged by `account_id`. Then
request the same feed with `status=candidate,canonical` and verify only
those statuses appear.

**Acceptance Scenarios**:

1. **Given** a valid operator session and seeded deltas across multiple
   accounts, **When** the operator requests the global delta feed with no
   filter, **Then** the response returns delta entries ordered by status
   timestamp (most recent first) with a stable tie-breaker, and every
   entry carries an `account_id` so the dashboard can group or link by
   account.
2. **Given** a valid operator session, **When** the operator requests the
   global delta feed filtered to a comma-separated list of lifecycle
   statuses, **Then** the response contains only entries whose status is
   in that set, in the same deterministic ordering.
3. **Given** more deltas exist than fit in one response, **When** the
   operator pages with cursors under quiescent data, **Then** every delta
   appears at most once across the traversal and inserts during paging
   never duplicate an already-seen entry. Concurrent updates that mutate
   a delta's status timestamp MAY cause that delta to be skipped or to
   reappear on a later page, by the same cursor-pagination contract as
   the account list.
4. **Given** no valid operator session, **When** the operator requests the
   global delta feed, **Then** the server returns `401`.

---

### User Story 7 - See A Global Feed Of In-Flight Proposals Across All Accounts (Priority: P3, smallest)

As an authenticated operator, I can request a single cross-account feed of
in-flight multisig proposals sorted by most recent first, so the dashboard
landing page can answer "which proposals are currently waiting for
signatures across all my accounts" with one call.

**Why this priority**: Useful for spotting stuck signing across the
inventory, but per-account proposal queues (US4) plus the lifecycle counts
on the info endpoint (US2) already cover the same ground at finer
granularity. The global feed is the convenience layer.

**Independent Test**: Seed multiple multisig accounts each with at least
one in-flight proposal, establish a valid operator session, request the
global proposal feed, and verify every seeded in-flight proposal appears
with the correct `account_id`, proposer identifier, signatures-collected
count, and signatures-required count.

**Acceptance Scenarios**:

1. **Given** a valid operator session and seeded in-flight proposals across
   multiple multisig accounts, **When** the operator requests the global
   proposal feed, **Then** the response returns each in-flight proposal
   ordered by originating timestamp (most recent first) with a stable
   tie-breaker, each entry carrying `account_id`, proposer identifier,
   originating timestamp, signatures-collected count, and
   signatures-required count.
2. **Given** a Guardian whose accounts have no in-flight proposals, **When**
   the operator requests the global proposal feed, **Then** the response
   returns an empty paginated result rather than `404`.
3. **Given** more in-flight proposals exist than fit in one response,
   **When** the operator pages with cursors, **Then** every proposal
   appears at most once across the traversal under quiescent data.
4. **Given** no valid operator session, **When** the operator requests the
   global proposal feed, **Then** the server returns `401`.

---

## Requirements *(mandatory)*

### Functional Requirements

#### Account list pagination

- **FR-001**: The dashboard account list endpoint is **always paginated**.
  It accepts an optional `limit` parameter that bounds the number of
  accounts in one response, and an optional `cursor` parameter that resumes
  the list from a previously returned position. There is no full-inventory
  fallback mode.
- **FR-002**: When `limit` is omitted (with or without `cursor`), the
  endpoint MUST apply a default `limit` of **50**. When `limit` is
  supplied, it MUST be an integer in `[1, 500]`. Requests with `limit`
  outside that range MUST be rejected with `400 InvalidLimit` rather than
  silently truncated. A bare `?limit=` (present but empty) MUST be treated
  as omitted and the default applies.
- **FR-003**: The endpoint MUST return responses in a stable
  cursor-pagination envelope that exposes the page of accounts plus an
  explicit next-cursor-or-end-of-list indicator. The envelope shape MUST
  be the same regardless of whether `limit` or `cursor` was supplied by
  the client.
- **FR-004**: The endpoint MUST preserve the existing default ordering of
  most-recently-updated first with `account_id` as the stable tie-breaker,
  and cursor traversal MUST follow that same ordering.
- **FR-005**: Cursor tokens MUST be opaque to the client, MUST be
  validated for tampering, and MUST be rejected as `400 InvalidCursor`
  when the data the cursor references no longer exists. Cursors MUST NOT
  require the client to construct or interpret them.

  Cursor stability is determined by the sort key:
  - **Account list** (`/dashboard/accounts`) sorts by
    `updated_at DESC, account_id ASC`. The sort key is mutable, so under
    concurrent updates to `updated_at` an account MAY be skipped or
    repeated across a traversal. Under concurrent inserts only, an
    entry already returned on a prior page MUST NOT reappear on a later
    page. This caveat is documented expected behavior and not a
    contract violation.
  - **All other endpoints** (per-account delta history, per-account
    proposal queue, global delta feed, global proposal feed) sort by an
    immutable monotonic record identifier (`delta.id DESC` /
    `delta_proposal.id DESC`), giving "newest first" semantics —
    Postgres assigns the PK on insert, so an entry's position in the
    ordering does not change after creation. Cursor traversal on these
    endpoints is **fully stable** under both concurrent inserts and
    concurrent status updates.

  Cursors are not given an explicit TTL; rotation of any signing
  secret used to validate cursors is a plan-phase operational concern.
- **FR-006**: The fields on each list entry MUST remain a
  superset-compatible shape relative to `003-operator-account-apis` so
  existing dashboard consumers continue to function on a per-entry basis.
  New fields MAY be added; existing per-entry fields MUST NOT be renamed
  or removed. (The envelope itself is a breaking change per FR-001.)
- **FR-007**: The list response envelope MUST NOT include a `total_count`
  of all configured accounts. The unparameterized full-inventory
  `total_count` from `003-operator-account-apis` is removed as part of
  this feature's breaking change. Aggregate inventory totals are
  available only via the dashboard info endpoint (FR-009), which is the
  one canonical place for cross-account counts.
- **FR-007a**: The single-account detail endpoint
  (`GET /dashboard/accounts/{account_id}`) MUST return the bare
  account-detail object as the response body — no `success` wrapper
  and no `account` outer key. This normalizes the read surface across
  feature 005 (the paged endpoints, the global feeds, and the info
  endpoint all return the payload directly). The `success: true,
  account: { ... }` envelope inherited from `003-operator-account-apis`
  is removed in the same breaking-change window introduced by FR-001
  and FR-007. Read-side success/failure is signaled solely by the HTTP
  status code and the typed error body (FR-028).

#### Dashboard info endpoint

- **FR-008**: The server MUST expose a new authenticated dashboard endpoint
  that returns a single point-in-time inventory and health summary for the
  Guardian instance.
- **FR-009**: The info response MUST include, at minimum: a service
  status indicator, a deployment environment identifier (e.g.
  `mainnet`/`testnet`), the total configured account count, a
  latest-activity timestamp (with explicit "no activity" handling),
  counts of persisted deltas grouped by lifecycle status (`candidate`,
  `canonical`, `discarded`), the count of in-flight proposals, build
  identity (`version`, `git_commit`, `profile`, `started_at`), backend
  configuration (`storage` ∈ {`filesystem`, `postgres`},
  `supported_ack_schemes`, and the optional canonicalization-worker
  config — `check_interval_seconds`, `max_retries`,
  `submission_grace_period_seconds` — which is `null` in
  optimistic-commit mode), and a per-auth-method account breakdown
  (`accounts_by_auth_method`, keyed by stable labels such as
  `miden_falcon`, `miden_ecdsa`, `evm`). The response MUST NOT include
  per-network account counts or a singular "the network" field — the
  dashboard knows its own deployment context and per-account network
  type can be derived from the account list if needed.
- **FR-010**: The info response MUST NOT expose secrets, raw session data,
  per-account private auth material, implementation-internal cursors,
  rate-limit knobs, database URLs, or any asset/balance/token-amount
  data. The build-identity, backend-config, and
  `accounts_by_auth_method` fields are explicitly non-secret operational
  metadata and are permitted (the endpoint remains operator-session
  gated).
- **FR-011**: When one underlying source for an info aggregate cannot be
  loaded, the response MUST either explicitly mark that aggregate as
  unavailable/degraded or return `503 DataUnavailable`. The endpoint MUST
  NOT report a healthy zero count for an aggregate that is actually
  unreadable.
- **FR-012**: The info response shape MUST remain stable across supported
  filesystem and Postgres backends; the same seeded inventory MUST produce
  the same logical response on either backend, with the explicit allowance
  of FR-029 for the filesystem backend to mark cross-account aggregates as
  degraded above an inventory threshold rather than perform a full scan.

#### Per-account delta history endpoint

- **FR-013**: The server MUST expose a new authenticated dashboard endpoint
  that returns the delta history for one account identified by canonical
  account ID in the path.
- **FR-014**: Each delta entry MUST include, at minimum: the per-account
  `nonce` (integer; the per-account sequence number used as the
  human-readable identifier in dashboard tables), the lifecycle status
  of the record (`candidate`, `canonical`, or `discarded`), the status
  timestamp at which the record entered that status, the prior state
  commitment (`prev_commitment`, hex string) the delta was applied
  against, and the resulting state commitment (`new_commitment`, hex
  string; nullable when the delta has no resulting commitment, e.g. a
  discarded entry that never produced one). The delta history endpoint
  MUST NOT surface `pending`-status entries; pending state-changes
  live in `delta_proposals` and are exposed only via the proposal
  queue endpoint (FR-017), which keeps the two state machines on
  separate endpoints with separate identifier schemes.
- **FR-015**: Each candidate-status delta entry MUST additionally include
  a numeric `retry_count` reflecting how many times canonicalization has
  been attempted, so an operator can distinguish a freshly accepted
  candidate from one that has been retrying. `retry_count` MUST always be
  present on candidate entries (never null), defaulting to `0` for legacy
  records that pre-date retry tracking. Canonical and discarded entries
  carry only the base fields from FR-014.
- **FR-016**: The delta history endpoint is **always paginated** and
  ordered newest-first by Postgres-assigned record identifier
  (`delta.id DESC`). It uses the same envelope shape and the same
  default/maximum `limit` policy (default 50, max 500) as the account
  list endpoint, and MUST apply the cursor-stability contract from
  FR-005 (fully stable for this endpoint, including under concurrent
  status updates, because the sort key is immutable). There is no
  full-history unparameterized mode — a known account with a
  10,000-entry history cannot be downloaded in a single response.

#### Per-account proposal queue endpoint

- **FR-017**: The server MUST expose a separate authenticated dashboard
  endpoint that returns the in-flight proposal queue for one account
  identified by canonical account ID in the path. "In-flight" is defined
  as `DeltaStatus::Pending` rows in the `delta_proposals` storage — i.e.
  Miden multisig proposals still collecting cosigner signatures and not
  yet committed as a delta. EVM accounts (`Auth::EvmEcdsa`) MUST NOT
  appear in the proposal queue or the global proposal feed in v1: EVM
  proposals are tracked in a separate, feature-gated storage path that
  does not flow through `delta_proposals`. The proposal queue therefore
  always returns an empty page for EVM accounts in v1, the same as for
  single-key Miden accounts.
- **FR-018**: Each proposal entry MUST include, at minimum: the
  proposal `commitment` (hex string; the cryptographic identifier
  cosigners are signing), the per-account `nonce` (integer), the
  proposer identifier, the originating timestamp at which the
  proposal was created, the count of cosigner signatures collected so
  far, the count of cosigner signatures required for the proposal to
  be promoted to a delta, the prior state commitment
  (`prev_commitment`, hex string) the proposal applies against, and
  the resulting state commitment (`new_commitment`, hex string;
  nullable) so operators can see what state change is being voted on.
- **FR-019**: The signatures-required count MUST be derived from the
  account's auth policy: for `MidenFalconRpo` and `MidenEcdsa`, the count
  equals the number of `cosigner_commitments` declared on the account
  metadata; for any future scheme that introduces an explicit threshold,
  the threshold MUST be used instead. The derivation rule MUST be stable
  per account and MUST NOT change without an account auth-policy update.
- **FR-020**: Proposal entries MUST NOT expose raw cosigner signature
  bytes, private keys, session data, or any signing material beyond the
  counts and the proposer identifier. Per-cosigner identity lists are out
  of scope for v1 and MAY be introduced in a follow-up feature.
- **FR-021**: The proposal queue endpoint is **always paginated** and
  ordered newest-first by Postgres-assigned record identifier
  (`delta_proposal.id DESC`). It uses the same envelope shape and the
  same default/maximum `limit` policy (default 50, max 500) as the
  account list endpoint. Cursor traversal is fully stable per FR-005
  because the sort key is immutable.

#### Per-account endpoint behavior (shared)

- **FR-022**: Both per-account history endpoints (delta history, proposal
  queue) MUST return `404 AccountNotFound` for an unknown account ID,
  MUST return `200` with an empty page for a known account that has no
  recorded entries (including any single-key account on the proposal
  queue, which never has proposals), and MUST return `503 DataUnavailable`
  distinct from `404` when the underlying records cannot be loaded for an
  account whose metadata exists.
- **FR-023**: Both per-account history endpoints MUST scope every
  response strictly to the requested account; one account's request MUST
  NOT be able to enumerate or leak data from another account.
- **FR-024**: Both per-account history endpoints MUST NOT expose any
  asset, balance, token-amount, vault, or TVL data. These views are
  deferred (see Out of Scope); USD/fiat conversion is out of scope for
  Guardian itself for the foreseeable future, to be re-evaluated only
  if and when a normalized state schema or account-inspector extension
  exists.

#### Cross-cutting

- **FR-025**: All new endpoints MUST require a valid operator dashboard
  session as defined by `002-operator-auth`. Requests without a valid
  session MUST return `401 Unauthorized` and MUST NOT reveal the existence
  of accounts, inventory counts, deltas, or proposals.
- **FR-026**: All new endpoints MUST remain read-only. Calling them MUST
  NOT mutate account metadata, proposal state, replay-protection
  timestamps, delta records, or any other persisted Guardian state.
- **FR-027**: All new endpoints MUST sit on the dashboard HTTP surface
  only for v1; the lower-level per-account gRPC and per-account HTTP
  authenticated surfaces are unchanged.
- **FR-028**: All new endpoints MUST use the following HTTP status code
  mapping for the documented error categories. Each `400` subtype MUST
  carry a stable, machine-readable code in the response body so clients
  can branch without string-matching:
  - `401 Unauthorized` — missing, tampered, or expired operator session.
  - `404 AccountNotFound` — path-addressed account does not exist.
  - `400 InvalidCursor` — cursor token is malformed, tampered, or
    references data that no longer exists.
  - `400 InvalidLimit` — `limit` is not an integer in `[1, 500]`.
  - `400 InvalidStatusFilter` — global delta feed received a `status`
    filter value not in `{candidate, canonical, discarded}` or an
    otherwise malformed filter value.
  - `503 DataUnavailable` — metadata for the resource exists but the
    underlying records cannot be read.
- **FR-029**: All new endpoints MUST behave consistently across supported
  filesystem and Postgres metadata/state/delta backends, with one
  documented exception: cross-account aggregates (info endpoint per-status
  counts, latest-activity timestamp, global delta and proposal feeds) on
  the filesystem backend MAY return a degraded marker (or a `503
  DataUnavailable` outcome with a clear reason) above a configurable
  inventory-size threshold rather than perform a full filesystem scan.
  The threshold MUST be exposed as a server config field with a
  documented default of **1,000 accounts**; it is not env-driven or
  hardcoded. Per-account endpoints MUST behave identically on both
  backends.
- **FR-030**: Dashboard read-side rate limiting and read-side audit
  logging are NOT introduced in v1. The operator session is treated as
  trusted for the purposes of these read endpoints.

#### Global cross-account feeds (smallest priority)

- **FR-031**: The server MUST expose a new authenticated dashboard
  endpoint that returns a cross-account feed of delta records across all
  configured accounts.
- **FR-032**: The global delta feed MUST default to including all
  surfaced lifecycle statuses (`candidate`, `canonical`, `discarded`) and
  MUST accept an optional `status` filter that takes a comma-separated
  list of those statuses (e.g. `status=candidate` or
  `status=candidate,canonical`). When a value not in the set is
  provided, or the filter value is otherwise malformed, the endpoint
  MUST return `400 InvalidStatusFilter` (per FR-028) rather than
  silently returning all entries.
- **FR-033**: The global delta feed MUST order entries newest-first by
  Postgres-assigned record identifier (`delta.id DESC`) so cursor
  traversal is deterministic and fully stable per FR-005. The sort key
  is immutable, so concurrent status updates do not move an entry's
  position in the ordering.
- **FR-034**: Each entry in the global delta feed MUST include the same
  base fields as a per-account delta entry from FR-014/FR-015 plus the
  `account_id` to which the delta belongs.
- **FR-035**: The server MUST expose a new authenticated dashboard
  endpoint that returns a cross-account feed of in-flight multisig
  proposals (those defined as in-flight by FR-017) across all configured
  accounts. The global proposal feed MUST NOT accept a `status` filter
  parameter — every entry is in-flight by definition. EVM accounts do
  not appear in this feed in v1, per FR-017.
- **FR-036**: The global proposal feed MUST order entries newest-first
  by Postgres-assigned record identifier (`delta_proposal.id DESC`),
  giving a fully stable cursor traversal per FR-005, and MUST return
  an empty paginated result rather than `404` when no in-flight
  proposals exist on any account.
- **FR-037**: Each entry in the global proposal feed MUST include the
  same base fields as a per-account proposal entry from FR-018/FR-019/FR-020
  plus the `account_id` to which the proposal belongs.
- **FR-038**: Both global feed endpoints are **always paginated** with
  the same envelope shape and the same default/maximum `limit` policy
  (default 50, max 500) as the other new endpoints, and MUST honor the
  cross-account aggregate degradation allowance from FR-029 on the
  filesystem backend.
- **FR-039**: Both global feed endpoints MUST be treated as the smallest
  priority slice of this feature and MAY be delivered after the
  per-account endpoints; the dashboard MUST remain functional without
  them.

### Contract / Transport Impact

- Adds `limit` (default 50, max 500) and `cursor` query parameters to
  the dashboard account list endpoint and converts the endpoint to a
  single, always-paginated contract. **Breaking change** vs.
  `003-operator-account-apis`: the full-inventory unparameterized mode
  is removed and `total_count` is no longer returned on the list
  response. The internal dashboard consumer is updated as part of this
  feature.
- Introduces one new authenticated dashboard endpoint for inventory/health
  info, returning a single JSON object.
- Introduces one new authenticated dashboard endpoint for per-account
  delta history, identified by canonical account ID in the path,
  returning a paginated JSON envelope of delta entries.
- Introduces one new authenticated dashboard endpoint for the per-account
  in-flight proposal queue, identified by canonical account ID in the
  path, returning a paginated JSON envelope of proposal entries.
- Introduces two new authenticated dashboard endpoints for cross-account
  feeds: one global delta feed (with optional comma-separated `status`
  filter) and one global in-flight proposal feed. Each entry carries
  `account_id`. Both use the same cursor-pagination envelope as the other
  endpoints.
- No gRPC surface changes are required for this feature.
- No Rust base-client (`guardian-client`) changes are required; the
  dashboard is the consumer.
- TypeScript `@openzeppelin/guardian-operator-client` MAY be extended with
  typed wrappers for the new endpoints; that extension is itself
  read-only and preserves the prior surface.
- All new endpoints use the operator session cookie established by
  `002-operator-auth`. They do not use the per-account
  `x-pubkey`/`x-signature`/`x-timestamp` request-auth scheme.
- Error taxonomy and HTTP status code mapping pinned by FR-028:
  `401 Unauthorized` / `404 AccountNotFound` / `400 InvalidCursor` /
  `400 InvalidLimit` / `400 InvalidStatusFilter` / `503 DataUnavailable`.
  `400` subtypes carry a stable machine-readable code in the body.

### Field Glossary

To prevent the plan phase from inventing two names for the same thing,
the spec uses these field names consistently for every endpoint that
exposes them:

- `account_id` — canonical Guardian account identifier (string).
- `limit` — page size on paginated endpoints (integer in `[1, 500]`,
  default 50).
- `cursor` — opaque cursor token; omitted on first page.
- `next_cursor` — opaque cursor in the response envelope; absent at
  end of list.
- `total_count` — total configured account count, returned **only** by
  the dashboard info endpoint (FR-009). Removed from the list endpoint
  as part of this feature's breaking change.
- `status` — lifecycle status of a delta (`candidate`, `canonical`,
  `discarded`); also the query parameter on the global delta feed.
- `status_timestamp` — wall-clock timestamp when the record entered
  its current status. Promoted to a typed `timestamptz` column on both
  `deltas` and `delta_proposals` by the Phase A migration so it can be
  indexed and used as the global-feed sort key.
- `nonce` — per-account integer sequence number on delta and proposal
  entries; the human-readable identifier shown in dashboard tables.
- `prev_commitment` / `new_commitment` — hex-string state commitments
  on delta and proposal entries. `prev_commitment` is the commitment
  the entry was applied against; `new_commitment` is the resulting
  commitment (nullable for entries that did not produce one, e.g. a
  discarded delta).
- `commitment` — hex-string proposal identifier on proposal entries
  (the cryptographic value cosigners are signing).
- `retry_count` — non-null integer on candidate delta entries (default
  `0`).
- `proposer_id` — proposer identifier on proposal entries.
- `signatures_collected` / `signatures_required` — integer counts on
  proposal entries; `signatures_required` derived per FR-019.
- `environment` — deployment environment identifier on the info
  response (e.g. `mainnet`/`testnet`).
- `latest_activity` — timestamp on the info response, derived per
  Assumptions; `null` when no activity exists.

### Data / Lifecycle Impact

- No new persistent entities are introduced.
- The new responses are read models derived from existing
  `AccountMetadata` records, current `StateObject` records, persisted
  delta records, and persisted `delta_proposals` records.
- A Phase A schema migration promotes `status_kind` (text) and
  `status_timestamp` (timestamptz) to typed, indexed columns on both
  `deltas` and `delta_proposals` and backfills them from the existing
  `status` `Jsonb` blob. Composite indexes on `(status_kind,
  status_timestamp DESC, account_id, id)` make the global delta and
  proposal feeds index range scans. Per-account history endpoints
  continue to sort by the immutable `id` primary key (newest first),
  preserving fully stable per-account cursors. The Postgres backend
  pushes pagination, status filtering, and sort entirely into SQL via
  the new columns; the filesystem backend keeps the fan-out
  implementation, bounded by `filesystem_aggregate_threshold`
  (FR-029). Existing deployments must run the migration; backfill is
  mandatory because both tables already have rows.
- No account, delta, proposal, canonicalization, or signer lifecycle
  semantics change as part of this feature.
- Backend parity requirements apply with the explicit FR-029 exception
  for cross-account aggregates on the filesystem backend.

## Edge Cases *(mandatory)*

- **Unauthenticated access**: Missing, tampered, or expired operator
  sessions return `401` for every new endpoint without leaking account or
  inventory existence.
- **Empty inventory**: The info endpoint returns explicit zero counts and
  an explicit no-activity marker rather than a synthetic error when no
  accounts or no recorded deltas/proposals exist.
- **Empty delta history / proposal queue**: Both per-account history
  endpoints return an empty paginated result for a known account with no
  records, not `404`. Single-key (non-multisig) accounts always return an
  empty proposal queue.
- **Concurrent inserts during paging**: Cursor traversal must remain
  consistent under concurrent inserts: prior pages must not lose entries
  already returned, and one entry must not appear twice in the same
  traversal due solely to inserts.
- **Concurrent sort-key updates during paging**: When a sort key
  (`updated_at` for accounts, status timestamp for deltas, originating
  timestamp for proposals) is mutated mid-traversal, the affected entry
  MAY be skipped or MAY appear on a later page. This is documented
  expected behavior of cursor pagination over a mutable sort key per
  FR-005 and is not a contract violation.
- **Cursor lifetime**: A cursor that is malformed, tampered, or
  references data that no longer exists returns `400 InvalidCursor`.
  Cursors are not given an explicit TTL.
- **`cursor` without `limit`**: A request that supplies `cursor` but
  omits `limit` resumes paging using the default `limit` of 50; it is
  not rejected and `cursor` is not silently ignored.
- **Bare `?limit=`**: A request with `limit` present but with no value
  is treated as if `limit` were omitted; the default of 50 applies.
- **`limit` out of range**: A request with `limit` ≤ 0 or > 500 is
  rejected with `400 InvalidLimit` and not silently truncated.
- **Mixed account identity formats**: Delta history and proposal queue
  endpoint paths must accept any canonical Guardian account identifier
  valid for the server, including non-Miden-only formats, when correctly
  URL-encoded.
- **Sensitive fields**: New responses must not expose private keys,
  session cookies, raw request-auth headers, raw cosigner signature
  bytes, or replay-protection internals. Asset/balance/token-amount/TVL
  data are out of scope for v1 entirely and must not appear on any new
  endpoint.
- **Backend parity**: A backend-specific missing-record or serialization
  issue must not change the contract semantics for healthy accounts on
  the other backend. Cross-account aggregates on the filesystem backend
  MAY be marked degraded above an inventory threshold per FR-029, but
  per-account endpoints behave identically on both backends.
- **Lifecycle statuses with no entries**: A status bucket with zero
  entries on the info response must appear with an explicit `0` count
  rather than be omitted, so dashboards can render a stable schema.
- **Network identity on the info response**: The info response carries
  a deployment environment identifier (e.g. `mainnet`/`testnet`) but
  does not carry a singular "the network" field or per-network account
  counts. In practice an instance is gated to one network family
  (Miden default; EVM behind a feature flag), and the dashboard knows
  its own deployment context.
- **Global feeds — empty inventory**: Both global feed endpoints return
  an empty paginated result for a Guardian with no matching records,
  never `404`.
- **Global feeds — unknown status filter**: The global delta feed must
  reject an unrecognized status filter value with the documented `400`
  error rather than silently returning all entries.
- **Filesystem-backend cross-account scan**: Above the configured
  inventory threshold, the filesystem backend MAY return a degraded
  marker on info aggregates and on the global feeds rather than perform
  a full scan; per-account endpoints remain fully operational.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: An authenticated operator can traverse an inventory of at
  least 10,000 configured accounts using cursor pagination with at most
  one cursor per page request, and every configured account appears
  exactly once across the traversal under quiescent inventory. Postgres
  serves this via SQL pushdown over the `(updated_at DESC, account_id
  ASC)` composite index added in migration
  `2026-05-10-000002_account_metadata_pagination_index`; the
  filesystem backend reads the in-memory metadata cache and is bounded
  by the FR-029 threshold.
- **SC-002**: An authenticated operator can render a top-level dashboard
  tile (status, deployment environment, total accounts, latest activity,
  counts by lifecycle status) from a single info request, and the totals
  match the underlying inventory in 100% of seeded validation runs.
- **SC-003**: An authenticated operator can list the delta history for
  one account with each entry carrying `nonce`, lifecycle status,
  status timestamp, `prev_commitment`, and `new_commitment` (nullable);
  and candidate entries carry `retry_count` in 100% of seeded
  validation cases that include a candidate entry.
- **SC-004**: An authenticated operator can list the in-flight proposal
  queue for one multisig account, and each entry carries `commitment`,
  `nonce`, proposer identifier, originating timestamp,
  signatures-collected count, signatures-required count,
  `prev_commitment`, and `new_commitment` (nullable) in 100% of seeded
  validation cases that include an in-flight proposal. Single-key
  accounts return an empty proposal queue.
- **SC-005**: 100% of requests to the new endpoints without a valid
  operator session are rejected with `401`.
- **SC-006**: 100% of requests carrying a tampered or malformed cursor
  are rejected with `400 InvalidCursor`; 100% of requests carrying
  `limit` outside `[1, 500]` are rejected with `400 InvalidLimit`; and
  100% of global delta feed requests carrying an unknown `status`
  value are rejected with `400 InvalidStatusFilter`. None of these
  cases silently fall back to a default response.
- **SC-007**: 95% of paginated list, per-account history, and global
  feed requests against v1-scale data sets (up to 10,000 accounts, up
  to 10,000 records per account) complete in under 1 second of
  perceived dashboard latency on the Postgres backend in validation
  runs. All paginated endpoints push pagination, sort, and filter
  into SQL via the indexes added by migrations
  `2026-05-10-000001_promote_delta_status` and
  `2026-05-10-000002_account_metadata_pagination_index`. Filesystem
  deployments remain bounded by FR-029.
- **SC-008**: The same seeded dataset produces semantically equivalent
  responses across filesystem-backed and Postgres-backed server runs for
  the account list (including paging), the per-account delta history,
  and the per-account proposal queue. Cross-account aggregates may
  return a degraded marker on the filesystem backend per FR-029 without
  failing this criterion.
- **SC-009**: 0 new endpoints expose asset, balance, token-amount, TVL,
  or fiat-derived values, and 0 new endpoints expose raw private auth
  material, raw cosigner signature bytes, or session data, in any
  validated response.
- **SC-010**: An authenticated operator can request the global delta
  feed with no filter and with multi-status filter values, paginate to
  end-of-feed under v1-scale data, and observe every seeded delta
  exactly once across the unfiltered traversal under quiescent data.
- **SC-011**: An authenticated operator can request the global in-flight
  proposal feed and see every seeded in-flight proposal across all
  multisig accounts exactly once, each entry tagged with its
  `account_id` and the proposer-and-signature counts from per-account
  proposal entries.
- **SC-012**: 100% of error responses on the new endpoints use the
  HTTP status code mapping pinned by FR-028
  (`401`/`404`/`400 InvalidCursor`/`400 InvalidLimit`/
  `400 InvalidStatusFilter`/`503 DataUnavailable`), with the `400`
  subtypes carrying their stable machine-readable code in the response
  body.

## Assumptions

- `002-operator-auth`, `003-operator-account-apis`, and
  `004-operator-http-client` are treated as prerequisites; this feature
  extends them rather than redefines them.
- Dashboard consumers want raw data (counts, statuses, lifecycle events)
  and perform any presentation-level derivation client-side; Guardian
  remains oracle-free.
- Asset/balance/TVL views are explicitly deferred. Today Guardian's
  `state_json` is an opaque client-defined blob with no normalized asset
  schema, and no Guardian-side account-inspector decodes asset vaults.
  A follow-up feature will spec the data shape (either a `state_json`
  schema convention or a network-specific account-inspector extension)
  before any asset surface is added to the dashboard.
- Standalone account-id lookup on the list endpoint is not added in v1
  because the existing
  `GET /dashboard/accounts/{account_id}` detail endpoint already serves
  "find one account by id" and returns richer fields than a list summary.
- Cursor pagination semantics in this spec guarantee insert-stability
  but explicitly accept skip/repeat under concurrent sort-key updates.
  This matches what cursor pagination over a mutable sort key
  (`updated_at`, status timestamp, originating timestamp) can deliver
  honestly without taking pessimistic locks.
- Cursors are validated for tampering and for reference to data that
  still exists; they are not given a TTL in v1. Any HMAC/signing-secret
  rotation strategy is a plan-phase operational concern.
- Default page `limit` is 50; server-side max is 500. There is no
  unparameterized full-inventory mode (breaking change vs.
  `003-operator-account-apis`).
- A Phase A schema migration promotes `status_kind` and
  `status_timestamp` to typed, indexed columns on both `deltas` and
  `delta_proposals`. Per-account history endpoints sort by the
  immutable `id` primary key (newest first) for fully stable
  per-account cursors; the global delta and proposal feeds sort by
  `(status_timestamp DESC, account_id ASC, id ASC)` over composite
  indexes. The Postgres backend pushes pagination, status filtering,
  and sort entirely into SQL. Existing deployments must run the
  migration; the `up.sql` backfills both columns from the existing
  `status` `Jsonb` blob so the typed columns arrive consistent with
  history.
- "Latest activity timestamp" on the info endpoint is defined as the
  greater of the most recent delta status timestamp and the most
  recent in-flight proposal originating timestamp across all
  configured accounts. Both new proposals and delta state-transitions
  count as activity from the operator's perspective. On Postgres this
  is two indexed `MAX(status_timestamp)` queries combined; on the
  filesystem backend it MAY be marked degraded above the inventory
  threshold per FR-029.
- Per-account history is exposed as two distinct endpoints — delta
  history and proposal queue — that share the same cursor-pagination
  envelope. This matches Guardian's storage split between deltas and
  `delta_proposals` and avoids a single combined feed conflating two
  different state machines or two different per-account record-identifier
  schemes (`delta.nonce` vs. `delta_proposal.commitment`).
- For v1, the proposal queue endpoint surfaces only counts of cosigner
  signatures collected vs. required and the proposer identifier;
  per-cosigner signer-identity lists are intentionally deferred to a
  follow-up feature.
- The `delta.transaction_type` / category descriptor field on delta
  entries is intentionally deferred until a stable cross-network
  derivation rule exists; v1 entries expose only identifier, status,
  and per-status detail.
- The global delta and global proposal feeds are the smallest priority
  slice of v1 and may be delivered after the per-account endpoints. The
  dashboard remains functional without them.
- The global delta feed defaults to including all surfaced lifecycle
  statuses; the most common ops view ("what's stuck") is expressible as
  `status=candidate` or `status=candidate,canonical`. No mandatory date
  window is imposed in v1; cursor pagination plus the server-side max
  page size are considered sufficient protection against unbounded
  reads.
- "Everything not yet canonical" (i.e. pending proposals + candidate
  deltas) requires composing the global delta feed
  (`status=candidate`) with the global proposal feed on the dashboard
  side, because pending proposals live on a different endpoint per
  FR-017. This is an explicit, accepted v1 trade-off; a unified
  not-yet-canonical view is not added in v1.
- This feature introduces a **breaking change** to the dashboard
  account list endpoint: the unparameterized full-inventory mode and
  the `total_count` field are removed in favor of a single, always-
  paginated contract. The internal dashboard consumer is updated as
  part of this feature; there is no dual-mode preservation.

## Dependencies

- `002-operator-auth` for operator session establishment, session
  validation, and middleware protection.
- `003-operator-account-apis` for the existing account list/detail
  surface whose shape this feature extends with pagination, and for the
  per-account detail endpoint that subsumes any standalone account-id
  lookup need.
- `004-operator-http-client` for the typed operator client surface that
  may be extended with new wrappers.
- Existing Guardian metadata/state read operations for account
  enumeration, per-account lookup, and inventory counts.
- Existing Guardian delta and `delta_proposals` storage operations for
  per-account history reads and lifecycle-status aggregates.
- A browser dashboard consumer that will render these responses once
  the server contract exists.
