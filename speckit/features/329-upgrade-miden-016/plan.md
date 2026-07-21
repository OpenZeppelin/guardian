# Implementation Plan: Upgrade Guardian to the Miden v0.16 Package Line

**Branch**: `329-upgrade-miden-016` | **Date**: 2026-07-20 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `speckit/features/329-upgrade-miden-016/spec.md`

## Summary

Migrate the Guardian Rust workspace from Miden 0.15.x to the 0.16 pre-release
line (exact-pinned alphas), because devnet already runs the 0.16 node and
Guardian is incompatible with it. Research shows this is redesign-sized: 0.16
removes the relative `AccountDelta` model in favor of absolute `AccountPatch`
(different commitment layout), reshapes the auth surface
(`Approver`/`ApproverSet`, `PublicKeyCommitment`, sync proving), and changes
the MASM authoring model (`@account_procedure`/`@transaction_script`
attributes, relocated procedures) — shifting every embedded procedure root and
the kernel-commitment constant. Sequencing is Rust-first (decided 2026-07-20):
the TypeScript package and browser examples follow when the upstream npm SDK
publishes any 0.16 release (web-sdk PR #225 is in flight); no Guardian package
releases until both SDKs target the same Miden line. Guardian's own concepts —
append-only delta records, explicit lifecycle, wire contract shape — are
preserved; what changes is the embedded Miden payload encoding and the
commitment math around it. Full details: [research.md](./research.md).

## Technical Context

**Language/Version**: Rust — toolchain bump required 1.93.0 → 1.96.1 (upstream MSRV), edition 2024 (already); TypeScript 5.x (deferred slice)
**Primary Dependencies**: miden-protocol/-standards/-tx `=0.16.0-alpha.4`, miden-testing `=0.16.0-alpha.2`, miden-client/-client-sqlite-store `=0.16.0-alpha.1`, miden-node-proto **blocked** (exact-pins protocol alpha.2 — conflict; resolution per research R2: wait for aligned alpha or switch to `miden-node-proto-build` bindings); transitive: miden-crypto 0.28, miden-vm 0.25, Plonky3 `p3-*` 0.6
**Storage**: unchanged (filesystem / Postgres behind compile-time feature); miden-client local stores (SQLite) recreated — upstream store-format break, no migration
**Testing**: cargo test (workspace + contracts MockChain suite + guard tests), e2e feature tests, examples/demo interactive smoke against a local 0.16 node; TS vitest/Playwright gates deferred with the TS slice
**Target Platform**: Linux/macOS server + CLI; browser WASM deferred to TS slice
**Project Type**: multi-crate Rust workspace + npm packages (monorepo)
**Performance Goals**: parity with 0.15 baseline — no regression in canonicalization throughput (Phase 0/1 scalability numbers remain the reference); sync-proving change (#3281) must not regress server-side concurrency
**Constraints**: exact pins for all pre-release deps (no caret drift); single consistent 0.16 protocol version in the lockfile; Guardian wire-contract shape unchanged (any drift triggers the AGENTS.md §4 contract-change workflow); no backwards-compatibility shims for 0.15 state
**Scale/Scope**: ~85 Rust files importing miden APIs across 9 crates + examples; 8 MASM files; 7 procedure roots ×2 languages; effort model PR #287 (131 files) — expect larger on the Rust side

## Constitution Check

*GATE: evaluated against Guardian Constitution v1.1.0 — pre-research and re-checked post-design.*

| Principle | Status | Evidence |
|---|---|---|
| I. Bottom-Up Change Propagation | PASS | Delivery order is contracts/shared → server → base clients → multisig SDK → examples/docs (see Project Structure); TS consumers explicitly deferred by recorded decision, not silently skipped — tracked as follow-up slice with a watchlist trigger (web-sdk 0.16 npm release). |
| II. Transport and Cross-Language Parity | PASS (documented divergence) | HTTP/gRPC semantics unchanged. Temporary Rust/TS protocol-version divergence is intentional, user-decided (spec FR-008), documented in spec + this plan before implementation, and bounded: no package releases until parity restored; cross-SDK determinism gate re-runs when TS lands. |
| III. Append-Only Integrity and Explicit Lifecycles | PASS | Delta→patch migration preserves Guardian's append-only records, `prev_commitment`/nonce lineage, and pending→candidate→canonical/discarded transitions (research R3). No new fallback paths; online/offline flows unchanged in shape. |
| IV. Explicit Authentication and Stable Boundary Errors | PASS | Auth surface changes (Approver/ApproverSet, PublicKeyCommitment, AuthRequest reshape) treated as high-risk: tests updated in each changed layer plus upstream consumers (SDK → demo); boundary error semantics preserved; version-mismatch errors surfaced actionably (spec FR-009). |
| V. Evidence-Driven Delivery | PASS | Spec has 4 independently testable stories; validation = existing guard tests (procedure roots, kernel commitment, contracts suite), workspace tests, e2e, demo smoke vs local 0.16 node + devnet; docs/examples updates enumerated. |

**System invariants**: delta lineage, canonicalization lifecycle, proposal
determinism, and per-account auth invariants are exactly the surfaces the
patch/auth migration touches — each has an existing test or e2e gate named in
the validation plan, and the design keeps Guardian-level semantics unchanged.
No constitution violations; Complexity Tracking not required.

## Project Structure

### Documentation (this feature)

```text
speckit/features/329-upgrade-miden-016/
├── plan.md              # This file
├── research.md          # Phase 0 — upstream 0.16 findings + decisions R1–R9
├── data-model.md        # Phase 1 — version matrix, upgrade surfaces, artifact registry
├── quickstart.md        # Phase 1 — migration execution & validation walkthrough
├── contracts/
│   └── compatibility-contract.md  # Phase 1 — wire/artifact/version compatibility guarantees
└── tasks.md             # Phase 2 (/speckit.tasks — not created by /speckit.plan)
```

### Source Code (repository root)

```text
Cargo.toml                      # workspace pins L68-74 → 0.16 alphas (exact); rust-toolchain.toml → 1.96.1
Cargo.lock                      # regenerated; verify single protocol version + p3-* 0.6 set
crates/
├── contracts/                  # 8 MASM files re-ported to 0.16 attribute model; masm_builder.rs;
│   │                           # tests/auth/multisig.rs (heaviest contract-behavior suite)
├── shared/                     # commitment/hex/auth helpers on miden-protocol
├── miden-rpc-client/           # node-proto knot (research R2) — typed re-exports at src/lib.rs:8
├── miden-keystore/             # Falcon key handling
├── client/                     # base client auth (miden_ecdsa, falcon)
├── miden-multisig-client/      # heaviest consumer: transaction/builder.rs, client/helpers.rs,
│   │                           # execution.rs, export.rs; procedures.rs roots; kernel-commitment
│   │                           # constant (src/transaction/mod.rs:110); =0.15.1 contracts pin → =0.16.x
├── server/                     # delta_summary/{projection,build}.rs (delta→patch), network/miden/,
│   │                           # metadata/auth/{miden_falcon_rpo,miden_ecdsa}.rs, canonicalization,
│   │                           # testing fixtures + e2e suites
│   └── bench/loadgen/          # scenarios.rs
examples/
├── demo/                       # Rust interactive CLI — primary manual smoke
└── rust/                       # kernel-commitment live check (src/main.rs:183)
docs/                           # CONFIGURATION, TROUBLESHOOTING, QUICKSTART, LOCAL_DEV, MULTISIG_SDK,
                                # guides — 0.16 baseline + reset guidance + divergence note

# DEFERRED to TS follow-up slice (trigger: @miden-sdk 0.16 on npm):
packages/miden-multisig-client/ # @miden-sdk pin, ~50 importing files, procedures.ts, vendored masm/,
                                # p2id-serial-vectors fixture
examples/{web,smoke-web,_shared/multisig-browser,operator-smoke-web}/  # @miden-sdk/* pins + lockfiles
```

**Structure Decision**: existing monorepo layout is unchanged — this feature
edits in place. Delivery follows constitution Principle I bottom-up:
(1) pins/toolchain + contracts/shared, (2) miden-rpc-client + keystore +
client, (3) miden-multisig-client + artifact regeneration, (4) server
(patch pipeline, auth metadata, canonicalization, e2e), (5) examples + docs.
The TS package and browser examples are a deferred slice with an explicit
re-entry trigger, per FR-008.

## Complexity Tracking

No constitution violations to justify.
