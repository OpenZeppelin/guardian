# Implementation Plan: Guardian Modules, a Hook Pipeline for Pluggable Policy

**Branch**: M2a on `251-modules-m2a` and its stacked layer branches (see "M2a delivery"); prototype on `251-modules-prototype` (draft PR #554); RFC and speckit merged with PR #544 | **Date**: 2026-10-13, M2a revision 2026-11-09 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `speckit/features/251-guardian-modules/spec.md`; design source [RFC 0002](../../../docs/rfcs/0002-guardian-modules.md), written against revision 19; M2a follows revision 26
**Status**: Prototype built (2026-10-13), gates G1 to G5 closed at component level ([research.md](./research.md#prototype-findings-2026-10-13)); RFC revisions 20 (prototype findings) and 21 (layered parameters, decision D11) applied. Still open: the end-to-end walk-through and latency A/B against a local Miden node. M2a implementation started 2026-11-09 against RFC revision 26: [tasks.md](./tasks.md); the prototype record is [tasks-p0.md](./tasks-p0.md).

## Summary

Add a hook pipeline to the Guardian: at proposal creation, transaction approval and account registration, an ordered set of configured module instances answers allow or deny, and a denial refuses the Guardian's approval (or the proposal, or the registration). Modules are first-party in-process kinds, remote gRPC services, or (M2c) operator-built kinds. M2b adds commit notifications and in-flight accounting so stateful limits are exact.

Technical approach: a new published crate `guardian-module-sdk` (traits, generated protocol types, `serve` harness, conformance suite); in `guardian-server`, a `modules` subsystem (catalog, registry, pipeline, request assembly, failure handling) wired in `ServerBuilder::build()` and invoked from the single ack function, from `push_delta_proposal`, and from `configure_account`; runtime state (assignments, instance flags) in the metadata store; module errors through the existing error envelope to every client layer; operator endpoints on `/dashboard/*`.

## Technical Context

**Language/Version**: Rust 1.98.1, edition 2024 (`rust-toolchain.toml`, workspace `rust-version`); TypeScript for client packages
**Primary Dependencies**: tonic 0.14 and tonic-prost-build (module protocol, `serve` harness, `grpc` kind), axum 0.8 (operator API), diesel and diesel-async (Postgres), `serde_json` and a new `toml` dependency (catalog, params, RFC 8785 canonical params), miden-protocol 0.17 (projection and account views, server side only)
**Storage**: metadata store on both backends for assignments and instance flags; delta rows gain `module_catalog_hash`, `signed_at`, `submitter`, `proposer`; Postgres only for the M2b outbox and delivery state (`module_events`, `module_delivery`)
**Testing**: `cargo test` per crate, the SDK conformance suite, Postgres-gated tests for M2b, `npm test` for TS packages, the multisig smoke skills, qualification scenarios for M2b
**Target Platform**: Linux server (official Docker image, ECS); module services anywhere reachable over TLS gRPC
**Project Type**: Rust server plus libraries and SDKs in a multi-language monorepo
**Performance Goals**: no measurable change with no modules (SC-001); at most 10 ms p99 added by one in-process module (SC-006, **prototype gate G3**)
**Constraints**: evaluation budget 1,000 ms per hook invocation; request cap 2 MiB; sequential evaluation; side-effect-free `evaluate`; protocol version 1 frozen at M2a; account view extraction must fail rather than return an empty view (**gate G1**)
**Scale/Scope**: a handful of instances per deployment, at most tens per account; projection and view built once per request and only when a module will be called (**gates G1, G2**)

No item is NEEDS CLARIFICATION: open questions are tracked as prototype gates with assumed defaults.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-checked after Phase 1 design.*

| Principle or invariant | Assessment | Status |
|---|---|---|
| I. Bottom-up propagation | Server changes (new errors, new hook, operator endpoints) are carried to `crates/client`, `packages/guardian-client`, both multisig SDKs, `packages/guardian-operator-client`, `examples/demo` and `examples/smoke-web` in the same slice ([contracts/client-errors.md](./contracts/client-errors.md)) | Pass |
| II. Transport and cross-language parity | Module errors identical on HTTP and gRPC; Rust and TS clients classify them identically (SC-010). The module protocol itself is gRPC-only until the `http` kind ships; that is the module transport, not a client surface, and is documented in the RFC | Pass |
| III. Append-only integrity, explicit lifecycles | A denial at approval stores no candidate; at creation stores no proposal; reconfiguration denial changes nothing. No new delta or proposal status; new delta columns are written once at signing. Resync is an explicit, audited operator transition | Pass |
| IV. Explicit authentication, stable boundary errors | `submitter` and `proposer` come only from request authentication; two new boundary errors with fixed codes and `meta`; tests in the server and at least one upstream consumer per layer | Pass |
| V. Evidence-driven delivery | Eight independently testable stories; validation matrix in [quickstart.md](./quickstart.md); docs listed per slice | Pass |
| Invariant: backend parity | Gate hooks, assignments, flags and the first-creator proposal rule behave the same on both backends (this feature *fixes* a current divergence). Accepted backend-specific limitation: the M2b outbox, `pending` and notify exist only on Postgres; a catalog subscribing to `after_transaction_committed` refuses to start on the filesystem backend. Documented in the RFC (section 3.3) and spec Assumptions | Pass, limitation accepted |
| Invariant: local work defaults to filesystem | M2a tests and the quickstart run on filesystem; only M2b tests require Postgres | Pass |
| Invariant: deterministic proposal ids, proposals deleted on canonical | Unchanged | Pass |
| Invariant: discarded deltas hidden | Unchanged | Pass |

Post-design re-check (after Phase 1): unchanged; no Complexity Tracking entries needed.

## Project Structure

### Documentation (this feature)

```text
speckit/features/251-guardian-modules/
├── spec.md
├── plan.md                         # this file
├── research.md                     # prototype gates G1-G5, decisions D1-D10
├── data-model.md
├── quickstart.md
├── contracts/
│   ├── guardian_module_v1.proto    # extracted from RFC section 3
│   ├── catalog-config.md
│   ├── operator-modules-api.md
│   └── client-errors.md
├── checklists/requirements.md
├── tasks-p0.md                     # P0 prototype tasks and the follow-on phases (record)
└── tasks.md                        # M2a implementation tasks
```

### Source Code (repository root)

```text
crates/guardian-module-sdk/                    # published crate (M2a)
├── proto/guardian_module_v1.proto             # protocol version 1, frozen at the end of M2a phase 2
├── build.rs                                   # tonic-prost-build
├── src/
│   ├── lib.rs                                 # Module, ModuleFactory, ModuleError, protocol types, request helpers
│   ├── serve.rs                               # gRPC harness, per-instance credentials
│   ├── basic_policy.rs                        # first-party reference kind
│   ├── conformance.rs                         # reusable suite, `conformance` feature
│   └── stateful.rs                            # M2b: watermark helper
├── examples/note-policy-serve.rs
└── tests/conformance.rs

crates/server/src/
├── modules/
│   ├── mod.rs                                 # loading at startup, wiring into AppState
│   ├── catalog.rs                             # TOML load, validation, hash, startup checks
│   ├── registry.rs                            # kinds, instance creation, describe and params checks
│   ├── pipeline.rs                            # ordered evaluation, layers, first deny, dry run
│   ├── request.rs                             # HookRequest assembly, views, ProposalContext
│   ├── grpc_kind.rs                           # remote kind client, TLS, auth
│   ├── disclosure.rs, check_limit.rs          # policy discovery and the dry-run rate limit
│   ├── admin.rs                               # operator views and writes
│   ├── store.rs, postgres_store.rs            # module state on the filesystem and Postgres
│   ├── testing.rs                             # stub modules and pipelines for tests
│   ├── budget.rs, breaker.rs, metrics.rs      # M2a: evaluation budget, circuit breaker and probe, metrics
│   └── notify/                                # M2b: outbox writer, notifier, delivery, resync
├── network/miden/module_views.rs              # AccountView extraction and MidenTransaction projection
├── services/ack_delta_internal.rs             # before_transaction_approved, shared by push_delta and the execution worker
├── services/push_delta_proposal.rs            # before_proposal_created and the dry-run check
├── services/configure_account.rs              # before_account_registered, before the account claim
├── services/get_policies.rs                   # policy summary
├── services/execute_proposal/worker.rs        # requester carried to the ack; module failure codes
├── api/dashboard_modules.rs                   # operator routes on /dashboard/modules
├── dashboard/{authz,permissions}.rs           # policies:write, policies:write_global, modules:disable
├── error.rs                                   # GUARDIAN_MODULE_* errors
└── builder/                                   # with_module_kind (M2c, prototyped), catalog in build()

crates/server/migrations/2026-10-15-000001_guardian_modules/   # module instances, account params, version, history
crates/server/migrations/<date>_guardian_module_delta_fields/  # M2a: approval trace, signed_at, submitter, proposer
crates/server/migrations/<date>_guardian_module_events/        # M2b: outbox and delivery
crates/server/examples/modules_admin.rs, custom_module_server.rs

crates/shared/src/execution.rs                 # GUARDIAN_EXECUTION_MODULE_* failure codes
crates/client/src/error.rs                     # ModuleRefusal, module_refusal()
crates/miden-multisig-client/src/              # ModuleDenied, ModuleUnavailable; get_policies; ProposalBuilder::check
packages/guardian-client/src/http.ts           # moduleRefusal(); checkDeltaProposal, getPolicies
packages/miden-multisig-client/src/            # M2a: getPolicies, checkTransaction
packages/guardian-operator-client/src/         # M2a: module routes
examples/policy-module/                        # M2a: basic-policy as a standalone gRPC service
examples/custom-guardian/                      # M2c custom build
docs/guides/policy-modules/                    # built-in, remote and custom guides
docs/, spec/                                   # CONFIGURATION, DASHBOARD, TROUBLESHOOTING, CONCEPTS, runbook, api and processes
docs/openapi*.json                             # regenerated with gen-openapi; CI checks drift
```

Per AGENTS.md, published crates and packages document new public API in their own README in the same PR, OpenAPI is regenerated for every HTTP change, and `examples/demo`, `examples/web` and `examples/operator-smoke-web` are the required smoke checks ([quickstart.md](./quickstart.md), section 4).

**Structure Decision**: one new library crate for everything a module author needs, and one new `modules` subsystem inside `guardian-server`, so module code is isolated from service code and the services gain one call each. The paths above are the prototype's, carried into M2a; the ones marked M2a, M2b or M2c do not exist yet.

## Delivery Phases

| Phase | Content | Exit criterion |
|---|---|---|
| **P0: Prototype** (branch `251-modules-prototype`, off `main`, not for merge) | SDK skeleton, catalog with `all` and `assigned`, pipeline at push and proposal creation, `basic-policy`, one remote module, account view and projection builders, demo walk-through, latency A/B | Gates G1 to G5 closed or accepted; findings in RFC revision 20 |
| **P1: M2a** | Everything tagged M2a in the spec, started 2026-11-09 after PR #510 merged (gate G5); tasks in [tasks.md](./tasks.md) | Stories 1 to 6 acceptance, SC-001 to SC-007, SC-009 to SC-011 |
| **P2: M2b** | Outbox, notifier, `pending`, retention, resync, stateful conformance | Story 7, SC-008 |
| **P3: M2c** | `ServerBuilder::from_env`, `with_module_kind`, custom-build template | Story 8 |

## M2a delivery

**Decision (2026-11-09)**: M2a is developed on one branch, `251-modules-m2a`. The layer split below was built and checked once and is kept as a recipe; reviewable branches are cut from `251-modules-m2a` by layer when needed, not maintained alongside it.


The prototype (#554) is one draft of about 13,000 lines and is not merged as it is. M2a starts from it and ships as a stack of pull requests off `main`, each building and passing its tests on its own, in this order:

| Layer | Branch | Content |
|---|---|---|
| 1. Module SDK | `251-m2a-sdk` | `crates/guardian-module-sdk` (protocol, `Module`, `serve`, `basic-policy`, conformance) and the workspace entry |
| 2. Server core | `251-m2a-server-core` | `crates/server/src/modules/`, `network/miden/module_views.rs`, the modules migration, the `GUARDIAN_MODULE_*` errors, permissions and audit kinds, the `/dashboard/modules` routes, `AppState` wiring and the `modules` field in test initializers; the guide's example catalogs (`docs/guides/policy-modules/*.toml`, parsed by the registry tests) and the TypeScript error-code vocabulary (`packages/guardian-client/src/error-codes.ts`, held to `error.rs` by a drift test); OpenAPI regenerated. No hook calls the pipeline |
| 3. Hooks | `251-m2a-hooks` | The three gate hooks (`ack_delta_internal.rs`, `push_delta_proposal.rs`, `configure_account.rs`), the execution worker, the discovery routes on both transports, the `guardian-shared` failure codes with the TypeScript execution vocabulary and the browser example that switches over it, and the compile fixes the non-`Copy` failure code needs (qualification driver, demo, one multisig test); OpenAPI regenerated |
| 4. Clients | `251-m2a-clients` | `crates/client`, `crates/miden-multisig-client`, `packages/guardian-client` (`http.ts` and its types), `packages/miden-multisig-client` |
| 5. Docs | `251-m2a-docs` | `docs/` (guides, configuration, dashboard, troubleshooting, RFC), `spec/`, the speckit feature, the `verify-module-counting` skill |

Each layer builds the workspace with `--all-targets`, passes `gen-openapi --check`, the server, client and multisig test suites, and the TypeScript client tests; the top of the stack equals the prototype tree. Rule found while splitting: a TypeScript vocabulary held to a Rust source by a drift test lands in the same layer as that source. `251-modules-m2a` is the integration branch at the top of the stack; M2a tasks land on the layer they belong to. The operator client (US5) joins layer 2 or its own PR when it is built.

## Decisions made during planning

Both are now recorded in RFC 0002 revision 19 (section 3.1):

- `basic-policy` recipient allowlist denies partial and non-P2ID output notes, since their recipient is unknown ([contracts/catalog-config.md](./contracts/catalog-config.md)).
- `MidenTransaction` (the Miden variant of the network-tagged `TransactionProjection`) gets its own builder sharing helpers with the dashboard projector rather than reusing dashboard types (research G2 default).

## Complexity Tracking

No constitution violations to justify.
