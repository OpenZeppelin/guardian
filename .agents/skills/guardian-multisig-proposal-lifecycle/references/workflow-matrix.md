# Proposal Workflow Matrix

Use this file to decide what else must be checked when one proposal stage changes.

## Create

Primary surfaces:
- `crates/miden-multisig-client/src/transaction/*`
- `crates/miden-multisig-client/src/client/proposals.rs`
- `packages/miden-multisig-client/src/transaction/*`
- `packages/miden-multisig-client/src/multisig/proposal/parser.ts`

Verify:
- metadata fields are complete and explicit
- required signatures are derived correctly
- proposal commitment is stable

## List And Parse

Primary surfaces:
- `crates/miden-multisig-client/src/proposal.rs`
- `packages/miden-multisig-client/src/proposal/metadata.ts`
- `packages/miden-multisig-client/src/types/proposal.ts`

Verify:
- malformed payloads fail loudly
- status mapping is exhaustive
- proposal type mapping is stable across Rust and TS

## Sign

Primary surfaces:
- `crates/miden-multisig-client/src/client/proposals.rs`
- `packages/miden-multisig-client/src/multisig/signing.ts`
- signature utility modules in both stacks

Verify:
- non-cosigner rejection
- already-signed rejection
- Falcon and ECDSA handling when relevant
- public key requirements for ECDSA are preserved

## Ready To Execute

Primary surfaces:
- readiness checks in both SDKs
- threshold and procedure-threshold logic

Verify:
- collected vs required signatures are reported correctly
- signer-update transactions validate against current account signers, not proposed future signers

## Execute

Primary surfaces:
- `crates/miden-multisig-client/src/execution.rs`
- `crates/miden-multisig-client/src/client/proposals.rs`
- `packages/miden-multisig-client/src/multisig/proposal/execution.ts`

Verify:
- canonical transaction summary binding
- guardian ack inclusion when required
- `SwitchGuardian` special handling
- post-execution sync behavior

## Export, Import, Offline Sign

Primary surfaces:
- `crates/miden-multisig-client/src/export.rs`
- `crates/miden-multisig-client/src/client/offline.rs`
- offline proposal helpers in `packages/miden-multisig-client`

Verify:
- proposal id remains stable after export and import
- imported signatures are preserved exactly
- offline execution claims stay explicit and narrow

## Upstream Canaries

Rust:
- `examples/demo/src/actions/proposal_management.rs`
- `examples/demo/src/actions/verify_state_commitment.rs`

TypeScript:
- `examples/smoke-web/src/smokeHarness.ts`
- `examples/_shared/multisig-browser/src/multisigApi.ts`
- `examples/web/src/lib/multisigApi.ts`

Required manual smoke increases when:
- proposal metadata changed
- offline import or export changed
- signer scheme behavior changed
- post-execution convergence changed
