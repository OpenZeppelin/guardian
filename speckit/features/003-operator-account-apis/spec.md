# Feature Specification: Operator Dashboard Account List and Detail APIs

**Feature Key**: `003-operator-account-apis`  
**Suggested Branch**: `003-operator-account-apis` (manual creation optional)  
**Created**: 2026-04-22  
**Status**: Draft  
**Input**: User description: "Operator dashboard account APIs: authenticated list accounts and get account details endpoints for the browser dashboard, backed by existing Guardian account metadata and state."

## Context

Guardian now has a dedicated operator authentication spec, but authenticated
operators still have no read-only dashboard data surface to answer the most
basic question: which accounts exist, and what does one account currently look
like.

This feature adds the first dashboard data slice on the existing Guardian
server: a read-only account list endpoint and a read-only account detail
endpoint for browser operators who already hold a valid dashboard session. The
responses should be derived from the existing account metadata and current state
records rather than introducing a second account system for the dashboard.

The change affects the server contract on the dashboard HTTP surface only. It
must reuse the operator session model defined in `002-operator-auth`, preserve
the existing multisig protocol and per-account authenticated APIs, and behave
consistently across supported metadata and state backends.

## Scope *(mandatory)*

### In Scope

- Add an authenticated dashboard HTTP endpoint to list configured accounts as
  concise summaries.
- Add an authenticated dashboard HTTP endpoint to retrieve one account's current
  details.
- Define stable dashboard read models derived from existing Guardian account
  metadata and current state records.
- Require deterministic list ordering and explicit behavior for empty, missing,
  and unauthenticated requests.
- Preserve parity across supported filesystem and Postgres metadata/state
  backends.

### Out of Scope

- Account creation, update, deletion, or any other mutating dashboard action.
- Dashboard endpoints for deltas, proposals, transactions, or signer activity
  beyond the account-level auth summary needed for the detail view.
- Pagination, search, filtering, and sorting controls beyond one fixed,
  deterministic default ordering in v1.
- Dashboard UI design and frontend interaction details.
- Changes to existing per-account authenticated HTTP or gRPC APIs, Rust/TS base
  clients, multisig SDKs, or example apps.

## User Scenarios & Testing *(mandatory)*

- Prioritize stories (P1, P2, P3). Each story must be independently testable and
  deliver user value.
- These endpoints are on the dashboard HTTP surface only and rely on the
  operator session established by `002-operator-auth`.
- Existing low-level Guardian HTTP/gRPC clients are unchanged; validation is
  primarily through server integration tests and the dashboard consumer once it
  exists.

### User Story 1 - View The Account List (Priority: P1)

As an authenticated operator, I can open the dashboard and see every configured
account as a concise summary so I can understand what the server is managing
without manually calling low-level account-specific APIs.

**Why this priority**: This is the dashboard landing view for account
operations. Without it, the operator still has no usable read surface after
logging in.  
**Independent Test**: Establish a valid operator session, seed multiple
accounts, call the list endpoint, and verify that the response includes one
summary per account in deterministic order with the expected summary fields.

**Acceptance Scenarios**:

1. **Given** a valid operator session and multiple configured accounts, **When**
   the operator calls the account list endpoint, **Then** the server returns a
   `200` response containing one summary entry per configured account.
2. **Given** a valid operator session and no configured accounts, **When** the
   operator calls the account list endpoint, **Then** the server returns `200`
   with an empty list rather than an error.
3. **Given** no operator session or an expired session, **When** the operator
   calls the account list endpoint, **Then** the server returns `401` and does
   not reveal account existence.

---

### User Story 2 - Inspect One Account (Priority: P2)

As an authenticated operator, I can open one account's detail view and inspect
its current account-level summary, including auth policy summary and current
state commitment, so I can reason about that account without switching to raw
storage or low-level client tooling.

**Why this priority**: The list alone is only an index. Operators need a detail
view to make the dashboard useful for actual account inspection.  
**Independent Test**: Seed one account with known metadata and current state,
establish a valid operator session, call the detail endpoint for that account,
and verify the returned fields match the seeded account data and its list
summary.

**Acceptance Scenarios**:

1. **Given** a valid operator session and a known account ID, **When** the
   operator calls the account detail endpoint, **Then** the server returns `200`
   with that account's detail record.
2. **Given** a known account appears in the account list, **When** the operator
   requests that account's detail, **Then** the detail view returns the same
   account identity and matching summary fields for auth scheme, pending
   candidate status, and current commitment.
3. **Given** a valid operator session and an unknown account ID, **When** the
   operator calls the detail endpoint, **Then** the server returns `404`.

---

### User Story 3 - Receive Explicit Read Outcomes (Priority: P3)

As an operator, I receive explicit outcomes when account data is unavailable or
an account disappears between requests so the dashboard does not silently hide
data quality problems or invent fallback behavior.

**Why this priority**: Read-only operator surfaces still need trustworthy error
semantics. Ambiguous failures would make the dashboard misleading.  
**Independent Test**: Exercise a missing-account request, an expired-session
request, and an account whose metadata exists but whose current state cannot be
loaded, and verify that each produces the specified explicit outcome.

**Acceptance Scenarios**:

1. **Given** an account summary is included in the list because metadata exists,
   **When** the current state cannot be loaded for that account during the list
   read, **Then** the list still includes the account and marks its state as
   unavailable instead of failing the entire list response.
2. **Given** an account existed when the list was loaded, **When** that account
   is removed before the operator requests its detail, **Then** the detail
   endpoint returns `404` rather than stale cached data.
3. **Given** a known account's metadata exists but its current state cannot be
   loaded during the detail read, **When** the operator requests its detail,
   **Then** the server returns an explicit data-unavailable error rather than a
   partial success that looks healthy.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The existing Guardian server MUST expose two authenticated
  dashboard HTTP endpoints for this feature: `GET /dashboard/accounts` and
  `GET /dashboard/accounts/{account_id}`.
- **FR-002**: Both endpoints MUST require a valid operator dashboard session as
  defined by `002-operator-auth`; requests without a valid session MUST return
  `401 Unauthorized`.
- **FR-003**: The account list endpoint MUST return a read-only collection of
  configured accounts derived from the existing metadata store rather than from
  dashboard-owned state.
- **FR-004**: The account detail endpoint MUST use the canonical Guardian
  account ID from the request path to look up one account and MUST return `404`
  when no such account exists.
- **FR-005**: The account list endpoint MUST return accounts in a deterministic
  default order of most recently updated first, with `account_id` as a stable
  tie-breaker.
- **FR-006**: Each account list entry MUST include, at minimum,
  `account_id`, `auth_scheme`, `authorized_signer_count`,
  `has_pending_candidate`, `current_commitment`, `state_status`,
  `created_at`, and `updated_at`.
- **FR-007**: The account detail response MUST include the same summary fields
  exposed by the list entry for that account and MUST additionally include the
  normalized set of `authorized_signer_ids` plus the current state's
  `state_created_at` and `state_updated_at` values when state data is
  available.
- **FR-008**: Account read models MUST summarize the existing account auth
  policy using stable, display-safe data only. They MUST NOT expose private
  keys, session data, raw request-auth headers, or replay-protection internals.
- **FR-009**: `current_commitment` MUST be derived from the current persisted
  state record for the account when that state is available.
- **FR-010**: The list endpoint MUST return `200 OK` with an empty `accounts`
  collection and `total_count = 0` when no accounts are configured.
- **FR-011**: The list endpoint MUST be resilient to per-account supplemental
  state-read failures. If metadata exists for an account but its current state
  cannot be loaded, the server MUST still include the account in the list with
  `current_commitment = null` and `state_status = "unavailable"`.
- **FR-012**: The detail endpoint MUST fail explicitly when metadata exists but
  the current state cannot be loaded for that account. This MUST be distinct
  from `404 AccountNotFound`.
- **FR-013**: The detail endpoint MUST accept any canonical Guardian account ID
  that is valid for the server at implementation time, including non-Miden-only
  identifiers, provided the path value is correctly URL-encoded.
- **FR-014**: These endpoints MUST remain read-only. Calling them MUST NOT
  mutate account metadata, proposal state, replay-protection timestamps, or any
  other persisted Guardian state.
- **FR-015**: The same seeded account fixture MUST produce semantically
  equivalent list and detail responses across the supported filesystem and
  Postgres metadata/state backends.

### Contract / Transport Impact

- Introduces two new HTTP endpoints on the dashboard surface:
  `GET /dashboard/accounts` and `GET /dashboard/accounts/{account_id}`.
- No gRPC surface changes are required for this feature.
- No Rust or TypeScript base-client changes are required for this feature; the
  browser dashboard is the intended consumer of these endpoints.
- These endpoints use the operator session cookie established by
  `002-operator-auth` and do not use the existing per-account
  `x-pubkey`/`x-signature`/`x-timestamp` request-auth scheme.
- Error behavior for these endpoints MUST distinguish at least:
  `Unauthorized`, `AccountNotFound`, and `AccountDataUnavailable`.
- No fallback behavior changes are introduced for online/offline multisig flows
  or existing low-level Guardian transports.

### Data / Lifecycle Impact

- No new persistent entities are introduced.
- The list and detail responses are read models derived from existing
  `AccountMetadata` records and current `StateObject` records.
- No account, delta, proposal, canonicalization, or signer lifecycle semantics
  change as part of this feature.
- Backend parity requirements do apply: the dashboard read surface must work for
  the same account data regardless of whether metadata/state are backed by the
  filesystem or Postgres implementations.

## Edge Cases *(mandatory)*

- **Unauthenticated access**: Missing, tampered, or expired operator sessions
  return `401` for both endpoints without leaking account existence.
- **No configured accounts**: The list endpoint returns an empty result set, not
  a synthetic error.
- **Account removed between requests**: An account may appear in one list
  response and still legitimately return `404` on a later detail request.
- **Metadata/state mismatch**: Metadata can exist even when the current state is
  unreadable; list and detail behavior must follow the explicit rules above
  rather than silently dropping the account or fabricating state.
- **Mixed account identity formats**: The detail route must not assume account
  IDs are always Miden-style hex strings; future or already-supported canonical
  account IDs must remain addressable when URL encoded.
- **Deterministic ordering**: Two accounts with identical `updated_at` values
  must still sort predictably via the stable tie-breaker.
- **Data sensitivity**: Auth summaries may expose signer identifiers needed for
  operator inspection, but must not expose secrets or raw session/auth tokens.
- **Backend parity**: A backend-specific missing-state or serialization issue
  must not change the contract semantics for healthy accounts on the other
  backend.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: An authenticated operator can retrieve the complete account list
  and then retrieve one listed account's detail using only the dashboard session
  established by the auth flow.
- **SC-002**: In seeded validation fixtures, 100% of configured accounts appear
  exactly once in the list response, and `total_count` matches the number of
  configured accounts.
- **SC-003**: For 100% of accounts selected from the list in validation
  fixtures, the detail response matches the list's `account_id`, `auth_scheme`,
  `has_pending_candidate`, and `current_commitment` values for that account.
- **SC-004**: 100% of requests to both endpoints without a valid operator
  session are rejected with `401`.
- **SC-005**: The same seeded dataset produces semantically equivalent list and
  detail results across filesystem-backed and Postgres-backed server runs.
- **SC-006**: For v1-scale datasets of up to 500 configured accounts, operators
  can load the account list in under 2 seconds in 95% of validation runs.

## Assumptions

- `002-operator-auth` is implemented first or is treated as a prerequisite for
  these endpoints.
- V1 dashboard deployments manage a bounded number of accounts (order of
  hundreds, not tens of thousands), so returning the full list without
  pagination is acceptable.
- The dashboard needs current account summaries, not full historical state,
  proposal, or transaction timelines.
- Existing Guardian account IDs are treated as stable identifiers for detail
  lookup and can be URL encoded by the dashboard client.
- Account auth policies may vary by scheme or network over time, so the
  dashboard response should use generic auth-summary terminology instead of a
  Miden-only projection.

## Dependencies

- `002-operator-auth` for operator session establishment, session validation,
  and middleware protection.
- Existing Guardian metadata store read operations for account enumeration and
  per-account metadata lookup.
- Existing Guardian state storage read operations for current commitment and
  state timestamps.
- A browser dashboard consumer that will render these responses once the server
  contract exists.
