---
name: guardian-contract-change
description: Propagate Guardian contract changes safely across server, Rust client, TypeScript client, multisig SDKs, examples, docs, and tests. Use when Codex changes `guardian.proto`, HTTP JSON payloads, response shapes, status enums, auth requirements, or any user-visible server contract.
---

# Guardian Contract Change

## Overview

Use this skill when the server contract is the source of truth for the task. Update the server first, then move outward through every consumer that depends on the changed shape or semantics.

## Read First

Read these sources before making edits:

- `AGENTS.md`
- `crates/server/proto/guardian.proto`
- `crates/server/src/api/http.rs`
- `crates/server/src/api/grpc.rs`
- `packages/guardian-client/src/server-types.ts`
- `packages/guardian-client/src/conversion.ts`
- `crates/client/src/client.rs`
- [`references/contract-surfaces.md`](references/contract-surfaces.md)

If the change touches proposal payloads or account state shapes, also inspect both multisig SDKs and the example harnesses before editing.

## Workflow

1. Confirm the contract change.
   Decide whether the change is:
   - gRPC-only
   - HTTP-only
   - shared semantic behavior exposed through both transports
2. Update the server contract source first.
   - gRPC: edit `crates/server/proto/guardian.proto`
   - HTTP JSON: edit `crates/server/src/api/http.rs` and the backing service modules
   - shared behavior: edit service and domain modules before the transport adapters
3. Update Rust client compatibility.
   Inspect and update:
   - generated proto usage in `crates/client`
   - request builders
   - response mapping
   - auth or signature handling if required by the new contract
4. Update TypeScript client compatibility.
   Inspect and update:
   - `packages/guardian-client/src/server-types.ts`
   - `packages/guardian-client/src/conversion.ts`
   - `packages/guardian-client/src/http.ts`
   - tests for malformed or missing fields
5. Propagate into multisig SDKs if the changed fields cross that boundary.
   Proposal metadata, state objects, ack fields, and status transitions are the common triggers.
6. Update examples and docs when the change is visible to users or integrators.
   Start with `examples/demo`, `examples/smoke-web`, and `examples/web`.
7. Run the smallest meaningful validation set, then expand if high-risk.

## Guardrails

- Do not change client adapters first and infer the new contract from them later.
- Do not add permissive parsing or silent fallback behavior unless the task explicitly requires it.
- Keep Rust and TypeScript client semantics aligned.
- Treat auth changes, proposal status changes, and JSON field optionality as high-risk.
- If the server contract changed, both `crates/client` and `packages/guardian-client` must be considered in the same task.

## Output Shape

Report:

- contract source changed
- dependent files updated
- transport surfaces affected
- client parity work completed
- upstream SDK or example fallout
- tests and smoke checks run
- any intentionally skipped propagation with reason
