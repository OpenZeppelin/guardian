# Feature Specification: Domain-separated EVM proposal support

**Feature Key**: `001-evm-proposal-support`  
**Suggested Branch**: `001-evm-proposal-support` (manual creation optional)  
**Created**: 2026-03-18  
**Updated**: 2026-04-29
**Status**: Draft  
**Input**: User description: "Add feature-gated EVM account and proposal support as a domain-separated extension of Guardian"

## Context

Guardian's current production model is Miden-centered: account configuration,
state sync, delta submission, delta proposals, and canonicalization are built
around Miden account state. EVM smart accounts need a different coordination
flow. They do not submit Guardian deltas, do not use Guardian acknowledgement
signatures, and do not need Guardian to build or submit on-chain transactions.

This feature introduces EVM proposal coordination as a feature-gated Guardian
domain under `/evm/*`. Miden keeps the existing `/configure`, `/delta`,
`/delta/proposal`, `/state`, and gRPC flows. EVM uses wallet-session
authentication, EVM smart account registration, and EVM proposal routes that are
separate from the Miden delta proposal envelope.

For EVM accounts, Guardian coordinates cosigner approvals over a client-supplied
UserOperation hash and opaque payload. Guardian validates that the authenticated
wallet is authorized by the configured ERC-7579 multisig validator, snapshots
the signer set and threshold for each proposal, verifies ECDSA signatures, and
returns executable proposal data once enough signatures are collected. Guardian
does not build UserOperations, decode opaque payloads, submit transactions
on-chain, or track execution beyond lazy cleanup when expiry or EntryPoint nonce
state makes a proposal inactive.

The default Guardian build remains Miden-only. EVM behavior is enabled only by
an explicit server-side `evm` feature flag. Default builds do not register
`/evm/*` routes and do not initialize EVM sessions, contract readers, storage
flows, or proposal handlers.

## Clarifications

### Session 2026-03-18

- Q: What is the canonical identity of an EVM account in Guardian? -> A: `chain_id + account_address`
- Q: What EVM scope is desired for v1? -> A: proposal sharing and signing only; delta/state/canonicalization support for EVM is not in v1
- Q: Which auth/signature model should EVM v1 use? -> A: keep the auth model extensible, but implement ECDSA only for EVM in v1
- Q: Should v1 use an indexer for EVM signer validation? -> A: no; use direct contract reads in v1
- Q: How should the EVM proposal identifier be represented? -> A: use a deterministic hash-based Guardian proposal identifier rather than raw concatenation

### Session 2026-04-09

- Q: What canonical API account identifier should EVM use? -> A: derive and enforce `evm:<chain_id>:<normalized_smart_account_address>`
- Q: Which EVM multisig contract model should v1 target? -> A: OpenZeppelin ERC-7579 multisig validator as the signer/threshold read model
- Q: What signer types should EVM v1 support? -> A: normalized EOA addresses only

### Session 2026-04-27

- Q: Should EVM support be enabled by default? -> A: no; EVM account and proposal support is gated behind an explicit server-side `evm` feature flag and default Guardian deployments remain Miden-only.
- Q: Does enabling the EVM feature mean Guardian supports every EVM chain by default? -> A: no; the feature enables the EVM account/proposal capability family only. Deployments must explicitly configure supported chain RPC and EntryPoint addresses.
- Q: What should happen when EVM requests reach a server without the EVM feature enabled? -> A: the `/evm/*` routes are absent, and no EVM session, contract, or persistence code is initialized.

### Session 2026-04-29

- Q: Should EVM reuse `/configure` and `/delta/proposal`? -> A: no. EVM has a different account and proposal lifecycle, so v1 uses a domain-separated `/evm/*` HTTP surface.
- Q: What should the EVM auth endpoints be? -> A: `/evm/auth/challenge`, `/evm/auth/verify`, and `/evm/auth/logout`.
- Q: Are cookie-backed sessions acceptable instead of JWTs? -> A: yes, if the authenticated identity is cryptographically derived from the wallet signature, sessions expire, and challenge nonces are single-use and time-limited.
- Q: Does Guardian submit EVM proposals on-chain? -> A: no. Guardian stores approvals and returns executable data; integrators choose their own submission mechanism.
- Q: Should EVM proposals expose Miden `DeltaObject` semantics? -> A: no. EVM proposals use EVM-specific request and response shapes, while implementations may reuse shared lower-level storage infrastructure where appropriate.

## Scope *(mandatory)*

### In Scope

- Add an EVM HTTP domain under `/evm/*` for wallet sessions, smart account
  registration, proposal creation, listing, retrieval, approval, executable-data
  export, and cancellation.
- Gate all EVM auth, account, proposal, contract-read, and cleanup behavior
  behind an explicit server-side `evm` feature flag that is disabled by default.
- Preserve existing Miden `/configure`, `/delta`, `/delta/proposal`, `/state`,
  canonicalization, and gRPC behavior.
- Register EVM accounts with the canonical account ID
  `evm:<chain_id>:<normalized_smart_account_address>`.
- Resolve EVM RPC and EntryPoint addresses from server-owned configuration
  rather than trusting client-provided endpoints.
- Validate ERC-7579 multisig validator installation and signer/threshold data
  through direct chain reads.
- Authenticate EVM users with EIP-712 wallet challenges and a secure,
  expiring `guardian_evm_session` cookie.
- Support EVM proposal coordination over a client-supplied 32-byte
  `user_op_hash`, opaque payload, full uint256 nonce, TTL, and ECDSA
  signatures.
- Snapshot proposal signer EOAs and threshold at proposal creation time.
- Verify EVM proposal signatures by recovering the EOA from the stored
  `user_op_hash`.
- Lazily remove expired EVM proposals and proposals that are inactive because
  the configured EntryPoint nonce has advanced.
- Provide a dedicated TypeScript EVM client package and smoke app for the EVM
  HTTP domain.

### Out of Scope

- EVM support through `/configure`, `/delta/proposal`, `/delta`,
  `/delta/since`, `/state`, or gRPC in v1.
- Miden delta semantics for EVM proposal records.
- Guardian-built UserOperations, payload decoding, bundler submission,
  on-chain transaction submission, or generalized execution tracking.
- ERC-1271 contract signers, weighted multisig, generic ERC-7913 verifier-key
  signers, and non-EOA signer bytes.
- Indexer-based signer validation.
- Enabling every EVM-compatible chain by default.
- Changes to the base TypeScript Guardian client or Rust Guardian client beyond
  non-behavioral compatibility work required by shared contract types.
- Introducing a separate PSM package or service name; PSM refers to Guardian in
  this feature context.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Authenticate EVM Wallet Sessions (Priority: P1)

As an EVM cosigner, I can sign a Guardian challenge with my wallet and receive a
short-lived session so Guardian can authorize my later EVM account and proposal
actions without requiring per-request headers or JWTs.

**Why this priority**: Every EVM account and proposal action depends on a
verified wallet identity.
**Independent Test**: Request a challenge, sign it with a wallet, verify it,
confirm the session cookie is set, then verify challenge replay, expired
challenge, and disabled-feature cases fail explicitly.

**Acceptance Scenarios**:

1. **Given** an EVM-enabled server, **When** a wallet requests and signs a valid
   challenge, **Then** Guardian derives the session EOA from the signature and
   creates an expiring cookie-backed session.
2. **Given** a challenge nonce has already been consumed, **When** the same
   challenge is submitted again, **Then** verification fails.
3. **Given** a server without EVM support enabled, **When** any `/evm/auth/*`
   route is called, **Then** the route is absent from the default router.

---

### User Story 2 - Register EVM Smart Accounts (Priority: P1)

As an authorized signer for an EVM smart account, I can register that account in
Guardian so proposals can later be shared and approved by the account's
validator signer set.

**Why this priority**: Proposal coordination needs a durable account identity,
validator address, and chain configuration before proposals can be accepted.
**Independent Test**: With a valid wallet session, register a smart account and
validator on a configured chain, then verify unauthorized signer, unsupported
chain, missing RPC/EntryPoint config, and validator-not-installed cases fail.

**Acceptance Scenarios**:

1. **Given** an EVM-enabled server with configured chain RPC and EntryPoint
   addresses, **When** a current validator signer registers a smart account,
   **Then** Guardian stores account metadata for the canonical EVM account ID.
2. **Given** the session EOA is not a current validator signer, **When** account
   registration is attempted, **Then** the request fails with
   `signer_not_authorized`.
3. **Given** the validator module is not installed on the smart account,
   **When** account registration is attempted, **Then** the request fails with
   an explicit contract-validation error.

---

### User Story 3 - Coordinate EVM Proposal Approvals (Priority: P2)

As an authorized EVM cosigner, I can create, list, retrieve, approve, cancel,
and export executable proposal data so a smart-account transaction can collect
the required approvals before another tool submits it on-chain.

**Why this priority**: This is the core EVM value: Guardian coordinates
off-chain approval collection while leaving UserOperation construction and
submission to the integrating application.
**Independent Test**: Create a proposal with an initial signature, list and
retrieve it from another authorized session, add approvals from multiple
signers, verify duplicate and unauthorized approvals fail, verify executable
data is withheld before threshold and returned after threshold, then cancel as
the proposer.

**Acceptance Scenarios**:

1. **Given** a registered EVM account and authorized proposer, **When** a valid
   proposal is created, **Then** Guardian stores it with deterministic
   `proposal_id`, signer snapshot, threshold, TTL, and the proposer's
   signature.
2. **Given** a pending EVM proposal and an authorized signer in the stored
   snapshot, **When** the signer submits a valid approval, **Then** the
   signature is appended and the updated proposal is returned.
3. **Given** stored signatures do not meet threshold, **When** executable data
   is requested, **Then** Guardian returns `insufficient_signatures`.
4. **Given** stored signatures meet threshold, **When** executable data is
   requested, **Then** Guardian returns the proposal hash, opaque payload,
   collected signatures, and signer addresses.
5. **Given** a proposal has expired or the EntryPoint nonce indicates finality,
   **When** it is listed, fetched, approved, or exported, **Then** Guardian
   lazily deletes it and returns no active proposal.

---

### User Story 4 - Preserve Miden Behavior And Network Boundaries (Priority: P1)

As an existing Guardian integrator, I can keep using Miden state, delta, and
proposal flows without behavioral drift, while EVM requests use their own
domain-specific routes and errors.

**Why this priority**: EVM support must not destabilize existing Guardian
behavior or overload Miden abstractions with incompatible semantics.
**Independent Test**: Run existing Miden account, delta, and proposal tests on
default and EVM-enabled builds, then verify EVM requests to Miden routes return
explicit unsupported behavior.

**Acceptance Scenarios**:

1. **Given** a Miden account, **When** existing `/configure`, `/delta`,
   `/delta/proposal`, `/state`, and gRPC flows are used, **Then** behavior
   remains unchanged.
2. **Given** an EVM account, **When** Miden state/delta/proposal routes are
   called, **Then** Guardian returns `unsupported_for_network` instead of
   coercing EVM data into Miden semantics.
3. **Given** a default server build, **When** any `/evm/*` route is called,
   **Then** the route is absent before any EVM persistence or contract read can
   run.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST keep Miden account configuration, state, delta,
  delta proposal, canonicalization, and gRPC behavior unchanged unless a request
  targets an EVM account, in which case unsupported Miden routes MUST fail with
  `unsupported_for_network`.
- **FR-002**: The system MUST expose EVM v1 behavior only through `/evm/*`
  HTTP routes.
- **FR-003**: The system MUST not register `/evm/*` routes when the server-side
  `evm` feature is not enabled, preventing session creation, contract reads,
  metadata writes, and proposal writes in default builds.
- **FR-004**: The system MUST support `/evm/auth/challenge`,
  `/evm/auth/verify`, and `/evm/auth/logout` for EVM wallet sessions.
- **FR-005**: EVM challenge verification MUST recover the authenticated EOA
  from a wallet signature, consume challenge nonces exactly once, reject expired
  challenges, and create an expiring `guardian_evm_session` cookie.
- **FR-006**: The system MUST register EVM accounts through `/evm/accounts`,
  not through `/configure`.
- **FR-007**: EVM account IDs MUST be canonical strings of the form
  `evm:<chain_id>:<normalized_smart_account_address>`.
- **FR-008**: EVM account registration MUST accept client-provided
  `chain_id`, `account_address`, and `multisig_validator_address`, and MUST
  resolve RPC and EntryPoint addresses from server-owned chain configuration.
- **FR-009**: EVM account registration MUST verify that the multisig validator
  is installed on the smart account before metadata is persisted.
- **FR-010**: EVM account registration MUST verify that the session EOA is a
  current signer of the configured validator before metadata is persisted.
- **FR-011**: EVM v1 signer identities MUST be normalized EOA addresses.
- **FR-012**: EVM proposals MUST be created through `/evm/proposals`, not
  through `/delta/proposal`.
- **FR-013**: EVM proposal creation MUST require `account_id`,
  `user_op_hash`, opaque `payload`, full uint256 `nonce`, `ttl_seconds`, and an
  initial ECDSA signature from the session EOA.
- **FR-014**: Guardian MUST verify EVM proposal signatures against the
  client-supplied 32-byte `user_op_hash`.
- **FR-015**: Guardian MUST snapshot signer EOAs and threshold for each EVM
  proposal at creation time and use that snapshot for proposal authorization,
  duplicate detection, and threshold checks.
- **FR-016**: EVM proposal IDs MUST be deterministic hash-based values derived
  from `(account_id, multisig_validator_address, user_op_hash, full_nonce)`.
- **FR-017**: Re-submitting an active EVM proposal with the same deterministic
  proposal ID MUST be idempotent.
- **FR-018**: EVM proposal approval MUST be performed through
  `/evm/proposals/{proposal_id}/approve`, derive signer identity from the
  session EOA, reject unauthorized signers, reject duplicate signatures, and
  append valid signatures to the active proposal.
- **FR-019**: EVM proposal listing and retrieval MUST return only active
  proposals visible to the session EOA according to the stored signer snapshot.
- **FR-020**: EVM executable-data export MUST return executable proposal data
  only after stored signatures meet the proposal threshold; before threshold it
  MUST return `insufficient_signatures`.
- **FR-021**: EVM proposal cancellation MUST be proposer-only and MUST delete
  the active proposal.
- **FR-022**: Guardian MUST lazily delete EVM proposals that are expired or
  inactive because EntryPoint nonce state indicates finality.
- **FR-023**: EVM proposal records MUST use EVM-specific request and response
  shapes and MUST NOT expose Miden `DeltaObject` fields as part of the public
  EVM API.
- **FR-024**: The system MAY reuse shared storage backend infrastructure for
  EVM persistence, but observable EVM behavior MUST be independent of Miden
  delta proposal semantics.
- **FR-025**: EVM routes MUST expose stable error codes for disabled-feature,
  unsupported-chain, missing-chain-config, contract-validation,
  signer-authorization, malformed-proposal, invalid-signature, duplicate
  signature, insufficient-signature, expired-proposal, and missing-proposal
  failures.
- **FR-026**: The dedicated TypeScript EVM client MUST use the `/evm/*`
  endpoints, browser wallet challenge signing, cookie credentials, proposal
  creation, approval, executable export, and cancellation helpers.
- **FR-027**: The base TypeScript Guardian client and Rust Guardian client MUST
  remain Miden-focused unless shared contract compatibility requires a
  non-behavioral update.

### Contract / Transport Impact

- EVM v1 is HTTP-only under `/evm/*`.
- Miden HTTP routes remain `/configure`, `/delta`, `/delta/since`,
  `/delta/proposal`, `/delta/proposal/single`, `/state`, and `/pubkey`.
- Miden gRPC methods remain Miden-oriented. EVM account registration, sessions,
  proposals, executable export, and cancellation are not added to gRPC in v1.
- EVM authentication uses `guardian_evm_session` rather than `x-pubkey`,
  `x-signature`, and `x-timestamp`.
- Cookie-backed EVM sessions are valid only when the session address is derived
  from the wallet signature; sessions expire; challenge nonces are single-use
  and time-limited.
- EVM proposal create and approve requests carry raw ECDSA signatures; signer
  identity is derived from the session EOA and verified against recovered
  signature identity.
- The public EVM proposal response is an EVM-specific record containing
  proposal ID, account ID, chain ID, smart account address, validator address,
  UserOperation hash, opaque payload, full nonce, nonce key, proposer, signer
  snapshot, threshold, signatures, creation time, and expiry time.
- Stable application error codes are required across HTTP responses.
- The EVM smoke app and dedicated EVM client must exercise the same public route
  shape defined by this specification.

### Data / Lifecycle Impact

- Account metadata remains the durable account registry. Miden metadata is
  created by `/configure`; EVM metadata is created by `/evm/accounts`.
- EVM account metadata includes canonical account ID, chain ID, smart account
  address, multisig validator address, signer snapshot metadata, and timestamps.
- RPC URLs and EntryPoint addresses are deployment configuration, not trusted
  client request fields.
- EVM proposals are pending-only coordination records, not Miden deltas.
- EVM proposal storage contains opaque payload, UserOperation hash, full uint256
  nonce, nonce key, signer snapshot, threshold, collected signatures, proposer,
  creation timestamp, and expiry timestamp.
- EVM proposals are removed explicitly by proposer cancellation or lazily when
  expiry or EntryPoint nonce finality makes the proposal inactive.
- Backend parity applies: filesystem and Postgres backends must expose the same
  EVM account/proposal behavior when the EVM feature is enabled.
- Miden canonicalization workers do not process EVM proposals.

## Edge Cases *(mandatory)*

- Any `/evm/*` request reaches a server without EVM routes registered.
- Challenge verification is attempted with an expired, malformed, wrong-address,
  or already-consumed nonce.
- EVM session cookies are missing, expired, malformed, or no longer associated
  with a known session.
- Chain ID is unsupported or lacks configured RPC or EntryPoint addresses.
- Smart account address, validator address, UserOperation hash, signature, or
  nonce encoding is malformed.
- Validator installation check fails or cannot be completed.
- Session EOA is not a current validator signer at account registration.
- Signer authority changes after account registration but before proposal
  creation.
- Proposal create signature does not recover the session EOA.
- Proposal create is repeated for an already-active deterministic proposal ID.
- Proposal approval is attempted by an EOA outside the stored signer snapshot.
- Proposal approval uses a duplicate signer or invalid signature.
- Executable data is requested before threshold is met.
- Proposal cancellation is attempted by a non-proposer.
- Proposal is expired or finalized by EntryPoint nonce advancement when it is
  listed, fetched, approved, exported, or cancelled.
- Miden and EVM accounts coexist on the same server and must not leak behavior
  across network boundaries.
- Filesystem and Postgres persistence must not produce different observable EVM
  proposal lifecycle behavior.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A default Guardian server registers no `/evm/*` routes, while
  existing Miden tests continue to pass.
- **SC-002**: On an EVM-enabled server with valid chain configuration, an
  authorized EVM signer can complete wallet login and account registration in a
  single browser session.
- **SC-003**: On an EVM-enabled server, authorized signers can create, list,
  retrieve, approve, and export an EVM proposal, and executable data is returned
  only after the configured threshold is met.
- **SC-004**: Duplicate approvals, unauthorized approvals, malformed proposal
  inputs, unsupported chains, expired proposals, and finalized proposals return
  stable explicit errors.
- **SC-005**: Existing Miden account, delta, proposal, and canonicalization
  flows remain behaviorally unchanged in default and EVM-enabled builds.

## Assumptions

- PSM means Guardian; no separate PSM service or package name is introduced.
- EVM v1 targets OpenZeppelin ERC-7579 multisig validator behavior for signer
  and threshold reads.
- EVM signer validation uses direct chain reads in v1.
- EVM sessions are cookie-backed and expire.
- Challenge nonces are single-use and time-limited.
- ECDSA EOA signers are the only EVM signer type in v1.
- Guardian verifies signatures over a client-supplied UserOperation hash and
  does not define the on-chain submission mechanism.
- Guardian does not build, decode, or submit UserOperations.
- Server operators configure supported chain RPC and EntryPoint addresses.
- The dedicated EVM TypeScript client owns EVM browser-wallet orchestration.
- Base Guardian clients remain Miden-focused unless shared compatibility work is
  required.

## Dependencies

- An ERC-7579-compatible smart account that exposes validator installation
  checks.
- An OpenZeppelin ERC-7579 multisig validator module that exposes signer and
  threshold reads for Guardian validation.
- Server-side chain configuration for supported EVM chain IDs, RPC URLs, and
  EntryPoint addresses.
- A browser wallet capable of signing EIP-712 typed data for EVM session
  challenges.
- Filesystem and Postgres persistence parity for EVM account and proposal data.

## Deferred Topics

- On-chain submission, bundler integration, and transaction execution UX.
- Full execution reconciliation beyond lazy EntryPoint nonce cleanup.
- ERC-1271, weighted multisig, generic ERC-7913 verifier-key signers, and
  contract signer authorization.
- Indexer-based validation and event-driven proposal cleanup.
- EVM gRPC support.
- Adding other network domains beyond Miden and EVM.

## Delivery Guidance

- Keep Miden and EVM implementation concerns separated wherever practical:
  shared layers should cover infrastructure such as errors, HTTP envelopes,
  clocks, account metadata access, and storage backend plumbing, while Miden and
  EVM domain rules should live behind network-specific modules and routes.
- Do not force EVM proposal behavior into Miden `DeltaObject` semantics.
- The default build must not initialize EVM contract readers, session state, or
  proposal handlers.
- Keep Speckit plan and contract artifacts aligned with the domain-separated
  `/evm/*` flow before implementation changes are finalized.
