# Feature Specification: Session Authentication for Miden Account Endpoints

**Feature Branch**: `011-miden-account-sessions`
**Created**: 2026-10-06
**Status**: Draft (review round 2, 2026-10-07)
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
current cosigners and with replay protection. Only the key that signs
changes; per-account authentication stays explicit, signed and
replay-protected (constitution principle IV, unchanged).

Sessions remove request prompts, not approval prompts: the wallet still signs
every transaction summary, and every route outside FR-008 still requires the
wallet itself.

The change is additive. Raw and EIP-712 request authentication, the operator
dashboard sessions and the EVM cookie sessions are unchanged.

## Scope *(mandatory)*

### In Scope

- A session grant, signed once by the wallet, that authorizes a P-256
  delegated signer for every account the signer cosigns on this Guardian,
  naming the website that asked for it.
- Session-signed per-account requests on the routes in FR-008; every other
  route is wallet-only (FR-009).
- Logout, wallet-signed revoke-all, expiry, and distinct codes for each.
- `SessionSigner` support in the Rust and TypeScript SDKs, and an example.

### Out of Scope

- EVM cookie sessions.
- Per-account scoping (`account_ids` in the grant); v1 states an all-accounts
  scope instead.
- Algorithms other than P-256 for the delegated signer.
- Registering Ledger display metadata (ERC-7730), which would let devices
  label and format grant fields without "Verbose EIP-712" enabled.
- Idle timeouts; sessions have an absolute expiry only.
- Session-signed `POST /delta`; it follows once #524 lands (see FR-009).

## User Scenarios & Testing *(mandatory)*

### User Story 1 - A hardware-wallet cosigner syncs without per-request prompts (Priority: P1)

A cosigner using a hardware wallet opens a multisig dApp. The SDK generates a
delegated signer and the wallet shows one session grant: the signer
commitment, the delegated signer's public key, the website, the lifetime with
a readable expiry, the account scope, the Guardian key commitment and the
network. After the cosigner approves it, syncing, reading state and listing
proposals cause no further wallet prompts until the session expires or is
revoked.

**Why this priority**: This is the problem the feature exists to solve.

**Independent Test**: Register a grant, then call every session-eligible read
route with delegated-signer credentials, and confirm each succeeds without the
wallet key.

**Acceptance Scenarios**:

1. **Given** a cosigner of an account and a valid grant, **When** the
   delegated signer signs `GET /state` for that account, **Then** Guardian
   returns the state and advances the delegated signer's replay floor for
   that account.
2. **Given** a registered grant, **When** a request is signed by a key that
   has no grant, or by the delegated signer over a different request body,
   **Then** Guardian rejects it with `authentication_failed`.
3. **Given** the delegated signer's last accepted request timestamp for an
   account, **When** it signs a request for that account with a timestamp that
   is not greater, **Then** Guardian rejects it with `authentication_replay`.

---

### User Story 2 - Proposals are created and signed through the session (Priority: P1)

The cosigner creates a proposal or adds a signature to one. The request to
Guardian is signed by the delegated signer, while the approval itself, the
signature over the transaction summary, is still produced by the wallet.

**Why this priority**: Proposal flows are the main source of request prompts
after reads.

**Independent Test**: With a registered grant, push a proposal and sign it
with delegated-signer request credentials and wallet-produced
transaction-summary signatures, and confirm Guardian accepts the requests and
verifies the approvals as #526 does.

**Acceptance Scenarios**:

1. **Given** a registered grant, **When** the cosigner creates a proposal
   whose only signature is their own wallet signature over the transaction
   summary, **Then** Guardian accepts it.
2. **Given** a registered grant, **When** a proposal is created or signed with
   a signature for another cosigner's slot, or a signature that does not
   verify against the transaction summary, **Then** Guardian rejects it.

---

### User Story 3 - Wallet-only operations stay wallet-only (Priority: P1)

Operations that commit state, change who controls an account, or discover
accounts by key keep requiring the wallet even while a session is active, and
so does every route not explicitly opened to sessions.

**Why this priority**: A delegated signer is reachable by any script running
in the page. Limiting what it can do bounds the damage of a compromised page.

**Independent Test**: With a registered grant, call each wallet-only route
with valid delegated-signer credentials and confirm each is rejected with the
dedicated error code before any state changes; call them with a forged
delegated signature and confirm `authentication_failed`; and confirm the same
calls signed by the wallet succeed.

**Acceptance Scenarios**:

1. **Given** a registered grant, **When** `POST /delta`,
   `POST /delta/proposal/execution` (#254), `POST /delta/candidate/abandon`,
   `POST /configure`, `GET /state/lookup` or `POST /session/revoke-all` is
   called with valid delegated-signer credentials, **Then** Guardian rejects
   it with `wallet_signature_required`.
2. **Given** a delegated signature that does not verify, **When** it is sent
   to any wallet-only route, **Then** Guardian rejects it with
   `authentication_failed`, revealing nothing about the route's policy.

---

### User Story 4 - Sessions end on expiry, logout, revoke-all or signer removal (Priority: P2)

A session stops working when it expires, when the page logs out, when the
wallet revokes all of its signer's sessions, or when the signer is removed
from an account. The SDK learns which, so it can start a new session instead
of guessing.

**Why this priority**: A delegated credential is only acceptable if it can be
withdrawn. Page logout alone is not enough: injected script holds the same
key, so the wallet must be able to revoke every session for its signer, and a
stolen session must not be able to stop it.

**Independent Test**: For each ending, register a grant, end the session that
way, and confirm the next delegated-signer request is rejected with the
expected code.

**Acceptance Scenarios**:

1. **Given** a session past its `expires_at`, **When** the delegated signer
   signs a request, **Then** Guardian rejects it with `session_expired`.
2. **Given** a session, **When** the delegated signer signs
   `POST /session/logout`, **Then** that session is revoked and its next
   request fails with `session_revoked`; logout of an unknown or already
   revoked session succeeds idempotently once the delegated signature
   verifies.
3. **Given** several sessions for one signer, **When** the wallet signs
   `POST /session/revoke-all` at time T, **Then** every session of that
   signer issued at or before T is revoked, and a replay of that request
   cannot revoke a session issued after T.
4. **Given** a stolen session that stamps its requests at the edge of the
   clock-skew window, **When** the wallet signs a request with the current
   time, **Then** Guardian accepts it, and the wallet can still revoke the
   session.
5. **Given** a session whose signer is removed from an account, **When** the
   delegated signer signs a request for that account, **Then** Guardian
   rejects it with `authorization_failed` while the session keeps working for
   the signer's other accounts; if the signer is added back before the grant
   expires, the grant works again.

---

### User Story 5 - The wallet shows what it authorizes (Priority: P2)

EIP-712 wallets display the grant as readable typed data, not an opaque hash,
so the user can see which signer is delegating, to which key, for which
website, until when, for which accounts and on which Guardian and network.
Raw (Falcon, raw ECDSA) wallets display a hash, so the SDK shows the same
fields to the user before invoking them.

**Why this priority**: Signing an opaque `bytes32` is the phishing case.

**Independent Test**: Build the typed data for a fixture grant, confirm every
field in FR-003 is a top-level member of the struct, and confirm the digest
matches an independent EIP-712 implementation; for raw wallets, confirm the
SDK presents the fields before signing and aborts when the user declines.

---

### User Story 6 - Clear failure against a Guardian without sessions (Priority: P3)

A new SDK pointed at an older Guardian gets a clear error when it tries to
start a session and does not silently fall back to per-request wallet signing.

**Independent Test**: Start a session against a server without the feature and
confirm the SDK raises a dedicated error.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001 — Additive change**: Raw and EIP-712 per-account request
  authentication MUST behave exactly as today. Session credentials MUST NOT be
  routed through `Auth::verify`. A client that asks a Guardian without session
  support to start a session MUST receive an explicit error, with no silent
  fallback.
- **FR-002 — Delegated signer encoding**: v1 supports one algorithm: ECDSA
  over P-256 with SHA-256, as WebCrypto produces it (WebCrypto ECDSA always
  hashes and has no prehash mode). The delegated signer signs the 32-byte
  `AuthRequestMessage` word; `x-signature` is raw `r || s` (64 bytes). There
  is one canonical public-key encoding: SEC1-compressed, 33 bytes. WebCrypto
  `exportKey('raw')` returns the 65-byte uncompressed point, so the SDK
  compresses it before sending; Guardian accepts only the 33-byte form, uses
  it as `x-pubkey`, and keys the session record by the SHA-256 of those 33
  bytes. Shared test vectors MUST pin this in Rust and TypeScript. Browsers
  SHOULD create the key with `extractable: false`; non-extractability is a
  browser property and is not enforced by Guardian.
- **FR-003 — Session grant contents**: A grant binds:
  - the signer commitment of the wallet key that signs it;
  - the delegated signer's public key (FR-002);
  - `origin`: the website that asked for the grant (e.g.
    `https://app.example`, at most 256 bytes), or empty for clients outside a
    browser. The page writes it, so it is **unverified**: a phishing page can
    write the legitimate site. Wallets display it; the verified signal is the
    wallet's own indicator of the requesting site (e.g. MetaMask's), and
    hardware-wallet users have no verified origin in v1. Guardian does not
    check it against requests (see Phishing in Edge Cases);
  - `issued_at` and `expires_at`, in Unix **seconds**;
  - `expires`: one canonical UTC rendering of `expires_at`
    (`YYYY-MM-DD HH:MM:SS UTC`), for devices that show raw numbers;
    `expires_at` stays authoritative;
  - the scope, a protocol constant: *Every account this signer cosigns on
    this Guardian, now or later, until this grant expires*;
  - the Guardian ACK-key commitment for the wallet's signature scheme;
  - the Miden network, exactly one of `local`, `devnet` or `testnet` (the
    `environment` of `GET /status`). Not a bech32 HRP: local and devnet share
    `mdev`.

  `expires` and the scope are derived by Guardian, never taken from the
  client: Guardian computes the signed digest from `expires_at` and the
  constant, so a wallet that displayed anything else (for example "this
  account only") signed a different digest and the grant fails.
- **FR-004 — Grant encoding**: EIP-712 wallets MUST sign the grant as typed
  data in which every field of FR-003 is a readable top-level member, with no
  opaque digest standing in for them:
  `GuardianSession(bytes32 signer,bytes sessionKey,string origin,uint64 issuedAt,uint64 expiresAt,string expires,string scope,bytes32 guardianKey,string network)`
  in domain `{ name: "Guardian Session", version: "1" }`. Falcon and raw ECDSA
  wallets MUST sign a domain-separated RPO digest of the same fields,
  distinct from `AuthRequestMessage` and `LookupAuthMessage` digests; because
  those devices show a hash, the SDK MUST show the grant fields before
  invoking them. That step is UX, not phishing protection: a phishing page
  does not run the SDK (or fakes its display), so for these wallets the grant
  is blind-signed against a malicious page, an accepted v1 risk (Edge Cases). Shared test vectors MUST pin both encodings, and the EIP-712
  digest MUST match an independent implementation.
- **FR-005 — Grant registration**: `POST /session` (gRPC `CreateSession`) MUST
  register a grant only if:
  - the ACK-key commitment and network match this Guardian;
  - `issued_at` is within the request clock-skew window, compared in seconds:
    `|issued_at − now_secs| ≤ 300` (`MAX_TIMESTAMP_SKEW_MS` = 300,000 ms);
  - `expires_at − now_secs` is greater than 300 and at most the configured
    maximum lifetime; the 300-second floor only keeps a session alive past
    the skew window;
  - the delegated signer public key is a valid 33-byte compressed P-256 point;
  - `origin` is at most 256 bytes;
  - the wallet signature verifies with the existing key rules: Falcon embeds
    its public key; raw ECDSA recovers the key and falls back to the optional
    supplied public key when recovery fails or yields another key; EIP-712
    always uses the supplied public key; the verified key's commitment MUST
    equal the grant's signer commitment;
  - the signer commitment is a current cosigner of at least one account on
    this Guardian (`authorization_failed`, HTTP 403, otherwise), so arbitrary
    keys cannot create session records.

  Checks that need no cryptography MUST run before signature verification;
  the cosigner check runs after it, so only the key's holder learns whether
  it cosigns anything. Guardian MUST record the session as issued at the
  earlier of `issued_at` and the registration time, so a grant dated into the
  skew window's future cannot outlive a revoke-all (FR-013).
- **FR-006 — Grant lifecycle**: Re-submitting the same grant while its session
  is live and its `issued_at` is still inside the skew window (the retry after
  a lost response) MUST succeed and return the same expiry, also when the
  wallet re-signed it with another `issued_at`; later the grant fails the
  `issued_at` check of FR-005. The record is
  left unchanged, so the recorded `issued_at` (FR-005) never moves: a
  re-submission cannot lift a session above a revoke-all T (FR-013). A grant for a
  key that is already bound to a different grant (another signer commitment,
  origin or expiry) MUST be rejected, so another cosigner who learns a session key
  from `x-pubkey` cannot re-bind it to their identity; clients MUST use a
  fresh key for every grant, renewal included. A revoked session MUST stay
  revoked until its `expires_at`, so re-submitting its grant fails; the
  session-store upsert that clears `revoked_at` MUST NOT be used for this
  realm, and the in-memory store MUST keep a revoked marker until expiry
  instead of deleting the record. Records MUST be removed after expiry; there
  are no permanent tombstones. The default and maximum lifetime is 8 hours,
  matching operator and EVM sessions; operators MAY configure a shorter
  maximum, which MUST be longer than the 300-second skew window.
- **FR-007 — Request authentication**: For a request with
  `x-auth-format: session`, `resolve_account` MUST, before and instead of
  `Auth::verify`, in this order:
  - verify the delegated signer's signature over `AuthRequestMessage`
    (`authentication_failed`);
  - resolve the session for that public key, failing with `session_expired`
    or `session_revoked` (FR-016) when it ended and `authentication_failed`
    when it is unknown;
  - apply the route allow-list: outside FR-008, `wallet_signature_required`
    (FR-009). An ended session on a wallet-only route therefore gets its
    FR-016 code, and the SDK drops it;
  - re-check that the grant's ACK-key commitment and network still match this
    Guardian for the account's scheme, so key rotation ends sessions;
  - require the grant's signer commitment to be a current cosigner of the
    account, failing with `authorization_failed` (HTTP 403, gRPC
    `PERMISSION_DENIED`) otherwise: the session stays valid for the signer's
    other accounts;
  - apply the replay CAS keyed by **(account, delegated key)**. Wallet
    requests keep their CAS keyed by (account, signer commitment). A session
    therefore cannot advance the wallet's floor: a stolen session that stamps
    its requests at the edge of the skew window cannot lock the wallet out of
    wallet-only routes. Requests on one floor that race (parallel reads of
    the same account through one session) can lose with
    `authentication_replay`; the SDK retries that code with a new timestamp.
    Session floors live in the existing account auth state, in the column
    that holds signer commitments, keyed `session-<hex SHA-256 of the
    delegated key>`: never a commitment (`0x…`) or the legacy sentinel row,
    and selectable by prefix without a schema change (a `-`, not `:`, because
    the filesystem store separates account and signer with `:`). A session
    floor MUST be deleted once no session can use it: the maximum lifetime
    plus twice the skew window after its last request.

  Logout (FR-012) is not a per-account route and sits outside this order: it
  verifies its own message and never returns `wallet_signature_required`.
- **FR-008 — Session-eligible routes**: A delegated signer MAY sign exactly:
  - reads: `GET /state`, `GET /state/nonce`, `GET /delta`, `GET /delta/since`,
    `GET /delta/history`;
  - proposal list and get: `GET /delta/proposal`, `GET /delta/proposal/single`;
  - execution status, when #254 adds them: `GET /delta/execution/current` and
    `GET /delta/proposal/execution`;
  - proposal create and sign: `POST /delta/proposal`, `PUT /delta/proposal`;
  - the gRPC twins of these routes, plus `GetCurrentExecution` and
    `GetDeltaProposalExecution` when #254 adds them.

  The list is an allow-list: a session credential on any other route is
  rejected per FR-009, including routes added later and gRPC methods with no
  HTTP twin.
- **FR-009 — Wallet-only routes (default deny)**: Every route not in FR-008
  MUST reject delegated-signer credentials with the stable code
  `wallet_signature_required` (HTTP 403, gRPC `PERMISSION_DENIED`), before any
  state change and only after the delegated signature verifies and the
  session resolves (FR-007 order); an unverifiable delegated signature is
  `authentication_failed`. This includes
  `POST /delta`, `POST /delta/proposal/execution` (#254),
  `POST /delta/candidate/abandon`, `POST /configure`, `GET /state/lookup` and
  `POST /session/revoke-all`. HTTP and gRPC MUST apply the same rule.
  `POST /delta` becomes session-eligible once #524 lands, because until then
  `push_delta` admits a candidate without checking cosigner signatures or
  threshold, so a stolen session could stall the candidate queue.
  Candidate abandon stays wallet-only: it is rare, and after quarantine the
  on-chain outcome can still be uncertain.
- **FR-010 — Approvals stay with the wallet**: Proposal approval signatures
  over the transaction summary MUST still come from the wallet key. A
  delegated signer cannot approve a transaction.
- **FR-011 — Proposal signature verification**: Done in #526 (merged as
  `c892795a`): Falcon and raw ECDSA approvals are verified against the
  transaction summary, creation accepts only the proposer's own signature,
  and each signature is attributed to its verified slot. This feature relies
  on it and keeps SC-006 as a regression check.
- **FR-012 — Logout**: `POST /session/logout` (gRPC `RevokeSession`), signed by
  the delegated signer over a domain-separated logout message with a
  timestamp in the skew window (Unix **milliseconds**, the request's
  `x-timestamp`), MUST revoke that session. It is idempotent,
  but only after the delegated signature verifies.
- **FR-013 — Revoke all**: `POST /session/revoke-all` (gRPC
  `RevokeAllSessions`), signed by the wallet over a domain-separated,
  account-less message carrying the signer commitment and a timestamp T in the
  skew window (Unix **milliseconds**, the request's `x-timestamp`, compared
  with the recorded `issued_at` at the same precision; raw RPO digest, or
  `GuardianSessionRevokeAll(bytes32 signer,uint64 timestamp)` typed data),
  MUST revoke every session of that signer on this Guardian whose recorded
  `issued_at` (FR-005) is at or before T, and return how many. A replay of the request therefore
  cannot end a session granted after T, with no extra stored floor. It MUST
  NOT read or advance any replay floor, so no session request can block it.
  It is idempotent. The SDK sets T to the current time. The message names no
  Guardian, so within the skew window the same signature also ends the
  signer's older sessions on any other Guardian it is sent to; it can only
  revoke, so this is accepted. Revoke-all ends only sessions already
  registered (accepted limits in Edge Cases), so the SDK guide MUST advise
  running it again 10 minutes later when a key may be compromised.
- **FR-014 — Signer removal**: Removing a signer from an account MUST end
  delegated access to that account on the next request, with
  `authorization_failed` (FR-007). Adding the
  signer back restores any grant that has not expired; this MUST be
  documented.
- **FR-015 — Advertisement**: `GET /status` MUST advertise the sessions block
  with the maximum lifetime, so SDKs can fail early (FR-001). An operator
  on/off switch MAY be added; a Guardian that turns sessions off omits the
  block, and the SDK fails the same way as against an older server.
- **FR-016 — Ended-session codes**: A request from an expired session MUST
  fail with `session_expired`, and from a revoked one with `session_revoked`,
  both HTTP 401 / gRPC `UNAUTHENTICATED`, so the SDK can tell revoke-all from
  expiry instead of treating both as `authentication_failed`. An unknown key
  (an expired one whose record was swept, or any key after a restart of a
  Guardian without persistent sessions) or a grant naming a
  rotated key or another network is `authentication_failed`. On any of these
  three codes in answer to a session-signed request the SDK MUST stop using
  the session, sign with the wallet again, forget the stored key and notify
  the app; `authorization_failed` (FR-007) leaves the session in place. A new
  session needs a new grant and key.
- **FR-017 — SDK `SessionSigner`**: Session handling lives in the Guardian SDKs
  as a `SessionSigner`, not in each dApp: key generation and storage, building
  the grant (with the page's origin by default), showing its fields to the
  user for raw wallets (the SDK MUST NOT start a session for a raw wallet
  without that confirmation step), signing it through the wallet, choosing
  the wallet or the delegated signer per route (FR-008, FR-009) with the
  wallet as the default, retrying `authentication_replay`, and dropping ended
  sessions and notifying the app so it can renew (FR-016). The SDK uses a
  session only while the wallet that granted it is the client's signer, and
  clears local session state only after logout or revoke-all succeed. When a
  grant expires the SDK MUST discard the key. The TypeScript SDK keeps the
  key non-extractable and MAY persist it in IndexedDB. The Rust SDK uses the
  same `x-auth-format: session` path with an in-memory key and an empty
  `origin`.
- **FR-018 — Observability**: Guardian MUST log grant registration, rejection
  reason category, logout and revoke-all with the signer commitment, and MUST
  NOT log signatures or session public keys in full.
- **FR-019 — Rate limiting**: `POST /session`, `POST /session/logout` and
  `POST /session/revoke-all` MUST NOT be exempted from the existing per-IP
  limiter (`RateLimitLayer` on HTTP, `GrpcRateLimitLayer` on gRPC).
  `POST /session` is limited by IP because the caller is unknown until the
  grant verifies.

### Contract / Transport Impact

- New HTTP endpoints `POST /session`, `POST /session/logout`,
  `POST /session/revoke-all`; new gRPC RPCs `CreateSession`, `RevokeSession`,
  `RevokeAllSessions`, kept in parity in both proto files and in the OpenAPI
  documents.
- New `x-auth-format: session` value on the FR-008 routes; existing `raw` and
  `eip712` values are unchanged.
- New stable error codes `wallet_signature_required`, `session_expired` and
  `session_revoked`, added to the TypeScript error-code list.
- `GET /status` gains a sessions block.
- Rust client and TypeScript clients (`guardian-client`,
  `miden-multisig-client`) gain `SessionSigner`; at least one example
  exercises the flow end to end.

### Data / Lifecycle Impact

- A new `miden` realm and `SessionSubject` in `auth_sessions`, keyed by the
  SHA-256 digest of the 33-byte delegated signer public key, storing the
  signer commitment, origin, ACK-key commitment, network, the grant's
  `issued_at` and its expiry. No schema migration: the realm column is free
  text.
- Revoke-all needs a `SessionStore` lookup by signer commitment: a JSON
  containment predicate on `subject` plus `issued_at ≤ T` in Postgres, a scan
  in memory. Operator and EVM sessions keep today's upsert.
- Reporting `session_expired` versus `session_revoked` needs the store to say
  why a record is inactive (`revoked_at` set or not) for records not yet
  swept.
- Session requests add one replay-floor row per (account, delegated key)
  they touch, in the existing account auth state; the session sweep deletes
  them once no session can use them (FR-007).
- Filesystem deployments use the in-memory session store with the same
  semantics, but a restart drops the revoked markers: a revoked grant can then
  be registered again until its `issued_at` leaves the skew window. Postgres
  keeps the markers.
- No change to account, proposal, or delta records.

## Edge Cases *(mandatory)*

- **Grant re-submitted after a dropped response**: succeeds while live (FR-006).
- **Grant re-submitted after logout or revoke-all**: rejected until it would
  have expired (FR-006).
- **Another cosigner registers a victim's session key**: rejected; the key
  stays bound to its first grant (FR-006).
- **Grant signed long before submission**: rejected by the `issued_at` skew
  check; the SDK dates the grant after the user confirms it.
- **Grant dated in the future**: recorded as issued at registration, so a
  revoke-all signed after registration still ends it (FR-005).
- **Clock skew between client and server**: grant lifetime is capped against
  server time; SDKs request less than the advertised maximum to absorb drift.
- **Guardian ACK-key rotation or network change**: existing sessions stop
  being accepted on the next request (FR-007).
- **Parallel tabs sharing one stored key**: requests share the delegated
  key's replay floor; a lost race returns `authentication_replay` and the SDK
  re-signs.
- **Stolen session stamping requests in the future**: it only advances its own
  floor; the wallet keeps working and revoke-all, which touches no floor,
  always succeeds (FR-007, FR-013).
- **Replayed revoke-all**: revokes nothing issued after its timestamp
  (FR-013).
- **Delegated signer used on a wallet-only route**:
  `wallet_signature_required`, no state change; an unverifiable delegated
  signature is `authentication_failed` (FR-009).
- **Signer removed, then added back**: access ends with
  `authorization_failed`, then returns for the unexpired grant (FR-014); the
  session keeps working for the signer's other accounts meanwhile.
- **Guardian restarts without persistent sessions**: every key is unknown;
  the SDK drops the session on the first `authentication_failed` and notifies
  the app (FR-016).
- **Account the signer is added to after the grant**: covered, because the v1
  scope is every account the signer cosigns; the signed message says so.
- **Compromised page**: injected script can use the delegated signer until the
  session ends. It cannot approve transactions or call wallet-only routes.
  Accepted risks until revoke-all or expiry: it can fill
  `max_pending_proposals` for the signer's accounts, and, once `POST /delta`
  is session-eligible after #524, repeatedly stall the candidate queue. Page
  logout does not help, because the script holds the same key; revoke-all
  from the wallet does.
- **Phishing page (no audience)**: a page that obtains a grant can, until it
  ends, read the state, balances and proposals of every account the signer
  cosigns and create proposals in the signer's name; it cannot approve,
  execute or move funds. Mitigation: the grant names the website that asked
  for it and the wallet displays it, so the user sees who is asking. Guardian
  does not compare it with the request's `Origin`: the page holds the key and
  can send its requests from a server with any `Origin`, so the check would
  only stop a careless page while breaking same-origin deployments. The read
  exposure to a page the user wrongly approves is accepted for v1. The
  grant's `origin` is written by the page and unverified (FR-003): the real
  signal is the wallet's own indicator of the requesting site, and
  hardware-wallet users have no verified origin in v1.
- **Blind-signed grants on raw wallets**: Falcon and raw ECDSA devices show a
  hash, and the SDK's confirmation step is shown by the page, so against a
  phishing page the grant is blind-signed. Accepted for v1. Follow-up: the
  Miden Wallet (or any raw wallet) recognizes the grant's domain tag
  (`guardian.session.v1`) and renders its fields itself.
- **Device clock behind**: T is the revoking device's clock and `issued_at`
  the granting device's (capped at server time). A replay of a revoke-all
  inside the skew window can end a session started just after it on a device
  whose clock runs behind. It can only revoke; the user starts a new session.
- **Grant signed but not yet registered at revoke-all**: accepted for v1.
  Revoke-all ends only sessions already registered. A page that got a grant
  signed can register it afterwards while its `issued_at` is within the skew
  window of server time, and it chooses `issued_at` up to 5 minutes ahead, so
  up to about 10 minutes after signing; the session then lives until its
  expiry. Running revoke-all again 10 minutes later ends it.
- **Revoking device clock behind Guardian**: accepted for v1. T is the
  revoking device's clock, so sessions registered in the gap between T and
  server time are not revoked. Running revoke-all again later ends them.
- **Hardware wallets without registered display metadata**: devices may need
  a setting such as Ledger's "Verbose EIP-712" to display typed data field by
  field; documented in the SDK guide. Raw wallets show a hash; the SDK shows
  the fields first (FR-004), which is UX, not phishing protection.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Discovery (`GET /state/lookup`) still prompts the wallet once.
  After account ids are known, one grant makes a full SDK sync, reads and
  proposal listing produce zero wallet prompts.
- **SC-002**: Every route outside FR-008 rejects valid delegated-signer
  credentials with `wallet_signature_required`, and forged ones with
  `authentication_failed`, on HTTP and gRPC, verified by tests.
- **SC-003**: Tests demonstrate rejection of: unknown delegated signer,
  signature over another body, expired grant (`session_expired`), revoked
  grant (`session_revoked`), re-submitted revoked grant, a second signer's
  grant for a bound key, grant for another Guardian key or network, stale
  `issued_at`, lifetime within the skew window or over the maximum, an origin
  over 256 bytes, a non-cosigner registration, a request for an account the
  signer does not cosign (`authorization_failed`), removed signer, and a
  rotated ACK key.
- **SC-004**: Rust and TypeScript produce byte-identical grant, logout and
  revoke-all digests and EIP-712 digests for the shared vectors, and the
  EIP-712 digests match an independent implementation.
- **SC-005**: Raw and EIP-712 request-auth test suites pass unchanged.
- **SC-006**: Proposal-signature tests from #526 keep passing (regression).
- **SC-007**: A test shows a session stamping requests at the edge of the
  skew window cannot block a wallet request, and that a replayed revoke-all
  spares a session granted after it.

## Assumptions

- The 5-minute request clock-skew window stays as today.
- Session records are small and few per signer; no per-signer cap is needed in
  v1 beyond rate limiting and the cosigner requirement.

## Dependencies

- #526, proposal signature verification (FR-011): merged as `c892795a`.
- Existing `auth_sessions` store, sweep and coordination handles.
- Existing replay CAS and request clock-skew window.
- #524 (cosigner signature and threshold checks on `POST /delta`): gates
  making `POST /delta` session-eligible.
- #254 (execution routes): adds the execution routes named in FR-008 and
  FR-009.

## Clarifications

### Session 2026-10-06

- Q: Does delegating the request signer fit constitution principle IV? → A: Yes, as written: every account request stays signed and replay-protected, and only the key that signs `AuthRequestMessage` changes. No constitution change. The session key is called a delegated signer.
- Q: Is P-256 acceptable? → A: Yes, as the only v1 algorithm: ECDSA-P256 with SHA-256 over the 32-byte `AuthRequestMessage` word, raw `r || s`, pinned with shared test vectors. secp256k1 and Falcon cannot back a non-extractable browser key.
- Q: What does a grant cover? → A: Every account the signer cosigns on this Guardian until it expires, stated in the signed message. `account_ids` scoping is out of scope for v1.
- Q: Is page logout enough? → A: No. Add a wallet-signed revoke-all for the signer.

### Session 2026-10-07 (review of #527)

- Q: `wallet_signature_required` as HTTP 403 / gRPC `PERMISSION_DENIED`? → A: Agreed: the caller is authenticated and not permitted on the route. Returned only after the delegated signature verifies.
- Q: Which routes may a delegated signer use? → A: An allow-list (FR-008); every other route is wallet-only by default (FR-009).
- Q: How does the grant identify the Guardian? → A: ACK-key commitment plus network, re-checked on every request. No display name. An operator on/off switch is optional (FR-015).
- Q: Readable expiry? → A: Yes: one extra signed field, the canonical UTC string derived from `expires_at`, which stays authoritative; anything else fails.
- Q: `POST /delta` and abandon on sessions? → A: Not in v1. `POST /delta` becomes session-eligible after #524 lands; abandon stays wallet-only.
- Q: How is revoke-all protected against replay? → A: It revokes only sessions issued at or before its signed timestamp, with no stored floor, and never touches a replay floor.
- Q: Can a stolen session lock the wallet out through the shared replay floor? → A: Session requests use their own floor per (account, delegated key); wallet requests keep theirs per (account, signer). This replaces the earlier "one floor per signer across wallet and session requests".
- Q: Distinct codes for ended sessions? → A: `session_expired` and `session_revoked`, both 401.
- Q: May anyone register a grant? → A: No: the signer must cosign at least one account on this Guardian.
- Q: Delegated-key collision? → A: A key stays bound to its first grant; a different grant for it is rejected; clients use a fresh key per grant.
- Q: Phishing (no audience)? → A: The grant names its origin and the wallet shows it; the read exposure is accepted for v1 (Edge Cases). Server enforcement: see 2026-10-08.

### Session 2026-10-08 (implementation review)

- Q: Should Guardian check the request `Origin` against the grant (suggested as optional in the #527 review)? → A: No. A phishing page holds the key and can replay from a server with any `Origin`, so the check only stops a careless page, and it breaks deployments where the dApp and Guardian share an origin (browsers omit `Origin` on same-origin `GET`). The origin stays in the grant and the wallet shows it (FR-003).
- Q: Can a grant dated into the future outlive revoke-all? → A: No: Guardian records the earlier of `issued_at` and the registration time (FR-005).
- Q: Should the revoke-all message name the Guardian? → A: No: it can only revoke; the cross-Guardian effect inside the skew window is accepted (FR-013).
- Q: Which answers end a session in the SDK? → A: `session_expired`, `session_revoked` and `authentication_failed` on a session-signed request. A request for an account the signer does not cosign is `authorization_failed` and keeps the session (FR-007, FR-016).
- Q: Do session replay floors accumulate? → A: No: the sweep deletes a floor once no session can use it (FR-007).

### Session 2026-10-08 (#527 round 2)

- Q: Per-(account, delegated key) replay floor? → A: Confirmed (zeljkoX): a session signature can never be replayed as a wallet signature, so separate floors cost nothing.
- Q: `authorization_failed` (403) for an account the signer does not cosign; origin shown, not enforced; raw-wallet confirmation? → A: Confirmed, with the origin stated as unverified and raw-wallet blind signing as an accepted v1 risk (FR-003, FR-004, Edge Cases).
- Q: Check order for a session request? → A: Signature, session, route allow-list, ACK key and network, cosigner, replay CAS (FR-007).
- Q: Units of the logout and revoke-all timestamps? → A: Milliseconds, like every request `x-timestamp` (FR-012, FR-013).

### Session 2026-10-08 (implementation review, after approval)

- Q: Revoke-all and grants signed but not yet registered, or a revoking device whose clock runs behind? → A: Accepted v1 limits, documented in Edge Cases; the SDK guide advises running revoke-all again 10 minutes later. A stored per-signer revoke mark would close both at the cost of a cool-down on new sessions; not in v1.
- Q: Re-submitting a live grant after its `issued_at` left the skew window? → A: Fails the FR-005 `issued_at` check; re-submission is the retry after a lost response (FR-006).

### Open for review

None: the last item, the per-(account, delegated key) replay floor, was confirmed in round 2.
