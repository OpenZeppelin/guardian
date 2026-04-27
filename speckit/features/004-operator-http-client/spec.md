# Feature Specification: Operator HTTP Client Package

**Feature Key**: `004-operator-http-client`  
**Suggested Branch**: `004-operator-http-client` (manual creation optional)  
**Created**: 2026-04-22  
**Status**: Draft  
**Input**: User description: "New super-lean TypeScript package for Guardian operators that interacts with the Guardian server over HTTP for dashboard auth and account reads."

## Context

Guardian now has a server-side operator authentication flow (`002`) and a first
read-only account dashboard surface (`003`), but any browser dashboard or other
operator-facing tool would still need to hand-roll raw HTTP calls, response
parsing, and error handling for those endpoints.

This feature introduces a new dedicated TypeScript package under `packages/`
for operator workflows only. Its job is to provide a very small HTTP client for
the existing operator dashboard surface: challenge issuance, signature verify,
logout, account list, and account detail. It should stay separate from the
broader `@openzeppelin/guardian-client` and multisig SDKs because operator
dashboard consumers do not need account-auth signing, proposal mutation, or
multisig orchestration to perform these read-only server interactions.

The change is additive at the TypeScript package layer. The existing Guardian
server HTTP contract from `002` and `003` remains the source of truth, and no
gRPC, multisig protocol, or server-storage behavior changes are required.

## Scope *(mandatory)*

### In Scope

- Add a new dedicated TypeScript package for Guardian operator HTTP workflows.
- Expose typed methods for the existing operator endpoints:
  `GET /auth/challenge`, `POST /auth/verify`, `POST /auth/logout`,
  `GET /dashboard/accounts`, and `GET /dashboard/accounts/{account_id}`.
- Define stable TypeScript models for operator challenge, auth verification,
  account list, account detail, and transport-level error outcomes.
- Preserve the existing commitment-based operator auth contract and session
  cookie behavior defined on the server.
- Keep the package small and operator-focused so a dashboard consumer can avoid
  raw `fetch` calls for these workflows.

### Out of Scope

- Browser wallet integration, challenge signing, commitment derivation, or any
  signer abstraction; callers supply the commitment and signature.
- React hooks, UI state containers, routing helpers, or any dashboard UI code.
- Proposal, transaction, signer-management, or account-mutation endpoints beyond
  the existing operator auth and account-read surface.
- gRPC transport support or changes to existing Rust/TypeScript base clients and
  multisig SDKs.
- New server endpoints, server-auth semantics, or storage-layer behavior.

## User Scenarios & Testing *(mandatory)*

- Prioritize stories (P1, P2, P3). Each story must be independently testable and
  deliver user value.
- This package is HTTP-only and consumes the existing server contract from
  `002-operator-auth` and `003-operator-account-apis`.
- Upstream validation should include at least one operator-facing browser
  consumer or a minimal TypeScript harness that exercises the real package API.

### User Story 1 - Authenticate Without Raw HTTP (Priority: P1)

As a dashboard developer, I can use one small operator package to issue a login
challenge, submit a signed challenge, and log out without manually wiring raw
HTTP paths and JSON parsing for each auth call.

**Why this priority**: The package is only useful if it removes the need for
raw HTTP in the operator login flow that gates every protected read request.  
**Independent Test**: Instantiate the package against a mocked or local
Guardian server, request a challenge for an allowlisted commitment, submit a
valid signature, call logout, and verify the typed results match the server
responses.

**Acceptance Scenarios**:

1. **Given** a configured client and an allowlisted operator commitment,
   **When** the consumer requests a challenge, **Then** the package returns a
   typed challenge result containing the server-provided domain, commitment,
   nonce, expiry, and signing digest.
2. **Given** a valid signature produced outside the package, **When** the
   consumer calls verify, **Then** the package submits the commitment and
   signature to the server and returns a typed success result without requiring
   raw endpoint construction by the caller.
3. **Given** an authenticated session, **When** the consumer calls logout,
   **Then** the package returns a success result for the server's idempotent
   logout response.

---

### User Story 2 - Read Accounts Through One Client Surface (Priority: P1)

As a dashboard developer, I can use the same operator package to list accounts
and fetch one account detail after login so the dashboard does not need
duplicated transport code for the first operator read-only workflows.

**Why this priority**: Account list and detail are the first server-backed
operator data views after auth; they are the immediate consumer of this new
package.  
**Independent Test**: After establishing a valid operator session in a local or
mocked environment, call the package methods for account list and detail and
verify the typed responses match the server JSON contract.

**Acceptance Scenarios**:

1. **Given** a valid operator session, **When** the consumer requests the
   account list, **Then** the package returns a typed list response containing
   the server's `total_count` and account summaries.
2. **Given** a valid operator session and a known account ID, **When** the
   consumer requests one account detail, **Then** the package returns a typed
   detail response for that account.
3. **Given** an expired session, unknown account, or unavailable account state,
   **When** the consumer calls a read method, **Then** the package surfaces the
   server failure explicitly instead of returning an invented fallback value.

---

### User Story 3 - Stay Lean And Operator-Focused (Priority: P3)

As a frontend team, I can adopt the operator package without pulling in
unrelated Guardian client surfaces so the integration remains easy to reason
about and limited to current operator workflows.

**Why this priority**: The user explicitly asked for a super-lean package. That
goal is lost if the package grows into a second general-purpose Guardian SDK.  
**Independent Test**: Review the public package surface and confirm it only
contains operator auth and account-read methods plus their associated data/error
types.

**Acceptance Scenarios**:

1. **Given** a consumer that only needs operator auth and account reads,
   **When** it imports this package, **Then** it does not need to use broader
   Guardian or multisig client packages for those workflows.
2. **Given** a consumer needs to sign the challenge, **When** it uses this
   package, **Then** the package requires externally supplied commitment and
   signature inputs rather than embedding wallet integration.
3. **Given** a consumer looks for unsupported workflows such as proposal
   mutation or signer management, **When** it inspects this package, **Then**
   those unrelated methods are absent rather than partially implemented.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: A new dedicated TypeScript package for Guardian operator
  workflows MUST be added under `packages/` as an additive client surface.
- **FR-002**: The package MUST expose exactly the current operator dashboard
  HTTP workflows required by features `002` and `003`: request challenge,
  verify signed challenge, logout, list accounts, and fetch account detail.
- **FR-003**: The challenge method MUST call the existing
  `GET /auth/challenge?commitment=<commitment>` server route and return a typed
  result containing, at minimum, `domain`, `commitment`, `nonce`, `expires_at`,
  and `signing_digest`.
- **FR-004**: The verify method MUST accept a caller-provided `commitment` and
  `signature`, submit them to the existing `POST /auth/verify` route, and
  return the typed verify result. The package MUST NOT generate signatures,
  connect to wallets, or derive commitments from addresses.
- **FR-005**: The logout method MUST call the existing `POST /auth/logout`
  route and treat the server's idempotent success response as a successful
  client outcome.
- **FR-006**: The account list and detail methods MUST call the existing
  `GET /dashboard/accounts` and `GET /dashboard/accounts/{account_id}` routes
  and expose typed read models aligned with the server JSON contract.
- **FR-007**: The package MUST use the operator session cookie model already
  defined by the server. It MUST NOT introduce alternative auth tokens,
  `x-pubkey` headers, or multisig request-signing behavior for these methods.
- **FR-008**: The package MUST let the caller control base URL and HTTP
  transport/session behavior. It MUST NOT silently assume cookie persistence in
  runtimes that do not provide it.
- **FR-009**: The package MUST validate required response fields and fail closed
  when the server returns malformed or incomplete JSON for a supported method.
- **FR-010**: The package MUST tolerate additive unknown response fields so that
  non-breaking server-side response expansion does not force immediate package
  breakage.
- **FR-011**: The package MUST surface structured HTTP failure information to
  callers, preserving at least the status code and any machine-readable server
  error payload when available.
- **FR-012**: The package MUST preserve important dashboard failure distinctions
  from the server contract, including at minimum `Unauthorized`,
  `AccountNotFound`, and `AccountDataUnavailable`, without collapsing them into
  a single opaque error case.
- **FR-013**: The package MUST remain operator-focused in v1 and MUST NOT
  expose proposal, delta, signer-management, account-configuration, or other
  unrelated Guardian workflows.
- **FR-014**: The package MUST be usable by the first operator-facing browser
  consumer without that consumer needing to issue raw HTTP calls for the five
  supported operator routes.

### Contract / Transport Impact

- No new Guardian server HTTP endpoints are introduced; this feature consumes
  the existing operator dashboard routes from `002` and `003`.
- No gRPC contract changes are required.
- A new TypeScript package is introduced; existing `packages/guardian-client`
  and `packages/miden-multisig-client` remain unchanged.
- The package consumes the commitment-based operator auth flow and session
  cookie behavior already implemented on the server.
- These methods do not use the existing per-account
  `x-pubkey`/`x-signature`/`x-timestamp` request-auth scheme.
- The first upstream consumer to validate is an operator-facing browser surface
  or a minimal TypeScript harness that exercises the real package API.
- No online/offline multisig fallback behavior changes are introduced.

### Data / Lifecycle Impact

- No new server-side persistent entities or lifecycle states are introduced.
- The package adds client-side models for operator challenge, auth verify
  result, logout result, account summary, and account detail.
- Session lifecycle remains server-owned; the package only participates by
  making HTTP requests in a cookie-capable environment.
- No changes are made to account, proposal, delta, canonicalization, or signer
  lifecycle semantics.
- Backend parity remains a server concern, but the package's typed models MUST
  support semantically equivalent responses from Guardian regardless of whether
  the backing metadata/state store is filesystem or Postgres.

## Edge Cases *(mandatory)*

- A runtime may successfully complete `verify` but fail to retain the session
  cookie; the package must leave that transport responsibility explicit rather
  than pretending session persistence is guaranteed everywhere.
- The server may return `401`, `404`, or `503` for supported methods; the
  package must preserve those distinctions for callers.
- The server may return malformed JSON or omit required fields; the package must
  reject the response rather than invent default values.
- The caller may supply an invalid commitment or malformed signature; the
  resulting server failure must be surfaced without hidden local retries.
- `account_id` path values must remain opaque strings to the package and must
  support properly encoded identifiers rather than only Miden-specific parsing.
- Additive server response fields should not break the package, but removal or
  type changes for required fields should fail fast as a contract error.
- The package must not drift into a second general-purpose Guardian client by
  accumulating proposal or multisig workflows under the same surface.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A TypeScript consumer can complete the full v1 operator workflow
  of challenge, verify, list accounts, fetch account detail, and logout using
  this package plus an external signing integration, with no direct raw HTTP
  calls for those five routes.
- **SC-002**: Automated package-level validation demonstrates that all five
  methods map to the existing Guardian HTTP contract and preserve status-based
  failures such as `401`, `404`, and `503`.
- **SC-003**: The package public API remains limited to operator auth and
  account-read workflows in v1, with no proposal, delta, or multisig mutation
  surfaces exposed.
- **SC-004**: A first operator-facing browser consumer can adopt the package
  without needing to import broader Guardian or multisig SDK packages just to
  communicate with the operator dashboard HTTP surface.

## Assumptions

- The existing operator auth and account-read server routes from `002` and
  `003` are the full v1 operator surface this package needs to cover.
- The primary consumer is a browser-based operator dashboard, but other
  JavaScript runtimes may use the package if they provide explicit cookie-aware
  HTTP transport behavior.
- Challenge signing and commitment discovery come from Miden Wallet or another
  external signer integration outside this package.
- The dedicated operator package is preferable to widening
  `@openzeppelin/guardian-client` because operator workflows are cookie-based
  and dashboard-specific rather than account-auth-header based.

## Dependencies

- Existing Guardian operator dashboard HTTP routes defined by
  `002-operator-auth` and `003-operator-account-apis`.
- The repository's TypeScript packaging, build, and test conventions under
  `packages/`.
- A browser dashboard or minimal TypeScript harness to validate the package as
  an upstream consumer.
