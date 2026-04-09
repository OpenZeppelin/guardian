# Implementation Plan: Add generic EVM proposal sharing and signing support

**Feature Key**: `001-evm-proposal-support` | **Branch**: `001-evm-proposal-support` (manual) | **Date**: 2026-03-18 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `/speckit/features/001-evm-proposal-support/spec.md`

## Summary

Introduce account-level `network_config` so Miden and EVM accounts can coexist
in the same server, then refactor proposal and auth flows to route through
network-specific capabilities instead of the current server-global network
client. The implementation starts in `crates/server`, propagates contract
changes into `crates/client` and `packages/guardian-client`, preserves current Miden
behavior, and makes unsupported EVM delta/state/canonicalization flows fail
explicitly until a later lifecycle feature is defined.

## Technical Context

**Language/Version**: Rust workspace crates + TypeScript packages; Miden `0.13.x` compatibility remains required  
**Primary Dependencies**: `axum`, `tonic`/`prost`, `tokio`, `serde_json`, current metadata/storage backends, `private_state_manager_shared`, TS `packages/guardian-client`; EVM RPC integration stays behind a server adapter boundary while the concrete contract-read surface is finalized  
**Storage**: Filesystem by default; Postgres metadata/storage parity remains required  
**Testing**: Targeted Rust server/client tests, HTTP/gRPC adapter tests, TS `packages/guardian-client` tests, backend parity tests, and conditional example smoke checks if the base-client surface reaches examples  
**Target Platform**: Rust server, Rust gRPC client, and TypeScript HTTP client  
**Project Type**: Multi-language monorepo  
**Performance Goals**: One signer-authority RPC read per authenticated EVM action is acceptable in v1; avoid backend-specific behavioral drift or unbounded proposal scans  
**Constraints**: Preserve existing Miden behavior, keep HTTP/gRPC and Rust/TS parity, keep fallback behavior explicit, require explicit `network_config` without backward-compatibility fallbacks, keep append-only proposal storage semantics, and avoid broad API replacement while the EVM contract shape is still settling  
**Scale/Scope**: `crates/server` (builder, main, network, metadata, services, api, proto, tests), `crates/client` (proto + request/response support), `packages/guardian-client` (types, conversion, HTTP client, tests); multisig SDKs and examples are assessed but expected to remain unchanged in v1 unless lower-layer contract changes force propagation

Implementation guidance: perform the account/network refactor as one unified
network-aware architecture, optionally guarded by a rollout switch that rejects
new EVM account configuration until enabled. Avoid a long-lived forked EVM
service path behind a deep feature flag.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

- [x] Bottom-up impact assessed: server -> clients -> multisig -> examples
- [x] HTTP and gRPC semantics remain aligned, or intentional divergence is documented
- [x] Rust and TypeScript behavior remain aligned, or intentional divergence is documented
- [x] Storage backend semantics remain aligned, or a backend-specific limitation is documented
- [x] Append-only, canonicalization, auth, and proposal invariants remain preserved
- [x] Fallback behavior remains explicit; no silent online/offline degradation is introduced
- [x] High-risk areas have an explicit test and validation plan
- [x] At least one upstream consumer validation path is defined for lower-layer changes

## Refactor Strategy

### Workstream 1: Account-Level Network Configuration

- Add persisted `network_config` to account metadata so account behavior no
  longer depends on one server-global network selection.
- Model `network_config` as a tagged enum with at least `miden` and `evm`
  variants.
- Preserve current Miden semantics by moving existing server-global Miden
  selection into account metadata for newly configured accounts.

### Workstream 2: Capability-Oriented Network Dispatch

- Replace the single `AppState.network_client` dependency with an account-aware
  network registry or resolver.
- Split current network responsibilities into focused capabilities:
  - account configuration validation
  - signer authority refresh/authorization
  - proposal normalization and proposal-id generation
  - delta/state/canonicalization operations
- Keep Miden as the reference implementation of the full capability set.
- Add an EVM implementation that only supports the v1 proposal/account
  capabilities and returns explicit unsupported errors for delta/state
  capabilities.

### Workstream 3: Auth And Authorization Decoupling

- Separate cryptographic request verification from signer authorization source
  lookup. The current `Auth::verify` path couples those concerns too tightly for
  RPC-backed EVM signer validation.
- Preserve request-auth headers and replay protection as they work today.
- For EVM v1, verify the request signature cryptographically, derive the signer
  commitment, then validate signer authorization through RPC on every relevant
  action.
- Keep a cached signer snapshot in metadata only as persisted state or
  optimization, not as the sole source of truth for EVM authorization.

### Workstream 4: Proposal-Service Refactor

- Keep the existing proposal endpoints and base-client method names in v1 to
  constrain blast radius.
- Move Miden-specific proposal normalization and deterministic proposal-id logic
  out of `push_delta_proposal` and into network-specific proposal strategies.
- Route `push_delta_proposal`, `get_delta_proposals`, `get_delta_proposal`, and
  `sign_delta_proposal` through the account-level network capability layer.
- Make EVM proposal identifiers deterministic hash-based values generated from a
  normalized proposal identity input set.
- Keep EVM proposals pending-only in v1 and defer lifecycle reconciliation until
  the contract team defines whether sync/execution tracking belongs in scope.

### Workstream 5: Contract And Client Propagation

- Extend HTTP and gRPC configure requests with `network_config`.
- Extend auth config support to include an EVM ECDSA variant while keeping the
  overall auth model extensible.
- Update `crates/client` and `packages/guardian-client` request/response types,
  conversion layers, and tests to reflect the new account-configuration and
  proposal semantics.
- Assess multisig SDKs and examples after server/base-client design is settled;
  update them only if the lower-layer contract change becomes observable there.

### Deferred Topics

- RPC endpoint replacement or rotation policy for EVM accounts.
- Future sync/reconciliation or execution-tracking behavior for pending EVM
  proposals.

## Project Structure

### Documentation (this feature)

```text
speckit/features/001-evm-proposal-support/
├── spec.md
├── plan.md
├── research.md
├── data-model.md
├── quickstart.md
├── contracts/
│   ├── grpc-contract.md
│   └── http-openapi.yaml
└── tasks.md
```

### Source Code (repository root)

```text
crates/
├── server/
├── client/
├── shared/
├── miden-multisig-client/
├── miden-rpc-client/
└── miden-keystore/

packages/
├── guardian-client/
└── miden-multisig-client/

examples/
├── demo/
├── web/
└── rust/

spec/
└── system and protocol reference docs
```

**Structure Decision**:

- `crates/server/src/metadata/`: add `network_config` to persisted account metadata and backend serialization.
- `crates/server/src/network/`: replace server-global network handling with account-level dispatch and add EVM-specific implementation modules.
- `crates/server/src/services/`: refactor account resolution, configure-account, and proposal services around network capabilities.
- `crates/server/src/api/` and `crates/server/proto/`: carry the network-aware contract changes through HTTP and gRPC.
- `crates/server/src/error.rs`: add explicit unsupported-operation errors for network/capability mismatches.
- `crates/client/`: mirror gRPC contract changes and keep request-auth behavior aligned.
- `packages/guardian-client/src/`: update request/response types, conversion, and HTTP behavior for network-aware accounts and EVM proposals.
- `examples/` and multisig SDKs: assess impact after base-layer changes; keep out of scope unless propagation is required.

## Validation Plan

- Targeted Rust tests:
  `cargo test -p private-state-manager-server`
  `cargo test -p private-state-manager-client`
- Targeted TypeScript tests:
  `cd packages/guardian-client && npm test`
- Server-specific regression targets:
  - account metadata serialization for filesystem and Postgres
  - `configure_account` for Miden and EVM
  - `resolve_account` request verification and EVM signer re-validation
  - proposal create/get/list/sign for Miden and EVM
  - explicit unsupported EVM `push_delta`, `get_delta`, `get_delta_since`, `get_state`, and canonicalization paths
  - HTTP and gRPC adapter parity for configure and proposal flows
- Upstream validation:
  - at least one Rust client path in `crates/client`
  - at least one TypeScript client path in `packages/guardian-client`
- Example validation when affected:
  `cargo run -p guardian-demo`
  `cd examples/web && npm run dev`
- Broader validation to run if blast radius grows:
  `cargo test --workspace`

## Post-Design Constitution Check

- The plan preserves bottom-up propagation by making the server change first and
  explicitly tracking Rust and TypeScript client propagation.
- HTTP and gRPC stay aligned by sharing the same account-level network concepts
  and explicit unsupported-operation semantics.
- Storage backend parity is preserved because `network_config` and proposal
  persistence changes are designed for both filesystem and Postgres metadata
  backends.
- Append-only proposal behavior is preserved by keeping pending proposal storage
  and signature appends explicit, while unsupported EVM lifecycle transitions
  return explicit errors instead of implicit fallbacks.

## Complexity Tracking

> Fill only when a constitution gate requires an explicit justification.

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| Reuse existing delta-proposal endpoints for EVM | Keeps the initial blast radius inside the current server/client layers while the EVM contract shape is still being settled | Introducing a brand-new proposal API would require a broader migration across server, Rust client, TS client, and examples before the contract team has finalized payload and signing semantics |
