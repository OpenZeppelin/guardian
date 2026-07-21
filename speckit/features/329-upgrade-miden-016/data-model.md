# Data Model: Upgrade Guardian to the Miden v0.16 Package Line

**Feature**: `329-upgrade-miden-016` | **Date**: 2026-07-20

A dependency migration's "data model" is the inventory of versioned things
that must move together. Four entity groups: version pins, upgrade surfaces,
regenerated artifacts, and persisted state affected by the break.

## Entity 1: Version Pin Matrix

The single source of truth for the Rust side is the root `Cargo.toml`
(lines 68–74); one override lives in the multisig crate.

| Pin site | Current | Target | Rule |
|---|---|---|---|
| `Cargo.toml:68` miden-protocol | 0.15.3 | `=0.16.0-alpha.4` | exact pin (FR-005) |
| `Cargo.toml:69` miden-standards | 0.15.3 | `=0.16.0-alpha.4` | exact pin |
| `Cargo.toml:70` miden-tx | 0.15.3 | `=0.16.0-alpha.4` | exact pin |
| `Cargo.toml:71` miden-node-proto | 0.15.0 | REPLACED by `miden-node-proto-build =0.16.0-alpha.2` (build-dep codegen in crates/miden-rpc-client) | R2 fallback taken |
| `Cargo.toml:72` miden-testing | 0.15.3 | `=0.16.0-alpha.2` | exact pin (latest published; matches miden-client's own pin) |
| `Cargo.toml:73` miden-client | 0.15.0 | `=0.16.0-alpha.1` | exact pin |
| `Cargo.toml:74` miden-client-sqlite-store | 0.15.0 | `=0.16.0-alpha.1` | exact pin |
| `crates/miden-multisig-client/Cargo.toml:16` miden-confidential-contracts | `=0.15.1` | `=0.16.0` DONE — workspace + all internal pins + 4 npm packages bumped to 0.16.0 (2026-07-21) | aligned with Miden line |
| `rust-toolchain.toml` | 1.93.0 | 1.96.1 | upstream MSRV |
| **Lockfile invariants** | — | — | exactly one 0.16 `miden-protocol`; consistent `p3-*` 0.6 set; no residual winter-*/0.15 miden crates |
| DEFERRED `packages/miden-multisig-client/package.json:31` @miden-sdk/miden-sdk | ^0.15.0 | exact 0.16.0-alpha.x when published | TS slice trigger |
| DEFERRED example pins (`examples/web`, `smoke-web`, `_shared/multisig-browser` @ 0.15.1; `operator-smoke-web` @ 0.15.0) | 0.15.x | matched 0.16 set | TS slice |

**Validation rule**: at completion, a dependency-listing audit shows zero
mixed-version Miden pins outside the documented deferred set (spec SC-003).

## Entity 2: Upgrade Surfaces (Rust slice)

Ordered bottom-up (constitution Principle I). "Weight" = files importing
miden APIs / notable hotspots from the repo survey.

| Surface | Weight | Migration concerns |
|---|---|---|
| `crates/contracts` | 3 files + 8 MASM + heavy test (tests/auth/multisig.rs) | MASM attribute model, relocated procedures, multisig MASM upstream changes, assembler 0.25 |
| `crates/shared` | 8 files | commitment/hex helpers, auth types |
| `crates/miden-rpc-client` | 1 file (typed node-proto re-exports) | version knot (R2); BlockHeader validator_keys; TransactionHeader.fee removed |
| `crates/miden-keystore` | 2 files | Falcon key types, PublicKeyCommitment |
| `crates/client` | 11 files | auth flows (miden_ecdsa, falcon) |
| `crates/miden-multisig-client` | 24 files; hotspots transaction/builder.rs, client/helpers.rs, execution.rs | delta→patch extraction, Approver/ApproverSet, sync proving, wallet factory split, AssetAmount, P2ID ≥1 asset |
| `crates/server` | 38 files; hotspots delta_summary/projection.rs, network/miden/mod.rs, metadata/auth/* | patch pipeline (apply/verify/summarize), canonicalization verify path, AuthRequest reshape, fee field removal in projection |
| `crates/server/bench/loadgen`, `benchmarks/prod-server` | 4 files | follows SDK changes |
| `examples/demo`, `examples/rust` | 8 files | store reset, debug-mode removal, live kernel-commitment check |
| DEFERRED `packages/miden-multisig-client` + browser examples | ~50 importing files + 63 repo-wide | accountPatch(), component naming, non-empty P2ID, per web-sdk PR #225 |

## Entity 3: Regenerated Artifact Registry

All regenerate **atomically in one step** (research R4); each has a guard.

| Artifact | Location | Regeneration | Guard |
|---|---|---|---|
| Procedure roots (Rust) | `crates/miden-multisig-client/src/procedures.rs:26-45` (7 roots) | `cargo run --example procedure_roots -p miden-multisig-client -- --json` | `procedure_roots_match_compiled_account` |
| Procedure roots (TS mirror) | `packages/miden-multisig-client/src/procedures.ts:14-20` | same generator output | TS tests (deferred slice; update constants now to avoid a stale mirror) |
| Kernel commitment constant | `crates/miden-multisig-client/src/transaction/mod.rs:110` | from 0.16 toolchain | `transaction_kernel_commitment_matches_network` + live check `examples/rust/src/main.rs:183` |
| MASM sources (canonical) | `crates/contracts/masm/` (8 files) | hand-ported to 0.16 model | contracts MockChain behavior suite |
| MASM vendored copy | `packages/miden-multisig-client/masm/` | rsync from canonical | `generate-masm.mjs` prefers canonical in-repo; vendored copy ships in npm tarball |
| P2ID serial vectors | `fixtures/miden-multisig-client/p2id-serial-vectors.json` | regenerated on 0.16 | `p2id-serial-vectors.test.ts` (runs in TS slice) |
| Server testing fixtures | `crates/server/src/testing/fixtures/*.json` | regenerated | server test suite |
| Cross-SDK determinism vector | Rust↔TS byte-for-byte account gate | re-run when TS slice lands | Playwright/ts-sdk gate (deferred) |

## Entity 4: Persisted State Affected by the Break

| State | Owner | 0.16 fate | User-visible handling |
|---|---|---|---|
| Miden-client local stores (SQLite / IndexedDB) | SDK users, demo | upstream format break (#2309, #2313) — must be recreated | documented reset; demo guidance for stale `~/.guardian` metadata |
| On-chain accounts/notes (devnet) | users | new chain at 0.16 — 0.15 state gone | recreate accounts; release-notes breaking change |
| Guardian server delta/state records for 0.15 accounts | server operators | remain readable as historical append-only records; not interoperable with 0.16 networks | documented; no shims (FR-006) |
| Pending proposals created under 0.15 | SDK users | invalid across the boundary (commitment layout changed) | documented reset |
| Guardian wire contract (proto/HTTP shapes) | all | **unchanged in shape**; opaque Miden payload encoding becomes 0.16 | verified during implementation; any shape drift triggers AGENTS.md §4 workflow |

## State transitions (lifecycle preservation)

Guardian's delta lifecycle (pending → candidate → canonical / discarded) and
proposal lifecycle are **unchanged** by this migration. The delta→patch
change swaps the payload encoding and commitment computation inside those
records, not the state machine. Any implementation step that finds itself
altering a lifecycle transition must stop and re-check against constitution
Principle III.
