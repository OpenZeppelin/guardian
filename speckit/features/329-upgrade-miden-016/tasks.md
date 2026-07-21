# Tasks: Upgrade Guardian to the Miden v0.16 Package Line

**Input**: Design documents from `speckit/features/329-upgrade-miden-016/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/compatibility-contract.md, quickstart.md

**Tests**: No new test-writing tasks — this migration's validation is the *existing* guard/e2e suites passing against regenerated 0.16 values (spec SC-001). Tasks reference the specific gates.

**Organization**: Grouped by user story from spec.md. US1 (Rust lifecycle) is the MVP. US2 (TypeScript parity) is a **trigger-gated deferred phase** per FR-008 — its tasks are written now, executable when `@miden-sdk/miden-sdk` publishes any 0.16 on npm.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: US1–US4 from spec.md

---

## Phase 1: Setup (pins, toolchain, environment)

**Purpose**: Move the version baseline. The workspace will NOT compile after this phase — that is expected; Phases 2–3 are the migration surface.

- [X] T001 Re-check upstream alphas before pinning (commands in quickstart.md §0): confirm latest `miden-node-proto`, `miden-client`, `miden-protocol` alphas on crates.io; record the final pin set and the node-proto decision (aligned alpha vs `miden-node-proto-build` bindings) as an update to research.md R1/R2
- [X] T002 [P] Bump Rust toolchain to 1.96.1 in rust-toolchain.toml and `rust-version` in Cargo.toml (line 29)
- [X] T003 Update workspace pins in Cargo.toml lines 68–74 to exact 0.16 alphas per data-model.md Entity 1, update `miden-confidential-contracts = "=0.15.1"` in crates/miden-multisig-client/Cargo.toml line 16 to the next Guardian contracts version, then run `cargo update` and verify lockfile invariants: `cargo tree -i miden-protocol` resolves one version; `p3-*` is a consistent 0.6 set; no residual 0.15 miden crates or winter-* crates (contracts/compatibility-contract.md Contract 2)
- [X] T004 [P] Reset local dev state: remove stale `~/.guardian` metadata and miden-client SQLite stores from 0.15 (they fail confusingly under 0.16 — research.md R6)

**Checkpoint**: Pins and toolchain final; compile errors now enumerate the migration surface.

---

## Phase 2: Foundational (bottom crates — block everything)

**Purpose**: Migrate the crates every story depends on, bottom-up (constitution Principle I). No user story work can complete until these compile and their unit tests pass.

- [X] T005 Re-port all 8 MASM files in crates/contracts/masm/ (auth/{multisig,multisig_ecdsa,guardian,guardian_ecdsa}.masm, account_components/auth/{multisig,multisig_ecdsa,multisig_guardian,multisig_guardian_ecdsa}.masm) to the 0.16 authoring model: `@account_procedure` / `@transaction_script` attributes, note creation via `basic_wallet::create_note`, relocated asset procedures (`protocol::asset::*` → `miden::standards::assets::*`), absorb upstream multisig MASM changes (signature.masm, update_signers_and_threshold, get_signer_at — research.md R4)
- [X] T006 Migrate crates/contracts/src (masm_builder.rs, account component builders) to assembler 0.25 / miden-project.toml model and the new `Approver`/`ApproverSet` + wallet-factory-split APIs (research.md R5); keep `OZ_MASM_DIR` build.rs wiring working
- [X] T007 Migrate crates/contracts/tests (tests/auth/multisig.rs MockChain behavior suite and siblings) to 0.16 APIs; suite green
- [X] T008 [P] Migrate crates/shared (src/lib.rs, src/hex.rs, src/auth.rs — commitment/hex/auth helpers) to miden-protocol 0.16 types
- [X] T009 [P] Migrate crates/miden-keystore to 0.16 (Falcon key types, `PublicKeyCommitment`)
- [X] T010 Migrate crates/miden-rpc-client/src/lib.rs per the T001 decision: aligned `miden-node-proto` alpha or switch to `miden-node-proto-build`-generated bindings (research.md R2); absorb `BlockHeader.validator_keys` (repeated) and `TransactionHeader.fee` removal
- [X] T011 [P] Migrate crates/client auth flows (src/auth/mod.rs, src/auth/miden_ecdsa.rs) to 0.16 types; update this layer's auth tests (constitution Principle IV)

**Checkpoint**: contracts, shared, miden-rpc-client, miden-keystore, client compile; their unit tests pass.

---

## Phase 3: User Story 1 — Guardian server and Rust SDK on a Miden v0.16 network (P1) 🎯 MVP

**Goal**: Full custody lifecycle (create → register → sync → propose/sign/execute → offline export/import → verify → canonicalize) works end-to-end on a v0.16 node via the Rust SDK and Guardian server.

**Independent Test**: `cargo test --workspace` + e2e suite green; interactive demo completes every lifecycle flow against a local 0.16 node (spec US1 acceptance scenarios).

### Multisig SDK migration

- [X] T012 [US1] Migrate crates/miden-multisig-client transaction layer (src/transaction/builder.rs, src/transaction/payment.rs, src/transaction/mod.rs) to 0.16: delta→patch model, sync proving call sites (#3281), `AssetAmount` typing, P2ID/P2IDE ≥1-asset typestate builders (research.md R3/R9)
- [X] T013 [US1] Migrate crates/miden-multisig-client client layer (src/client/helpers.rs, src/execution.rs, src/client/proposals.rs, src/export.rs, src/builder.rs) to 0.16: `TransactionResult::account_patch`, `AccountPatch::merge`/`apply_patch`, wallet factory split, `Approver`/`ApproverSet` (research.md R3/R5)

### Atomic artifact regeneration (contracts/compatibility-contract.md Contract 3)

- [X] T014 [US1] Regenerate procedure roots via `cargo run --example procedure_roots -p miden-multisig-client -- --json` and update crates/miden-multisig-client/src/procedures.rs (lines 26–45); `procedure_roots_match_compiled_account` green
- [X] T015 [P] [US1] Keep the TS mirror in lockstep now (do not defer to US2 — stale mirrors were the 0.15 failure mode): copy the same roots into packages/miden-multisig-client/src/procedures.ts (lines 14–20) and rsync crates/contracts/masm/ → packages/miden-multisig-client/masm/ (TS package build/tests NOT required yet)
- [X] T016 [US1] Update `EXPECTED_KERNEL_COMMITMENT` in crates/miden-multisig-client/src/transaction/mod.rs (line ~110) from the 0.16 toolchain; `transaction_kernel_commitment_matches_network` green (guards the p3 0.6 coupling — research.md R8)
- [X] T017 [US1] Regenerate toolchain-sensitive fixtures: fixtures/miden-multisig-client/p2id-serial-vectors.json and crates/server/src/testing/fixtures/*.json

### Server migration

- [X] T018 [US1] Migrate crates/server delta pipeline (src/delta_summary/projection.rs, src/delta_summary/build.rs) to `AccountPatch`: absolute-patch summarization, per-slot Create/Update/Remove ops, unified vault asset delta, dropped fee field — preserving Guardian's append-only records, `prev_commitment`/nonce lineage, and lifecycle transitions unchanged (research.md R3, constitution Principle III)
- [X] T019 [US1] Migrate crates/server network bridge and canonicalization verify path (src/network/miden/mod.rs, src/canonicalization/processor.rs) to patch-based commitment recompute/verification
- [X] T020 [US1] Migrate crates/server auth metadata handlers (src/metadata/auth/miden_falcon_rpo.rs, src/metadata/auth/miden_ecdsa.rs) to 0.16: `AuthRequest` signature-or-summary reshape, `pub_key_commitment: PublicKeyCommitment`; update tests in this layer AND one upstream consumer (SDK or demo) per constitution Principle IV
- [X] T021 [US1] Migrate crates/server test infrastructure and e2e suites (src/testing/helpers.rs, src/testing/e2e/*.rs incl. switch_guardian_canonicalization.rs) to 0.16 APIs
- [X] T022 [P] [US1] Migrate crates/server/bench/loadgen/src/scenarios.rs and benchmarks/prod-server to the migrated SDK APIs
- [X] T023 [P] [US1] Migrate examples/demo (src/state.rs + flows) and examples/rust (src/main.rs incl. live kernel-commitment check at line ~183, src/multisig.rs) to 0.16; delete removed debug-mode usage (`DebugMode`/`in_debug_mode`/`MIDEN_DEBUG` — research.md R9)

### Validation gates

- [X] T024 [US1] Run full Rust gates per quickstart.md §4: `cargo fmt --all --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --workspace`, e2e feature tests, and `cargo run --features evm --bin gen-openapi -- --check docs` (Contract 1: zero wire-shape drift)
- [ ] T025 [US1] Manual lifecycle smoke with examples/demo against a LOCAL 0.16 node (skill: smoke-test-rust-multisig-sdk): create/register account, cosigner sync, propose/sign/execute, offline export/import, verify state commitment, switch-guardian
- [X] T026 [US1] Devnet validation: repeat the happy path against public devnet; verify the version-mismatch error is recognizable and actionable when pointed at a wrong-version endpoint (spec FR-009, Contract 4); watch for remote-prover version skew (issue #3113 class)

**Checkpoint**: MVP complete — Guardian works on Miden 0.16 (Rust). Independently demonstrable.

---

## Phase 4: User Story 3 — Examples, smoke harnesses, and documentation reflect v0.16 (P3)

*(Sequenced before US2 because US2 is externally blocked; US3 depends only on US1.)*

**Goal**: A new integrator following docs/examples gets a working 0.16 setup first try; the temporary Rust/TS divergence is visible.

**Independent Test**: Quickstart walkthrough on a clean checkout completes against a 0.16 endpoint; docs audit finds no stale 0.15 version statements (spec US3 acceptance scenarios).

- [X] T027 [P] [US3] Update docs/QUICKSTART.md, docs/LOCAL_DEV.md, docs/CONFIGURATION.md for the 0.16 baseline (node version, endpoints, toolchain 1.96.1, any env-surface changes)
- [X] T028 [P] [US3] Add docs/TROUBLESHOOTING.md entries: client↔node version-mismatch symptom (genesis-commitment rejection) and remedy; mandatory 0.15→0.16 local-store/state reset; stale-`~/.guardian` symptom (Contract 4, research.md R6)
- [X] T029 [P] [US3] Update docs/MULTISIG_SDK.md and README.md: 0.16 baseline, breaking-change note (0.15 accounts/networks not interoperable), and the explicit documented divergence — TS package remains on 0.15 until upstream ships 0.16; no package releases until parity (Contract 6)
- [X] T030 [P] [US3] Update docs/guides/ committed artifacts (compose files, .env examples) and skill reference docs that pin Miden versions or endpoints
- [X] T031 [US3] Repo-wide version-reference audit: grep docs/, examples/, spec/ for 0.15-specific version statements and endpoints; fix stragglers (spec US3 scenario 3)

**Checkpoint**: Onboarding surface consistent with 0.16; divergence visible.

---

## Phase 5: User Story 2 — TypeScript SDK parity (P2) ⏸ TRIGGER-GATED

**Goal**: TS SDK on the same 0.16 line with byte-for-byte cross-SDK determinism and mixed-cosigner interop.

**Trigger**: any `@miden-sdk/miden-sdk` 0.16.x on npm (watch `0xMiden/web-sdk` PR #225). Until then only T032 runs.

**Independent Test**: Cross-SDK determinism gate green (TS account == Rust account byte-for-byte); mixed Rust/browser cosigner proposal lifecycle completes (spec US2 acceptance scenarios).

- [X] T032 [US2] Record the watch procedure in the feature workspace: `npm view @miden-sdk/miden-sdk versions --json | tail -5` + web-sdk PR #225 status; check before any release and at least weekly while the divergence stands (Contract 6)
- [X] T033 [US2] (on trigger) Bump `@miden-sdk/miden-sdk` to the exact published 0.16 version in packages/miden-multisig-client/package.json (line 31 — drop the caret) and regenerate its package-lock.json
- [X] T034 [US2] (on trigger) Migrate packages/miden-multisig-client/src to the 0.16 web SDK per web-sdk PR #225 breaks: `accountDelta()` → `accountPatch()` (absolute patches), exact component naming in linking, non-empty P2ID/P2IDE assets, `debugMode` no-op removal (hotspots: client.ts, raw-client.ts, multisig.ts, inspector.ts, account/builder.rs → builder.ts, account/storage.ts, transaction/*.ts, signers/*.ts)
- [X] T035 [US2] (on trigger) Rebuild and verify the TS package: `npm run build` (runs generate:masm against the T015-synced sources), `npm test` (vitest incl. p2id-serial-vectors.test.ts against the T017 fixture), `tsc --noEmit`
- [X] T036 [P] [US2] (on trigger) Bump `@miden-sdk/*` pins in examples/web/package.json, examples/smoke-web/package.json, examples/_shared/multisig-browser/package.json (0.15.1) and examples/operator-smoke-web/package.json (0.15.0) to the matched 0.16 set; regenerate the four package-lock.json files (do NOT delete lockfiles — memory: caret drift breaks builds); verify wallet-adapter/Para compatibility rather than assuming lockstep
- [ ] T037 [US2] (on trigger) Run the cross-SDK determinism gate (Playwright: TS-constructed account byte-for-byte equals Rust) and browser smoke harness (skill: smoke-test-ts-multisig-sdk): create, sync, propose/sign/execute, offline export/import
- [ ] T038 [US2] (on trigger) Mixed-cosigner interop test: proposal created in browser signed/executed by Rust CLI cosigner and vice versa (spec US2 scenario 2; watch the nonce-convention and re-exec parity gotchas from the 0.15 cycle)

**Status note (2026-07-21)**: trigger fired (`@miden-sdk/miden-sdk 0.16.0-alpha.1`).
T033–T036 done: package + examples bumped (Para held at 0.15.1 via npm overrides),
327/327 vitest green including the cross-SDK procedure-roots gate (TS constants vs
live Rust generator), all example vite builds green. T037/T038 headless portions
covered by that gate; the in-browser smoke (account create/sync/propose/sign/execute,
offline export/import) and mixed Rust↔browser cosigner run still need a live
browser session (skill: smoke-test-ts-multisig-sdk) alongside T025's demo TUI pass.

**Checkpoint**: Parity restored; divergence note removable; release gate (Contract 6) unblocked.

---

## Phase 6: User Story 4 — Coordinated package releases (P4) ⏸ GATED on US2

**Goal**: Downstream consumers get a coherent breaking-change release.

**Independent Test**: Release pipeline dry-run passes; fresh consumer project installs both SDKs and runs against 0.16 (spec US4 acceptance scenarios).

- [X] T039 [US4] Draft release notes declaring the Miden 0.16 line as a breaking change: no 0.15↔0.16 interop, mandatory store/account reset, toolchain floor, node-version requirement (spec FR-010)
- [ ] T040 [US4] (after US2) Prepare the coordinated release manifest: consistent versions across crates.io and npm packages, zero mixed Miden pins (spec SC-003 audit via dependency listing), dry-run publish in dependency order (skill: release-guardian-sdk-packages); hold actual publish until upstream 0.16 stabilizes per plan constraints

**Checkpoint**: Release ready, gated on parity + upstream stability.

---

## Phase 7: Polish & Cross-Cutting

- [ ] T041 [P] Re-run quickstart.md end-to-end on a clean checkout as final validation (spec SC-004)
- [X] T042 [P] Verify spec SC-005 posture: confirm all pre-release pins are exact throughout the repo (no caret/tilde on any miden dep) so the eventual alpha→stable move is pins + validation-matrix only; document any upstream API deltas encountered mid-migration in research.md
- [X] T043 Update speckit artifacts with as-built reality: research.md R2 outcome (which node-proto path was taken), data-model.md pin matrix final values, and mark the divergence status in contracts/compatibility-contract.md Contract 6

---

## Dependencies & Execution Order

### Phase dependencies

- **Phase 1 (Setup)** → nothing; start immediately. T001 gates T003 (pin values); T002/T004 parallel.
- **Phase 2 (Foundational)** ← Phase 1. Internal order: T005 → T006 → T007 (MASM before builders before tests); T008/T009/T011 parallel with the T005–T007 chain; T010 after T001's decision.
- **Phase 3 (US1)** ← Phase 2. T012–T013 (SDK) → T014–T017 (regen; T015 parallel with T016/T017) → T018–T021 (server; T018→T019 sequential, T020 parallel with T019, fixtures from T017 needed by T021) → T022/T023 parallel → T024 → T025 → T026.
- **Phase 4 (US3)** ← Phase 3 (docs describe migrated behavior). T027–T030 all parallel; T031 last.
- **Phase 5 (US2)** ← Phase 3 + **external trigger** (npm 0.16). T032 immediate; T033 → T034 → T035 → T037 → T038; T036 parallel with T034–T035.
- **Phase 6 (US4)** ← T039 can draft after Phase 3; T040 strictly after Phase 5.
- **Phase 7** ← Phases 3–4 (T041/T042); T043 whenever facts land.

### Story independence

- **US1**: independent MVP once Foundational is done.
- **US3**: depends only on US1; deliverable while US2 waits.
- **US2**: externally blocked; T015/T017 in US1 pre-stage its artifacts so the trigger-fire slice is mechanical.
- **US4**: gated on US2 by design (Contract 6 — no releases until parity).

### Parallel opportunities

```text
Phase 1: T002 ∥ T004 (T001 → T003)
Phase 2: [T005→T006→T007] ∥ T008 ∥ T009 ∥ T011  (T010 after T001)
Phase 3: T015 ∥ T016 ∥ T017 after T014; T020 ∥ T019; T022 ∥ T023
Phase 4: T027 ∥ T028 ∥ T029 ∥ T030
Phase 5 (on trigger): T036 ∥ [T034→T035]
```

---

## Implementation Strategy

**MVP first**: Phases 1–3 only (T001–T026) restore Guardian on devnet — that is the forcing function. Stop, validate US1 independently (demo smoke + e2e), then ship the branch for review.

**Incremental delivery**: Phase 4 (docs) rides in the same PR or an immediate follow-up. Phase 5 fires whenever upstream publishes — its prep (T015/T017/T032) is already done, so the slice is small. Phase 6 waits for parity by design.

**Reality check on effort**: the 0.14→0.15 equivalent (PR #287) was 131 files; this adds the delta→patch pipeline rework and MASM attribute re-port, so Phases 2–3 dominate. T012/T013/T018 are the largest individual tasks — expect them to surface unlisted upstream API churn; log discoveries in research.md as you go (T043).
