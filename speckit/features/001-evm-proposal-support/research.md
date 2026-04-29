# Research: Domain-separated EVM proposal support

## Decision 1: Keep Miden and EVM HTTP domains separate

- Decision: Miden continues to own `/configure`, `/delta`, `/delta/proposal`,
  `/state`, canonicalization, and gRPC. EVM v1 uses `/evm/auth/*`,
  `/evm/accounts`, and `/evm/proposals*`.
- Rationale: EVM account registration, cookie wallet sessions, UserOperation
  hashes, expiry, and cancel/export semantics differ enough from Miden deltas
  that forcing them through the delta proposal API creates misleading coupling.
- Alternatives considered:
  - Reuse `/configure` and `/delta/proposal` for EVM. Rejected because the API
    would expose EVM-only fields through Miden-shaped routes and make future
    networks harder to add cleanly.
  - Create a fully independent service stack. Rejected because storage,
    metadata, errors, clocks, routing infrastructure, and validation conventions
    can still be shared.

## Decision 2: Keep EVM behind a server feature flag

- Decision: EVM behavior is enabled only with the server-side `evm` feature.
  Default builds do not register `/evm/*` routes and do not initialize EVM
  state, sessions, contract readers, or proposal services.
- Rationale: EVM support adds a new wallet-session model, chain RPC reads, and
  optional dependencies. The feature flag keeps default deployments Miden-only.
- Alternatives considered:
  - Enable EVM by default. Rejected because it implies broader chain support and
    operational readiness than v1 provides.
  - Split feature flags by chain. Rejected because chain support is deployment
    configuration, not code architecture.

## Decision 3: Resolve EVM chain endpoints server-side

- Decision: EVM account registration carries `chain_id`, `account_address`, and
  `multisig_validator_address`. RPC URLs and EntryPoint addresses come from
  server environment maps.
- Rationale: clients should not choose Guardian's chain authority. Keeping RPC
  and EntryPoint configuration server-owned avoids per-request trust drift.
- Alternatives considered:
  - Accept RPC URLs from clients. Rejected because it lets untrusted callers
    define the source of signer and nonce truth.
  - Store one global EVM RPC URL. Rejected because configured accounts may live
    on different chains.

## Decision 4: Use cookie-backed EVM sessions

- Decision: EVM auth uses EIP-712 challenge verification and an expiring
  `guardian_evm_session` cookie. The server derives the session address from
  signature recovery and uses that address for authorization.
- Rationale: this matches Guardian operator-session conventions while meeting
  the requirement that identity is cryptographically derived from the wallet
  signature, challenges are single-use, and sessions expire.
- Alternatives considered:
  - JWT sessions. Rejected because cookies already fit the server session model.
  - Reuse Miden `x-pubkey` request signing. Rejected because EVM wallet auth is
    naturally session-based and route-scoped for this feature.

## Decision 5: Validate EVM authority from the smart account validator

- Decision: `/evm/accounts` verifies ERC-7579 validator installation and reads
  signer/threshold snapshots through Alloy. Proposal actions authorize the
  session signer against the stored proposal signer snapshot.
- Rationale: account registration must prove the smart account uses the target
  multisig validator, while proposals need deterministic membership for the
  lifetime of the coordination record.
- Alternatives considered:
  - Trust client-provided signer lists. Rejected because the server would not
    independently know proposal authority.
  - Re-read signer lists for every proposal approval. Deferred because proposal
    snapshot semantics are simpler and predictable for v1.

## Decision 6: Store EVM proposals separately at the API boundary

- Decision: public EVM responses are EVM proposal records, not `DeltaObject`.
  The implementation may reuse existing proposal storage infrastructure where
  that stays simple, but it must not leak Miden delta fields into `/evm/*`.
- Rationale: this preserves a clean EVM client API while avoiding a larger
  storage migration for v1.
- Alternatives considered:
  - Add a dedicated EVM proposal table immediately. Rejected as unnecessary for
    the first implementation.
  - Return `DeltaObject` from EVM endpoints. Rejected because most fields are
    Miden-specific and semantically unused for EVM.

## Deferred Topics

- EVM gRPC support.
- On-chain submission and bundler integration.
- ERC-1271, weighted multisig, generic ERC-7913 signer bytes, and execution
  tracking beyond lazy EntryPoint nonce cleanup.
