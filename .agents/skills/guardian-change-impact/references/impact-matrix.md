# Guardian Change Impact Matrix

Use this file to classify the request before editing code.

## Server Contract

Trigger:
- `guardian.proto`
- HTTP JSON fields
- endpoint semantics
- status enums
- auth metadata requirements

Likely surfaces:
- `crates/server/proto/guardian.proto`
- `crates/server/src/api/http.rs`
- `crates/server/src/api/grpc.rs`
- `crates/server/src/services/*`
- `crates/client/src/client.rs`
- `packages/guardian-client/src/server-types.ts`
- `packages/guardian-client/src/conversion.ts`
- both multisig SDKs if proposal or state shapes change

Validation:
- `cargo test -p guardian-server`
- `cargo test -p guardian-client`
- `cd packages/guardian-client && npm test`
- one upstream smoke when user-visible behavior changes

## Canonicalization Or State Lifecycle

Trigger:
- pending, candidate, canonical, discarded behavior
- nonce or commitment convergence
- `get_delta_since`
- canonicalization worker or storage rules

Likely surfaces:
- `crates/server/src/jobs/canonicalization/*`
- `crates/server/src/services/get_delta_since.rs`
- `crates/server/src/services/push_delta.rs`
- `crates/server/src/services/push_delta_proposal.rs`
- `crates/server/src/storage/*`
- `crates/server/src/metadata/*`
- multisig sync and example canaries if client-observable

Validation:
- `cargo test -p guardian-server`
- integration or e2e server tests when transport semantics changed
- at least one Rust or TS multisig smoke if convergence behavior is visible upstream

## Auth Or Signature Flow

Trigger:
- request signing
- `x-pubkey`, `x-signature`, `x-timestamp`
- Falcon or ECDSA behavior
- ack key, ack signature, ack scheme, ack pubkey
- keystore or signature encoding

Likely surfaces:
- `crates/server/src/ack/*`
- `crates/server/src/metadata/auth/*`
- `crates/client/src/auth/*`
- `crates/client/src/keystore/*`
- `packages/guardian-client/src/auth-request.ts`
- multisig execution or signature utility modules

Validation:
- both Rust and TS client tests where relevant
- both Falcon and ECDSA targeted coverage
- manual smoke if proposal execution or browser signers are affected

## Base Client Parity

Trigger:
- raw client request or response shapes
- conversion rules
- error shapes
- HTTP or gRPC client semantics

Likely surfaces:
- `crates/client/src/*`
- `packages/guardian-client/src/*`

Validation:
- `cargo test -p guardian-client`
- `cd packages/guardian-client && npm test`

## Multisig Proposal Lifecycle

Trigger:
- create, list, sign, execute
- threshold counting
- export, import, offline sign
- proposal metadata mapping
- `SwitchGuardian`

Likely surfaces:
- `crates/miden-multisig-client/src/client/*`
- `crates/miden-multisig-client/src/proposal.rs`
- `crates/miden-multisig-client/src/execution.rs`
- `packages/miden-multisig-client/src/multisig/*`
- `packages/miden-multisig-client/src/proposal/*`
- `packages/miden-multisig-client/src/transaction/*`

Validation:
- Rust and TS multisig package tests
- example smoke in `examples/demo` or `examples/smoke-web`

## Browser Signer Or Example Integration

Trigger:
- Para or Miden Wallet integration
- browser-only workflows
- shared example adapters

Likely surfaces:
- `examples/_shared/multisig-browser/src/*`
- `examples/smoke-web/src/*`
- `examples/web/src/*`
- TS multisig signers and client wrappers

Validation:
- `cd examples/smoke-web && npm run typecheck && npm run build`
- `cd examples/web && npm run build`
- browser smoke with isolated profiles when signer behavior changed

## Deploy, Release, Or Benchmarks

Trigger:
- ECS, Terraform, AWS auth, prod rollout
- SDK version bump or publish
- benchmark execution or reporting

Use the existing specialized skills instead of expanding this one:
- `deploy-guardian-aws`
- `release-guardian-sdk-packages`
- `run-guardian-prod-benchmarks`
