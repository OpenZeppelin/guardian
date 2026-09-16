# Tasks: Miden system qualification (black-box system E2E and multisig)

**Input**: Design documents from `speckit/features/001-system-e2e-qualification/`
**Prerequisites**: [plan.md](./plan.md), [spec.md](./spec.md), [research.md](./research.md), [data-model.md](./data-model.md), [contracts/](./contracts/)

**Tests**: Test tasks appear only where the logic under test is subtle and pure (manifest validation, result derivation) or where the spec demands proof that a check actually fires. This is a test harness; blanket TDD over it would mostly test the SDKs again.

**Organization**: Phases follow user-story priority, as required. **Execution order does not.** See "Recommended build order" below before starting: the plan deliberately ships the P2 deterministic profile first, because it is the fastest route to a required green check and needs no treasury, no networks, and no unattended TypeScript runner.

## Implementation status (2026-09-16, after the first Docker and live stack runs)

100 of 138 tasks. Both profiles have now run against a real Docker daemon.

**Deterministic through the stack**: the image builds, Postgres and both
GUARDIANs come up, the Rust scenarios pass including the post-restart durability
assertion, artifacts are redacted and scanned, teardown is clean. One scenario
fails, `det-operator-allowlist-reload`, for a host reason rather than a product
one (F12). It stays dispatch-only until it has been seen to go red on a real
regression.

**Live through the stack, testnet.** The manifest carries 18 live scenarios and
each SDK runs 17 of them: 16 are shared and the two cross-SDK handoffs are
single-SDK by definition.

Last full matrix, measured:

| | passed | skipped | env-blocked | failed |
|---|---|---|---|---|
| Rust | 14 | 2 | 0 | 1 |
| TypeScript | 15 | 1 | 0 | 1 |

That run predates the migration fixes. Since then, and verified in focused runs
rather than a full matrix:

- `live-guardian-migrate-offline-1of1-ecdsa` passes on **both** SDKs. It was the
  Rust failure and the TypeScript environment-blocked result above.
- `live-remove-signer-2of3-falcon` still fails on TypeScript (F2) and passes on
  Rust, now including the new `signer-removed-refused` assertion.

So the expected full-matrix result is Rust 15 passed / 2 skipped and TypeScript
16 passed / 1 failed, but **that has not been run end to end** and should not be
quoted as though it had.

The Rust skips are documented capability gaps (F1 threshold change, F3 offline
signing of acknowledgement-bearing proposals). The TypeScript failure is F2, a
stale signer listing after a removal; the removal itself is enforced, which the
`signer-removed-refused` action establishes rather than assumes.

GUARDIAN migration reaching a pass took one product fix (F13, a scheme-blind
endpoint check that made ECDSA migration impossible) and three harness fixes,
each only visible after the one before it was corrected.

**Verified**: 83 Rust driver tests, the full TypeScript suite (638), 24 harness
tests, clippy and rustfmt clean, both typecheck configs clean.

**Findings**: 13 recorded in
[QUALIFICATION_FINDINGS.md](../../../docs/QUALIFICATION_FINDINGS.md), one fixed.

**Not done**: the negative control for a discarded delta, the funding handoff
that would let a reviewer opt in on a pull request, and the published pairing,
which is refused rather than implemented so it cannot claim coverage nobody has.

## Implementation note on file layout

**Many task lines name a file that does not exist.** The work landed, in fewer
files than the plan anticipated. Task text is kept verbatim so the mapping stays
traceable rather than rewritten to match what happened.

Thirty-four checked tasks are affected. The mapping:

| Tasks name | Work actually lives in |
|---|---|
| `scenario/{proposal,negative,recovery,assets,offline,migrate,durability,fixture_grpc}.rs` | `crates/qualification-driver/src/scenario/live.rs` and `account.rs` |
| `src/{client,keys,sync,budget}.rs` | `crates/qualification-driver/src/scenario/live.rs` and `duration.rs` |
| `funding/{balance,cap,report}.rs` | `crates/qualification-driver/src/funding/{usability,budget,summary}.rs` |
| `preflight/skew.rs` | `crates/qualification-driver/src/report/artifacts.rs` (`check_skew`) |
| `assert/offline.rs` | asserted inline by the offline actions in `scenario/live.rs` |
| `tests/qualification/actions/{proposal,commitment,negative,offline,migrate}.ts` | `packages/miden-multisig-client/tests/qualification/actions/live.ts` |
| `tests/qualification/{client,run,fixtureHttp,errorEnvelope}.ts` | `tests/qualification/{live,runner,fixtures}.ts` and `actions/` |
| `tests/qualification/operator/*.ts` | `packages/miden-multisig-client/tests/qualification/actions/operator.ts` |
| `tests/qualification/{manifestValidate,derive}.test.ts` | `tests/qualification/qualification.test.ts` |

The live actions all operate on one session and share its funding, signature
collection and completion helpers. Splitting them across the files named above
would have meant exporting that session's internals for no gain in readability.

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Create the skeleton both drivers and the workflows build on.

- [X] T001 Create the `qualification/` tree with `manifest/`, `stack/`, `stack/lib/`, `stack/rpc-stub/` and `report/` subdirectories
- [X] T002 Scaffold the Rust driver crate at `crates/qualification-driver/Cargo.toml` with `clap`, `tokio`, `serde`, `miden-multisig-client` and `guardian-client` path dependencies, and register it in the workspace `members` list in `Cargo.toml`
- [ ] T003 Exclude `qualification-driver` from the default workspace test runs in `.github/workflows/ci.yml`, alongside the existing `guardian-demo` and `guardian-rust-example` exclusions
- [X] T004 [P] Scaffold the TypeScript driver at `packages/miden-multisig-client/tests/qualification/` with a vitest project entry reusing `vitest.config.ts` module aliasing and `tests/setup-wasm.ts`
- [X] T005 [P] Add `qualification-results/` and `qualification/stack/.env.generated` to `.gitignore`
- [X] T006 [P] Write `qualification/README.md` pointing at this feature's spec, plan and quickstart

**Checkpoint**: directories and build wiring exist; nothing runs yet.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: The scenario model, result schema and stack provisioner. Every user story depends on all three.

**CRITICAL**: No user story phase can start until this phase completes.

### Scenario model

- [X] T007 Author the initial scenario set in `qualification/manifest/scenarios.toml` per `contracts/scenario-manifest.md`, covering the FR-019a action vocabulary across the FR-017 shapes and both schemes
- [X] T008 Author the coverage matrix in `qualification/manifest/matrix.toml` declaring both networks available for both SDKs, with per-pair required sets that respect each network's retention window
- [X] T009 [P] Implement the typed manifest model and loader in `crates/qualification-driver/src/manifest/mod.rs`
- [X] T010 [P] Implement the matching manifest loader in `packages/miden-multisig-client/tests/qualification/manifest.ts`
- [X] T011 Implement the seven load-time validation rules in `crates/qualification-driver/src/manifest/validate.rs`, rejecting before any container starts
- [ ] T012 [P] Mirror the same seven validation rules in `packages/miden-multisig-client/tests/qualification/manifestValidate.ts`
- [X] T013 [P] Unit-test each validation rule in `crates/qualification-driver/src/manifest/validate.rs` tests module, including offline-mode-on-non-migration and budget-exceeds-window
- [X] T014 [P] Unit-test the same rules in `packages/miden-multisig-client/tests/qualification/manifestValidate.test.ts`
- [X] T015 Add shared accept/reject manifest fixtures in `qualification/manifest/fixtures/` and assert both loaders agree on every fixture, so the two drivers cannot drift on what a scenario means

### Result schema and reporting

- [X] T016 Write the JSON schema for a run result in `qualification/report/run-result.schema.json` per `contracts/run-result.md`
- [X] T017 [P] Implement the result emitter in `crates/qualification-driver/src/report/emit.rs`
- [X] T018 [P] Implement the result emitter in `packages/miden-multisig-client/tests/qualification/report.ts`
- [X] T019 Implement `conclusion` and `qualification_claim` derivation in `crates/qualification-driver/src/report/derive.rs`
- [ ] T020 [P] Mirror the derivation in `packages/miden-multisig-client/tests/qualification/derive.ts`
- [X] T021 [P] Unit-test derivation in `crates/qualification-driver/src/report/derive.rs`, explicitly covering the invariant that a run may conclude `success` while claiming `partial`
- [X] T022 [P] Unit-test the same in `packages/miden-multisig-client/tests/qualification/derive.test.ts`
- [X] T023 Implement a `report merge` subcommand in `crates/qualification-driver/src/report/merge.rs` that combines driver results per network without collapsing them into one verdict
- [X] T024 Implement `not_covered` population in `crates/qualification-driver/src/report/derive.rs`, naming the EVM surface, the deferred operator surface and the mixed-scheme capability gap

### Stack provisioner

- [X] T025 Write `qualification/stack/compose.yml` with parameterized host ports and no fixed container names, so per-run project namespacing isolates volumes, network and containers
- [ ] T026 [P] Write `qualification/stack/compose.registry.yml` as the published-image overlay, modelled on the existing `docker-compose.registry.yml`
- [X] T027 [P] Implement the h2 stand-in service in `qualification/stack/rpc-stub/` that completes a transport handshake and nothing more
- [X] T028 Implement acknowledgement key provisioning in `qualification/stack/lib/ack.sh`, writing `0600` plain-hex key files and configuring the file-backed provider
- [X] T029 [P] Implement environment generation in `qualification/stack/lib/env.sh`, setting the network type, raised rate limits and database URL, and leaving the stage variable unset
- [X] T030 Implement readiness polling in `qualification/stack/lib/wait.sh`, polling both the HTTP and gRPC ports against a deadline with no fixed sleeps
- [X] T031 Implement teardown in `qualification/stack/lib/teardown.sh` with signal traps, so cancellation removes the stack
- [X] T032 Implement orphan recovery in `qualification/stack/lib/orphans.sh`, adopting or removing resources left by a dead earlier run
- [X] T033 Implement image build in `qualification/stack/lib/image.sh` for built-from-ref mode, passing the commit as a build argument so the identity assertion is not vacuous
- [X] T034 Extend `qualification/stack/lib/image.sh` with pulled-from-registry mode: resolve the tag to a digest and read the source revision from the image's own metadata
- [X] T035 Implement pairing inference and inconsistency rejection in `qualification/stack/lib/pairing.sh`, refusing a mixed image and SDK source rather than reconciling it
- [X] T036 Implement bounded diagnostics capture in `qualification/stack/lib/diagnostics.sh`, collecting driver output, server logs and container state on failure
- [X] T037 Implement artifact redaction and a secret scan in `qualification/stack/lib/redact.sh`, failing the run if a key, credential, cookie or signed payload reaches an artifact

**Checkpoint**: a stack can be provisioned, torn down and reported on. User story phases may now begin.

---

## Phase 3: User Story 1 - Nightly Miden multisig canary on the TypeScript SDK (Priority: P1)

**Goal**: The most used Guardian flows run end to end against a real Miden network through the TypeScript SDK, unattended.

**Independent Test**: Run the live profile with only the TypeScript scenarios selected, against a configured network with a funded treasury, and confirm every named scenario reports a pass or an explicit skip without the Rust scenarios running.

**Depends on**: Phase 2, and Phase 5 (US3) for a funded treasury.

- [X] T038 [US1] Implement client construction in `packages/miden-multisig-client/tests/qualification/client.ts`: WASM init from bytes, IndexedDB shim installed before SDK import, per-cosigner store isolation
- [ ] T039 [US1] Implement deterministic signer construction from seeds in `packages/miden-multisig-client/tests/qualification/signers.ts` for both Falcon and ECDSA, with no wallet-backed paths
- [ ] T040 [US1] Implement in-process commitment exchange between cosigners in `packages/miden-multisig-client/tests/qualification/cosigners.ts`, replacing the manual out-of-band step the browser harness requires
- [X] T041 [P] [US1] Implement the `account-create` and `account-register` actions in `packages/miden-multisig-client/tests/qualification/actions/account.ts`, reading the account back through Guardian to confirm registration
- [X] T042 [P] [US1] Implement the `proposal-create`, `proposal-sign` and `proposal-execute` actions in `packages/miden-multisig-client/tests/qualification/actions/proposal.ts`
- [ ] T043 [US1] Implement the four-part completion assertion in `packages/miden-multisig-client/tests/qualification/assert/completion.ts`: chain confirmation, canonical delta in history, commitment agreement, and absence from pending proposals. Do not wait for a terminal proposal status; it never arrives
- [X] T044 [P] [US1] Implement the `commitment-verify` action in `packages/miden-multisig-client/tests/qualification/actions/commitment.ts`
- [X] T045 [P] [US1] Implement `proposal-reject-below-threshold` in `packages/miden-multisig-client/tests/qualification/actions/negative.ts`, asserting the proposal stays pending
- [ ] T046 [US1] Implement the step-budget guard in `packages/miden-multisig-client/tests/qualification/budget.ts`, classifying a proposal lost to anchor pruning as environment-blocked and never silently re-proposing
- [ ] T047 [US1] Implement the stale-artifact check in `packages/miden-multisig-client/tests/qualification/staleness.ts`, failing when the SDK's built output does not correspond to the commit under test
- [ ] T048 [US1] Record `embedded_retry` on every TypeScript scenario result, since the bundled client may retry submissions below this project's control
- [ ] T049 [US1] Implement unknown-outcome resolution by observing chain and Guardian state in `packages/miden-multisig-client/tests/qualification/actions/resolve.ts`, never by resubmitting
- [X] T050 [US1] Wire the TypeScript driver entry point in `packages/miden-multisig-client/tests/qualification/run.ts` to read the manifest, honour scenario selection, and emit the shared result schema
- [X] T051 [US1] Add the negative-control check: introduce a deliberate break in the execution path and confirm a named scenario fails rather than passing or erroring ambiguously, documented in `qualification/README.md`

**Checkpoint**: the TypeScript canary runs standalone against one network.

---

## Phase 4: User Story 2 - The same canary runs on the Rust SDK (Priority: P1)

**Goal**: The identical scenarios run through the Rust SDK, so the reference implementation is covered to the same depth as the published one.

**Independent Test**: Run the live profile with only the Rust scenarios selected and confirm they complete without the TypeScript scenarios running.

**Depends on**: Phase 2, and Phase 5 (US3).

- [X] T052 [US2] Implement multi-client construction in `crates/qualification-driver/src/client.rs`: N clients in one process, each with its own account directory, with `reset_miden_client` called after build as the demo does
- [X] T053 [US2] Implement deterministic key loading in `crates/qualification-driver/src/keys.rs` using the builder's caller-supplied secret key methods for both schemes, never the generate methods
- [X] T054 [US2] Port the sync retry and reinitialize-on-store-error logic from `examples/demo/src/actions/sync_account.rs` into `crates/qualification-driver/src/sync.rs`
- [X] T055 [P] [US2] Implement the `account-create` and `account-register` actions in `crates/qualification-driver/src/scenario/account.rs`
- [X] T056 [P] [US2] Implement the `proposal-create`, `proposal-sign` and `proposal-execute` actions in `crates/qualification-driver/src/scenario/proposal.rs`, asserting executability from the returned proposal before executing
- [ ] T057 [US2] Implement the four-part completion assertion in `crates/qualification-driver/src/assert/completion.rs`, matching the TypeScript semantics exactly
- [ ] T058 [P] [US2] Implement `commitment-verify` in `crates/qualification-driver/src/scenario/commitment.rs`, wrapping the SDK call that errors on mismatch into a pass or fail outcome
- [X] T059 [P] [US2] Implement `proposal-reject-below-threshold` and `proposal-reject-duplicate-signature` in `crates/qualification-driver/src/scenario/negative.rs`
- [X] T060 [P] [US2] Implement `account-recover-by-cosigner` in `crates/qualification-driver/src/scenario/recovery.rs`
- [X] T061 [P] [US2] Implement `asset-transfer`, `note-consume` and `balance-assert` in `crates/qualification-driver/src/scenario/assets.rs`, reading balances from the account vault since no balance accessor exists
- [X] T062 [US2] Implement the step-budget guard in `crates/qualification-driver/src/budget.rs` with the same classification rules as the TypeScript side
- [X] T063 [US2] Wire the driver entry point in `crates/qualification-driver/src/main.rs` with clap subcommands, no interactive input anywhere
- [ ] T064 [US2] Generate the coverage-gap listing comparing the Rust and TypeScript scenario sets, failing the build when a flow is covered on one side only without a declared reason

**Checkpoint**: both canaries run standalone.

---

## Phase 5: User Story 3 - Test accounts fund themselves from a CI-held treasury (Priority: P1)

**Goal**: Each run derives ephemeral accounts and funds them from the network's treasury with no manual step.

**Independent Test**: Trigger a live run on a clean runner with only treasury configuration present and confirm every scenario account is funded automatically and the run reports the amount spent.

**Depends on**: Phase 2. **Blocks**: Phases 3 and 4.

- [X] T065 [US3] Implement treasury configuration loading in `crates/qualification-driver/src/funding/treasury.rs`, reading credentials from the environment only, never from a repository file
- [X] T066 [US3] Implement treasury usability verification in `crates/qualification-driver/src/funding/usability.rs`: present, holding the fee asset the chain names, and compatible with the contract version the pinned SDK expects
- [X] T067 [US3] Implement the balance precheck in `crates/qualification-driver/src/funding/balance.rs`, stopping with the distinct underfunded outcome naming observed and required amounts
- [X] T068 [US3] Implement fee-model detection in `crates/qualification-driver/src/funding/fees.rs`, reading the verification base fee and fee faucet from the anchor block header, and recording that funding was not required on a zero-fee chain
- [X] T069 [US3] Implement ephemeral account derivation in `crates/qualification-driver/src/funding/accounts.rs`, fresh per run and never reused
- [X] T070 [US3] Implement the funding transfer in `crates/qualification-driver/src/funding/transfer.rs`, sending the minimum each account needs before first use
- [X] T071 [US3] Implement bootstrap-from-note in `crates/qualification-driver/src/funding/bootstrap.rs`, consuming the inbound note whose own value pays the consuming transaction's fee
- [X] T072 [US3] Implement the per-run spend cap in `crates/qualification-driver/src/funding/cap.rs`
- [X] T073 [US3] Implement per-network treasury serialization in `crates/qualification-driver/src/funding/lock.rs`, so two runs cannot interleave a balance check and a transfer
- [X] T074 [US3] Implement per-key signing serialization in `crates/qualification-driver/src/funding/lock.rs`, and classify a replay rejection as correct server behaviour rather than a product failure
- [X] T075 [US3] Implement the residual-balance policy in `crates/qualification-driver/src/funding/residual.rs`, either sweeping back to the treasury or recording the accepted residue in the spend report
- [X] T076 [US3] Implement the funding summary in `crates/qualification-driver/src/funding/report.rs`: starting balance, amount spent, and projected remaining runs
- [X] T077 [US3] Expose treasury funding to the TypeScript driver in `packages/miden-multisig-client/tests/qualification/funding.ts`, reusing the Rust driver's funding subcommand rather than reimplementing the lifecycle
- [ ] T078 [US3] Assert no treasury key appears in any log or artifact, as part of the secret scan from T037

**Checkpoint**: live runs are self-funding on one network.

---

## Phase 6: User Story 4 - One command reproduces any qualification failure locally (Priority: P1)

**Goal**: A developer runs the same suite CI runs, with one documented command.

**Independent Test**: On a clean checkout with a container runtime, run the documented command and confirm the deterministic profile completes with no additional setup.

**Depends on**: Phase 2.

- [X] T079 [US4] Implement the full `qualification/stack/run.sh` option surface per `contracts/harness-cli.md`, including profile, network, image source, pairing, scenario and SDK selection
- [X] T080 [US4] Implement the five exit codes in `qualification/stack/run.sh`, keeping environment-blocked distinct from product failure so a schedule does not sit red through an upgrade window
- [X] T081 [US4] Implement scenario and dimension filtering in `qualification/stack/run.sh`, marking any filtered run as claiming no qualification
- [X] T082 [US4] Implement `--keep-stack` in `qualification/stack/run.sh`, printing the teardown command it skipped
- [X] T083 [US4] Ensure secrets reach the drivers by environment only and never as command arguments, so they stay out of the process table and shell history
- [ ] T084 [US4] Verify a cancelled run leaves no containers, volumes, databases or working directories behind, and add the check to `qualification/stack/lib/teardown.sh` tests

**Checkpoint**: the suite is reproducible locally.

---

## Phase 7: User Story 5 - Pull requests are gated on the assembled system (Priority: P2)

**Goal**: A deterministic profile proves the assembled system on every pull request, with no external dependency.

**Independent Test**: Run the deterministic profile against a freshly built image and confirm it passes; point it at a stale image and confirm the identity assertion fails.

**Depends on**: Phase 2. **Independent of** all live-profile stories.

- [X] T085 [US5] Implement the build-identity assertion in `crates/qualification-driver/src/scenario/identity.rs`, comparing against the ref in built mode and against the image digest and its recorded revision in pulled mode
- [X] T086 [P] [US5] Implement the fixture account and proposal flow over gRPC in `crates/qualification-driver/src/scenario/fixture_grpc.rs`, reusing the existing server test fixtures and their Falcon credential construction
- [X] T087 [P] [US5] Implement the same fixture flow over HTTP in `packages/miden-multisig-client/tests/qualification/fixtureHttp.ts` using the TypeScript base client
- [X] T088 [US5] Implement the structured-error assertion in `crates/qualification-driver/src/scenario/error_envelope.rs` against an error raised inside a service, keying on the stable code and never on message wording
- [X] T089 [P] [US5] Mirror the structured-error assertion in `packages/miden-multisig-client/tests/qualification/errorEnvelope.ts`, asserting the typed error surface exposes the same code
- [X] T090 [US5] Implement the restart-durability scenario in `qualification/stack/lib/restart.sh` plus `crates/qualification-driver/src/scenario/durability.rs`, restarting only the server and re-reading account, state and proposal data through both clients
- [ ] T091 [P] [US5] Add a discarded-delta visibility scenario in `crates/qualification-driver/src/scenario/discarded.rs`, asserting discarded deltas stay out of default retrieval flows
- [X] T092 [US5] Write `.github/workflows/qualification-deterministic.yml` running on every non-documentation pull request, on default-branch pushes, and on dispatch, using the newest pinned action generation
- [X] T093 [US5] Add the published-digest dispatch path to `.github/workflows/qualification-deterministic.yml`, so a newly published artifact is checked for assembly and durability before any live scenario spends treasury funds
- [ ] T094 [US5] Confirm the profile stays under the 15-minute budget on a standard runner, and record the measured time in `qualification/README.md`
- [ ] T095 [US5] Register the workflow as a required check and document the setting in `docs/CONTRIBUTING.md`

**Checkpoint**: merges are gated on the assembled system. This is the MVP.

---

## Phase 8: User Story 6 - Cross-SDK divergence is caught (Priority: P2)

**Goal**: Both SDKs run the same scenarios, their outcomes sit side by side, and divergence fails the run.

**Independent Test**: Introduce a deliberate convention difference on one SDK and confirm the parity assertion fails and names both sides.

**Depends on**: Phases 3 and 4.

- [ ] T096 [US6] Implement typed-outcome comparison in `crates/qualification-driver/src/report/parity.rs`, comparing proposal status, threshold accounting, structured error code and executability decision
- [ ] T097 [US6] Implement same-account commitment comparison in `crates/qualification-driver/src/report/parity.rs`, used only by scenarios that deliberately share an account
- [ ] T098 [US6] Implement the dependency-pin check in `crates/qualification-driver/src/preflight/pins.rs`, failing fast with a pin-mismatch outcome before any scenario runs
- [X] T099 [P] [US6] Implement the `handoff-rust-to-ts` action, creating an account and proposal in Rust and signing or executing in TypeScript
- [X] T100 [P] [US6] Implement the `handoff-ts-to-rust` action, the reverse direction
- [ ] T101 [US6] Render the side-by-side outcome view in `crates/qualification-driver/src/report/render.rs`, keyed on the shared scenario identifier
- [ ] T102 [US6] Verify the parity assertion fires: introduce a deliberate divergence, confirm failure names both sides, then revert

**Checkpoint**: parity is evidenced, not assumed.

---

## Phase 9: User Story 7 - Offline signing and guardian migration (Priority: P2)

**Goal**: Export, external signing, import and execution work, as does migrating an account to a different guardian.

**Independent Test**: Run the offline and migration scenarios alone and confirm they complete without the online execution scenarios.

**Depends on**: Phases 3 and 4.

- [X] T103 [P] [US7] Implement `proposal-export`, `proposal-sign-external` and `proposal-import` in `crates/qualification-driver/src/scenario/offline.rs`
- [X] T104 [P] [US7] Implement the same three actions in `packages/miden-multisig-client/tests/qualification/actions/offline.ts`
- [X] T105 [US7] Assert the imported signature counts toward the threshold and the proposal executes, in `crates/qualification-driver/src/assert/offline.rs`
- [ ] T106 [US7] Assert the offline path produces state equivalent to the online path for the same scenario
- [X] T107 [US7] Implement `proposal-create-offline` restricted to guardian migration, and assert that attempting it for another proposal type is rejected rather than treated as coverage
- [X] T108 [P] [US7] Implement `guardian-migrate` in `crates/qualification-driver/src/scenario/migrate.rs`, asserting both the old and new guardian report the expected ownership and that subsequent proposals execute against the new one
- [X] T109 [P] [US7] Implement `guardian-migrate` in `packages/miden-multisig-client/tests/qualification/actions/migrate.ts`

**Checkpoint**: the low-frequency, high-risk flows are covered.

---

## Phase 10: User Story 8 - Operator and dashboard APIs (Priority: P3)

**Goal**: The operator client is exercised against the real Postgres-backed server.

**Independent Test**: Run the operator scenarios against a stack with a seeded allowlist and confirm each assertion independently of the multisig scenarios.

**Depends on**: Phase 2.

- [X] T110 [P] [US8] Seed a deterministic operator key and allowlist in `qualification/stack/lib/operator.sh`
- [X] T111 [US8] Implement operator authentication and session inspection in `packages/miden-multisig-client/tests/qualification/operator/session.ts` using the operator client package
- [X] T112 [P] [US8] Implement account listing and account detail assertions in `packages/miden-multisig-client/tests/qualification/operator/accounts.ts`
- [X] T113 [P] [US8] Implement the permission-denial assertion in `packages/miden-multisig-client/tests/qualification/operator/denial.ts`, keyed on the structured error code
- [X] T114 [US8] Implement the allowlist hot-reload assertion in `packages/miden-multisig-client/tests/qualification/operator/allowlist.ts`, changing the allowlist without restarting the server
- [X] T115 [P] [US8] Implement logout and session invalidation in `packages/miden-multisig-client/tests/qualification/operator/logout.ts`
- [ ] T116 [US8] Implement the audit-persistence assertion in `crates/qualification-driver/src/scenario/audit.rs`, querying the database directly for the expected audit records
- [X] T117 [US8] Add the operator scenarios to the full set carried by pre-release and post-publication runs

**Checkpoint**: the operator surface is qualified.

---

## Phase 11: User Story 9 - Releases carry a recorded qualification result (Priority: P3)

**Goal**: A release carries a result recorded against the exact artifact, informing but not blocking the decision.

**Independent Test**: Trigger a pre-release run against a tagged ref and confirm the recorded result names the ref, the digest, the network and the per-scenario outcomes.

**Depends on**: Phases 3, 4 and 12.

- [ ] T118 [US9] Implement release-record emission in `crates/qualification-driver/src/report/release.rs`, keyed on ref and resolved digest, with one result per network and no merged verdict
- [ ] T119 [US9] Publish the result to a durable surface attached to the release, so it outlives the run logs
- [ ] T120 [US9] Implement the decision record in `crates/qualification-driver/src/report/release.rs`, naming who proceeded and what was outstanding, without amending the qualification result
- [ ] T121 [US9] Add the pre-release trigger to `.github/workflows/qualification-live.yml`, carrying the full set against both networks
- [ ] T122 [US9] Add a job to `.github/workflows/docker-publish.yml` that triggers qualification against the digest it just pushed, exposing the digest as a job output
- [ ] T123 [US9] Verify that renaming or moving `docker-publish.yml` is not required by the change, since the deployment workflow's attestation check pins that filename

**Checkpoint**: releases carry evidence.

---

## Phase 12: Published pairing (Cross-Cutting)

**Purpose**: Qualify what consumers actually install, not just what this repository builds.

**Depends on**: Phase 4.

- [ ] T124 Implement registry installation in `qualification/stack/lib/install-published.sh`, resolving packages fresh outside the repository workspace so path linking cannot substitute local code for published code
- [X] T125 Implement artifact-set recording in `crates/qualification-driver/src/report/artifacts.rs`: image digest, installed package versions, integrity hashes and resolved dependency versions
- [X] T126 Implement the image-and-package skew check in `crates/qualification-driver/src/preflight/skew.rs`, failing fast because the image and packages are released by separate pipelines and can diverge
- [X] T127 Implement consumer-workaround reporting in `crates/qualification-driver/src/report/findings.rs`, recording a finding against the artifact whenever the harness applies a workaround a consumer would also need
- [ ] T128 Add the published-pairing triggers to `.github/workflows/qualification-live.yml`: after publication, before a release, and on a weekly cadence

**Checkpoint**: the published artifact set is qualified.

---

## Phase 13: Polish and Cross-Cutting Concerns

- [X] T129 Write `.github/workflows/qualification-live.yml` covering the schedule (full set, both networks, branch pairing), dispatch with every dimension selectable, and the pull-request opt-in
- [X] T130 Implement the pull-request opt-in path so it resolves its workflow definition from the default branch, never runs for a fork, and records who requested it
- [X] T131 [P] Create the qualification GitHub Environments holding treasury secrets, separate from the existing deployment environments, and validate required variables at job start with an explicit error naming what is missing
- [X] T132 [P] Pin every third-party action in both new workflows to the newest generation in use, and request minimum permissions per job
- [ ] T133 Implement scheduled-failure notification naming the failing scenarios
- [ ] T134 [P] Implement the account-scheme policy precheck, reporting schemes the target server excludes as configuration-excluded rather than failed
- [X] T135 [P] Write the operator documentation in `docs/QUALIFICATION.md`: running each profile locally and in CI, interpreting each outcome class, and the treasury top-up and re-creation runbook
- [X] T136 [P] Add the qualification entry to the documentation table in `CONTRIBUTING.md` and the skills list in `CLAUDE.md`
- [ ] T137 Measure the scheduled full run against the 3-hour per-network target and record the result; if it does not fit, raise the window or add capacity rather than dropping scenarios
- [ ] T138 Verify the concurrency design: run two scheduled runs against one network simultaneously and confirm neither corrupts the other's treasury accounting

---

## Dependencies

```
Setup (P1) ──► Foundational (P2) ──┬──► US5 (Phase 7) ──► [MVP]
                                    ├──► US4 (Phase 6)
                                    ├──► US8 (Phase 10)
                                    └──► US3 (Phase 5) ──┬──► US1 (Phase 3) ──┐
                                                          └──► US2 (Phase 4) ──┤
                                                                                ├──► US6 (Phase 8)
                                                                                ├──► US7 (Phase 9)
                                                                                └──► Phase 12 ──► US9 (Phase 11)
```

- **US3 blocks US1 and US2.** Funding is the enabling constraint on both canaries.
- **US5 and US4 depend on nothing but the foundation.** This is why they ship first despite being P2 and P1 respectively.
- **US6 needs both canaries.** Parity cannot be asserted until both sides exist.
- **US9 needs Phase 12.** A release record names an artifact set, which the published pairing defines.

## Parallel opportunities

- **Phase 2**: the two manifest loaders (T009, T010), the two validation mirrors (T011, T012), all four unit-test tasks (T013, T014, T021, T022), and the stack library files (T026 to T029) are independent files.
- **Phase 3 and Phase 4 run in parallel** once US3 lands: different languages, different directories, no shared files.
- **Within Phase 4**: the five action modules (T055, T056, T058 to T061) are separate files under `scenario/`.
- **Phase 10 (US8) can run at any time after Phase 2**, in parallel with the live-profile work, since it touches neither the treasury nor a chain.
- **Phase 13**: documentation (T135, T136), workflow hygiene (T131, T132) and the scheme precheck (T134) are independent.

## Implementation strategy

**MVP is Phase 7 (US5) plus Phase 6 (US4)**, not User Story 1. Story priority ranks risk and the live TypeScript canary carries the most value at risk, but it depends on a treasury, two networks and an unattended runtime. The deterministic profile depends on none of those and produces a required, merge-blocking check that proves the assembled system. Shipping it first means the repository gains a real gate in the first slice instead of the fifth.

**Then follow the recommended build order table at the top.** Each slice is independently valuable: slice 2 gives self-funding accounts, slice 3 gives the first live canary, and so on. Stop after any slice and what exists still works.

**Two decisions stay open in plan.md** and should be settled before their tasks start: the treasury account shape (affects T065 to T071) and the publishing-side trigger mechanism (affects T122).
