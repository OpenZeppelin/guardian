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
  `chain_id + contract_address`.
- **FR-004**: The system MUST persist EVM-specific account configuration through
  a `network_config`-style model rather than ad-hoc unrelated fields.
- **FR-005**: The system MUST support EVM proposal creation, listing, retrieval,
  and signature collection in v1.
- **FR-006**: The system MUST re-validate signer authority for EVM accounts on
  all relevant account and proposal actions.
- **FR-006a**: EVM signer validation in v1 MUST use direct RPC reads against the
  configured RPC endpoint and MUST fail explicitly when signer validation cannot
  be completed.
- **FR-007**: The system MUST keep the auth/signature model extensible across
  networks while implementing ECDSA-only signing for EVM in v1.
- **FR-008**: EVM proposal identifiers MUST be deterministic hash-based values
  derived from a PSM-defined normalized proposal identity scheme.
- **FR-009**: Unsupported EVM flows, including `push_delta`, `get_delta`,
  `get_delta_since`, `get_state`, and canonicalization-related behavior, MUST
  fail explicitly rather than reusing Miden semantics or silently degrading.
- **FR-010**: Existing Miden proposal behavior MUST remain unaffected by EVM
  support.

### Contract / Transport Impact

- HTTP and gRPC account configuration requests will need to carry per-account
  network configuration rather than assuming only the current Miden model.
- HTTP and gRPC proposal requests and responses must remain semantically aligned
  for EVM proposal create/list/get/sign flows.
- Rust and TypeScript base clients will need corresponding request/response
  support for network-aware account configuration and EVM proposal workflows.
- Auth headers and gRPC metadata remain explicit; EVM v1 uses ECDSA signatures,
  while the overall auth model remains extensible.
- EVM signer validation depends on the configured RPC endpoint rather than an
  indexer in v1.
- At least one upstream client surface must validate the new network-aware
  account configuration and EVM proposal flows once the server contract changes.
- Fallback behavior remains explicit: unsupported EVM delta/state flows must not
  silently fall back to Miden or to partially supported online/offline logic.

### Data / Lifecycle Impact

- Account metadata will need a network-aware configuration model that can
  represent at least Miden and EVM account settings.
- The EVM account configuration is expected to include `chain_id`,
  `contract_address`, and `rpc_endpoint`.
- EVM proposals are pending proposals in v1. Terminal lifecycle handling beyond
  pending status is not yet defined for v1.
- EVM proposal records use a deterministic PSM-defined hash identifier derived
  from normalized proposal contents rather than raw field concatenation.
- EVM proposal signatures append to pending proposal records within the
  account/network namespace; v1 does not redefine append-only proposal storage
  semantics.
- Backend parity applies because the same network-aware account metadata and
  proposal semantics must persist consistently across filesystem and Postgres.
- If the new EVM workflow is surfaced through higher-level SDKs or example
  applications, the corresponding docs and examples must be updated in the same
  change; otherwise the current Miden-facing examples remain unchanged in v1.

## Edge Cases *(mandatory)*

- EVM account configuration provides an invalid `chain_id`, invalid contract
  address, or malformed network config.
- Signer authority changes between account configuration and later proposal
  actions.
- The configured RPC endpoint is unavailable or returns state that does not
  match the expected signer set.
- Duplicate proposal signatures are submitted by the same signer.
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

- The first EVM target is a generic EVM multisig model with a normalized signer
  and threshold view rather than a contract-specific execution model.
- EVM signer validation is re-checked on every relevant action.
- ECDSA is the only implemented EVM signature scheme in v1, but the model is
  intentionally extensible.
- The EVM proposal identifier is a deterministic PSM-defined hash, but the
  exact normalized input set still needs team confirmation.
- A future explicit sync or reconciliation flow may be added to resolve EVM
  proposal status, but execution tracking is not part of this feature unless the
  team decides otherwise.
- The desired architectural direction is to remove server-global network
  selection, persist account-specific network configuration in metadata, and
  keep network-specific validation logic behind network-specific implementations.
- No backward-compatibility fallback is required for accounts missing
  `network_config`; explicit configuration is required in this feature.

## Dependencies

- Team decisions on the EVM contract shape and RPC-readable signer validation shape.
- Updates to the server contract, Rust client, and TypeScript client.
- A network-aware configuration model that can safely support both existing
  Miden accounts and new EVM accounts.

## Open Questions For Team

1. Is there already a target multisig contract definition for v1? If so, which
   read methods or verified contract shape will PSM rely on through RPC to fetch
   signer set, threshold, nonce, and any other required account state?
2. What exact proposal payload shape defines an EVM proposal in v1? Is it a
   regular transaction payload, EIP-712 typed data, or another structured form?
3. What exact bytes or structured fields do EVM cosigners sign in v1?
4. Without execution tracking, should v1 keep EVM proposals pending
   indefinitely, or should it include an explicit sync or reconciliation
   operation based on RPC-readable contract state?
5. What is the intended RPC failure and mutability policy for EVM accounts? If
   the configured RPC endpoint fails or becomes invalid, can users update it,
   and if so what security constraints must apply?

## Recommended Future Revisit

- Revisit whether EVM proposals should gain explicit sync or execution-tracking
  support once the team settles the contract and validation-source design.
