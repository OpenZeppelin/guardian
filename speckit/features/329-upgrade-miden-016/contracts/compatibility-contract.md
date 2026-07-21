# Compatibility Contract: Miden v0.16 Upgrade

**Feature**: `329-upgrade-miden-016` | **Date**: 2026-07-20

This feature adds no new API endpoints. Its "contracts" are compatibility
guarantees the migrated system must honor. Each is testable and maps to spec
requirements.

## Contract 1: Guardian wire contract shape is invariant

The Guardian server's gRPC (`guardian.proto`) and HTTP JSON surfaces keep
their existing shapes, field names, status enums, and error semantics. The
Miden-encoded payloads carried opaquely inside them (state blobs, delta
payloads, commitments as hex words) change *encoding version* to 0.16, not
shape.

- **Verification**: OpenAPI drift check (`gen-openapi --check docs`) passes with no schema change; proto untouched by the diff.
- **Escape hatch**: if implementation discovers a genuine shape change is required, work stops and the AGENTS.md §4 contract-change workflow runs before proceeding (spec FR-001 boundary, constitution Principles I/IV).

## Contract 2: Single-version lockfile

The resolved dependency graph contains exactly one 0.16.x `miden-protocol`
version, a mutually consistent Plonky3 `p3-*` 0.6 set, and no residual 0.15
miden crates or winterfell crates.

- **Verification**: `cargo tree -i miden-protocol` shows one version; `transaction_kernel_commitment_matches_network` passes (guards the p3/kernel-commitment coupling); dependency-listing audit per spec SC-003.

## Contract 3: Embedded artifacts are regenerated atomically and stay in lockstep

Procedure roots in `procedures.rs` and `procedures.ts` are byte-identical
lists produced by one run of the `procedure_roots` generator against the 0.16
toolchain; vendored MASM in the TS package equals canonical MASM in
`crates/contracts/masm/`; the kernel-commitment constant matches the compiled
0.16 kernel.

- **Verification**: `procedure_roots_match_compiled_account` (Rust), MASM copy diff (empty), kernel-commitment guard test, contracts MockChain suite.
- **Note**: TS constants/MASM are updated in this slice even though the TS package cannot build against 0.16 yet — leaving them stale was the 0.15 cycle's recurring failure mode. The TS slice re-verifies them.

## Contract 4: Version-mismatch behavior is explicit (spec FR-009)

A 0.16 Guardian/SDK pointed at a 0.15 network (or vice versa) fails at the
RPC boundary via the genesis-commitment header check with a recognizable,
actionable error — no silent corruption, no partial writes.

- **Verification**: manual/e2e probe against a mismatched node; TROUBLESHOOTING entry documents the symptom and remedy.

## Contract 5: Lifecycle and auth invariants survive the payload swap

Append-only delta records, `prev_commitment`/nonce lineage, explicit
pending→candidate→canonical/discarded transitions, deterministic proposal
IDs, duplicate-signature rejection, and per-account replay-protected auth all
behave identically on 0.16.

- **Verification**: existing server unit + e2e suites (canonicalization, switch-guardian, proposal lifecycle) pass unmodified in their assertions (fixtures regenerated); auth changes carry updated tests in each changed layer plus one upstream consumer (constitution Principle IV).

## Contract 6: Rust/TS divergence is bounded and visible (spec FR-008)

**Status (2026-07-21, later): divergence RESOLVED at source level.**
`@miden-sdk/miden-sdk 0.16.0-alpha.1` published; TS package and browser
examples bumped, 327/327 vitest green, all example builds green. Release
gate stays closed only for upstream 0.16 stabilization (Contract 6's
parity condition is met). Para wallet packages remain 0.15 via npm
overrides until upstream updates them.


While the TS slice is deferred: the divergence is documented in README/docs;
no Guardian package (crates.io or npm) is released; the TS package keeps
building against 0.15 unchanged. The re-entry trigger is any
`@miden-sdk/miden-sdk` 0.16 release on npm (watch: web-sdk PR #225). Parity is
restored by the TS slice and proven by the cross-SDK determinism gate before
any release.
