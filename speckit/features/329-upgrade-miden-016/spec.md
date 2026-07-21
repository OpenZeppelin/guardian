# Feature Specification: Upgrade Guardian to the Miden v0.16 Package Line

**Feature Branch**: `329-upgrade-miden-016`
**Created**: 2026-07-20
**Status**: Draft
**Input**: User description: "https://github.com/OpenZeppelin/guardian/issues/329 — We recently updated everything to miden v0.15.x packages. Now we need to plan update to latest released v0.16.x packages."
**Source Issue**: [OpenZeppelin/guardian#329](https://github.com/OpenZeppelin/guardian/issues/329) — "Upgrade Guardian to support Miden v0.16 sdk"

## Release-Line Reality Check (as of 2026-07-20)

The upstream v0.16 line exists only as pre-releases today, but **Miden devnet
has already rolled forward to it** (status.devnet.miden.io reports node
components at v0.16.0-alpha.2 / 0.16.0-alpha.1, with at least one service
still on 0.15.1). This is the same forcing function that drove the v0.15
migration: Guardian is currently incompatible with devnet.

| Upstream package line | Latest stable | Latest 0.16 available |
|---|---|---|
| miden-protocol / miden-standards / miden-tx (crates.io) | 0.15.3 | 0.16.0-alpha.4 |
| miden-client / miden-client-sqlite-store (crates.io) | 0.15.4 | 0.16.0-alpha.1 |
| miden-node (GitHub releases; devnet runs this line) | 0.15.1 | 0.16.0-alpha.2 |
| @miden-sdk/miden-sdk and companions (npm) | 0.15.7 | **none published** |

Guardian's current baseline: Rust workspace pins 0.15.0–0.15.3; the TypeScript
multisig client depends on `@miden-sdk/miden-sdk` ^0.15.0; browser examples pin
the `@miden-sdk/*` family at 0.15.1.

Consequence: "latest released v0.16.x packages" means tracking the 0.16
pre-release line now with exact pins, then moving pins to stable 0.16.0 when
it publishes. The TypeScript side is blocked on any upstream v0.16 npm
release existing at all; sequencing is Rust-first with TS following (FR-008).

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Guardian server and Rust SDK operate on a Miden v0.16 network (Priority: P1)

An operator runs the Guardian server, and a developer uses the Rust multisig
SDK, against a Miden network running the v0.16 node — including today's
devnet. The complete custody lifecycle works exactly as it does on v0.15:
creating a multisig account, registering it with Guardian, creating proposals,
collecting signatures, executing transactions, and having Guardian
canonicalize the resulting state.

**Why this priority**: The server and Rust SDK are the core of the product,
and devnet has already moved to v0.16 — Guardian is unusable there until this
lands.

**Independent Test**: Run the full Rust-side end-to-end suite and the
interactive demo against a v0.16 Miden node; every lifecycle step (create,
sync, propose, sign, execute, verify state commitment) completes successfully.

**Acceptance Scenarios**:

1. **Given** a Guardian server and Rust SDK built on the v0.16 package line and a v0.16 Miden node, **When** a user creates a multisig account and registers it with Guardian, **Then** the account is created on-chain and Guardian accepts the registration.
2. **Given** a registered multisig account, **When** cosigners create, sign, and execute a proposal at threshold, **Then** the transaction lands on-chain and Guardian canonicalizes the delta with local and on-chain state commitments matching.
3. **Given** the upgraded workspace, **When** the existing regression gates run (state/delta determinism vectors, contract procedure-root checks, contract behavior suite), **Then** all pass with regenerated v0.16 reference values.

---

### User Story 2 - TypeScript SDK reaches the same v0.16 baseline with cross-SDK parity (Priority: P2)

A developer building a browser wallet uses the TypeScript multisig SDK on the
v0.16 line. Accounts and proposals created from the browser are byte-for-byte
identical to those created by the Rust SDK, and proposals flow between Rust
and TypeScript cosigners without divergence — the parity guarantee Guardian
established during the v0.15 migration.

**Why this priority**: The repository's core change rules forbid silent
behavior drift between the Rust and TypeScript clients. A v0.16 Rust upgrade
without the TypeScript counterpart leaves the SDKs on incompatible protocol
versions and breaks mixed-client cosigning. It is P2 only because it is
mechanically blocked on the upstream npm SDK publishing any v0.16 release.

**Independent Test**: Run the browser smoke harness and the cross-SDK
determinism gate; a TypeScript-constructed account matches the Rust-constructed
account byte-for-byte, and a proposal created in the browser can be signed and
executed by a Rust cosigner (and vice versa).

**Acceptance Scenarios**:

1. **Given** both SDKs on the v0.16 line, **When** the cross-SDK determinism gate runs, **Then** the TypeScript-built account equals the Rust-built account byte-for-byte.
2. **Given** a mixed cosigner set (one browser, one Rust CLI), **When** a proposal is created by one and signed/executed by the other, **Then** the lifecycle completes with no commitment mismatches.
3. **Given** the upstream npm SDK has not yet published a v0.16 release, **When** the Rust-side upgrade lands first, **Then** the temporary divergence is explicitly documented and no Guardian package releases ship until both SDKs target the same Miden version line (per FR-008).

---

### User Story 3 - Examples, smoke harnesses, and documentation reflect v0.16 (Priority: P3)

A new integrator following Guardian's examples (interactive demo, web example,
browser smoke harness) and documentation gets a working v0.16 setup on the
first attempt, with no stale version references or instructions that only
apply to v0.15.

**Why this priority**: Examples and docs are the onboarding surface; stale
ones generate support load and erode trust, but they do not block core
functionality.

**Independent Test**: Execute each example's smoke-test procedure from a clean
checkout against a v0.16 network endpoint; each completes without manual
fixes. A review of the documentation set finds no dangling v0.15 version
statements.

**Acceptance Scenarios**:

1. **Given** a clean checkout on the upgraded branch, **When** a user follows the quickstart and runs the interactive demo, **Then** every menu flow completes against a v0.16 network.
2. **Given** the browser examples, **When** their smoke procedures run, **Then** account creation, cosigner sync, propose/sign/execute, and offline export/import all pass.
3. **Given** the documentation set, **When** reviewed for version-specific statements, **Then** all version references, compatibility notes, and troubleshooting entries reflect the v0.16 baseline.

---

### User Story 4 - SDK consumers receive coordinated v0.16 package releases (Priority: P4)

A downstream team consuming Guardian's published Rust crates and npm packages
upgrades to a Guardian release that declares v0.16 compatibility, with release
notes that state the breaking protocol change and what consumers must do
(notably that accounts and networks on v0.15 do not interoperate with v0.16).

**Why this priority**: Publishing is the last step and depends on everything
above; publishing stable Guardian packages pinned to upstream pre-releases
also needs an explicit decision.

**Independent Test**: Dry-run the release pipeline for the affected Rust and
TypeScript packages; version metadata, changelogs, and compatibility notes are
consistent and the packages install cleanly in a fresh consumer project.

**Acceptance Scenarios**:

1. **Given** the completed upgrade, **When** the release manifest is prepared, **Then** every published Guardian package declares dependency ranges from a single, consistent Miden version line (no mixed 0.15/0.16 pins).
2. **Given** the release notes, **When** a consumer reads them, **Then** the breaking network-compatibility change and required consumer actions are explicit.

### Edge Cases

- **Pre-release churn**: tracking 0.16.0-alpha.N means a later alpha or the final 0.16.0 may break APIs again; pins must be exact (no caret drift) and the re-validation cost at each bump must be accounted for.
- **Version skew between client and network**: a v0.16 client against a v0.15 node (or vice versa) must fail with a recognizable error, not silent corruption — devnet and testnet will not roll forward simultaneously, and remote proving services may lag the node version (this exact skew broke v0.15 devnet flows).
- **Mixed-version network services**: devnet itself currently reports a mix of 0.16 alphas and a 0.15.1 component; flows that touch a lagging service (e.g., remote prover, faucet) may fail even when the node itself is compatible.
- **Existing persisted state**: accounts, local stores, and Guardian-side state created under v0.15 — the upgrade must define whether they remain readable, require reset, or are explicitly unsupported (project policy is no compatibility shims unless required, so an explicit documented reset is the expected shape).
- **Stale transitive pins**: lockfiles can retain old proving-system transitive versions that silently change kernel commitments (the exact failure seen in the v0.15 cycle); the existing regression test must be re-verified against v0.16's transitive set.
- **Embedded contract artifacts**: compiled contract code, procedure roots, and determinism reference vectors embedded in the repo and in the built TypeScript package must be regenerated together — a partial regeneration reproduces the stale-artifact mismatches seen during v0.15.
- **Third-party browser integrations**: wallet adapter and hosted-wallet dependencies in the examples may lag the v0.16 SDK; each needs a compatibility check rather than an assumed lockstep bump.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: All Miden platform dependencies across the repository (Rust workspace, TypeScript package, examples) MUST move to a single consistent v0.16 version line, with no component left on v0.15 except where explicitly deferred by the sequencing decision in FR-008.
- **FR-002**: The complete multisig custody lifecycle (account creation, Guardian registration, cosigner sync, proposal create/sign/execute, offline export/import, state-commitment verification, canonicalization) MUST work end-to-end on a v0.16 Miden network via the Rust SDK and Guardian server.
- **FR-003**: The same lifecycle MUST work via the TypeScript SDK in the browser, and cross-SDK determinism (byte-for-byte identical account construction, interoperable proposals between Rust and TypeScript cosigners) MUST be preserved and verified by the existing automated gate.
- **FR-004**: All embedded protocol artifacts — compiled contract code, procedure roots, determinism reference vectors, and any serialized fixtures — MUST be regenerated from the v0.16 toolchain, and the existing regression tests guarding them MUST pass against the regenerated values.
- **FR-005**: Version pins MUST be exact for any pre-release dependency (no floating ranges), and lockfiles MUST be verified to carry no stale transitive versions that alter protocol commitments.
- **FR-006**: The upgrade MUST define and document the fate of state created under v0.15 (local stores, registered accounts, pending proposals); per project policy, no backwards-compatibility shims are added unless the plan explicitly justifies them.
- **FR-007**: All user- and operator-facing documentation, examples, and smoke-test procedures MUST be updated to the v0.16 baseline, including any version-compatibility and troubleshooting guidance.
- **FR-008**: Sequencing (decided 2026-07-20): the Rust-side upgrade proceeds first and may merge to the main branch with the temporary Rust/TypeScript protocol-version divergence explicitly documented; the TypeScript package upgrades as soon as the upstream npm SDK publishes any v0.16 release. No Guardian package releases (crates.io or npm) ship until both SDKs target the same Miden version line, so consumers can never receive a silently incompatible SDK pair. This mirrors the v0.15 migration sequencing.
- **FR-009**: The upgraded system MUST surface a recognizable, actionable error when pointed at a Miden network whose version is incompatible with the client, rather than failing silently or corrupting state.
- **FR-010**: Published Guardian package releases carrying the upgrade MUST declare the Miden version-line change in their release notes as a breaking change, including required consumer actions.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: 100% of the existing automated validation matrix (Rust workspace tests, TypeScript package tests, contract behavior suite, cross-SDK determinism gate, CI jobs) passes on the v0.16 line.
- **SC-002**: Every multisig lifecycle operation available in the interactive demo and browser smoke harness completes successfully against a v0.16 Miden network — including public devnet — matching the operation set that worked on v0.15 (zero functional regressions).
- **SC-003**: Zero mixed-version Miden pins remain in the repository at completion (audited by dependency listing), except components explicitly deferred by the recorded sequencing decision.
- **SC-004**: A new integrator can follow the quickstart on a clean machine and reach a working v0.16 multisig flow without undocumented manual steps.
- **SC-005**: When stable 0.16.0 publishes, moving from the tracked pre-release pins to stable requires only pin updates plus a re-run of the validation matrix — any additional source changes are driven by documented upstream API deltas, not by gaps left in this migration.

## Assumptions

- Devnet already runs the v0.16 alpha line (confirmed via status.devnet.miden.io on 2026-07-20), so the upgrade proceeds now against the latest available 0.16 pre-releases with exact pins, moving to stable 0.16.0 when it publishes. This repeats the v0.15 devnet-forced pattern.
- The v0.16 upgrade is expected to be protocol-breaking relative to v0.15 (as v0.15 was to v0.14): accounts and networks do not interoperate across the boundary. Consumer-facing framing treats this as a coordinated breaking upgrade, not a drop-in bump.
- The scope covers dependency migration and restoration of existing behavior. Adopting new v0.16 features (beyond what the migration forces) is out of scope.
- Third-party example dependencies (browser wallet adapters, hosted wallet SDKs) follow their own release cadence; the examples adopt whatever combination is compatible, mirroring the v0.15 approach of not forcing lockstep.
- Guardian package publishing to registries waits until both SDKs target the same Miden version line (per FR-008); landing the upgrade on the main branch is independent of publishing.

## Dependencies

- Upstream publication of Miden v0.16 packages: Rust crates (available as alphas), the Miden node (alpha, already deployed to devnet), and the TypeScript SDK on npm (not yet available in any v0.16 form — blocks User Story 2).
- Availability of a v0.16 Miden network endpoint (local node for development; devnet already rolled forward).
- Compatibility of remote proving infrastructure with v0.16 (a known failure mode during the v0.15 cycle on devnet, and devnet currently reports one service still on 0.15.1).
