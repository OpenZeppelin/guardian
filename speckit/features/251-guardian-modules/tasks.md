# Tasks: Guardian Modules, Phase P0 (Prototype)

**Input**: Design documents from `speckit/features/251-guardian-modules/`
**Prerequisites**: [plan.md](./plan.md), [spec.md](./spec.md), [research.md](./research.md), [data-model.md](./data-model.md), [contracts/](./contracts/), [quickstart.md](./quickstart.md)

**Scope**: phase P0 of the plan only. These tasks build enough of M2a to close the prototype gates G1 to G5 in [research.md](./research.md) and to try the feature end to end. M2a, M2b and M2c tasks are generated after the gates close and RFC revision 20 records the results.

**Branch**: the code lives on `251-modules-prototype` (draft PR #554, based on the RFC branch); `251-guardian-modules-rfc` (PR #544) stays doc-only.

**Excluded from P0**: the operator API (Story 5), `default` scope, evaluation budget, circuit breaker and health probe, request size cap, metrics, M2b (Story 7), M2c (Story 8), TypeScript parity, the execution path (PR #510 not merged).

**Prototype shortcuts** (not the M2a design): secrets are read from files (`token_file`, `tls.*_file`) instead of the secrets backend; the approval record lives in delta metadata instead of delta columns. Per-account assignments started in the catalog file and moved to the operator API with Phase 10 (T041).

**Tests**: included, because the spec's acceptance scenarios are the evidence that closes the gates (constitution principle V).

## Format: `[ID] [P?] [Story] Description`

- **[P]**: can run in parallel (different files, no dependency on an incomplete task)
- **[Story]**: user story from spec.md (US1, US2, US3, US4, US6)

---

## Phase 1: Setup

**Purpose**: the SDK crate and the server module skeleton.

- [X] T001 Create crate `crates/guardian-module-sdk` (`Cargo.toml` with workspace version, edition, rust-version, license; dependencies `tonic`, `tonic-prost`, `prost`, `async-trait`, `thiserror`, `serde_json`, `tokio`; build dependency `tonic-prost-build`) and add it to `members` and `default-members` in the root `Cargo.toml`
- [X] T002 Copy `speckit/features/251-guardian-modules/contracts/guardian_module_v1.proto` to `crates/guardian-module-sdk/proto/guardian_module_v1.proto` and add `crates/guardian-module-sdk/build.rs` compiling it with client and server stubs, following `crates/server/build.rs`
- [X] T003 Add `guardian-module-sdk` as a path dependency of `guardian-server` in `crates/server/Cargo.toml`, add the `toml` dependency, and create `crates/server/src/modules/mod.rs` declared from `crates/server/src/lib.rs`

---

## Phase 2: Foundational

**Purpose**: everything every story needs: the module interface, catalog, request assembly (gates G1 and G2), the pipeline and the errors.

**Checkpoint**: `cargo test -p guardian-module-sdk` and `cargo test -p guardian-server modules::` pass; a server started without `GUARDIAN_MODULES_CONFIG` behaves exactly as before.

- [X] T004 Define `Module` (`describe`, `validate_params`, `evaluate`, `notify`, `notify_gap` with default no-op), `ModuleFactory`, `ModuleError` and re-exports of the generated `guardian.module.v1` types in `crates/guardian-module-sdk/src/lib.rs`, using `async_trait` so `Arc<dyn Module>` is object safe
- [X] T005 [P] Implement catalog parsing and validation for `[[module]]` entries (`id`, `kind`, `endpoint`, `scope` limited to `all` and `assigned`, `hooks`, `timeout_ms`, `on_error`, `enabled`, `params`, `account_params`) and the prototype `[[assignment]]` table (`account_id`, ordered `modules` with `params`), with a catalog hash, in `crates/server/src/modules/catalog.rs`; unit tests for unknown kind, empty hooks, `account_params` outside `scope = "all"`, and an assignment naming an `all` instance
- [X] T006 [P] Implement the kind registry and instance creation (factory per kind, `describe` hook check, `validate_params` on `params` and each merged `account_params` entry, startup failure for an enabled instance) in `crates/server/src/modules/registry.rs`
- [X] T007 [P] Add `AccountView` extraction (gate G1): a `NetworkClient::account_view(&serde_json::Value) -> Result<AccountView, String>` method in `crates/server/src/network/mod.rs`, implemented in `crates/server/src/network/miden/mod.rs` with new `MidenAccountInspector` readers for the default threshold, the procedure thresholds map and the auth scheme in `crates/server/src/network/miden/account_inspector.rs`, returning an error (never an empty view) for unsupported or undecodable auth state; update test mocks in `crates/server/src/testing/mocks.rs`
- [X] T008 [P] Implement the `TxProjection` builder (gate G2) in `crates/server/src/modules/projection.rs`, reusing helpers from `crates/server/src/delta_summary/projection.rs` and producing every proto field (`nonce_delta`, `expiration_block`, vault changes, output notes with `partial`, `recipient_digest`, `script_root`, `p2id_target`, input notes, storage changes with create, update and remove); record each field it cannot derive in a `G2` section at the end of `speckit/features/251-guardian-modules/research.md`
- [X] T009 Implement `ProposalContext { proposer, metadata }` and a lookup returning `Result<Option<ProposalContext>, GuardianError>` that separates absence from storage failure, next to `lookup_matching_proposal_payload`, in `crates/server/src/modules/request.rs`
- [X] T010 Make filesystem proposal creation create-if-absent and give `update_delta_proposal` its own replace-if-present implementation in `crates/server/src/storage/filesystem.rs`; tests: a duplicate creation keeps the first proposer, and create, sign, reload keeps the cosigner signature
- [X] T011 Implement `HookRequest` assembly (`submitter`, `before` and `after` views, `TransactionSubject` with projection, raw summary, proposer and `unverified_metadata`, `RegistrationSubject` with `initial_state`; params as RFC 8785 canonical JSON with `account_params` merged), built only when at least one module will be called, in `crates/server/src/modules/request.rs`
- [X] T012 Implement the pipeline in `crates/server/src/modules/pipeline.rs`: resolve the ordered set for a hook (enabled `all` instances in catalog order, then the account's assignment), call each under its `timeout_ms`, stop on deny or on a failure of an `on_error = "deny"` instance, continue past a failure of an `on_error = "allow"` instance with a WARN log, reject a deny whose `reason_code` does not match `[a-z0-9_]{1,64}` as a failure
- [X] T013 Add `GuardianError::ModuleDenied { module, reason_code }` (403, `PERMISSION_DENIED`, code `GUARDIAN_MODULE_DENIED`, non-retryable) and `GuardianError::ModuleUnavailable { module }` (503, `UNAVAILABLE`, code `GUARDIAN_MODULE_UNAVAILABLE`, retryable) with `meta` and user messages in `crates/server/src/error.rs`, following `AccountPaused`
- [X] T014 Load the catalog from `GUARDIAN_MODULES_CONFIG` in `ServerBuilder::build()`, register the first-party kinds, and add `modules: Option<Arc<ModulePipeline>>` to `AppState` in `crates/server/src/builder/mod.rs` and `crates/server/src/builder/state.rs`; `None` when the variable is unset

---

## Phase 3: User Story 1, enforce a mandatory policy (P1)

**Goal**: an `all` instance of `basic-policy` refuses over-cap or off-list transactions on every approval path available on `main`.

**Independent Test**: quickstart steps "Propose a transfer of 500", "Propose one transaction sending 600 and 600", "Export a 5,000 transfer, sign offline, push directly".

- [X] T015 [P] [US1] Implement `basic-policy` transaction rules in `crates/server/src/modules/basic_policy.rs`: `max_amount_per_tx` as a map from faucet id to decimal amount over the gross amount sent per faucet across all output notes (inputs do not offset, fee excluded, unlisted faucets uncapped, NFTs ignored), `asset_allowlist`, `recipient_allowlist` denying partial and non-P2ID notes; `validate_params` rejecting unknown params; reason codes `amount_cap_exceeded`, `asset_not_allowed`, `recipient_not_allowed`
- [X] T016 [P] [US1] Unit tests for `basic-policy` in `crates/server/src/modules/basic_policy.rs`: one note under the cap, two 600 notes against a 1,000 cap, receiving 1,000 and sending 1,500, an uncapped faucet, an NFT, a partial note under a recipient allowlist, forged `unverified_metadata` not changing the decision
- [X] T017 [US1] Run the `before_transaction_approved` pipeline in `crates/server/src/services/push_delta.rs` after the pause check and before `state.ack.ack_delta`, with `submitter` from `resolved.signer_commitment`, `before` from the base state and `after` from the applied state, and the proposer from the `ProposalContext` lookup (a lookup error applies `on_error`)
- [X] T018 [US1] Service tests in `crates/server/src/services/push_delta.rs`: a denied push returns `ModuleDenied`, stores no candidate and returns no signature; an allowed push behaves as today; with `modules = None` every existing push test passes unchanged

---

## Phase 4: User Story 2, refusal before anyone signs (P1)

**Goal**: an instance subscribed to `before_proposal_created` refuses a proposal before it is stored.

**Independent Test**: quickstart step "Propose a transfer of 5,000": refused, proposal count unchanged.

- [X] T019 [US2] Run the `before_proposal_created` pipeline in `crates/server/src/services/push_delta_proposal.rs` after the proposer signature check and before `submit_delta_proposal`, with `before` the pinned tail and `after` the tail with the delta applied, `accounting_time` not stored
- [X] T020 [US2] Service tests in `crates/server/src/services/push_delta_proposal.rs`: a denied creation stores nothing and leaves the pending-proposal count unchanged; an account one below its limit can still create a valid proposal after a denial; an instance subscribed only to `before_transaction_approved` is not called at creation

---

## Phase 5: User Story 3, control which keys may sign (P1)

**Goal**: one signer rule enforced at registration, proposal creation and approval through the account views.

**Independent Test**: register with an unlisted signer, propose adding an unlisted signer, push an offline signer change; each is refused.

- [X] T021 [P] [US3] Add `signer_allowlist`, `signer_denylist` (mutually exclusive) and `signer_check` (`added` default, `all`) to `crates/server/src/modules/basic_policy.rs`, evaluated only from `before` and `after`, reason code `signer_not_allowed`; unit tests for added versus all, absent `before` at registration, and an extraction error applying `on_error`
- [X] T022 [US3] Run the `before_account_registered` pipeline in `crates/server/src/services/configure_account.rs` after the credential, signer set and commitment checks and before any write, with `submitter` from the verified credential, `after` from `initial_state`, and `before` from a `pull_state` of the stored state only for existing accounts; no pause or release check is added
- [X] T023 [US3] Service tests in `crates/server/src/services/configure_account.rs`: a denied new registration writes nothing; a denied reconfiguration leaves state, metadata, pause and release unchanged; an allowed reconfiguration of a paused account keeps it paused; an allowed re-onboarding of a released account reactivates it and a denied one leaves it released

---

## Phase 6: User Story 4, choose failure behaviour (P1, partial)

**Goal**: `on_error` and timeouts behave as specified. Budget, breaker and probe are out of P0.

**Independent Test**: stop a remote module configured fail-closed, then fail-open, and push.

- [X] T024 [US4] Pipeline tests in `crates/server/src/modules/pipeline.rs` with stub modules: a timeout of a fail-closed module returns `ModuleUnavailable`; a timeout of a fail-open module continues; a fail-open module that errors followed by a denying module is refused by the second; a malformed `reason_code` counts as a failure

---

## Phase 7: User Story 6, a remote module without rebuilding (P2)

**Goal**: `basic-policy` served over gRPC gives the same decisions as in process.

**Independent Test**: quickstart section 3.

- [X] T025 [P] [US6] Implement the `serve` harness in `crates/guardian-module-sdk/src/serve.rs`: wraps any `Module` in the generated gRPC service, binds a bearer token per `instance_id` and rejects a mismatch with `PERMISSION_DENIED`
- [X] T026 [P] [US6] Implement the `grpc` kind in `crates/server/src/modules/grpc_kind.rs`: tonic client per instance, bearer token from the instance's `auth` (a file path in P0), `describe` and `validate_params` at startup, transport errors and timeouts as module failures
- [X] T027 [US6] Move the `basic-policy` rule code into `crates/guardian-module-sdk/src/basic_policy.rs` so the server's in-process kind and the remote example share it, keeping `crates/server/src/modules/basic_policy.rs` as the factory
- [X] T028 [US6] Add the runnable example `crates/guardian-module-sdk/examples/basic-policy-serve.rs` taking `--listen` and `--token-file`
- [X] T029 [US6] Conformance tests in `crates/guardian-module-sdk/tests/conformance.rs`: the same requests evaluated in process and through the harness over loopback give identical decisions; evaluating a request twice leaves module state unchanged; a request authenticated for one instance and naming another is rejected

---

## Phase 8: Clients and demo (US1, US2, US3)

**Goal**: module refusals are typed in Rust and visible in `examples/demo`.

- [X] T030 [P] [US1] Add typed accessors for `GUARDIAN_MODULE_DENIED` (`module`, `reason_code`, not retryable) and `GUARDIAN_MODULE_UNAVAILABLE` (`module`, retryable) to `ClientError` in `crates/client/src/error.rs`, with tests parsing both from gRPC status details
- [X] T031 [US2] Surface both errors from proposal creation, push and registration in `crates/miden-multisig-client` without wrapping them into generic errors; test that a denied proposal creation exposes `module` and `reason_code`
- [X] T032 [US3] Print module refusals (instance, reason code, retryable or not) in `examples/demo` where proposal creation, execution and account registration report errors

---

## Phase 9: Gate closure

**Purpose**: turn the prototype into findings for RFC revision 20 and the M2a task list.

- [ ] T033 Run the quickstart walk-through (sections 1 to 3) against a local server with the filesystem backend and record each step's result in `speckit/features/251-guardian-modules/research.md` under each gate
- [X] T034 [P] Gate G1: test `account_view` on single-key, multisig and guardian-multisig fixture accounts and on an unsupported layout in `crates/server/src/network/miden/mod.rs`; record decode cost per call and the decision on `procedure_thresholds` in `research.md`
- [X] T035 [P] Gate G2: test the projection builder on one fixture per transaction kind (P2ID, P2IDE, consume notes, add and remove signer, change threshold, update procedure threshold, switch guardian) in `crates/server/src/modules/projection.rs`; record fields kept and dropped in `research.md`
- [X] T036 Gate G3: measure p50 and p99 of `push_delta` and `push_delta_proposal` with no catalog, one in-process `basic-policy`, and one remote `basic-policy` over loopback, including the share spent building views and projections, with a script in `crates/guardian-module-sdk/examples/latency_ab.rs` or the existing loadgen in `crates/server/bench/loadgen`; record results against SC-001 and SC-006 in `research.md`; done 2026-10-16 with the loadgen `state-push` scenario and `crates/server/examples/bench_server.rs`, results in `research.md` (G3 end to end); follow-up fixes: account view cache by state commitment and a single proposal read in `push_delta`
- [X] T037 Gate G4 and G5: record module author findings (trait shape, typed request helpers such as `added_signers()`, branching needed per hook) and pipeline placement on `main` versus `acknowledge_delta` in PR #510 in `research.md`
- [X] T038 Mark gates G1 to G5 closed or accepted in `research.md`, list the RFC changes they require for revision 20, and update the Status line of `plan.md`

---

## Phase 10: Layered parameters (RFC revision 21)

**Goal**: operator-managed params from the dashboard API: global params seeded from the catalog, account layers that only add restrictions, assignments, history and version. The catalog keeps instance registration only.

**Storage** (research D12): module state lives in its own tables, `module_instances`, `module_account_params` and `module_params_history`, on Postgres through a migration (plus the one-row `module_state_version` lock) and on the filesystem backend as one atomically replaced file, `<GUARDIAN_METADATA_PATH>/modules/module_state.json` (`GUARDIAN_MODULES_STATE` overrides the directory).

- [X] T039 [US5] Add `recipient_denylist` and `deny_unknown_recipients` to `crates/guardian-module-sdk/src/basic_policy.rs`, reason code `recipient_blocked`, with tests
- [X] T040 [US5] Implement the params store (global params with the seed rule, account layers, assignments, history, version, file persistence) in `crates/server/src/modules/params.rs`, with tests
- [X] T041 [US5] Remove `account_params` and the `[[assignment]]` table from `crates/server/src/modules/catalog.rs`; mark params holding secret references as file-managed
- [X] T042 [US5] Evaluate an `all` instance's account layer as a second pass in `crates/server/src/modules/pipeline.rs`; `layer` on `GuardianError::ModuleDenied` and in `meta`; validate stored global params at startup; tests for extension, no loosening and runtime global changes
- [X] T043 [US5] Implement the operator operations (validation, history, audit kinds `modules.params.global`, `modules.params.account`, `modules.assignment`) in `crates/server/src/modules/admin.rs`, `GuardianError::ModuleNotFound`, permission `policies:write_global`, with tests
- [X] T044 [US5] Add the routes in `crates/server/src/api/dashboard_modules.rs` and `crates/server/src/builder/handle.rs`, with a router test for permission gating, global writes, drift and history
- [X] T045 [US5] Carry `layer` through `ModuleRefusal` in `crates/client` and `MultisigError::ModuleDenied`
- [X] T046 [US5] Add the operator CLI `crates/server/examples/modules_admin.rs` (keygen, login, call)
- [X] T048 [US5] Replace the single-file params store with the `ModuleStore` trait over the three tables in `crates/server/src/modules/store.rs` (filesystem: one file per table), make evaluation read the account's rows with one query and cache global params per replica by params version, and fail closed with a storage error when the tables are unreadable
- [X] T049 [US5] Add the migration `crates/server/migrations/2026-10-15-000001_guardian_modules` and `PgModuleStore` in `crates/server/src/modules/postgres_store.rs` (each write and its history row in one transaction), wired through `CoordinationHandles`, with a Postgres test across two replicas
- [X] T050 [US5] Runtime enable and disable: `operator_can_disable` in the catalog, `set_enabled` on both stores (recorded in the history so the version moves), the pipeline and startup reading the stored flag, `POST /dashboard/modules/{id}/enable` (`policies:write`, re-runs the startup checks) and `/disable` (`policies:write` or `modules:disable` through a new any-of authz mode; `GUARDIAN_MODULE_NOT_DISABLEABLE` when the catalog forbids it), audit kinds `modules.enable` and `modules.disable`, with tests on every layer
- [X] T047 [US5] Walk the blocklist scenario against a running server with the CLI (quickstart section 4); confirmed live on 2026-10-15 (Postgres backend, devnet demo): global cap, a stricter account layer refusing with `layer: account`, a looser account layer still refused by the global rule, layer removal, and the params history

## Phase 11: Review fixes (fleet review 2026-10-15)

- [X] T051 Decode the projection and `raw_tx_summary` (Miden binary) from exactly the summary the Guardian signs: `NetworkClient::module_transaction` takes the bare summary, the proposal path passes `tx_summary`, the decoder refuses wrappers; `decode_transaction_summary` prefers a top-level `data` for the dashboard metadata
- [X] T052 Move the params version to the one-row `module_state_version` lock in the migration and `crates/server/src/modules/postgres_store.rs`; read the previous assignment inside the write transaction; Postgres test for concurrent replacements
- [X] T053 Check the effective params (stored global params, else the catalog seed) at startup and before enable, and seed new instances only after every instance passed its checks, in `crates/server/src/modules/pipeline.rs`, `registry.rs` and `admin.rs`
- [X] T054 Resolve a request's modules once with one version read (`ModulePipeline::plan`, `Plan::run`) and use it in the three services
- [X] T055 Record `catalog_hash` and `params_version` on the signed delta (`DeltaMetadata.modules`)
- [X] T056 Report the runtime flag for assigned modules in the account view
- [X] T057 Persist the filesystem module tables as one file, synced and renamed
- [X] T058 Single-key accounts: a view without signers and `default_threshold` from the state, the signer and scheme from the registered auth configuration
- [X] T059 TLS for remote modules: `grpc://` is TLS (system roots or `tls.ca_file`, optional client certificate), plain text only through `grpc+insecure://` to a loopback host, prod stage refuses plain text and requires `auth`
- [X] T060 Add the four `GUARDIAN_MODULE_*` codes to `packages/guardian-client/src/error-codes.ts`

## Phase 12: Replayable approvals

- [X] T061 Module builds (`name version` from `describe`) in the startup log and in `GET /dashboard/modules`
- [X] T062 Evaluation trace on the signed delta (`DeltaMetadata.modules`): `catalog_hash`, `params_version`, `failed_open`
- [ ] T063 Replay command: rebuild a delta's request at its `params_version`, re-run the modules and diff against the trace; adds what it needs: configuration snapshots with module builds, the request hash (ordered proto maps), `accounting_time`, `proposer` (M2a)
- [ ] T064 Decision log for denials and proposal-time evaluations, same trace shape (M2a)

## Phase 13: In-process custom kinds (M2c, prototyped early)

- [X] T065 `ServerBuilder::with_module_kind` registering a kind the catalog can name; `Registry::register` refuses a taken name, including `basic-policy` and `grpc`; example `crates/server/examples/custom_module_server.rs` with `note-policy` shared with the gRPC example
- [X] T066 Isolate every module call (`describe`, `validate_params`, `evaluate`) on its own task: a panic is a module failure and `on_error` applies, the timeout holds when a module blocks, a factory panic fails startup with an error
- [X] T067 Policy modules guide `docs/guides/policy-modules/` (built-in, remote, custom over gRPC and in process) with catalog templates loaded by a test; `CONFIGURATION.md` and `DASHBOARD.md` reference entries
- [ ] T068 `ServerBuilder::from_env` so a custom build does not copy `main.rs` (M2c)

---

## Dependencies & Execution Order

- **Setup (T001 to T003)** before everything.
- **Foundational (T004 to T014)** before any story. T005, T006, T007, T008 are parallel; T009 to T012 build on T004 and T007 to T008; T014 last.
- **US1 (T015 to T018)** first: it proves the pipeline. **US2 (T019, T020)** and **US3 (T021 to T023)** after T017, parallel with each other. **US4 (T024)** after T012. **US6 (T025 to T029)** after US1; T027 after T015.
- **Clients and demo (T030 to T032)** after T013; T031 and T032 after T019 and T022.
- **Gate closure (T033 to T038)** last; T034 and T035 can start as soon as T007 and T008 are done.

```text
T001 → T002 → T003 → T004 ─┬─ T005 ─┐
                           ├─ T006 ─┤
                           ├─ T007 ─┼─ T009 → T011 → T012 → T013 → T014
                           └─ T008 ─┘         T010 (parallel)
T014 → US1 (T015-T018) ─┬─ US2 (T019-T020) ─┐
                        ├─ US3 (T021-T023) ─┼─ T030-T032 → T033-T038
                        ├─ US4 (T024)       │
                        └─ US6 (T025-T029) ─┘
```

## Parallel Examples

```text
Foundational: T005 catalog.rs | T006 registry.rs | T007 network account_view | T008 projection.rs
US1:          T015 basic_policy.rs rules | T016 basic_policy.rs tests (same file: run T016 after T015 if editing together)
US6:          T025 serve.rs | T026 grpc_kind.rs
Gates:        T034 account_view fixtures | T035 projection fixtures
```

## Implementation Strategy

1. **MVP**: Setup, Foundational, US1. A mandatory cap refuses an over-cap push on `main`. Stop and run the US1 quickstart steps.
2. Add US2 and US3: the same instance filters proposals and controls signers at all three hooks.
3. Add US4 and US6: failure modes and the remote path.
4. Clients and demo, then gate closure. The findings drive RFC revision 20 and the M2a `tasks.md`.
