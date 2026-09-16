# Implementation Plan: Miden system qualification (black-box system E2E and multisig)

**Feature**: `001-system-e2e-qualification` · **Spec**: [spec.md](./spec.md) · **Research**: [research.md](./research.md) · **Branch**: `001-system-e2e-qualification` · **Date**: 2026-09-15 · **Issue**: [#432](https://github.com/OpenZeppelin/guardian/issues/432)

## Summary

Build a qualification suite that proves the assembled Guardian system works:
the shipped server image against a real database over real sockets, driven by
both base clients and both multisig SDKs, with the multisig flows executing
real transactions on public Miden networks funded from a CI-held treasury.

Two profiles. The **deterministic profile** provisions a stack and drives
fixture flows with no external chain; it is fast and reliable enough to gate
merges. The **live profile** drives real transactions; it runs on a schedule,
before a release, after publication, and on reviewer opt-in for a pull request.

The work divides into four buildable pieces: a scenario model shared by both
drivers, a stack provisioner, a Rust driver, and a TypeScript driver, wired
together by workflows. No product code changes are required. The SDKs are
already headless-capable.

## Technical Context

**Languages**: Rust 1.98.1 (driver, workspace toolchain), TypeScript on Node 24
(driver), Bash (stack provisioning), YAML (workflows).

**Primary dependencies**: existing `crates/miden-multisig-client` and
`crates/client`; existing `packages/miden-multisig-client` and
`packages/guardian-client`; Docker and Compose v2; Postgres 16.

**Storage**: Postgres for the provisioned server, forced by the published
image's compile-time backend. Per-run ephemeral SQLite inside the Rust SDK; an
in-memory IndexedDB shim inside the TypeScript driver.

**Testing**: `cargo` for the Rust driver, `vitest` for the TypeScript driver,
both emitting the same result schema. The existing Playwright determinism spec
is retained unchanged.

**Target platform**: `ubuntu-latest` runners and developer machines with a
container runtime.

**Project type**: test and CI infrastructure over an existing multi-language
workspace.

**Performance goals**: deterministic profile under 15 minutes (SC-002);
scheduled full live run within 3 hours per network (SC-013), achieved by
running scenarios that do not share an account concurrently.

**Constraints**: proposals must complete inside the target network's historical
state window (FR-018b); submissions are never retried (FR-038); the TypeScript
store does not survive a process boundary; treasury-signed requests serialize
per key (FR-033c).

**Scale/scope**: three multisig shapes by two schemes by two SDKs by two
networks, plus offline, handoff, migration, recovery, transfer and consumption
scenarios, minus pairs declared unavailable.

## Constitution Check

*Gate evaluated against `speckit/constitution.md` v1.1.0. Verdict: PASS.*

| Principle | Assessment |
|---|---|
| I. Bottom-up propagation | No wire contract changes. The suite itself is built bottom-up: stack, base clients, multisig SDKs, scenarios. FR-017a records a capability gap this feature deliberately does not close. |
| II. Transport and cross-language parity | Directly served. FR-011 exercises both transports; FR-023 and SC-011 hold the TypeScript set at least as complete as Rust; FR-023e requires cross-SDK handoffs, since independent per-SDK runs do not evidence parity. |
| III. Append-only integrity and explicit lifecycles | FR-018 asserts completion through the real lifecycle (chain confirmation, canonical delta, commitment agreement, absence from pending) instead of inventing a terminal status. FR-038 and FR-038a forbid silent resubmission. FR-003 keeps outcomes explicit; skip is not pass. |
| IV. Explicit auth and stable boundary errors | FR-012 asserts the envelope by stable code over a real socket; FR-012a forbids asserting on wording; FR-019a covers below-threshold rejection and duplicate signatures; FR-042 covers the operator surface. |
| V. Evidence-driven delivery | This feature is the principle. It discharges the standing requirement that high-risk areas receive updated validation. |

**Documented divergence carried forward, per Principle II**: the TypeScript
bundled client retries submissions internally and cannot be told not to; the
Rust path disables its transport retry loop and never retries submissions.
FR-038a records this. It is a dependency-imposed asymmetry, not a design
choice, and the TypeScript canary cannot prove non-resubmission.

**Second documented divergence, found while building the offline scenario**:
the Rust SDK ties offline *signing* to offline *execution*. Its
`TransactionType::supports_offline_execution` is true only for
`SwitchGuardian`, and `sign_imported_proposal` refuses anything else, so a
consume-notes proposal cannot have its signatures collected off-channel. The
TypeScript SDK signs any proposal type offline and contacts GUARDIAN only to
execute, which is the same thing the Rust path would do. The restriction
therefore blocks an air-gapped cosigning workflow that the protocol allows.
The Rust leg of `live-offline-export-import-2of3-falcon` reports this as a
skip naming the gap, so it stays visible instead of reading as a pass.

**Invariant deviation**: see Complexity Tracking.

## Project Structure

### Documentation (this feature)

```text
speckit/features/001-system-e2e-qualification/
├── spec.md
├── plan.md              # this file
├── research.md          # Phase 0
├── data-model.md        # Phase 1
├── quickstart.md        # Phase 1
├── contracts/           # Phase 1
│   ├── scenario-manifest.md
│   ├── run-result.md
│   └── harness-cli.md
└── checklists/requirements.md
```

### Source code

```text
qualification/
├── manifest/                     # scenario manifest + coverage matrix (data, not code)
│   ├── scenarios.toml
│   └── matrix.toml
├── stack/                        # deterministic + live stack provisioning
│   ├── compose.yml               # parameterized ports, project-namespaced
│   ├── compose.registry.yml      # published-image overlay
│   ├── rpc-stub/                 # h2 stand-in for the deterministic profile
│   └── run.sh                    # single entry point (FR-010)
└── report/                       # result schema + merge/render

crates/qualification-driver/      # Rust driver (new workspace member)
├── src/
│   ├── main.rs                   # clap subcommands, non-interactive
│   ├── scenario/                 # scenario execution per capability
│   ├── funding/                  # treasury + ephemeral account lifecycle
│   └── report/                   # emits the shared result schema
└── tests/

packages/miden-multisig-client/
└── tests/qualification/          # TypeScript driver, reuses existing WASM setup

.github/workflows/
├── qualification-deterministic.yml
└── qualification-live.yml
```

**Structure decision**: a top-level `qualification/` directory holds the
language-neutral assets (manifest, stack, report schema) so neither driver owns
them, plus one new Rust workspace member and one new test directory inside the
existing TypeScript package. The TypeScript driver lives inside the package
rather than as a new workspace so it inherits the module-resolution and WASM
initialization plumbing that already works there (D2).

## Layer-by-layer (bottom-up, Constitution I)

1. **Scenario model** (`qualification/manifest/`). Scenario identifiers,
   dimensions, and the coverage matrix as data. Both drivers read it; neither
   defines it. This is what makes FR-003c through FR-003f enforceable and what
   keeps the two drivers describing the same thing.
2. **Stack provisioner** (`qualification/stack/`). Compose project per run,
   parameterized host ports, published-image overlay, the h2 stand-in, the
   acknowledgement key material, raised rate limits, readiness polling on both
   ports against a deadline, teardown that survives cancellation, and orphan
   recovery.
3. **Result schema and reporting** (`qualification/report/`). One schema both
   drivers emit; merge across drivers and networks; derive the run conclusion
   from outcomes per FR-003a and FR-003b; render the matrix claim per FR-003d.
4. **Rust driver** (`crates/qualification-driver/`). Constructs N clients in
   one process with deterministic keys, exchanges commitments in memory, runs
   scenarios, funds from the treasury, emits results.
5. **TypeScript driver** (`packages/miden-multisig-client/tests/qualification/`).
   Same manifest, same schema, server-side runtime, one process per scenario.
6. **Workflows**. Deterministic as a required check; live on schedule,
   dispatch, publication, pre-release, and PR opt-in.
7. **Docs**. Local and CI execution, outcome classes, treasury top-up and
   re-creation runbook (FR-045, FR-033e).

## Delivery slices

Story priority ranks risk; this ranks build order. Slice 1 produces a required
green check long before any funded canary exists.

| Slice | Contents | Stories |
|---|---|---|
| 1 | Scenario model, stack provisioner, result schema, deterministic profile, local command, required workflow | US5, US4 |
| 2 | Treasury and funded-account lifecycle, one network | US3 |
| 3 | TypeScript live driver, core subset, one network | US1 |
| 4 | Rust live driver, then second network for both | US2 |
| 5 | Parity assertions, handoffs, offline, migration | US6, US7 |
| 6 | Published pairing (install step plus skew check on top of slice 4) | (none) |
| 7 | Operator surface, release record | US8, US9 |

## Validation

- **Slice 1**: the deterministic profile passes against a freshly built image;
  pointing it at a stale image fails the identity assertion; killing a run
  mid-flight leaves no containers, volumes or databases.
- **Slices 2 to 4**: a scenario passes end to end on testnet; an underfunded
  treasury stops the run with the distinct outcome rather than failing partway;
  a deliberately broken execution path fails a named scenario rather than
  erroring ambiguously (spec US1 acceptance 5).
- **Slice 5**: a deliberate convention difference on one SDK fails the parity
  assertion and names both sides.
- **Slice 6**: the published pairing detects a version skew between image and
  packages, and records the artifact set.
- **Throughout**: no secret, key, session cookie or signed payload appears in
  any artifact (SC-009), checked by scanning retained artifacts.

## Open decisions

1. ~~**Target network scope.**~~ **Resolved** (requester, 2026-09-15): both
   networks are full targets, published pairing included. The devnet transport
   refusal is treated as a temporary outage, absorbed at run time by FR-026f,
   so devnet stays declared available and recovers without a manifest edit.
   The separate retention-window constraint (issue #462) is structural and
   persists: it shrinks devnet's *required* set rather than its availability,
   so long multi-step scenarios run there opportunistically but are not
   required. Two treasuries.
2. ~~**Treasury account shape.**~~ **Resolved** (requester, 2026-09-16): a
   single-signature basic wallet driven with the Miden client directly, not a
   guarded account driven by the SDK under test. The deciding argument is
   diagnostic independence: a guarded treasury shares a failure mode with the
   subject, so a regression in the proposal lifecycle, which is exactly what
   this suite exists to catch, would break funding and turn all 24 live
   scenario legs into "setup: could not fund" instead of naming the defect.
   The cost is accepted: the funding path is test infrastructure, so a bug
   there is ours rather than the product's.
3. **Publishing-side trigger.** Adding a job to the publishing workflow
   satisfies FR-025c directly; re-resolving the digest from the registry needs
   no publishing-side change but does not have the pipeline initiate.

## Complexity Tracking

| Violation | Why needed | Simpler alternative rejected because |
|---|---|---|
| Postgres instead of the filesystem default (constitution invariant: "Local development and test work default to the filesystem backend unless a task explicitly requires Postgres") | The task explicitly requires it. Storage backend is a compile-time feature and the published image is built with Postgres, with no filesystem fallback. | A filesystem-backed run cannot exercise the artifact operators actually run, which is the whole point of the profile. |
| A new workspace member rather than extending `examples/demo` | The demo is interactive top to bottom, with prompts threaded through every action signature. | Adding a non-interactive mode would grow a second control path through a 1655-line interactive module and leave both under-tested. |
