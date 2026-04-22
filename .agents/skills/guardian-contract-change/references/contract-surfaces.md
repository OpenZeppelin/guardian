# Guardian Contract Surfaces

Use this file to decide which consumers must move with the server contract.

## Primary Server Sources

- `crates/server/proto/guardian.proto`
- `crates/server/src/api/http.rs`
- `crates/server/src/api/grpc.rs`
- `crates/server/src/services/*`
- `crates/server/src/delta_object.rs`
- `crates/server/src/state_object.rs`

## Rust Client Surfaces

- `crates/client/src/client.rs`
- `crates/client/src/lib.rs`
- `crates/client/src/error.rs`
- `crates/client/src/testing/*`

Use these when:
- request fields changed
- response field presence changed
- auth metadata or signing behavior changed
- proto messages changed

## TypeScript Client Surfaces

- `packages/guardian-client/src/server-types.ts`
- `packages/guardian-client/src/conversion.ts`
- `packages/guardian-client/src/http.ts`
- `packages/guardian-client/src/types.ts`
- tests in `packages/guardian-client/src/*.test.ts`

Use these when:
- HTTP JSON shape changed
- union or status mapping changed
- optionality changed
- error decoding changed

## Multisig Surfaces

Rust:
- `crates/miden-multisig-client/src/client/proposals.rs`
- `crates/miden-multisig-client/src/client/account.rs`
- `crates/miden-multisig-client/src/client/offline.rs`
- `crates/miden-multisig-client/src/proposal.rs`
- `crates/miden-multisig-client/src/execution.rs`

TypeScript:
- `packages/miden-multisig-client/src/client.ts`
- `packages/miden-multisig-client/src/raw-client.ts`
- `packages/miden-multisig-client/src/multisig/proposal/*`
- `packages/miden-multisig-client/src/proposal/*`
- `packages/miden-multisig-client/src/transaction/*`

Inspect these when:
- proposal metadata changed
- proposal status semantics changed
- ack data changed
- state fields used by multisig sync changed

## Example And Doc Surfaces

- `examples/demo/src/actions/*`
- `examples/smoke-web/src/smokeHarness.ts`
- `examples/_shared/multisig-browser/src/*`
- `examples/web/src/lib/*`
- `docs/MULTISIG_SDK.md`
- `spec/api.md`

Update these when the public workflow or expected responses changed.

## Typical Validation Set

Minimum:
- `cargo test -p guardian-server`
- `cargo test -p guardian-client`
- `cd packages/guardian-client && npm test`

Expand when the contract crosses layers:
- `cargo test -p miden-multisig-client`
- `cd packages/miden-multisig-client && npm test`
- `cargo test -p guardian-demo`
- `cd examples/smoke-web && npm run typecheck && npm run build`
- manual smoke with `smoke-test-rust-multisig-sdk` or `smoke-test-ts-multisig-sdk`
