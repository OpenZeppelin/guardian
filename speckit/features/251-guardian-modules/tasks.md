# Tasks: Guardian Modules, Milestone M2a (stateless gates)

**Input**: Design documents from `speckit/features/251-guardian-modules/`; design source [RFC 0002](../../../docs/rfcs/0002-guardian-modules.md) revision 26, section 7 (M2a scope and the protocol version 1 freeze criteria)
**Prerequisites**: [plan.md](./plan.md), [spec.md](./spec.md), [research.md](./research.md), [data-model.md](./data-model.md), [contracts/](./contracts/), [quickstart.md](./quickstart.md)

**Starting point**: the prototype on `251-modules-prototype` (draft PR #554, merged with `main` at `73990a84`, RFC revision 26 work at `1274651c`). Its task record is [tasks-p0.md](./tasks-p0.md); tasks below that finish a prototype task cite it as "P0 Tnnn". Built and kept: the pipeline, catalog, registry, `grpc` and in-process kinds, `basic-policy`, the `serve` harness, all three gate hooks (approval in the shared `ack_delta_internal::acknowledge_applied`, so pushes and Guardian executions are both gated), the operator API routes on `/dashboard/modules` with layered params and history on both backends, policy discovery (`GET /policies`, `POST /delta/proposal/check`, rate limit), protocol negotiation and params migration, network-tagged projections, `signer_schemes`, and typed module errors in the server, both Rust crates and the TypeScript base client.

**Not in M2a**: M2b (outbox, `notify`, `pending`, resync: P0 T092, T096) and M2c (`ServerBuilder::from_env`: P0 T068; done on the same branch, Phase M2c at the end). Also excluded, per RFC section 7 future work: the durable decision log and replay command (P0 T063, T064), the protocol deprecation window until a version 2 exists (P0 T072), and `ModuleInfo.networks` until a second network (P0 T083).

**Tests**: included. The spec's acceptance scenarios and success criteria are the evidence for each story (constitution principle V), and the RFC freezes protocol version 1 only once its surface is implemented with tests.

**Code rule**: no spec or task IDs (`FR-`, `SC-`, `US`, `T0nn`) in source comments or test names (AGENTS.md, `speckit-implement`).

## Format: `[ID] [P?] [Story] Description`

- **[P]**: can run in parallel (different files, no dependency on an unfinished task)
- **[Story]**: the spec user story (US1 to US6) the task serves
- Paths are relative to the repository root

---

## Phase 1: Setup

**Purpose**: put the plan on the M2a footing and fix how the work is delivered.

- [X] T001 Update `speckit/features/251-guardian-modules/plan.md`: status M2a against RFC revision 26; replace the indicative source tree with the prototype's real paths (`crates/server/src/modules/{catalog,registry,pipeline,request,grpc_kind,admin,store,postgres_store,disclosure,check_limit}.rs`, `crates/server/src/network/miden/module_views.rs`, `crates/server/src/services/ack_delta_internal.rs`, `crates/guardian-module-sdk/src/{basic_policy,serve}.rs`) plus the files this list adds (`breaker.rs`, `budget.rs`, `metrics.rs`, `crates/guardian-module-sdk/src/conformance/`); record the delivery as stacked PRs off `main` per the "Delivery as stacked PRs" section below
- [ ] T002 Create the M2a integration branch `251-modules-m2a` from `main` and carry the prototype onto it by layer (the stack in "Delivery as stacked PRs"), each layer building and testing on its own; close or retarget draft PR #554 to point at the stack in its description. Local stack built 2026-11-09 (layers `251-m2a-sdk` to `251-m2a-docs`, top equal to the prototype tree); remaining: push the branches, open the layer PRs, update #554

---

## Phase 2: Foundational (protocol version 1 freeze)

**Purpose**: every field a module receives is fixed here. Modules written against version 1 must not change after this phase, so nothing that adds or renumbers request fields may be left for later stories.

**CRITICAL**: no user story phase starts until this phase is complete.

- [X] T003 Regenerate `crates/guardian-module-sdk/proto/guardian_module_v1.proto` from the RFC's renumbered version 1 block (`docs/rfcs/0002-guardian-modules.md`, section 3, matching `speckit/features/251-guardian-modules/contracts/guardian_module_v1.proto`, including `AccountView.signer_schemes = 8`), and update every constructor and test that sets field values in `crates/server/src/modules/request.rs`, `crates/server/src/network/miden/module_views.rs`, `crates/guardian-module-sdk/src/basic_policy.rs`, `crates/guardian-module-sdk/src/serve.rs` and `crates/guardian-module-sdk/tests/` (P0 T084)
- [X] T004 [P] Add `network_id` to `HookRequest` (and to `CommitEvent` and `Gap` in the proto only, unused until M2b), filled from the account's `network_config` in `crates/server/src/modules/request.rs`; test that a request for a testnet account names its network (P0 T086)
- [X] T005 [P] Add `AccountView.code_commitment` and `procedure_roots` in `crates/server/src/network/miden/module_views.rs`, and filter `procedure_thresholds` to effective overrides (non-zero, on a procedure the code exports, not the auth procedure); fixture tests in the same file with an inert override on a non-exported root and on the auth procedure (P0 T087)
- [X] T006 [P] Add `MidenTransaction.reference_block`, optional `expiration_block` (absent when `expiration_delta` is 0) and `new_code_commitment` (from the delta's code patch) in `projection_from_summary`, `crates/server/src/network/miden/module_views.rs`; tests for each, including a summary with no expiration and one that replaces the account's code (P0 T088)
- [X] T007 Add `HookRequest.submitter_session` from the resolved credential's auth format: pass it from `ResolvedAccount` through `ack_delta_internal::Submitter` (`crates/server/src/services/ack_delta_internal.rs`), `push_delta_proposal` and `configure_account` into `crates/server/src/modules/request.rs`; on the execution path carry it on `ExecutionJob` (`crates/server/src/services/execute_proposal/worker.rs`, `service.rs`) next to `requester`; tests for a wallet-signed and a session-signed push (P0 T093)
- [X] T008 Dropped 2026-11-09 (RFC revision 27): `proposer_session` removed from protocol version 1 instead of stored, since a session key cannot approve a proposal and no catalog policy uses it; version 1 can add it later
- [X] T009 Record `signed_at` (fixed before the pipeline runs and sent as `accounting_time`), `submitter` and `proposer` with the approval trace (`catalog_hash`, `params_version`, `failed_open`) in `ModuleApproval` (`crates/server/src/delta_summary/mod.rs`), written at signing in `crates/server/src/services/ack_delta_internal.rs`; the pipeline returns only the configuration it ran with (`Evaluation`, `crates/server/src/modules/pipeline.rs`). Revised 2026-11-09: kept in `metadata.modules` instead of new delta columns, since delta metadata is already stored on both backends and served only on HTTP, so no migration and no wire change; `data-model.md` updated
- [X] T010 [P] Build account views from the single decode the push path already does: carry the view fields on `AppliedState` (`crates/server/src/network/mod.rs`, `crates/server/src/network/miden/mod.rs` `verify_and_apply_delta`) and use them in `crates/server/src/modules/request.rs` instead of a second `Account::from_json`; run proposal creation through `verify_and_apply_delta` when a module will be called (P0 T089)
- [X] T011 Measure the frozen extraction against real account layouts, not only synthetic slots: views of accounts built from the upstream `miden-standards` components (single-key Falcon and ECDSA, 2-of-3 Falcon guarded multisig, mixed-scheme guarded multisig with a procedure override, an override on the auth procedure) asserted in `crates/server/src/network/miden/module_views.rs` (`standard_account_tests`); the code-replacement projection is covered by the `new_code_commitment` test. Revised 2026-11-09: built from the real components instead of captured from devnet, which needs funded accounts; a devnet capture remains for the T059 walk-through. Found: the `DELTA_1` fixture adds an approver key without its scheme entry, which the standard multisig always writes, so its resulting view is refused
- [X] T012 Mark protocol version 1 frozen: state it in the proto header of `crates/guardian-module-sdk/proto/guardian_module_v1.proto` and the contract copy, and record the freeze (commit and date) in RFC 0002 section 7 and Appendix C (`docs/rfcs/0002-guardian-modules.md`). Runs last among the freeze criteria (RFC 0002 section 7): after T019 and T020 (dry run with approval instances), T024 (lost claim) and T025 (`code_check`), which use fields already in the proto but must be implemented with tests before the freeze

**Checkpoint**: requests carry the complete version 1 surface, measured on real account layouts (T003 to T011). User story phases can start in parallel. T012, the freeze itself, waits for the behaviour-only freeze criteria in US2 and US3.

---

## Phase 3: User Story 1, enforce a mandatory policy on every account (P1)

**Goal**: an `all` instance refuses rule-breaking transactions on every path that reaches the ack, and a Guardian without modules is unchanged.

**Independent Test**: one `basic-policy` instance with a per-transaction cap; transfers below and above it through a proposal push, an offline push without a proposal, and a Guardian execution.

### Tests for User Story 1

- [X] T013 [P] [US1] Acceptance suite with a real `basic-policy` cap of 1,000: under the cap is acked; over the cap is refused with `GUARDIAN_MODULE_DENIED` (instance, reason code, layer) and nothing is stored, through a stored proposal and through an offline push without one; a proposal labelled `change_threshold` that moves funds is judged on its effect (`crates/server/src/services/push_delta.rs` tests); a Guardian execution over the cap ends `GUARDIAN_EXECUTION_MODULE_DENIED` with nothing proved or submitted (`crates/server/src/services/execute_proposal/tests.rs`). Placed next to each path's fixtures rather than in a separate file, which would duplicate them
- [X] T014 [P] [US1] No-modules regression: without a catalog a push builds no account view or projection and records no approval, while the same push with a module does (`MockNetworkClient::module_decodes`, `crates/server/src/services/push_delta.rs`). The p99 A/B against `main` is measured with the other configurations in T059, in one session

### Implementation for User Story 1

- [X] T015 [US1] `basic-policy` review fixes in `crates/guardian-module-sdk/src/basic_policy.rs`: a fixed `client_message` per reason code; a missing projection is a module error, not a `projection_missing` deny; tests for both (P0 T091)
- [X] T016 [US1] Denial log line with the fields of RFC section 2.2 (instance, layer, reason code, `request_id`, account, submitter) in `crates/server/src/modules/pipeline.rs`, with `request_id` defined as in RFC section 3.2; test that a generic denial still logs the full decision (P0 T091)

**Checkpoint**: SC-002 holds on all three approval paths and SC-001 has its baseline.

---

## Phase 4: User Story 2, learn about a refusal before anyone signs (P1)

**Goal**: refusals at creation store nothing and take no slot, and cosigners can check a proposal without storing it, in every client.

**Independent Test**: an instance on `before_proposal_created`; an over-cap proposal from each client is refused, nothing is stored and the slot count is unchanged; the dry-run check answers the same.

### Tests for User Story 2

- [X] T017 [P] [US2] Creation refusal through the Rust layers: `crates/client/src/testing/client_tests.rs` (typed `module_refusal()` on `push_delta_proposal`) and `crates/miden-multisig-client/src/client/proposals.rs` tests (`MultisigError::ModuleDenied` from proposal creation, slot count unchanged on the server mock)
- [X] T018 [P] [US2] Creation refusal through the TypeScript layers: `packages/guardian-client/src/http.test.ts` (`pushDeltaProposal` rejects with `moduleRefusal()` set) and `packages/miden-multisig-client/src/` proposal creation tests (refusal surfaces unchanged to the caller)

### Implementation for User Story 2

- [X] T019 [US2] Dry-run check runs `before_transaction_approved`-only instances after the creation instances, each instance once, every request with `HookRequest.dry_run = true`, in `crates/server/src/modules/pipeline.rs` (`plan_dry_run`) and `crates/server/src/services/push_delta_proposal.rs`; tests that an approval-only cap refuses the check (P0 T095)
- [X] T020 [US2] `complete` derived from the catalog, not the account: `false` whenever any enabled gate instance has `dry_run = false`, in `crates/server/src/modules/pipeline.rs`; test that two accounts, one with the hidden instance assigned and one without, get the same `complete` (P0 T090, T095)
- [X] T021 [US2] TypeScript multisig `getPolicies` and `checkTransaction` in `packages/miden-multisig-client/src/multisig.ts`, matching the Rust `MultisigClient::get_policies` and `ProposalBuilder::check`, with tests; the packages workspace links `@openzeppelin/guardian-client` locally, so no client release was needed; the check runs the real `create*Proposal` code on a short-lived view whose push becomes `checkDeltaProposal` (P0 T078)
- [X] T022 [US2] Document the dry-run check and policy listing in `crates/miden-multisig-client/README.md`, `packages/miden-multisig-client/README.md`, `crates/client/README.md` and `packages/guardian-client/README.md`

**Checkpoint**: SC-003 holds and every client exposes the check.

---

## Phase 5: User Story 3, control which keys may sign and which code may run (P1)

**Goal**: signer and code rules hold at registration, at signer or code changes, and offline; registration follows the account claim of #558.

**Independent Test**: a signer allowlist on all three hooks; register with an unlisted signer, propose adding one, push an offline signer change: each is refused. A code allowlist refuses registration and a code replacement to an unlisted commitment.

### Tests for User Story 3

- [X] T023 [P] [US3] Registration races in `crates/server/src/services/configure_account.rs` tests: a losing first configuration is evaluated again against the winner's assignment before it consumes anything; a denied first configuration claims nothing; a denied re-onboarding leaves released state, metadata and assignment unchanged; a mixed-scheme account's registration request carries `signer_schemes` for every signer

### Implementation for User Story 3

- [X] T024 [US3] Registration per RFC section 2.3 "Registration" in `crates/server/src/services/configure_account.rs`: for a lost claim, re-plan and re-run the pipeline against the winning account (unfinished claim or existing account) before `consume_auth_timestamp`; keep the hook before the claim (P0 T085)
- [X] T025 [US3] `basic-policy` `allowed_code_commitments` with `code_check` (`changed` or `all`, reason code `code_not_allowed`) in `crates/guardian-module-sdk/src/basic_policy.rs`, reading `AccountView.code_commitment` and `MidenTransaction.new_code_commitment`; params validation and tests at registration and at code replacement (P0 T093)
- [X] T026 [US3] Document the code allowlist and signer modes in `docs/guides/policy-modules/built-in.md` and its `built-in.toml`

**Checkpoint**: US3 acceptance scenarios 1 to 5 pass, and the code allowlist is in `basic-policy`.

---

## Phase 6: User Story 4, choose and observe failure behaviour (P1)

**Goal**: a slow or failing module costs at most its budget, an open circuit refuses or proceeds at once, and operators see per-instance health.

**Independent Test**: one fail-closed and one fail-open instance; stop each module and observe approvals, errors, latency and metrics.

### Tests for User Story 4

- [X] T027 [P] [US4] Failure acceptance in `crates/server/src/modules/pipeline.rs` tests: fail-closed unreachable refuses retryably; fail-open unreachable proceeds and is counted; consecutive failures open the circuit and later requests do not wait the timeout; combined time over the budget times out the remaining instances; a fail-open failure followed by a deny is refused by the second

### Implementation for User Story 4

- [X] T028 [US4] Evaluation budget `GUARDIAN_MODULE_EVALUATION_BUDGET_MS` (default 1000) in a new `crates/server/src/modules/budget.rs`, enforced per hook invocation in `crates/server/src/modules/pipeline.rs` (remaining instances count as timed out and their `on_error` applies)
- [X] T029 [US4] Startup budget checks in `crates/server/src/modules/catalog.rs`: in the prod stage refuse to start when, for any hook, the timeouts of `all` and `default` instances (enabled or not) exceed the budget, or the budget is not below the execution reservation lease; report over-budget stored assignments without blocking
- [X] T030 [US4] Budget check on operator writes in `crates/server/src/modules/admin.rs`: refuse `PUT /dashboard/accounts/{account_id}/modules` and account-layer writes whose effective set exceeds the budget on any hook (`invalid_input`), with tests
- [X] T031 [US4] Circuit breaker per instance in a new `crates/server/src/modules/breaker.rs` (consecutive-failure threshold, open, half-open probe), consulted in `crates/server/src/modules/pipeline.rs`; an open circuit applies `on_error` without calling the module
- [X] T032 [US4] Periodic health probe (`Describe`) per instance on each replica in `crates/server/src/modules/breaker.rs`, closing the circuit on success and detecting protocol or hook changes; wire its task in `crates/server/src/builder/mod.rs` with shutdown
- [X] T033 [US4] Request size cap `GUARDIAN_MODULE_REQUEST_MAX_BYTES` (default 2 MiB) in `crates/server/src/modules/request.rs`: an oversized request is a module failure without the call Revised 2026-11-10: an oversized request, and one that cannot be built, is refused whatever `on_error` says (RFC 0002 section 4.2).
- [X] T034 [US4] Module metrics in a new `crates/server/src/modules/metrics.rs` with names in `crates/server/src/metrics/names.rs`: `guardian_module_evaluate_duration_seconds{module, outcome}` (`allow`, `deny`, `error`, `skipped`), `guardian_module_fail_open_total{module}`, availability, circuit state, disabled state, configuration drift, catalog fingerprint, orphaned and over-budget assignment counts
- [X] T035 [US4] Catalog fingerprint in `GET /dashboard/info` (`crates/server/src/services/dashboard_info.rs`) and the `circuit` field in `GET /dashboard/modules` (`crates/server/src/modules/admin.rs`); regenerate `docs/openapi*.json` with `cargo run --features evm --bin gen-openapi -- docs`
- [X] T036 [US4] Document the budget, size cap and breaker variables in `docs/CONFIGURATION.md` ("Runtime: policy modules") and the metrics in `docs/guides/observability/`; the metric rows in the taxonomy of `spec/api.md` ("Metrics Endpoint")

**Checkpoint**: US4 acceptance scenarios 1 to 7 pass and SC-007 holds.

---

## Phase 7: User Story 5, tune policy per account without a deploy (P2)

**Goal**: `default` modules reach new accounts with the claim, operator writes are bounded and auditable, and the operator client covers every route.

**Independent Test**: extend a global blocklist and one account's layer through `guardian-operator-client`, read the history, try to remove a mandatory module, and register a new account with a `default` instance.

### Tests for User Story 5

- [X] T037 [P] [US5] `default` copy tests in `crates/server/src/services/configure_account.rs`: the copy carries the evaluated params and is written with the claim on Postgres; on the filesystem it replaces an earlier unclaimed attempt's copy; an unfinished claim with no assignment history repeats it, one an operator emptied does not
- [X] T038 [P] [US5] Operator client tests in `packages/guardian-operator-client/src/` for every module route, including permission errors for `modules:disable` holders

### Implementation for User Story 5

- [X] T039 [US5] Add the `default` scope, which the prototype never built (`Scope` in `crates/server/src/modules/catalog.rs` has only `all` and `assigned`): parse `scope = "default"`, plan it as an account-owned assignment like `assigned`, count it in `ModulePipeline::worst_case` and the startup budget check, and allow removing it per account; then write `default` instances into the assignment together with the claim in `crates/server/src/services/configure_account.rs` and `crates/server/src/modules/postgres_store.rs` (same transaction as `metadata.create`), with the evaluated params from the same snapshot (P0 T085)
- [X] T040 [US5] Superseded by the T039 design (RFC revision 29): the copy is written after the claim on both backends through `ModuleStore::seed_assignment`, only into an account with no assignment and no history; the unfinished-claim retry repeats a copy a crash interrupted (P0 T094)
- [X] T041 [US5] `params_drift` against `seeded_params`, `GUARDIAN_MODULE_PARAMS_MAX_BYTES` on every params write, and paginated history (`?before=&limit=`) in `crates/server/src/modules/admin.rs`, `store.rs` and `postgres_store.rs` (P0 T090)
- [X] T042 [US5] `orphaned` on account view rows naming an instance the catalog no longer declares (skipped at evaluation, counted at startup) and an `undeclared` list of such stored instances in `GET /dashboard/modules` (contract revised from a per-instance `declared` flag) in `crates/server/src/modules/admin.rs`
- [X] T043 [US5] Dropped: RFC 0002 section 3.5 ("Parameter migration") has account layers and assignments migrate when they are next written, and requires modules to keep accepting their previous shapes until then; only global params migrate at startup and before enabling, which the prototype does (P0 T073)
- [X] T044 [US5] Module routes in `packages/guardian-operator-client/src/` (list, account modules read and replace, global params, account layer set and delete, history, enable, disable) with types matching `speckit/features/251-guardian-modules/contracts/operator-modules-api.md`, and its README
- [X] T045 [US5] OpenAPI entries for the `/dashboard/modules` and account module routes (`utoipa` annotations in `crates/server/src/api/dashboard_modules.rs`), regenerated into `docs/openapi-dashboard.json`
- [X] T046 [US5] Document the routes, permissions, limits, `undeclared`, `orphaned` and history paging in `docs/DASHBOARD.md` ("Policy modules"); the browser smoke of these routes through `examples/operator-smoke-web` runs with the other smokes in T058

**Checkpoint**: US5 acceptance scenarios 1 to 16 pass and SC-009, SC-012 hold.

---

## Phase 8: User Story 6, plug in a third-party module without rebuilding (P2)

**Goal**: an operator runs the official image with a remote module that authenticates per instance through the secrets backend, and module authors have an SDK, a conformance suite and an example.

**Independent Test**: run `examples/policy-module` as a service, configure it as a remote instance with credentials from the secrets backend, and run the conformance suite against it and against the in-process kind.

### Tests for User Story 6

- [X] T047 [P] [US6] Conformance suite: turn the prototype's `crates/guardian-module-sdk/tests/conformance.rs` into a reusable suite in `crates/guardian-module-sdk/src/conformance.rs` (one file; it also checks `Describe`, decision shape and that one instance's token is refused for another) (behind a `conformance` feature) that any module author can run, covering identical decisions in process and over gRPC, `evaluate` purity (the same request repeated leaves module state unchanged), params validation, a request at the size cap, and instance-id isolation between two instances on one service; run it for `basic-policy` from `crates/guardian-module-sdk/tests/conformance.rs`

### Implementation for User Story 6

- [X] T048 [US6] Secret references through the secrets backend: replace `auth.token_file` and `tls.*_file` in `crates/server/src/modules/catalog.rs` and `crates/server/src/modules/grpc_kind.rs` with secret references resolved when the instance is created, following `docs/runbooks/secrets.md`; resolve `$secret` in in-process params at creation and pass them only to the in-process module, never in a request; keep rejection in remote params, account layers and the operator API; update `speckit/features/251-guardian-modules/contracts/catalog-config.md`
- [X] T049 [US6] `examples/policy-module/`: `basic-policy` as a standalone gRPC service through the `serve` harness, with `Cargo.toml`, `README.md` (run, TLS, per-instance token), a `Dockerfile`, and a matching remote catalog entry in `docs/guides/policy-modules/remote.toml`
- [X] T050 [US6] Publishable `guardian-module-sdk`: crates.io metadata in `crates/guardian-module-sdk/Cargo.toml`, `crates/guardian-module-sdk/README.md` (module author quick start), and the crate added to the release order of the `release-guardian-sdk-packages` skill
- [X] T051 [US6] Remote module author guide in `docs/guides/policy-modules/custom.md` and `remote.md`: protocol version 1, request fields, `reason_code` rules, `client_message`, `dry_run`, conformance suite, credentials; written so SC-011 holds without reading server code

**Checkpoint**: SC-005 and SC-011 hold; US6 acceptance scenarios 1 to 5 pass.

---

## Phase 9: Polish and cross-cutting

**Purpose**: documentation, verification and release readiness across stories.

- [X] T052 [P] `docs/TROUBLESHOOTING.md`: `GUARDIAN_MODULE_DENIED`, `GUARDIAN_MODULE_UNAVAILABLE`, `GUARDIAN_MODULE_NOT_FOUND`, `GUARDIAN_MODULE_NOT_DISABLEABLE` with their `meta`, and the denial log fields
- [X] T053 [P] Break-glass runbook `docs/runbooks/policy-modules.md` (RFC section 4.2): disabling an instance, deploy-only changes, circuit and budget symptoms; link it from `docs/runbooks/` and `CLAUDE.md`'s lookup table
- [X] T054 [P] Trust boundary and data protection in `docs/CONCEPTS.md`: what external modules receive (full transaction details, proposals that never execute, proposer descriptions), and the operator and vendor guidance of RFC invariant 5
- [X] T055 [P] Protocol spec coverage: module error codes and `meta` on proposal creation, push and registration, `GET /policies` and `POST /delta/proposal/check` in `spec/api.md`, and the gate points in the push, proposal and registration processes of `spec/processes.md`
- [X] T056 [P] Client READMEs for module errors and `moduleRefusal()` / `module_refusal()`: `crates/client/README.md`, `packages/guardian-client/README.md`, `crates/miden-multisig-client/README.md`, `packages/miden-multisig-client/README.md`
- [X] T057 Rust and TypeScript parity fixture: one shared JSON fixture of module error bodies under `packages/guardian-client/src/` (or the repo's existing shared fixture location) driving both `crates/client` and `packages/guardian-client` tests, so SC-010 is checked by the same cases
- [ ] T058 (operator module routes done 2026-11-09 through `modules_admin`: validation, `$secret` refusal, `modules:disable` allowed and the 403/409/404 paths, history; the SDK, demo/web and browser smokes still open) End-to-end smokes with a catalog: `smoke-test-rust-multisig-sdk` and `smoke-test-ts-multisig-sdk` skills, refusal at creation, push and registration, and `examples/demo` and `examples/web` with and without a catalog (quickstart section 5); and the operator module routes, including `modules:disable` permission errors, through `examples/operator-smoke-web` with the `smoke-test-operator-dashboard` skill
- [ ] T059 (latency A/B done 2026-11-09, see research.md "M2a end to end"; walk-through against a 0.17 local node still open) Quickstart walk-through against a local node with the filesystem backend and the latency A/B (zero, one in-process, one remote module), results in `speckit/features/251-guardian-modules/research.md` (P0 T033; SC-001, SC-006)
- [ ] T060 SC-004 run: enable the built-in policies (`spending-limit`, `recipient-policy`, `signer-policy`, `code-policy`; `basic-policy` until 2026-11-10) on all accounts with the official image and the guides only, timed, gaps fixed in `docs/guides/policy-modules/`
- [X] T061 Re-check the TLA+ models whose fingerprints the modules work moved (`verify-auth-replay`, `verify-candidate-queue`, `verify-client-sync`, `verify-guardian-switch`, `verify-module-counting`, `verify-multisig-push-gate`, `verify-proposal-lifecycle` under `.agents/skills/`) with each `check.sh`, then `--accept-drift` once the model still mirrors the code
- [X] T062 (2026-11-09: checks, OpenAPI, 1,729 server tests with --test-threads 4, 96 Postgres tests, Rust clients, TS 172/118/1001, terraform validate) Validation matrix of `speckit/features/251-guardian-modules/quickstart.md` section 5 (M2a rows) run end to end, including `cargo test -p guardian-server --features postgres` for module stores and proposal storage, and `gen-openapi --check`
- [X] T063 Export the `guardian_module_*` metrics: add them to the CloudWatch export allowlist in `infra/observability.tf` (selector groups by label dimensions) and a "Policy modules" section of panels in `docs/guides/observability/grafana/dashboards/guardian.json`, listed in `docs/guides/observability/README.md`; validate with `terraform validate` and the local Prometheus and Grafana stack

---

## Dependencies and Execution Order

### Phase dependencies

- **Setup (T001, T002)** first.
- **Foundational (T003 to T011)** blocks every story; T012 (the freeze) follows the freeze criteria in US2 and US3. T003 first (it renumbers the proto); T004, T005, T006 and T010 then run in parallel; T007 and T008 after T003; T009 after T007 (it stores `submitter` and `proposer`); T011 after T005 and T006; T012 after T019, T020, T024 and T025.
- **Stories** start after T011 and are independent of each other, with these exceptions: T030 (budget on writes) needs T028; T039 and T040 (`default` copy) touch `configure_account.rs` after T024, so run US3 before US5 or coordinate the file; T021 needs the `guardian-client` dependency decision.
- **Polish (T052 to T062)** after the stories it documents; T061 and T062 last.

### User story dependencies

- **US1 (P1)**: needs only Foundational. MVP.
- **US2 (P1)**: needs Foundational; T021 depends on a TypeScript client release or link.
- **US3 (P1)**: needs Foundational (T005, T006 for the code allowlist).
- **US4 (P1)**: needs Foundational; should land before any production enablement, since without the breaker and budget a down fail-closed module costs a full timeout on every request.
- **US5 (P2)**: needs Foundational; shares `configure_account.rs` with US3.
- **US6 (P2)**: needs Foundational (T003 for the conformance suite's request shapes).

### Within each story

Tests first and failing, then implementation, then docs for that story.

---

## Parallel Examples

```text
# Foundational, after T003:
T004 network_id            (modules/request.rs)
T005 code fields in views  (network/miden/module_views.rs, view half)
T006 projection fields     (network/miden/module_views.rs, projection half; coordinate with T005)
T010 single decode         (network/mod.rs, network/miden/mod.rs)

# Stories, after T011, one developer each:
US4  T028 to T036  (modules/budget.rs, breaker.rs, metrics.rs)
US6  T047 to T051  (guardian-module-sdk, examples/policy-module)
US2  T019 to T022  (pipeline dry run, TypeScript multisig)

# Polish docs, any time after their story:
T052, T053, T054, T055, T056
```

---

## Implementation Strategy

### MVP

Foundational, then US1: the frozen protocol and a mandatory policy enforced on every approval path. Before enabling modules in production, also US4: the budget and breaker are what keep a failing fail-closed module from slowing every approval.

### Incremental delivery

1. Foundational: version 1 frozen.
2. US1 and US4: enforceable and operable.
3. US3: signer and code control, registration per #558.
4. US2: creation-time refusal and the check in every client.
5. US5: operator tuning with the operator client.
6. US6: remote modules through the secrets backend, conformance suite, example, published SDK.
7. Polish: docs, smokes, A/B, TLA+ re-check, validation matrix.

### Delivery as stacked PRs

Decision 2026-11-09: all M2a work lands on `251-modules-m2a`; layer branches are cut from it when review needs them, following the recipe below.


The prototype is one 13,000-line draft (#554). M2a ships as a stack of five PRs off `main`, each building and testing on its own, the way the PR #510 review stack was layered by path. The layers, their branches and what each contains are in [plan.md](./plan.md), "M2a delivery": module SDK, server core, hooks, clients, docs. New M2a work goes to the layer it belongs to: Phase 2 proto tasks and US6 to the SDK layer; US4 and the operator routes of US5 to server core; US1, US3 and the server half of US2 to hooks; the client halves of US2 and US5 to clients; Polish docs to docs.

---

## Phase M2c: operator-built kinds (US8)

**Purpose**: RFC 0002 section 7, M2c: a custom build registers its own kinds without copying the official setup, from a per-release template pinned to the release lockfile. Carried on `251-modules-m2a`. `ServerBuilder::with_module_kind` already shipped with the prototype (P0 T065).

- [X] T100 [US8] `ServerBuilder::from_env()` in `crates/server/src/builder/env.rs` with the official binary's wiring; `crates/server/src/main.rs` reduced to `from_env` and `build`; `build` and `from_env` return `BuildError` (a `std::error::Error`); integration test `crates/server/tests/from_env.rs`
- [X] T101 [US8] `examples/custom-guardian` (workspace member, `postgres` feature, `Dockerfile`, README) registering `note-policy`; replaces `crates/server/examples/custom_module_server.rs`
- [X] T102 [US8] `scripts/custom-guardian-template.sh`: template with both Guardian crates at the release tag and the release `Cargo.lock`, refusing a template whose lockfile moves any non-Guardian version
- [X] T103 [US8] `.github/workflows/custom-guardian-template.yml`: on release, build the template, `docker build` it with `--locked`, attach `custom-guardian-<tag>.tar.gz`
- [X] T104 [P] [US8] Docs: in-process path of the module author guide (`docs/guides/policy-modules/custom.md`), RFC 0002 revision 33, `crates/server/README.md`, `docs/MIDEN_COMPATIBILITY.md`
