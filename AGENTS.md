# AGENTS.md - Private State Manager

This file is the operational guide for coding agents working in this repository.
It is optimized for safe, cross-layer changes in a multi-language codebase.

## 1) System Shape (Read First)

Work from the bottom up:

1. `crates/server` (core system of record)
2. `crates/client` and `packages/psm-client` (base Rust/TS clients over PSM)
3. `crates/miden-multisig-client` and `packages/miden-multisig-client` (higher-level multisig SDKs)
4. `examples/` (verification and debugging surfaces)
   - `examples/demo` = CLI/TUI multisig flow
   - `examples/web` = browser multisig flow
   - `examples/rust` = low-level Rust integration examples

If a behavior changes in a lower layer, verify and propagate impact upward.

## 2) Repo Map

- `crates/server`: PSM server (HTTP + gRPC), storage, metadata, auth, canonicalization jobs
- `crates/client`: Rust gRPC client SDK
- `packages/psm-client`: TS HTTP client SDK
- `crates/miden-multisig-client`: Rust multisig SDK on top of Miden + PSM
- `packages/miden-multisig-client`: TS multisig SDK on top of Miden + PSM
- `crates/shared`: shared Rust primitives/utilities
- `spec/`: system and protocol-level behavior docs
- `examples/`: validation apps and reference flows

## 3) Core Change Rules

1. Preserve protocol compatibility unless explicitly asked to break it.
2. Treat server contract changes as multi-package changes by default.
3. Update tests in the layer where behavior changes, plus at least one upstream consumer.
4. Prefer minimal, surgical edits over broad refactors.
5. Do not introduce silent behavior drift between Rust and TypeScript clients.

## 4) Contract-Change Workflow (Mandatory)

Use this when changing endpoints, payloads, status enums, signatures, or auth behavior.

1. Update server contract source first:
   - gRPC: `crates/server/proto/state_manager.proto`
   - HTTP shapes/serialization in server services and API modules
2. Update Rust client compatibility:
   - `crates/client` (proto, request/response mapping, auth/signature handling)
3. Update TS client compatibility:
   - `packages/psm-client/src/server-types.ts`
   - request/response adapters and tests
4. Update multisig SDK layers if proposal/state shape changed:
   - `crates/miden-multisig-client`
   - `packages/miden-multisig-client`
5. Validate via examples:
   - `examples/demo` for CLI flow
   - `examples/web` for browser flow
6. Run targeted tests, then broader suite.

## 5) Layer-Specific Guidance

### Server (`crates/server`)

- Keep business logic in `src/services/`; keep transport logic thin in `src/api/`.
- Respect auth expectations: unauthenticated-only endpoints must remain explicit.
- Maintain storage/metadata backend parity (filesystem/postgres where applicable).
- Preserve canonicalization semantics (pending/candidate/canonical/discarded lifecycle).
- Default local development/test backend is `filesystem` unless a task explicitly requires Postgres.

### Rust PSM Client (`crates/client`)

- Mirror proto/service changes quickly.
- Keep signer/auth flow explicit and deterministic.
- Verify ack-signature related behavior whenever push/sign flows are changed.

### TS PSM Client (`packages/psm-client`)

- Keep `server-types.ts` aligned with real server JSON responses.
- Keep conversion code explicit rather than permissive.
- Validate error-shape handling (`PsmHttpError`) when endpoint responses change.

### Rust Multisig SDK (`crates/miden-multisig-client`)

- Treat proposal lifecycle and threshold logic as high-risk behavior.
- Validate online + offline flows when changing proposal or signature handling.
- Keep execution path and imported/exported proposal path behaviorally consistent.

### TS Multisig SDK (`packages/miden-multisig-client`)

- Keep transaction/procedure builders stable and typed.
- Validate external-signing flow when touching signature or signer types.
- Ensure proposal cache/list/sync semantics remain coherent after mutations.

### Examples (`examples/`)

- Use examples as integration checks, not just demos.
- If user-facing flow changes, update example docs and startup assumptions.

## 6) Fast Validation Matrix

Run the smallest meaningful set first, then expand:

### Rust

```bash
cargo test -p private-state-manager-server
cargo test -p private-state-manager-client
cargo test -p miden-multisig-client
cargo test --workspace
```

Feature-gated server suites when relevant:

```bash
cargo test -p private-state-manager-server --features integration
cargo test -p private-state-manager-server --features e2e
```

### TypeScript

```bash
cd packages/psm-client && npm test
cd packages/miden-multisig-client && npm test
```

### Examples (smoke/integration)

```bash
cargo run -p psm-demo
cd examples/web && npm run dev
```

Manual policy:
- Treat `examples/demo` and `examples/web` as required manual smoke checks for changes affecting server/client/multisig behavior.
- Document in PR notes what was exercised and any skipped path with reason.

## 7) High-Risk Areas (Require Extra Care)

- Auth and signature scheme handling (Falcon vs ECDSA paths)
- Delta/proposal status transitions and canonicalization timing
- Request/response schema drift between server and TS client
- Multisig threshold/signature counting logic
- Offline import/export proposal format compatibility

When touching any high-risk area, add or update tests before finishing.

## 8) Definition of Done for Agent Changes

Before finishing, confirm all are true:

1. Architecture impact assessed bottom-up (server -> clients -> multisig -> examples).
2. Protocol/data-shape changes reflected in both Rust and TS stacks.
   - If server contract changed, updates in `crates/client` and `packages/psm-client` must be included in the same PR.
3. Tests updated where behavior changed.
4. At least one upstream consumer validated for changed lower-layer behavior.
5. README/docs touched if external behavior changed.
6. No unrelated file churn.

## 9) Practical Defaults

- Prefer `rg`/`rg --files` for discovery.
- Keep edits ASCII unless existing file requires otherwise.
- Keep comments minimal and only where logic is non-obvious.
- Avoid speculative refactors during bugfixes.

## 10) Versioning Policy

- Keep crate/package versions aligned with the active Miden dependency line.
- Current baseline is Miden `0.13.x`; changes must remain compatible with that line unless migration is explicit.
- If a change requires moving to a new Miden line (for example `0.14.x`), treat it as a coordinated release task:
  1. Update workspace/dependency constraints.
  2. Update both multisig SDKs and both base clients as needed.
  3. Re-run cross-layer validation (including examples).
  4. Update docs and changelog/release notes to call out the dependency line change.

## 11) Coding Style (Multisig Focus)

Apply these rules especially to:
- `crates/miden-multisig-client`
- `packages/miden-multisig-client`

### Function Design

- Prefer small, single-purpose functions over long multi-step procedures.
- Separate orchestration from transformation:
  - Orchestrators coordinate calls and side effects.
  - Helpers perform pure data transformation/validation.
- Avoid mixing transport calls, business rules, and serialization in one function.
- Target shape:
  - Helper functions: short and focused.
  - Long workflows should be split into named steps with explicit inputs/outputs.

### Comment Style

- Do not add inline comments (`// ...`) in implementation code.
- Do not add step-by-step procedural comments (for example `1.`, `2.`, `3.`) in method docs.
- Prefer clear naming and small functions over explanatory comments.
- If documentation is required, keep it concise, high-level, and non-procedural.

### Module Boundaries

- Organize by capability, not by file size:
  - Account lifecycle
  - Proposal lifecycle (create/sign/list/execute)
  - Signature/advice preparation
  - PSM transport adapters
  - Metadata mapping/normalization
- Group files by concept using folders when two or more files belong to the same concern.
  - TypeScript example: `multisig/proposal/parser.ts`, `multisig/proposal/execution.ts`
  - Rust equivalent: `client/proposal/parser.rs`, `client/proposal/execution.rs`
- Keep scheme-specific logic (Falcon/ECDSA) behind focused helpers/strategies.
- Minimize cross-module reach; prefer narrow interfaces.

### API and Type Discipline

- Keep public APIs explicit and stable unless change is intentional.
- Prefer typed structures/enums over ad-hoc maps or stringly-typed branching.
- Model proposal state transitions explicitly (pending/ready/finalized).
- Ensure Rust and TypeScript behavior remain semantically equivalent for the same workflow.

### Type-Centric Operations

- Prefer type-driven operations over standalone procedural functions.
- TypeScript:
  - Prefer classes/types owning parsing and execution behavior.
  - Prefer patterns like:
    - `new CosignerAdviceMap(...)` instead of `buildCosignerAdviceMap(...)`
    - `proposalWorkflow.execute()` instead of `executeProposalWorkflow(...)`
    - `ProposalMetadata.fromPsmMetadata(...)` instead of `fromPsmMetadata(...)`
- Rust:
  - Apply the same principle with constructors/associated functions on domain types.
  - Prefer patterns like:
    - `SignatureInputs::from_json(delta_payload_json)` instead of `parse_unique_signature_inputs(...)`

### Immutability (TypeScript)

- Prefer immutable variable bindings by default.
- Use `const` unless reassignment is required.
- Use `let` only when mutation/rebinding is intentionally needed.

### Additional Style Rules

- Hex/Bytes Boundary Rule:
  - Validate and normalize external commitments, pubkeys, signatures, and account IDs at boundaries before domain logic.
  - Enforce expected shape (`0x` prefix, canonical casing, expected length) at parse time.
- No Implicit Crypto Conversions:
  - Do not inline ad-hoc conversion between hex/base64/bytes/felts in feature code.
  - Use dedicated codec/helper types/modules for conversions.
- Exhaustive State Handling:
  - Use exhaustive handling for status/scheme/proposal-type enums.
  - TypeScript: use `never`-based exhaustiveness checks.
  - Rust: use full `match` coverage.
- Deterministic Time/Nonce Sources:
  - In core logic, avoid direct `Date.now()` and random calls.
  - Pass clock/nonce providers so logic remains testable and deterministic.
- No Silent Fallbacks:
  - Fallback behavior (for example online -> offline) must be explicit in API flow/return types.
  - Do not hide fallback behavior in side effects.
- Typed Errors With Stable Codes:
  - Use structured errors with stable codes at module boundaries.
  - Avoid branching on free-form error strings in core paths.
- TypeScript Strictness:
  - Do not use `any` in core modules.
  - Avoid non-null assertions (`!`) where a guard/narrowing can be used.
- Cross-Language Naming Parity:
  - Keep Rust and TypeScript names/semantics aligned for equivalent workflows unless divergence is documented.

### Error Handling

- Use structured errors at boundaries; avoid losing context in generic string errors.
- Add context at adapter edges (PSM, Miden node, serialization/parsing).
- Fail fast on malformed signatures/metadata instead of silently coercing.

### Testing Expectations for Refactors

- Refactors must preserve behavior:
  - Add characterization tests before moving complex logic.
  - Keep or improve existing integration coverage.
- For multisig changes:
  - Validate both Falcon and ECDSA paths.
  - Validate online and offline proposal flows.
  - Validate both examples (`examples/demo`, `examples/web`) when applicable.

### Propagation Rule

- If a public API changes in either multisig client, propagate updates to:
  - `examples/demo`
  - `examples/web`
  - any affected docs and tests in the same PR.
