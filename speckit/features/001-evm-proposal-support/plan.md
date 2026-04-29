# Implementation Plan: Domain-separated EVM proposal support

**Feature Key**: `001-evm-proposal-support` | **Date**: 2026-04-29 | **Spec**: [spec.md](./spec.md)

## Summary

Implement EVM proposal coordination as a feature-gated HTTP domain under
`/evm/*`. Miden keeps its existing `/configure`, `/delta`, `/delta/proposal`,
`/state`, canonicalization, and gRPC behavior. EVM gets separate wallet-session,
account-registration, proposal, contract-reader, client, and smoke-test flows.

## Workstreams

### Server Contract

- Add `/evm/auth/challenge`, `/evm/auth/verify`, and `/evm/auth/logout`.
- Add `/evm/accounts`.
- Add `/evm/proposals`, `/evm/proposals/{id}`,
  `/evm/proposals/{id}/approve`, `/evm/proposals/{id}/executable`, and
  `/evm/proposals/{id}/cancel`.
- Remove EVM behavior from `/configure` and `/delta/proposal`.
- Keep EVM behavior behind the `evm` feature; default builds do not register
  `/evm/*`.

### Server Domain

- Keep Miden services unchanged except explicit `unsupported_for_network` for
  EVM account IDs on Miden state/delta routes.
- Store EVM account metadata through the existing metadata store.
- Keep EVM domain logic in `crates/server/src/evm/`.
- Resolve chain RPC and EntryPoint addresses from server env maps.
- Verify validator installation and signer/threshold snapshots through Alloy
  only when the `evm` feature is enabled.
- Store EVM proposals behind EVM service methods; public API returns EVM
  proposal records, not `DeltaObject`.

### TypeScript

- Keep `packages/guardian-client` unchanged.
- Keep EVM behavior isolated in `packages/guardian-evm-client`.
- Update `examples/evm-smoke-web` and the EVM smoke skill to use `/evm/*`.

## Validation

```bash
cargo test -p guardian-server
cargo test -p guardian-server --features evm
cargo check -p guardian-server --features postgres
cargo check -p guardian-server --features postgres,evm
cd packages/guardian-evm-client && npm test && npm run build
cd examples/evm-smoke-web && npm run typecheck && npm run build
```

## Deferred

- EVM gRPC support.
- On-chain submission and bundler integration.
- Execution tracking beyond lazy EntryPoint nonce cleanup.
- ERC-1271, weighted multisig, and generic ERC-7913 signer support.
