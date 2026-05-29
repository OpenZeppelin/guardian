# Implementation Plan: Memory-Resident Secret Hygiene

**Branch**: `008-zeroize-secrets` | **Date**: 2026-05-29 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `speckit/features/008-zeroize-secrets/spec.md`

## Summary

Wrap every long-lived secret value held in the Guardian **server** process in a small set of typed wrappers that (a) zero their backing buffer on `Drop`, (b) omit `Display` and `serde::Serialize`/`Deserialize` impls entirely so the compiler refuses accidental log/JSON exposure, (c) redact `Debug` to a non-disclosing marker, and (d) route the few remaining byte-by-byte equality checks against untrusted input through a single constant-time helper. The wrappers live in `crates/server/src/secret/` and ship behind a small API: construct → use via an explicit `expose_secret()` call site → drop and zeroize.

Crate selection (resolved in Phase 0 research): the `secrecy` crate (which composes `zeroize` for `Drop`) provides the redaction-on-`Debug`, no-`Display`, no-`Serialize`-by-default contract out of the box; `subtle` provides the constant-time-equality primitive. `zeroize` is brought in transitively but is also depended on directly for any newtype that needs a custom `Zeroize`/`ZeroizeOnDrop` derive.

Implementation order: (1) introduce the `secret` module and four named wrapper types; (2) migrate the in-scope inventory site-by-site in independent commits, smallest blast radius first (cursor HMAC secret → DB URL → EVM RPC URLs → session-token stores → ACK signer key-load path → AWS Secrets Manager fetched bytes); (3) verify the `miden-protocol::SecretKey` boundary per FR-011 and either cite upstream zeroization or wrap with a local adapter; (4) at every secret-bearing env-var read, fold the `std::env::var(...)` call and the wrapper constructor into a single expression so no intermediate `String` is held (FR-012); (5) add the compile-time and runtime tests required by SC-002 / SC-003 / SC-005 / SC-009; (6) document the FR-007 guard as a `CONTRIBUTING.md` reviewer checklist plus the `quickstart.md` audit recipe.

**Env-var handling — explicit scope**: this feature wraps the *Rust-side* destination of each secret-bearing env var (`DATABASE_URL`, `GUARDIAN_DASHBOARD_CURSOR_SECRET`, `GUARDIAN_EVM_RPC_URLS`). It does **not** address the OS process environment block — `/proc/<pid>/environ`, coredumps, fork-inherited env, and ECS/docker-injected env vars all remain visible at the OS layer. Mitigating that is an infra concern (prefer AWS Secrets Manager runtime fetch over env-var injection, already used by the Falcon/ECDSA key load path). The plan does **not** call `unsafe { std::env::remove_var(...) }` after reads; the threat reduction is small relative to the process-global `unsafe` cost. See spec Out-of-Scope.

## Technical Context

**Language/Version**: Rust 1.x (workspace's pinned toolchain — `rust-toolchain.toml`).
**Primary Dependencies (new)**:
- `secrecy = { version = "0.10", default-features = false }` — explicitly without the `serde` feature; FR-003 forbids transparent serialization, and `secrecy`'s `serde` feature would add a `Deserialize` impl that defeats the compile-time guarantee. The compile-time `assert_not_impl_any!` checks (SC-002 / SC-003) are the backstop if the feature is enabled by accident.
- `subtle = "2.5"` — constant-time equality primitive.
- `zeroize = { version = "1.7", features = ["derive"] }` — direct dep where a `#[derive(Zeroize, ZeroizeOnDrop)]` is needed, and (separately) used by `crates/miden-keystore` to wrap its disk-read buffer as `zeroize::Zeroizing<Vec<u8>>` (see FR-011 and the dep-direction note below).
- `static_assertions = "1.1"` (dev-dependency) — compile-time `assert_not_impl_any!` checks.

**Dep-direction note (FR-011 / contracts)**: the `crates/server/src/secret` wrapper module is `pub(crate)` and never reachable from `crates/miden-keystore`. To satisfy FR-001 inside `miden-keystore` without inverting the dependency graph, the keystore's transient disk-read buffer uses **`zeroize::Zeroizing<Vec<u8>>`** directly (a tiny dep already in the workspace lock graph via the crypto stack). `Zeroizing<T>` provides the zero-on-drop contract; the keystore does not need the no-`Display`/no-`Serialize` posture of the server-only wrappers because its buffers are stack-local and never reach a formatter or serializer in normal code paths. This keeps `server → miden-keystore` as the only dependency direction.
**Primary Dependencies (existing, untouched)**: `tracing`, `serde`, `serde_json`, `axum`, `diesel_async`, `miden-keystore` (local crate), `miden-protocol` (external).
**Storage**: N/A — this feature does not touch persisted data. Existing Postgres/filesystem backends are unchanged.
**Testing**: `cargo test -p guardian-server` (unit tests in-tree), plus targeted compile-fail tests (either `trybuild` or a documented `#[cfg(compile_fail)]` snippet — see Phase 0 decision).
**Target Platform**: Linux server (existing AWS ECS deployment; no platform-specific code added).
**Project Type**: Server-side library refactor inside `crates/server`. No new binary, no new HTTP/gRPC surface.
**Performance Goals**: Zeroize-on-drop overhead must be negligible at session-expiry rates (well under 1 ms per dropped session token, which is comfortable headroom over the per-session work the server already does). No measurable change in p95 request latency.
**Constraints**: No change to the public HTTP/gRPC surface. No change to the SDK crates (`crates/client`, `crates/miden-multisig-client`) or to the TypeScript packages. Server wrappers (`SecretBox`-backed types in `crates/server/src/secret`) stay strictly inside the server crate. The independent local change in `crates/miden-keystore` uses `zeroize::Zeroizing<Vec<u8>>` directly — no shared module, no inverted dependency.
**Scale/Scope**: ~7 in-scope storage sites across 5 modules (cursor, dashboard state, evm session, builder/storage, evm config, ACK signers, secrets-manager fetch). Estimated ~300–500 LOC change including tests.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-evaluated post-Phase-1 design — see end of this section.*

The Guardian Constitution v1.1.0 lists five principles plus system invariants. This feature is an internal, server-only, type-system refactor with no wire-contract changes. Each principle is evaluated below.

| Principle | Status | Justification |
|---|---|---|
| **I. Bottom-Up Change Propagation** | ✅ Pass (vacuous) | The change is contained in `crates/server`. There are no downstream contract changes that need to propagate to Rust/TS clients, multisig SDKs, or examples. The wrapper types are private to the server crate (or live in a server-local sub-module) and never appear in a public response or in a serialized form on the wire. |
| **II. Transport and Cross-Language Parity** | ✅ Pass (vacuous) | No HTTP/gRPC surface change. No client-observable behavior change. Rust and TypeScript clients are not modified. |
| **III. Append-Only Integrity and Explicit Lifecycles** | ✅ Pass (vacuous) | No change to state, delta, proposal records, lifecycle transitions, fallback paths, or status enums. |
| **IV. Explicit Authentication and Stable Boundary Errors** | ✅ Pass | This feature *strengthens* the existing auth posture (constant-time HMAC verify on cursors, scoped zeroization of session tokens). No change to error surfaces, payload shapes, status enums, or signature schemes. Tests in the dashboard / evm session modules are updated where the equality path changes; no downstream consumer is affected because the wrappers do not cross the response boundary. |
| **V. Evidence-Driven Delivery** | ✅ Pass | Three independently testable user stories (disclosure / erasure / timing) are defined in the spec. The Phase 1 quickstart documents the validation steps. Tests added: compile-time non-impl assertions (SC-002 / SC-003), runtime `{:?}` redaction assertions (SC-005), drop-zero sanity check (SC-007), structural review for response/config boundary (SC-009). |

**System Invariants**: none are affected. Per-account auth remains explicit. Replay protection is unchanged (this feature does not touch nonce/digest *semantics* — challenges were explicitly removed from scope in the second review round). Filesystem/Postgres backend semantics are unchanged. Storage shapes are unchanged.

**Initial Constitution Check: PASS.**

**Post-Phase-1 Constitution Re-Check**: PASS (no new violations introduced — see end of plan).

## Project Structure

### Documentation (this feature)

```text
speckit/features/008-zeroize-secrets/
├── plan.md              # This file
├── spec.md              # Feature specification (already complete)
├── research.md          # Phase 0 output (crate selection, keystore boundary verification, redaction pattern)
├── data-model.md        # Phase 1 output (wrapper type taxonomy + in-scope-site mapping)
├── contracts/           # Phase 1 output (internal wrapper-type API contract)
│   └── secret-module.md
├── quickstart.md        # Phase 1 output (developer onboarding + verification steps)
├── checklists/
│   └── requirements.md  # Already complete
└── tasks.md             # Phase 2 output (NOT created here — produced by /speckit-tasks)
```

### Source Code (repository root)

```text
crates/server/src/
├── secret/                                    # NEW — wrapper types live here
│   ├── mod.rs                                 #   re-exports + module-level docs
│   ├── wrappers.rs                            #   FixedKey<N>, SecretBytes, SecretString, CredentialUrl
│   ├── ct.rs                                  #   constant-time equality helper (single named fn)
│   └── tests.rs                               #   compile-time assertions + Debug-redaction tests
│
├── dashboard/
│   ├── cursor.rs                              # MODIFIED — CursorSecret now wraps FixedKey<32>; HMAC verify continues to use hmac::Mac::verify_slice (constant-time per RustCrypto); manual Debug redaction removed
│   └── state.rs                               # MODIFIED — sessions keyed by sha256(token); token bytes not retained in memory after Set-Cookie response
│
├── evm/
│   ├── config.rs                              # MODIFIED — EvmChainConfig.rpc_url is CredentialUrl
│   └── session.rs                             # MODIFIED — sessions keyed by sha256(token); same shape change as dashboard/state.rs
│
├── builder/
│   └── storage.rs                             # MODIFIED — StorageMetadataBuilder.database_url is CredentialUrl
│
├── ack/
│   ├── miden_falcon_rpo/signer.rs             # MODIFIED — keystore field unchanged; FR-011 verification recorded in research.md
│   ├── miden_ecdsa/signer.rs                  # MODIFIED — same
│   └── secrets_manager.rs                     # MODIFIED — fetched bytes returned as SecretBytes; cache field (if present) holds SecretBytes
│
└── (NO public API changes — no api/ files modified)

crates/miden-keystore/src/
├── keystore.rs                                # MODIFIED — read_key_file's local key_bytes is zeroize::Zeroizing<Vec<u8>>; FR-011 verification noted in a code comment
└── ecdsa_keystore.rs                          # MODIFIED — same

speckit/features/008-zeroize-secrets/
└── (artifacts listed above)
```

**Structure Decision**: Single-project Rust workspace. All new code lives in a new `crates/server/src/secret/` module. Modifications to existing modules are surgical (changing one field type and adjusting a small set of call sites per site). Cross-crate touch is limited to the in-repo `miden-keystore` and only if FR-011 verification requires it.

## Phase 0 Output

See [`research.md`](./research.md) — covers:
- Crate selection: `secrecy` vs hand-rolled, `zeroize` direct vs transitive, `subtle` vs `constant_time_eq`.
- FR-011 verification: does `miden-protocol::SecretKey` zeroize on drop, and if not, what is the adapter shape.
- Pattern for compile-time non-impl assertions (`static_assertions` vs `trybuild` vs `#[deny(...)]` lints).
- Pattern for the `Debug` redaction in enclosing structs that already derive `Debug`.
- FR-007 guard mechanism choice (manual reviewer checklist plus audit recipe).
- A small decision on whether `SecretString` for session tokens warrants a 32-byte `FixedKey<32>` variant instead (mostly stylistic).

## Phase 1 Outputs

See:
- [`data-model.md`](./data-model.md) — wrapper-type taxonomy, the in-scope inventory mapped to a wrapper variant per site, and the migration order with blast-radius rationale.
- [`contracts/secret-module.md`](./contracts/secret-module.md) — the internal API contract for the `crates/server/src/secret` module: public surface, allowed/forbidden trait impls, constructor signatures, exposure signatures, constant-time helper signature, redaction-marker constant.
- [`quickstart.md`](./quickstart.md) — how a developer adds a new secret field, how to run the test suite for this feature, and how a reviewer enumerates exposure call sites.

## Complexity Tracking

> Fill ONLY if Constitution Check has violations that must be justified.

No violations. Table omitted.

## Post-Phase-1 Constitution Re-Check

After completing Phase 1 design (data model, contracts, quickstart):

- **I.** Still server-internal. No downstream effect.
- **II.** No transport surface change. Wrappers explicitly forbidden from crossing the response/config boundary by spec FR-003 + SC-009.
- **III.** No lifecycle or append-only change.
- **IV.** Auth posture strengthened (constant-time HMAC verify, session-token zeroization). Existing tests in `dashboard/state.rs` and `evm/session.rs` are updated to thread the new wrapper through their fixtures; no upstream consumer is affected.
- **V.** Evidence plan complete: see quickstart.md and the test list in data-model.md.

**Re-Check: PASS.**
