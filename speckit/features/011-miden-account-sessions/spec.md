# Feature Specification: Session Authentication for Miden Account Endpoints

**Feature Branch**: `011-miden-account-sessions`
**Created**: 2026-10-06
**Status**: Draft
**Input**: User description: "Let a wallet authorize a session once so routine Guardian requests stop prompting the wallet (issue #219)"
**Tracking issue**: [#219](https://github.com/OpenZeppelin/guardian/issues/219)

## Context

Every per-account Miden request carries a fresh wallet signature over
`AuthRequestMessage`. For hardware and remote wallets (Ledger, MPC wallets)
that means a prompt for every request, including routine reads such as
`GET /state` or the proposal list during a sync. With ECDSA becoming the
default scheme (#397), that becomes the default experience.

This feature adds a **delegated signer**: a client-held P-256 key that the
wallet authorizes once with a signed **session grant**. Afterwards the
delegated signer signs the same `AuthRequestMessage` a wallet would sign, and
Guardian authorizes the request exactly as before, against the account's
current cosigners and with the same replay protection. Only the key that signs
changes; per-account authentication stays explicit, signed and
replay-protected (constitution principle IV, unchanged).

Sessions remove request prompts, not approval prompts: the wallet still signs
every transaction summary, and the operations listed in FR-009 still require
the wallet itself.

The change is additive. Raw and EIP-712 request authentication, the operator
dashboard sessions and the EVM cookie sessions are unchanged.

## Scope *(mandatory)*

### In Scope

- A session grant, signed once by the wallet, that authorizes a P-256
  delegated signer for every account the signer cosigns on this Guardian.
- Session-signed per-account requests on the routes in FR-008, with the
  existing authorization and replay protection.
- Wallet-only enforcement for the routes in FR-009.
- Logout, wallet-signed revoke-all, and expiry.
- `SessionSigner` support in the Rust and TypeScript SDKs, and an example.
- Proposal signature verification (FR-011), shipped first as its own change (#526).

### Out of Scope

- EVM cookie sessions.
- Per-account scoping (`account_ids` in the grant); v1 states an all-accounts
  scope instead.
- Algorithms other than P-256 for the delegated signer.
- Registering Ledger display metadata (ERC-7730), which would let devices
  label and format grant fields without "Verbose EIP-712" enabled.
- Idle timeouts; sessions have an absolute expiry only.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - A hardware-wallet cosigner syncs without per-request prompts (Priority: P1)

A cosigner using a hardware wallet opens a multisig dApp. The SDK generates a
delegated signer and the wallet shows one session grant, with the signer
commitment, the delegated signer's public key, the expiry, the account scope,
the Guardian key commitment and the network. After the cosigner approves it,
syncing, reading state and listing proposals cause no further wallet prompts
until the session expires or is revoked.

**Why this priority**: This is the problem the feature exists to solve.

**Independent Test**: Register a grant, then call every session-eligible read
route with delegated-signer credentials, and confirm each succeeds without the
wallet key and that the replay protection advances per signer.

**Acceptance Scenarios**:

1. **Given** a cosigner of an account and a valid grant, **When** the
   delegated signer signs `GET /state` for that account, **Then** Guardian
   returns the state and records the request timestamp against the
   cosigner's commitment.
2. **Given** a registered grant, **When** a request is signed by a key that
   has no grant, or by the delegated signer over a different request body,
   **Then** Guardian rejects it with `authentication_failed`.
3. **Given** the signer's last accepted request timestamp for an account,
   **When** a session-signed request for that account arrives with a timestamp
   that is not greater, **Then** Guardian rejects it with
   `authentication_replay`, exactly as it does for wallet requests today. The
   signer's wallet and session requests share one replay floor.

---

### User Story 2 - Proposals are created and signed through the session (Priority: P1)

The cosigner creates a proposal or adds a signature to one. The request to
Guardian is signed by the delegated signer, while the approval itself, the
signature over the transaction summary, is still produced by the wallet.

**Why this priority**: Proposal flows are the main source of request prompts
after reads.

**Independent Test**: With a registered grant, push a proposal and sign it
with delegated-signer request credentials and wallet-produced
transaction-summary signatures, and confirm Guardian verifies the approval
signatures against the transaction summary (FR-011) and accepts the
requests.

**Acceptance Scenarios**:

1. **Given** a registered grant, **When** the cosigner creates a proposal
   whose only signature is their own wallet signature over the transaction
   summary, **Then** Guardian accepts it.
2. **Given** a registered grant, **When** a proposal is created or signed with
   a signature for another cosigner's slot, or a signature that does not
   verify against the transaction summary, **Then** Guardian rejects it.

---

### User Story 3 - Wallet-only operations stay wallet-only (Priority: P1)

Operations that change who controls an account, lock it, or discover
accounts by key keep requiring the wallet even while a session is active.

**Why this priority**: A delegated signer is reachable by any script running
in the page. Limiting what it can do bounds the damage of a compromised page.

**Independent Test**: With a registered grant, call each wallet-only route in
FR-009 with delegated-signer credentials and confirm each is rejected with the
dedicated error code before any state changes, and that the same call signed
by the wallet succeeds.

**Acceptance Scenarios**:

1. **Given** a registered grant, **When** `POST /delta`, `/configure`,
   `POST /delta/candidate/abandon`, `GET /state/lookup` or
   `POST /session/revoke-all` is called with delegated-signer credentials,
   **Then** Guardian rejects it with `wallet_signature_required`.

---

### User Story 4 - Sessions end on expiry, logout, revoke-all or signer removal (Priority: P2)

A session stops working when it expires, when the page logs out, when the
wallet revokes all of its signer's sessions, or when the signer is removed
from an account.

**Why this priority**: A delegated credential is only acceptable if it can be
withdrawn. Page logout alone is not enough: injected script holds the same
key, so the wallet must be able to revoke every session for its signer.

**Independent Test**: For each ending, register a grant, end the session that
way, and confirm the next delegated-signer request is rejected.

**Acceptance Scenarios**:

1. **Given** a session past its `expires_at`, **When** the delegated signer
   signs a request, **Then** Guardian rejects it.
2. **Given** a session, **When** the delegated signer signs
   `POST /session/logout`, **Then** that session is revoked, and logout of an
   unknown or already revoked session succeeds idempotently.
3. **Given** several sessions for one signer, **When** the wallet signs
   `POST /session/revoke-all`, **Then** every session for that signer on this
   Guardian is revoked.
4. **Given** a session whose signer is removed from an account, **When** the
   delegated signer signs a request for that account, **Then** Guardian
   rejects it; if the signer is added back before the grant expires, the
   grant works again.

---

### User Story 5 - The wallet shows what it authorizes (Priority: P2)

EIP-712 wallets display the grant as readable typed data, not an opaque hash,
so the user can see which signer is delegating, to which key, until when, for
which accounts and on which Guardian and network.

**Why this priority**: Signing an opaque `bytes32` is the phishing case.

**Independent Test**: Build the typed data for a fixture grant, confirm every
field in FR-003 is a top-level member of the struct, and confirm the digest
matches an independent EIP-712 implementation.

---

### User Story 6 - Clear failure against a Guardian without sessions (Priority: P3)

A new SDK pointed at an older Guardian, or at a Guardian that has not enabled
sessions, gets a clear error when it tries to start a session and does not
silently fall back to per-request wallet signing.

**Independent Test**: Start a session against a server without the feature and
confirm the SDK raises a dedicated error.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001 — Additive change**: Raw and EIP-712 per-account request
  authentication MUST behave exactly as today. Session credentials MUST NOT be
  routed through `Auth::verify`. A client that asks a Guardian without session
  support to start a session MUST receive an explicit error, with no silent
  fallback.
- **FR-002 — Delegated signer algorithm**: v1 supports one algorithm: ECDSA
  over P-256 with SHA-256, as WebCrypto produces it (WebCrypto ECDSA always
  hashes and has no prehash mode). The delegated signer signs the 32-byte
  `AuthRequestMessage` word; the signature is raw `r || s` (64 bytes) and the
  public key is SEC1-compressed (33 bytes). Shared test vectors MUST pin this
  encoding in Rust and TypeScript. Browsers SHOULD create the key with
  `extractable: false`; non-extractability is a browser property and is not
  enforced by Guardian.
- **FR-003 — Session grant contents**: A grant binds:
  - the signer commitment of the wallet key that signs it;
  - the delegated signer's public key;
  - `issued_at` and `expires_at` (Unix seconds);
  - the account scope. In v1 the scope is fixed: every account this signer
    cosigns on this Guardian, now or later, until the grant expires. The
    signed message MUST state that scope in words;
  - the Guardian ACK-key commitment for the wallet's signature scheme;
  - the Miden network.
- **FR-004 — Grant encoding**: EIP-712 wallets MUST sign the grant as typed
  data in which every field of FR-003 is a readable top-level member, with no
  opaque digest standing in for them. Falcon and raw ECDSA wallets MUST sign a
  domain-separated RPO digest of the same fields, distinct from
  `AuthRequestMessage` and `LookupAuthMessage` digests. Shared test vectors MUST
  pin both encodings, and the EIP-712 digest MUST match an independent
  implementation.
- **FR-005 — Grant registration**: `POST /session` (gRPC `CreateSession`) MUST
  register a grant only if:
  - the wallet signature verifies for the claimed scheme and the signer
    commitment matches the signing key;
  - the ACK-key commitment and network match this Guardian;
  - `issued_at` is within the request clock-skew window;
  - `expires_at` is in the future relative to server time, its remaining
    lifetime is strictly longer than the clock-skew window, and it is no later
    than server time plus the configured maximum lifetime;
  - the delegated signer public key is a valid P-256 point.

  Checks that need no cryptography MUST run before signature verification.
- **FR-006 — Grant lifecycle**: Re-submitting the same grant while its session
  is live MUST succeed and return the same expiry. A revoked session MUST stay
  revoked until its `expires_at`, so re-submitting its grant fails; existing
  session-store upserts that clear `revoked_at` MUST NOT be used for this
  realm. Records MUST be removed after expiry; there are no permanent
  tombstones. The default and maximum lifetime is 8 hours, matching operator
  and EVM sessions; operators MAY configure a shorter maximum, which MUST be
  longer than the clock-skew window.
- **FR-007 — Request authentication**: For a request with
  `x-auth-format: session`, `resolve_account` MUST, before and instead of
  `Auth::verify`:
  - verify the delegated signer's signature over `AuthRequestMessage`;
  - resolve the active grant for that public key;
  - re-check that the grant's ACK-key commitment and network still match this
    Guardian for the account's scheme, so key rotation ends sessions;
  - require the grant's signer commitment to be a current cosigner of the
    account;
  - apply the existing replay CAS keyed by the grant's signer commitment, not
    by the delegated signer key, so timestamps strictly increase per signer
    across wallet and session requests, including parallel session reads.
- **FR-008 — Session-eligible routes**: A delegated signer MAY sign:
  - reads: `GET /state`, `GET /state/nonce`, `GET /delta`, `GET /delta/since`,
    `GET /delta/history`;
  - proposal list and get: `GET /delta/proposal`, `GET /delta/proposal/single`;
  - execution status queries;
  - proposal create and sign: `POST /delta/proposal`, `PUT /delta/proposal`.
- **FR-009 — Wallet-only routes**: The wallet MUST sign `POST /delta`,
  `POST /delta/proposal/execution` (#254, when it exists),
  `POST /delta/candidate/abandon`, `POST /configure`, `GET /state/lookup` and
  `POST /session/revoke-all`. The server MUST enforce this list and reject
  delegated-signer credentials on these routes with the stable code
  `wallet_signature_required`, before any state change. HTTP and gRPC MUST
  apply the same list.
- **FR-010 — Approvals stay with the wallet**: Proposal approval signatures
  over the transaction summary MUST still come from the wallet key. A
  delegated signer cannot approve a transaction.
- **FR-011 — Proposal signature verification (prerequisite)**: Before this
  feature ships, in its own change (#526):
  - `sign_delta_proposal` MUST verify Falcon and raw ECDSA signatures against
    the transaction summary, as it already does for EIP-712;
  - every stored signature's `signer_id` MUST be a current cosigner and match
    the verified key;
  - on create, `push_delta_proposal` MUST accept only the caller's own
    signature.
- **FR-012 — Logout**: `POST /session/logout` (gRPC `RevokeSession`), signed by
  the delegated signer over a domain-separated logout message with a
  timestamp in the skew window, MUST revoke that session. It is idempotent.
- **FR-013 — Revoke all**: `POST /session/revoke-all`, signed by the wallet
  over a domain-separated, account-less message with a timestamp in the skew
  window, MUST revoke every session of that signer on this Guardian. It is
  idempotent.
- **FR-014 — Signer removal**: Removing a signer from an account MUST end
  delegated access to that account on the next request (FR-007). Adding the
  signer back restores any grant that has not expired; this MUST be
  documented.
- **FR-015 — Advertisement**: `GET /status` MUST advertise whether sessions are
  enabled and the maximum lifetime, so SDKs can fail early (FR-001).
- **FR-016 — SDK `SessionSigner`**: Session handling lives in the Guardian SDKs
  as a `SessionSigner`, not in each dApp: key generation and storage, building
  and signing the grant through the wallet, choosing the wallet or the
  delegated signer per route (FR-008, FR-009), and renewal. When a grant
  expires the SDK MUST discard the key; a new session needs a new grant. The
  TypeScript SDK keeps the key non-extractable and MAY persist it in
  IndexedDB. The Rust SDK uses the same `x-auth-format: session` path with an
  in-memory key.
- **FR-017 — Observability**: Guardian MUST log grant registration, rejection
  reason category, logout and revoke-all with the signer commitment, and MUST
  NOT log signatures or session public keys in full.
- **FR-018 — Rate limiting**: `POST /session`, `POST /session/logout` and
  `POST /session/revoke-all` MUST be covered by the existing per-IP rate
  limiter, since `POST /session` performs a signature verification before the
  caller is known.

### Contract / Transport Impact

- New HTTP endpoints `POST /session`, `POST /session/logout`,
  `POST /session/revoke-all`; new gRPC RPCs `CreateSession`, `RevokeSession`,
  `RevokeAllSessions`, kept in parity in both proto files and in the OpenAPI
  documents.
- New `x-auth-format: session` value on existing per-account routes; existing
  `raw` and `eip712` values are unchanged.
- New stable error code `wallet_signature_required`, added to the TypeScript
  error-code list.
- `GET /status` gains an optional sessions block.
- Rust client and TypeScript clients (`guardian-client`,
  `miden-multisig-client`) gain `SessionSigner`; at least one example
  exercises the flow end to end.

### Data / Lifecycle Impact

- A new `miden` realm and `SessionSubject` in `auth_sessions`, keyed by the
  SHA-256 digest of the delegated signer public key, storing the signer
  commitment, ACK-key commitment, network and expiry. No schema migration: the
  realm column is free text.
- Filesystem deployments use the in-memory session store with the same
  semantics; sessions are lost on restart and clients start a new session.
- No change to account, proposal, delta or replay-state records, beyond the
  proposal-signature checks of FR-011.

## Edge Cases *(mandatory)*

- **Grant re-submitted after a dropped response**: succeeds while live (FR-006).
- **Grant re-submitted after logout or revoke-all**: rejected until it would
  have expired.
- **Grant signed long before submission**: rejected by the `issued_at` skew
  check.
- **Clock skew between client and server**: grant lifetime is capped against
  server time; SDKs request less than the advertised maximum to absorb drift.
- **Guardian ACK-key rotation or network change**: existing sessions stop
  being accepted on the next request (FR-007).
- **Parallel tabs sharing one stored key**: requests share the signer's replay
  CAS; a lost race returns `authentication_replay` and the SDK re-signs.
- **Delegated signer used on a wallet-only route**:
  `wallet_signature_required`, no state change.
- **Signer removed, then added back**: access ends, then returns for the
  unexpired grant (FR-014).
- **Account the signer is added to after the grant**: covered, because the v1
  scope is every account the signer cosigns; the signed message says so.
- **Compromised page**: injected script can use the delegated signer until the
  session ends. It cannot approve transactions or call wallet-only routes.
  Accepted risk: it can fill `max_pending_proposals` for the signer's accounts
  until revoked. Page logout does not help, because the script holds the same
  key; revoke-all from the wallet does.
- **Hardware wallets without registered display metadata**: devices may need
  a setting such as Ledger's "Verbose EIP-712" to display typed data field by
  field; documented in the SDK guide.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: After one grant, a full SDK sync and proposal listing produce
  zero wallet prompts.
- **SC-002**: Every wallet-only route rejects delegated-signer credentials
  with `wallet_signature_required` on HTTP and gRPC, verified by tests.
- **SC-003**: Tests demonstrate rejection of: unknown delegated signer,
  signature over another body, expired grant, revoked grant, re-submitted
  revoked grant, grant for another Guardian key or network, stale `issued_at`,
  over-long lifetime, removed signer, and a rotated ACK key.
- **SC-004**: Rust and TypeScript produce byte-identical grant, logout and
  revoke-all digests and EIP-712 digests for the shared vectors, and the
  EIP-712 digests match an independent implementation.
- **SC-005**: Raw and EIP-712 request-auth test suites pass unchanged.
- **SC-006**: Proposal-signature tests (FR-011) reject an unverifiable
  signature, a signature in another cosigner's slot, and a non-cosigner
  `signer_id`, on create and on sign.

## Assumptions

- The 5-minute request clock-skew window stays as today.
- Session records are small and few per signer; no per-signer cap is needed in
  v1 beyond rate limiting.

## Dependencies

- FR-011 proposal-signature verification, shipped first in its own change (#526).
- Existing `auth_sessions` store, sweep and coordination handles.
- Existing replay CAS and request clock-skew window.

## Clarifications

### Session 2026-10-06

- Q: Does delegating the request signer fit constitution principle IV? → A: Yes, as written: every account request stays signed and replay-protected, and only the key that signs `AuthRequestMessage` changes. No constitution change. The session key is called a delegated signer.
- Q: Is P-256 acceptable? → A: Yes, as the only v1 algorithm: ECDSA-P256 with SHA-256 over the 32-byte `AuthRequestMessage` word, raw `r || s`, pinned with shared test vectors. secp256k1 and Falcon cannot back a non-extractable browser key.
- Q: Which routes may a delegated signer use? → A: The split in FR-008 and FR-009, enforced by the server with its own error code.
- Q: What does a grant cover? → A: Every account the signer cosigns on this Guardian until it expires, stated in the signed message (proposed 2026-10-06 — pending confirmation). `account_ids` scoping is out of scope for v1.
- Q: Is page logout enough? → A: No. Add a wallet-signed revoke-all for the signer.

### Open for review

- The error code name `wallet_signature_required` and its status (proposed: HTTP 403, gRPC `PERMISSION_DENIED`).
- Whether sessions are enabled by an operator setting and whether the grant also carries an operator-chosen display name for the Guardian, or are always on with the ACK-key commitment and network as the only Guardian identity.
- Whether the EIP-712 struct carries a human-readable expiry string next to `expires_at`, since devices without display metadata show the raw number.
- Whether `POST /delta` and abandon stay session-eligible (raised in https://github.com/OpenZeppelin/guardian/issues/219#issuecomment-6024637794): the transition is authorized by the wallet-made approvals and the on-chain threshold, so a stolen session can at most grief, as with the accepted pending-proposals risk; wallet-signing `/delta` costs a second device prompt for a summary the executing cosigner already approved.
