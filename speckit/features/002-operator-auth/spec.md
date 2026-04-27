# Feature Specification: Guardian Operator Dashboard Authentication (Signed Requests)

**Feature Key**: `002-operator-auth`  
**Suggested Branch**: `002-operator-auth` (manual creation optional)  
**Created**: 2026-04-18  
**Status**: Draft  
**Input**: User description: "Guardian operator dashboard authentication via signed requests (challenge-response with ephemeral server-side sessions)."

## Context

Guardian needs a read-only operator dashboard that exposes accounts, transactions, signers, and proposals to a small, trusted set of human operators. Operators already possess cryptographic keypairs (the same class of keys used elsewhere in the Miden multisig stack), so password-based or third-party identity providers add unnecessary surface area and external dependencies.

This feature defines the authentication protocol only: how a registered operator proves possession of their private key to establish an authenticated browser session against the dashboard surface served by the existing Guardian server, and how the backend validates subsequent requests. It does not define the dashboard UI, its data views, or per-operator permission scoping — all operators authorized here have equal access in v1.

Layers affected: a new authentication surface in the existing Guardian server (challenge/verify/logout endpoints plus session middleware) and a configuration channel for the operator allowlist. V1 is Falcon-only and assumes challenge signing happens in-browser with a Falcon signer. No changes are required to the multisig protocol, proposal lifecycle, or existing gRPC/HTTP client SDKs.

## Scope *(mandatory)*

### In Scope

- Configuration-driven operator allowlist derived from serialized Falcon public keys, with operator identity set to the public key commitment in v1.
- Local operator configuration through a JSON file for smoke testing and local development.
- Deployed operator configuration through an AWS Secrets Manager secret containing the same JSON payload.
- Challenge-response login protocol (`GET /auth/challenge`, `POST /auth/verify`) with domain-bound, short-lived, single-use nonces.
- Issuance and validation of opaque, HttpOnly, Secure, SameSite=Strict session cookies after successful signature verification.
- Server-side session storage with absolute (non-sliding) expiry.
- Authentication middleware that protects dashboard API endpoints and attaches the authenticated operator identity to the request context.
- Explicit logout (`POST /auth/logout`) that invalidates the server-side session record.
- Operator revocation by removing the public key from the configured JSON file or AWS secret, with the backend rejecting affected sessions after the next allowlist reload.

### Out of Scope

- The dashboard UI itself and any data-view endpoints it consumes.
- Per-operator permission scoping, role-based access, or multi-tenancy (all authenticated operators have equal access in v1).
- Password fallback, OAuth/SSO, magic links, or WebAuthn.
- Operator self-service key rotation workflows beyond editing the configured public-key source.
- Detailed wallet or browser signer UX beyond the requirement that the dashboard obtain an in-browser Falcon signature.
- Changes to the multisig gRPC/HTTP SDKs, proposal lifecycle, or signer-set semantics.
- Audit-log persistence or external observability integrations (basic application logging is assumed; structured audit trail is a follow-up).

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Operator Establishes an Authenticated Dashboard Session (Priority: P1)

A registered Guardian operator visits the dashboard in their browser. They are prompted to sign a challenge with their registered Falcon key. After providing a valid signature, they receive a session cookie that transparently authenticates all subsequent dashboard requests until it expires or they log out.

**Why this priority**: This is the core login flow. Without it, no protected dashboard endpoint is reachable, and every other feature in the dashboard is blocked.

**Independent Test**: With an operator public key present in configuration, issue a challenge for the derived commitment, sign the returned payload in-browser with the matching Falcon private key, submit the signature to the verify endpoint, and confirm the response sets a session cookie and that a subsequent protected request succeeds using only that cookie.

**Acceptance Scenarios**:

1. **Given** an operator public key is listed in the allowlist source and the operator possesses the matching private key, **When** the operator requests a challenge for the derived commitment and submits a valid signature over the returned payload within the nonce's validity window, **Then** the verify endpoint returns success, sets a single HttpOnly, Secure, SameSite=Strict session cookie, and the session is recorded server-side with an absolute expiry.
2. **Given** a valid session cookie, **When** the operator calls a protected dashboard endpoint, **Then** the request succeeds and the operator identity is available to the handler.
3. **Given** no session cookie (or an unknown/expired one), **When** the operator calls a protected dashboard endpoint, **Then** the server responds with 401 Unauthorized and does not leak operator-allowlist information.

---

### User Story 2 - Invalid or Replayed Signatures Are Rejected (Priority: P1)

A malicious actor attempts to authenticate as a registered operator without possessing the private key, or attempts to reuse a previously observed signature.

**Why this priority**: The entire security posture of the dashboard depends on this. A weak verify path means the auth layer is worse than no auth, because it creates a false sense of safety.

**Independent Test**: Exercise each rejection path individually (unknown commitment, allowlisted commitment with invalid signature, valid signature over a stale/expired nonce, valid signature over a consumed nonce, valid signature over a payload bound to a different domain) and confirm each returns a non-success response, does not issue a session cookie, and does not consume or create server-side state beyond nonce accounting.

**Acceptance Scenarios**:

1. **Given** a challenge was issued and its signed payload was submitted successfully once, **When** the same `{commitment, signature}` is submitted again, **Then** verification is rejected because the nonce has already been consumed.
2. **Given** a challenge was issued more than the configured nonce lifetime ago, **When** a valid signature over that payload is submitted, **Then** verification is rejected due to expiry.
3. **Given** a challenge issued for domain A, **When** the same signature is replayed against a server configured for domain B, **Then** verification is rejected because the signed payload is bound to the issuing domain.
4. **Given** a commitment that is not in the operator allowlist, **When** a challenge is requested, **Then** the server either declines to issue a challenge or issues one that cannot ultimately produce a session, and in no case is a session ever created for a non-allowlisted key.
5. **Given** a valid commitment in the allowlist, **When** an invalid signature is submitted, **Then** verification is rejected and the nonce is not consumed, preserving the operator's ability to retry.

---

### User Story 3 - Operator Logout and Revocation (Priority: P2)

An operator explicitly logs out at the end of their session, or an administrator removes an operator public key from the configured source. In either case, all existing sessions for that operator must stop working once the backend reloads the allowlist.

**Why this priority**: Bounded-lifetime sessions are only meaningful if operators can also revoke early. This story is what makes "remove an operator" a believable security control rather than a pending action.

**Independent Test**: After establishing a session, call the logout endpoint and confirm the session cookie stops being accepted. Separately, remove an operator public key from the configured JSON file or AWS secret, reload the allowlist, and confirm any previously issued sessions for that operator are rejected.

**Acceptance Scenarios**:

1. **Given** an authenticated session, **When** the operator calls `POST /auth/logout`, **Then** the server-side session record is deleted and subsequent requests using the same cookie return 401.
2. **Given** an operator public key is removed from the configured JSON file or AWS secret and the backend reloads its allowlist, **When** a previously issued session cookie for that operator is presented, **Then** the request is rejected with 401 regardless of remaining session lifetime.
3. **Given** a session has passed its absolute expiry, **When** any protected request is made with its cookie, **Then** the server treats it as unauthenticated and the stored record is cleaned up.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001 — Operator allowlist configuration**: The dashboard backend MUST read an operator allowlist from a configured source containing serialized Falcon public keys. Local and smoke-test deployments MUST support a JSON file source. AWS deployments MUST support an AWS Secrets Manager secret source containing the same JSON payload. Each entry MUST be a serialized Falcon public key; the backend MUST derive the operator commitment from that public key and use the commitment as the v1 operator identifier. Malformed public keys or duplicate derived commitments MUST be treated as a configuration error and prevent successful allowlist loading.
- **FR-002 — Challenge issuance**: `GET /auth/challenge?commitment=<encoded_commitment>` MUST, when the commitment is present in the allowlist, return a fresh nonce and its absolute expiry timestamp, and record the nonce server-side keyed by `(commitment, nonce)` with a short validity window. The server MUST NOT reveal whether an unknown commitment is unknown versus rate-limited; it MAY return an indistinguishable response (either a decoy or a uniform error) for unknown commitments to reduce allowlist enumeration.
- **FR-003 — Domain-bound signed payload**: The payload the operator signs MUST include at minimum the nonce, the server's canonical domain (host identity), and the nonce's expiry. The server MUST reject any verification whose signed payload does not exactly match what the server would reconstruct for the referenced nonce.
- **FR-004 — Signature verification**: `POST /auth/verify` accepting `{commitment, signature}` MUST validate, in order: commitment is in the allowlist; a matching unused, unexpired nonce exists; the signature's embedded Falcon public key reconstructs to the allowlisted commitment; and the signature verifies against the reconstructed payload. Any failure MUST return an error response without issuing a session.
- **FR-005 — Nonce consumption**: On successful verification, the nonce MUST be consumed atomically (single-use). On failed verification due to an invalid signature, the nonce MUST NOT be consumed, so that an honest operator who mis-signed can retry without requesting a new challenge. On expiry or explicit nonce cleanup, entries MUST be removed from server-side storage.
- **FR-006 — Session issuance**: On successful verification, the server MUST generate a cryptographically random opaque session token of at least 32 bytes, record `{token → operator_id, commitment, issued_at, expires_at}` server-side, and return the token in a single `Set-Cookie` header with the attributes `HttpOnly`, `Secure`, `SameSite=Strict`, `Path=/`, and `Expires`/`Max-Age` matching the session's absolute expiry. The token MUST NOT be derivable from the commitment or nonce.
- **FR-007 — Session expiry policy**: Sessions MUST use absolute expiry with no sliding renewal. The default absolute lifetime is 8 hours; the exact value MUST be configurable but MUST NOT exceed 24 hours in v1.
- **FR-008 — Authentication middleware**: All dashboard API endpoints, except the three auth endpoints themselves and any explicitly whitelisted health/metadata endpoint, MUST pass through middleware that extracts the session cookie, looks up the corresponding server-side record, verifies it is present and unexpired, re-confirms the operator is still in the current allowlist, and attaches the operator identity to the request context. Any failing check MUST produce a 401 response without revealing which check failed.
- **FR-009 — Logout**: `POST /auth/logout`, when called with a valid session cookie, MUST delete the server-side session record and return a response that clears the cookie on the client (e.g., `Set-Cookie` with `Max-Age=0`). Logout MUST be idempotent — calling it with an already-invalid cookie MUST still return success.
- **FR-010 — Revocation by allowlist change**: When the backend's operator allowlist is reloaded from the configured JSON file or AWS secret, sessions whose operator is no longer in the allowlist MUST be rejected at the next request. Implementations MAY eagerly purge affected session records, but the security-relevant guarantee is rejection at request time.
- **FR-011 — Allowlist source precedence**: If both the AWS secret source and local file source are configured, the AWS secret source MUST take precedence so deployed instances cannot accidentally authenticate from a local test file.
- **FR-012 — TLS enforcement**: The auth endpoints MUST only be served over TLS in any non-local environment. Serving them over plain HTTP in production is a configuration error. Local development MAY relax the `Secure` cookie attribute but MUST NOT be the default and MUST require an explicit opt-in flag.
- **FR-013 — Observable authentication events**: The backend MUST emit structured log events for every auth decision (challenge issued, verify success, verify failure with reason category, logout, session rejected, allowlist reload) including the operator identifier when known and a correlation ID, but MUST NOT log raw signatures, private data, or full session tokens.
- **FR-014 — Rate limiting**: The `challenge` and `verify` endpoints MUST be rate-limited per source IP and per commitment to constrain brute-force and enumeration attempts. Specific limits are implementation-defined but MUST exist from v1.

### Contract / Transport Impact

- Introduces three new HTTP endpoints (`GET /auth/challenge`, `POST /auth/verify`, `POST /auth/logout`) on the existing Guardian server for the dashboard surface. Existing Guardian gRPC surfaces and multisig HTTP surfaces are unchanged.
- No changes to the Rust or TypeScript multisig client SDKs. The dashboard's own frontend client (out of scope here) will be the only consumer of these endpoints.
- The auth cookie is an opaque bearer credential scoped to the dashboard backend; it is not used on any multisig SDK transport and does not interact with existing auth headers, if any.
- No change to fallback behavior across online/offline multisig flows; the dashboard is strictly online.

### Data / Lifecycle Impact

- Introduces two short-lived server-side records: **nonces** (keyed by `(commitment, nonce)`, TTL = nonce validity window) and **sessions** (keyed by token, TTL = absolute session expiry).
- No changes to account, proposal, signer, or transaction entities or their state transitions.
- Operator configuration is a runtime allowlist source, not persistent Guardian account metadata. Updating the configured file or AWS secret MUST NOT require a database migration or account-state mutation.
- Storage backend for nonces and sessions is a plan-phase decision; an in-process map is acceptable for a single-instance deployment, and a shared store is required if the backend runs multiple replicas. The specification imposes no filesystem/Postgres parity requirement because these records are ephemeral and are not part of the multisig data plane.

## Edge Cases *(mandatory)*

- **Replay of a signed verify payload**: Prevented by single-use nonces; second submission of the same `{commitment, signature}` must fail even if the signature itself remains cryptographically valid.
- **Cross-domain replay**: Prevented by binding the signed payload to the server's canonical domain; a signature captured against environment A must not authenticate against environment B.
- **Stale nonce**: A signature over an expired nonce must be rejected even if the signature is otherwise valid; the operator must request a fresh challenge.
- **Clock skew between operator and server**: The server is the sole authority on nonce and session expiry; operator-side timestamps are not trusted. Any included timestamp in the signed payload must match the server's recorded expiry for that nonce, not an operator-supplied value.
- **Concurrent challenges for the same commitment**: Multiple outstanding nonces for the same commitment are allowed; each is independently single-use. An upper bound per commitment should be enforced to prevent memory exhaustion.
- **Signature over a mutated payload**: If the client signs a payload whose structure does not exactly match the server's reconstruction (field order, encoding, whitespace, canonicalization), verification must fail closed. The server MUST use a fixed, documented canonical encoding for the signed payload.
- **Allowlist hot-reload during an active request**: Reload must be atomic from the middleware's perspective; a request must see either the pre- or post-reload allowlist, never a partial view.
- **Multiple concurrent sessions for the same operator**: Allowed by default (new login does not invalidate earlier sessions), which preserves operator usability across devices. Each session is independently revocable via logout.
- **Session cookie theft**: Mitigated — not eliminated — by `HttpOnly`, `Secure`, `SameSite=Strict`, TLS, and bounded absolute expiry. Out-of-scope mitigations include token binding and device attestation.
- **Misconfigured operator entry** (malformed Falcon public key, duplicate derived commitment): MUST fail allowlist loading loudly rather than silently skipping the entry.
- **Operator removed from allowlist mid-session**: Next request is rejected even if the session has remaining lifetime.
- **Nonce or session storage loss on restart** (if using in-memory storage): All active sessions are invalidated on restart. This is an acceptable operational behavior for v1 and MUST be explicitly documented.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001 — Login success path works end-to-end**: A registered operator can move from "no session" to "authenticated request accepted" in a single round-trip after signing one challenge, with no manual intervention beyond producing the signature.
- **SC-002 — Unauthorized access is uniformly blocked**: 100% of protected dashboard endpoints return 401 when called without a valid session cookie, including the cases of missing cookie, expired session, tampered cookie value, and session whose operator was removed from the allowlist.
- **SC-003 — Replay and cross-domain attacks are blocked**: Automated tests demonstrate rejection of every edge case enumerated above (replayed signature, expired nonce, cross-domain payload, mutated payload, invalid signature), with no code path producing a session cookie for any of them.
- **SC-004 — Bounded session lifetime**: No session remains accepted beyond its configured absolute expiry (verified by a test that advances the clock or waits out a short-lifetime configuration).
- **SC-005 — Operational revocation is fast**: Removing an operator public key from the configured JSON file or AWS secret and reloading the allowlist causes any session for that operator to be rejected on its next request, without a process restart when the configured source path or secret ID stays the same.
- **SC-006 — No sensitive material in logs**: Inspection of logs produced during the full test suite shows operator identifiers and correlation IDs but no raw signatures, no full session tokens, and no private keys.
- **SC-007 — Low-friction operator onboarding**: Adding a new operator requires only appending one serialized Falcon public key to the configured JSON file or AWS secret; no database migration, no code change, and no per-operator private secret provisioned by the service.

## Assumptions

- The dashboard backend served by the existing Guardian server runs behind TLS in all non-local environments; this is a deployment requirement, not a software feature. Browsers will reject `Secure` cookies over plain HTTP, so a non-TLS production deployment is self-defeating rather than quietly insecure.
- Operators are a small, bounded set (order of 10, not hundreds). Design choices that would not scale to thousands of operators are acceptable.
- Operators already possess the Falcon private keys corresponding to public keys registered in configuration and access the dashboard from a browser environment where a Falcon signer can sign the challenge payload.
- Local smoke testing uses a JSON file containing an array of serialized Falcon public key hex strings. Deployed instances use an AWS Secrets Manager secret with the same JSON shape.
- The dashboard backend runs as a single logical instance for v1. Multi-replica deployments are a plan-phase concern that, if adopted, will force the nonce/session store to be shared.
- Clock drift between the backend and real time is small (single-digit seconds). The server is the sole authority on expiry, and clients do not enforce timestamps.
- An application-level logger already exists and will be used for auth events; structured audit persistence is a follow-up feature, not a prerequisite.
- The existing Guardian stack already exposes Falcon signature verification and can invoke it from the dashboard backend without new cryptographic primitives.

## Dependencies

- Existing Guardian Falcon signature-verification primitives reused without modifying multisig client behavior.
- The existing Guardian server process (`crates/server`), which will host the auth endpoints and middleware for the dashboard surface.
- Standard library–level cryptographically secure random number generation for nonces and session tokens.
- A TLS-terminating deployment environment (existing Guardian production infrastructure).

## Clarifications

### Session 2026-04-20

- Q: Which process hosts the auth endpoints and middleware? → A: The existing Guardian server (`crates/server`).
- Q: Which operator signing schemes are in scope for v1? → A: Falcon only.
- Q: How do operators sign the challenge payload? → A: In-browser with a Falcon signer; the smoke path uses a generated local Falcon key.

### Session 2026-04-24

- Q: How should operator public keys be configured? → A: Local testing uses a JSON file containing serialized Falcon public keys; deployed instances use an AWS Secrets Manager secret with the same JSON payload.
- Q: Can operators be added without restarting Guardian? → A: Yes, if the configured file path or secret ID stays the same; the backend reloads the allowlist source during operator auth checks.
