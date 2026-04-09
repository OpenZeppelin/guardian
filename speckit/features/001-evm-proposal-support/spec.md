# Feature Specification: Add generic EVM proposal sharing and signing support

**Feature Key**: `001-evm-proposal-support`  
**Suggested Branch**: `001-evm-proposal-support` (manual creation optional)  
**Created**: 2026-03-18  
**Status**: Draft  
**Input**: User description: "Add generic EVM proposal sharing and signing support"

## Context

Private State Manager currently assumes a Miden-centric account and proposal
model. This feature introduces per-account network configuration so the system
can support both existing Miden accounts and new EVM accounts without moving
network selection to a server-global setting.

For EVM accounts, the initial scope is proposal sharing and cosigner signature
collection only. The system must support configuring an EVM account, validating
its signer set through the configured RPC endpoint, creating/listing/getting
pending proposals, and appending signatures to those proposals. Existing Miden
flows must continue to behave as they do today.

This change is expected to affect the server contract first, then the Rust and
TypeScript base clients. Multisig SDK layers and examples may remain unchanged
in v1 unless they surface the new EVM proposal workflow.

## Clarifications

### Session 2026-03-18

- Q: What is the canonical identity of an EVM account in PSM? -> A: `chain_id + contract_address`
- Q: Which EVM account configuration fields are initially expected? -> A: start with `chain_id`, `contract_address`, and required `rpc_endpoint`
- Q: What EVM scope is desired for v1? -> A: proposal sharing and signing only; delta/state/canonicalization support for EVM is not in v1
- Q: How should signer authority be validated for EVM accounts? -> A: re-check signer authority on every relevant action
- Q: Which auth/signature model should EVM v1 use? -> A: keep the auth model extensible, but implement ECDSA only for EVM in v1
- Q: How should per-account network configuration be represented? -> A: prefer a `network_config` model rather than unrelated top-level fields
- Q: Should v1 use an indexer for EVM signer validation? -> A: no; use direct RPC reads only in v1 and require `rpc_endpoint`
- Q: How should the EVM proposal identifier be represented? -> A: use a deterministic hash-based PSM proposal identifier rather than raw concatenation
- Q: Should this feature preserve backward compatibility for accounts missing `network_config`? -> A: no; missing `network_config` is invalid and new account configuration must be explicit

### Session 2026-04-09

- Q: What canonical API account identifier should EVM use? -> A: derive and enforce `evm:<chain_id>:<normalized_contract_address>`
- Q: Which EVM multisig contract model should v1 target? -> A: OpenZeppelin `ERC7579Multisig` as the signer/threshold read model
- Q: What EVM proposal shape should v1 support? -> A: only ERC-7579 `execute(mode, executionCalldata)` coordination payloads
- Q: Which ERC-7579 modes are supported in v1? -> A: single-call and batch-call only, with default exec type and zero selector/mode payload; delegatecall, try-mode, and custom selector/payload are out of scope
- Q: What signer types should EVM v1 support? -> A: normalized EOA addresses only
- Q: What transport auth shape should EVM v1 use? -> A: keep `x-pubkey`, `x-signature`, and `x-timestamp`, but verify EVM requests using EIP-712 over a server-reconstructed payload; `x-pubkey` remains the legacy header name and carries the normalized signer address for EVM
- Q: What exact bytes should EVM proposal cosigners sign? -> A: a PSM-defined EIP-712 coordination message over `(mode, keccak256(execution_calldata))`; execution is out of scope and the signature is not required to be directly reusable on-chain in v1
- Q: Is the proposal create path allowed to include already-collected EVM signatures? -> A: no; create rejects non-empty signature arrays and signatures are appended only through `sign_delta_proposal`
- Q: What does the EVM proposal `nonce` mean? -> A: it is PSM-local ordering only, not an on-chain multisig nonce and not part of the proposal identifier
- Q: How should duplicate EVM proposal creation behave? -> A: same computed proposal identifier is idempotent and returns the existing pending proposal
- Q: What do legacy delta fields mean for EVM proposals? -> A: `prev_commitment`, `new_commitment`, `ack_sig`, `ack_pubkey`, and `ack_scheme` are explicitly unused for EVM v1 proposals
- Q: How should pending EVM proposals age out in v1? -> A: they stay pending until a future explicit cleanup/reconciliation feature, subject to the existing pending-proposal cap
- Q: What implementation approach is preferred? -> A: one unified network-aware implementation with an optional rollout gate for EVM, not a long-lived forked feature path

## Scope *(mandatory)*

### In Scope

- Add per-account network configuration so account configuration is no longer
  modeled as server-global network selection.
- Support EVM account configuration with network-aware metadata.
- Support EVM proposal creation, listing, retrieval, and signature collection.
- Support EVM signer validation through direct RPC reads.
- Re-validate signer authority for EVM accounts on all relevant account and
  proposal actions.
- Preserve existing Miden account and proposal behavior.
- Return explicit unsupported behavior for EVM flows that remain out of scope in
  v1.

### Out of Scope

- EVM delta push, state retrieval, merged delta retrieval, and canonicalization.
- Automatic execution tracking for EVM proposals.
- Indexer-based EVM validation in v1.
- Non-ECDSA EVM signing schemes in v1.
- Broad multisig SDK or example-app support unless required to validate the new
  lower-layer behavior.

## User Scenarios & Testing *(mandatory)*

- Prioritize stories (P1, P2, P3). Each story must be independently testable and
  deliver user value.
- Include transport expectations (HTTP/gRPC) and auth behavior when relevant.
- Because the server contract changes, at least one upstream client surface must
  be validated.

### User Story 1 - Configure Network-Aware Accounts (Priority: P1)

As an operator, I can configure an account with explicit per-account network
settings so PSM knows whether the account follows Miden or EVM behavior and can
preserve the correct validation rules for that account.

**Why this priority**: Every later EVM flow depends on account-level network
configuration, and this is the minimum change needed to avoid interfering with
existing Miden features.  
**Independent Test**: Configure one Miden account and one EVM account through
both HTTP and gRPC and verify that the persisted account configuration retains
the correct network-specific shape and validation behavior.

**Acceptance Scenarios**:

1. **Given** a Miden account configuration request, **When** the account is
   created, **Then** the persisted account keeps Miden-compatible behavior and
   no EVM-specific validation is required.
2. **Given** an EVM account configuration request with the required network
   fields, **When** the account is created, **Then** the account is persisted
   with EVM-specific network configuration and can later use EVM proposal
   workflows.
3. **Given** an EVM account configuration request missing required network
   fields, **When** the account is created, **Then** the request fails with an
   explicit validation error.

---

### User Story 2 - Share And Sign EVM Proposals (Priority: P2)

As an authorized cosigner for an EVM account, I can create, list, retrieve, and
sign pending proposals so proposal coordination works before any execution
tracking exists.

**Why this priority**: This is the core feature requested, and it should work
without requiring the broader EVM delta/canonicalization model in v1.  
**Independent Test**: Create a pending EVM proposal, retrieve it through both
transports, append signatures from authorized signers, and verify duplicate
signatures are rejected.

**Acceptance Scenarios**:

1. **Given** an EVM account with valid signer authority, **When** an authorized
   caller creates a proposal, **Then** the proposal is stored as pending with a
   deterministic hash-based PSM proposal identifier.
2. **Given** a pending EVM proposal, **When** an authorized cosigner signs it,
   **Then** the signature is appended and the updated pending proposal is
   returned.
3. **Given** a pending EVM proposal already signed by a signer, **When** the
   same signer signs again, **Then** the request fails with an explicit
   duplicate-signature error.
4. **Given** equivalent normalized EVM proposal contents for the same account,
   **When** those contents are submitted through either HTTP or gRPC, **Then**
   the resulting proposal identifier is the same.

---

### User Story 3 - Fail Explicitly For Unsupported EVM Flows (Priority: P3)

As an integrator, I can distinguish supported EVM proposal workflows from
unsupported EVM delta/state workflows so the system does not silently fall back
to Miden assumptions or leave behavior ambiguous.

**Why this priority**: The feature is intentionally partial in v1, so the
boundaries must be explicit to avoid architectural drift and accidental misuse.  
**Independent Test**: Call unsupported EVM delta/state/canonicalization flows
and verify they return explicit unsupported behavior rather than partial or
silent fallback semantics.

**Acceptance Scenarios**:

1. **Given** an EVM account, **When** an unsupported delta or state workflow is
   invoked, **Then** the system returns an explicit unsupported error for that
   account/network combination.
2. **Given** both Miden and EVM accounts exist, **When** supported flows are
   invoked on each, **Then** each account follows only its own network rules and
   no server-global network assumption leaks across accounts.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST support per-account network configuration so each
  configured account declares its network behavior independently of any
  server-global network selection.
- **FR-002**: The system MUST continue to support existing Miden accounts after
  the introduction of per-account network configuration.
- **FR-003**: The system MUST support an EVM account identity model based on
  the canonical string `evm:<chain_id>:<normalized_contract_address>`.
- **FR-003a**: For EVM accounts, `account_id` and `network_config` MUST agree
  on the same normalized `chain_id + contract_address` identity.
- **FR-004**: The system MUST persist EVM-specific account configuration through
  a `network_config`-style model rather than ad-hoc unrelated fields.
- **FR-005**: The system MUST support EVM proposal creation, listing, retrieval,
  and signature collection in v1.
- **FR-005a**: EVM proposal payloads in v1 MUST represent ERC-7579
  `execute(mode, executionCalldata)` requests and MUST NOT introduce a separate
  EVM proposal domain model.
- **FR-005b**: EVM proposal payloads in v1 MUST support only single-call and
  batch-call execution with default exec type and zero selector/mode payload;
  delegatecall, try-mode, and custom selector/payload combinations MUST fail
  explicitly.
- **FR-005c**: EVM proposal creation MUST reject non-empty submitted signature
  arrays; signatures are collected only through the sign-proposal flow.
- **FR-006**: The system MUST re-validate signer authority for EVM accounts on
  all relevant account and proposal actions.
- **FR-006a**: EVM signer validation in v1 MUST use direct RPC reads against the
  configured RPC endpoint and MUST fail explicitly when signer validation cannot
  be completed.
- **FR-006b**: EVM signer identities in v1 MUST be normalized EOA addresses;
  ERC-1271 contract signers and generic ERC-7913 verifier-key signers are out
  of scope for this feature.
- **FR-007**: The system MUST keep the auth/signature model extensible across
  networks while implementing ECDSA-only signing for EVM in v1.
- **FR-007a**: Authenticated EVM API requests MUST use EIP-712 signatures over
  a server-reconstructed request message derived from the canonical account ID,
  request timestamp, and request payload hash, while preserving the existing
  `x-pubkey`, `x-signature`, and `x-timestamp` transport fields.
- **FR-007b**: EVM proposal cosigning MUST use a PSM-defined EIP-712
  coordination message over the normalized proposal payload and MUST remain
  separate from any future on-chain execution signature format.
- **FR-008**: EVM proposal identifiers MUST be deterministic hash-based values
  derived from the normalized tuple `(chain_id, contract_address, mode,
  keccak256(execution_calldata))`.
- **FR-008a**: EVM proposal identifiers MUST exclude collected signatures,
  timestamps, and the PSM-local proposal nonce.
- **FR-009**: Unsupported EVM flows, including `push_delta`, `get_delta`,
  `get_delta_since`, `get_state`, and canonicalization-related behavior, MUST
  fail explicitly rather than reusing Miden semantics or silently degrading.
- **FR-010**: Existing Miden proposal behavior MUST remain unaffected by EVM
  support.
- **FR-011**: The EVM proposal `nonce` field MUST remain a PSM-local ordering
  field and MUST NOT be interpreted as an on-chain multisig nonce.
- **FR-012**: Re-submitting an EVM proposal whose normalized identity matches an
  existing pending proposal MUST be idempotent and return the existing proposal.
- **FR-013**: HTTP and gRPC error surfaces MUST expose stable application error
  codes for EVM-specific failures in addition to transport-native status
  information.
- **FR-014**: EVM proposal records MUST leave `prev_commitment`,
  `new_commitment`, `ack_sig`, `ack_pubkey`, and `ack_scheme` unused in v1
  rather than assigning Miden-specific semantics to them.

### Contract / Transport Impact

- HTTP and gRPC account configuration requests will need to carry per-account
  network configuration rather than assuming only the current Miden model.
- HTTP and gRPC proposal requests and responses must remain semantically aligned
  for EVM proposal create/list/get/sign flows.
- Rust and TypeScript base clients will need corresponding request/response
  support for network-aware account configuration and EVM proposal workflows.
- Auth headers and gRPC metadata remain explicit; EVM v1 uses ECDSA signatures
  over EIP-712 typed messages while the overall auth model remains extensible.
- For EVM accounts, the existing transport fields `x-pubkey`, `x-signature`,
  and `x-timestamp` remain in place; `x-pubkey` keeps its legacy name and
  carries the normalized signer address.
- EVM request-auth payload binding is transport-specific but semantically
  aligned:
  - HTTP uses `keccak256` of canonical JSON request bytes.
  - gRPC uses `keccak256` of protobuf-encoded request bytes.
- The supported HTTP route shape remains the current `/delta`, `/delta/since`,
  `/delta/proposal`, `/delta/proposal/single`, `/state`, and `/configure`
  surface rather than introducing a second parallel HTTP contract for EVM.
- EVM signer validation depends on the configured RPC endpoint rather than an
  indexer in v1.
- At least one upstream client surface must validate the new network-aware
  account configuration and EVM proposal flows once the server contract changes.
- Fallback behavior remains explicit: unsupported EVM delta/state flows must not
  silently fall back to Miden or to partially supported online/offline logic.
- Stable application error codes are required for unsupported-network,
  RPC-validation, signer-authorization, invalid-payload, and duplicate-signature
  failures.

### Data / Lifecycle Impact

- Account metadata will need a network-aware configuration model that can
  represent at least Miden and EVM account settings.
- The EVM account configuration is expected to include `chain_id`,
  `contract_address`, and `rpc_endpoint`.
- EVM account configuration uses the canonical account identity
  `evm:<chain_id>:<normalized_contract_address>`.
- EVM configure requests use an empty-object `initial_state` placeholder in v1;
  the server derives the signer snapshot and threshold view from RPC rather than
  requiring a Miden-style initial state payload.
- The EVM signer/threshold read model is based on OpenZeppelin
  `ERC7579Multisig`, using RPC reads for the signer slice and current threshold.
- EVM proposals are pending-only proposals in v1 and model ERC-7579
  `execute(mode, executionCalldata)` requests rather than Miden `tx_summary`
  payloads.
- EVM proposal records use a deterministic PSM-defined hash identifier derived
  from normalized proposal contents rather than raw field concatenation.
- EVM proposal signatures append to pending proposal records within the
  account/network namespace; v1 does not redefine append-only proposal storage
  semantics.
- EVM proposal identifiers exclude local ordering nonce, collected signatures,
  and timestamps.
- EVM proposal `nonce` remains local ordering only and may differ between
  otherwise identical create attempts without changing proposal identity.
- Repeated create attempts for the same normalized EVM proposal are idempotent.
- EVM proposal records leave Miden-specific delta fields unset/unused in v1.
- Pending EVM proposals stay pending until a future explicit cleanup or
  reconciliation feature is defined, subject to the existing per-account
  pending-proposal limit.
- Backend parity applies because the same network-aware account metadata and
  proposal semantics must persist consistently across filesystem and Postgres.
- If the new EVM workflow is surfaced through higher-level SDKs or example
  applications, the corresponding docs and examples must be updated in the same
  change; otherwise the current Miden-facing examples remain unchanged in v1.

## Edge Cases *(mandatory)*

- EVM account configuration provides an invalid `chain_id`, invalid contract
  address, or malformed network config.
- EVM account configuration provides a non-canonical `account_id` that does not
  match `chain_id + contract_address`.
- Signer authority changes between account configuration and later proposal
  actions.
- The configured RPC endpoint is unavailable or returns state that does not
  match the expected signer set.
- Duplicate proposal signatures are submitted by the same signer.
- A create request submits a non-empty EVM signature list.
- A create request uses an unsupported ERC-7579 mode such as delegatecall,
  try-mode, or non-zero selector/mode payload.
- Equivalent EVM proposals are re-submitted with different local ordering
  nonces.
- Deterministic proposal identity diverges across transports or languages unless
  proposal inputs are normalized identically before hashing.
- Miden and EVM accounts coexist on the same server and must not leak network
  behavior into one another.
- Backend-specific persistence differences must not change observable EVM
  account or proposal behavior.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A user can configure both Miden and EVM accounts through HTTP and
  gRPC, and Miden account behavior remains unchanged after the contract update.
- **SC-002**: A user can create, list, retrieve, and sign EVM proposals through
  both transports, and duplicate signatures are rejected explicitly.
- **SC-003**: Unsupported EVM delta/state/canonicalization flows return explicit
  unsupported behavior rather than partial success, silent fallback, or Miden
  semantics.

## Assumptions

- The first EVM target is OpenZeppelin `ERC7579Multisig` as the signer and
  threshold read model for PSM account validation.
- EVM signer validation is re-checked on every relevant action.
- ECDSA is the only implemented EVM signature scheme in v1, but the model is
  intentionally extensible.
- EVM request authentication and EVM proposal cosigning both use PSM-defined
  EIP-712 typed messages reconstructed by the server from the received request
  and proposal payloads.
- EVM signer identities are normalized EOA addresses only in v1.
- EVM proposal identifiers are fixed by the normalized tuple `(chain_id,
  contract_address, mode, keccak256(execution_calldata))`.
- A future explicit sync or reconciliation flow may be added to resolve EVM
  proposal status, but execution tracking is not part of this feature.
- The desired architectural direction is to remove server-global network
  selection, persist account-specific network configuration in metadata, and
  keep network-specific validation logic behind network-specific implementations.
- No backward-compatibility fallback is required for accounts missing
  `network_config`; explicit configuration is required in this feature.
- `rpc_endpoint` is trusted per-account configuration in v1; endpoint
  replacement policy is deferred.

## Dependencies

- OpenZeppelin `ERC7579Multisig` and its corresponding ERC-7579 mode encoding
  rules as the EVM signer/threshold and execution-shape reference.
- Updates to the server contract, Rust client, and TypeScript client.
- A network-aware configuration model that can safely support both existing
  Miden accounts and new EVM accounts.

## Deferred Topics

- RPC endpoint replacement or rotation policy is deferred; v1 treats
  `rpc_endpoint` as trusted immutable account configuration.
- Proposal execution reuse and explicit reconciliation against on-chain state
  are deferred follow-up features.

## Delivery Guidance

- Implement the account/network refactor as one unified network-aware
  architecture rather than forking EVM service logic behind a deep feature flag.
- A rollout gate may reject new EVM account configuration until the deployment
  is ready, but the code path should still use the same shared account
  resolution, auth, and proposal-service architecture as Miden.

## Recommended Future Revisit

- Revisit whether EVM proposals should gain explicit sync or execution-tracking
  support once the team settles the contract and validation-source design.
