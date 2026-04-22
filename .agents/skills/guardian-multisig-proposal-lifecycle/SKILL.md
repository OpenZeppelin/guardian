---
name: guardian-multisig-proposal-lifecycle
description: Implement, debug, and validate proposal lifecycle changes across the Rust and TypeScript multisig SDKs and their example harnesses. Use when Codex touches proposal creation, listing, signing, readiness, execution, import or export, offline signing, sync behavior, threshold counting, or `SwitchGuardian` flows.
---

# Guardian Multisig Proposal Lifecycle

## Overview

Use this skill for high-risk multisig workflow changes. Keep the Rust and TypeScript SDKs behaviorally aligned and verify the user-visible flow in at least one upstream example surface.

## Read First

Read the current workflow surface before editing:

- `AGENTS.md`
- `crates/miden-multisig-client/src/client/proposals.rs`
- `crates/miden-multisig-client/src/client/offline.rs`
- `crates/miden-multisig-client/src/proposal.rs`
- `crates/miden-multisig-client/src/execution.rs`
- `packages/miden-multisig-client/src/multisig/proposal/execution.ts`
- `packages/miden-multisig-client/src/multisig/proposal/parser.ts`
- `packages/miden-multisig-client/src/proposal/metadata.ts`
- `examples/demo/src/actions/proposal_management.rs`
- `examples/smoke-web/src/smokeHarness.ts`
- [`references/workflow-matrix.md`](references/workflow-matrix.md)

## Workflow

1. Identify the stage being changed.
   Classify the work as:
   - proposal creation
   - proposal parsing or metadata
   - signature collection
   - readiness or threshold logic
   - execution and ack integration
   - export, import, or offline signing
   - post-execution sync or state verification
2. Inspect both Rust and TypeScript implementations for the same stage.
   If the workflow exists in both stacks, do not change one in isolation without at least confirming whether the other must also move.
3. Preserve lifecycle invariants.
   Use [`references/workflow-matrix.md`](references/workflow-matrix.md) as the checklist for what must remain true.
4. Update the nearest example surface.
   - `examples/demo` for Rust flow verification
   - `examples/smoke-web` and `examples/web` for browser flow verification
5. Validate the minimal affected path first, then expand to adjacent risky paths.

## Guardrails

- Do not silently change threshold semantics.
- Do not hide fallback from online to offline flows. Fallback must stay explicit.
- Keep proposal identifiers and metadata stable across export and import.
- Treat `SwitchGuardian` as a distinct path. Do not generalize offline execution claims beyond what the current code supports.
- Preserve Rust and TypeScript naming and behavior parity unless the divergence is intentional and documented.
- Fail fast on malformed signature, commitment, or metadata data rather than coercing it.

## Validation

Default targeted checks:

```bash
cargo test -p miden-multisig-client
cd packages/miden-multisig-client && npm test
```

Then expand as needed:

- `cargo test -p guardian-client` or `cd packages/guardian-client && npm test` if the change crosses the GUARDIAN client boundary
- `cargo test -p guardian-demo`
- `cd examples/smoke-web && npm run typecheck && npm run build`
- `cd examples/web && npm run build`
- `smoke-test-rust-multisig-sdk`
- `smoke-test-ts-multisig-sdk`

## Output Shape

Report:

- proposal stages touched
- Rust files updated
- TypeScript files updated
- example surfaces updated or checked
- lifecycle invariants preserved
- targeted tests and smoke coverage
- gaps or skipped paths
