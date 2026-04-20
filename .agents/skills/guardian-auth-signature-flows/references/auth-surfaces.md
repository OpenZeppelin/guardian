# Auth And Signature Surfaces

Use this file to find the boundary modules for Guardian auth and signing behavior.

## Server

Ack signing:
- `crates/server/src/ack/mod.rs`
- `crates/server/src/ack/miden_falcon_rpo/*`
- `crates/server/src/ack/miden_ecdsa/*`
- `crates/server/src/bin/ack-keygen.rs`

Auth verification:
- `crates/server/src/metadata/auth/*`
- `crates/server/src/api/http.rs`
- `crates/server/src/api/grpc.rs`

Use these when:
- ack scheme handling changed
- server public key or commitment behavior changed
- auth metadata validation changed

## Rust Client

- `crates/client/src/auth/*`
- `crates/client/src/keystore/*`
- `crates/client/src/client.rs`
- `crates/client/src/error.rs`
- `crates/client/src/testing/*`

Use these when:
- request signing changed
- signer abstraction changed
- public key export changed
- auth header generation changed

## TypeScript Client

- `packages/guardian-client/src/auth-request.ts`
- `packages/guardian-client/src/http.ts`
- `packages/guardian-client/src/conversion.ts`
- `packages/guardian-client/src/server-types.ts`
- `packages/guardian-client/src/*.test.ts`

Use these when:
- canonicalization of signed request payloads changed
- header or error shape changed
- ack field decoding changed

## Multisig And Examples

Rust:
- `crates/miden-multisig-client/src/execution.rs`
- `crates/miden-multisig-client/src/transaction/guardian.rs`

TypeScript:
- `packages/miden-multisig-client/src/utils/signature.ts`
- `packages/miden-multisig-client/src/utils/encoding.ts`
- `packages/miden-multisig-client/src/signers/*`
- `packages/miden-multisig-client/src/multisig/proposal/execution.ts`

Examples:
- `examples/rust/src/main.rs`
- `examples/smoke-web/src/smokeHarness.ts`
- `examples/_shared/multisig-browser/src/*`

Inspect these when auth or signature changes are observable in proposal execution or browser signers.

## Validation Rules

- Exercise both Falcon and ECDSA when the touched path supports both.
- Verify public key requirements separately for ECDSA proposal signatures.
- Verify ack fields remain populated where execution depends on them.
- Prefer targeted unit tests first, then manual smoke for end-to-end signature collection or execution flows.
