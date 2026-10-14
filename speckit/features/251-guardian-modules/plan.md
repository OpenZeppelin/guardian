# Implementation Plan: Guardian Modules, a Hook Pipeline for Pluggable Policy

**Branch**: `251-guardian-modules` (spec and plan on `251-guardian-modules-rfc`, PR #544) | **Date**: 2026-10-13 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `speckit/features/251-guardian-modules/spec.md`; design source [RFC 0002](../../../docs/rfcs/0002-guardian-modules.md) revision 19
**Status**: Prototype built (2026-10-13). Gates G1 to G5 closed at component level, findings and the changes for RFC revision 20 in [research.md](./research.md#prototype-findings-2026-10-13). Still open: the end-to-end walk-through and latency A/B against a local Miden node. M2a `tasks.md` follows RFC revision 20.

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
└── tasks.md                        # not yet: after the prototype gates
```

### Source Code (repository root)

```text
crates/guardian-module-sdk/              # NEW, published (M2a)
├── proto/guardian_module_v1.proto
├── build.rs                             # tonic-prost-build
├── src/
│   ├── lib.rs                           # Module, ModuleFactory, ModuleError, re-exported protocol types
│   ├── serve.rs                         # gRPC harness, per-instance credentials
│   ├── stateful.rs                      # watermark helper (M2b)
│   └── conformance/                     # suite run against in-process and remote modules
└── examples/basic-policy-serve.rs

crates/server/src/
├── modules/                             # NEW
│   ├── catalog.rs                       # TOML load, validation, hash, startup checks
│   ├── registry.rs                      # kind registration, instance creation, secret resolution
│   ├── pipeline.rs                      # ordered evaluation, budget, first-deny
│   ├── request.rs                       # HookRequest assembly, ProposalContext, size cap
│   ├── account_view.rs                  # AccountView extraction (gate G1)
│   ├── projection.rs                    # TxProjection builder (gate G2)
│   ├── grpc_kind.rs                     # remote kind client, TLS, auth
│   ├── breaker.rs                       # circuit breaker and health probe
│   ├── basic_policy.rs                  # first-party reference kind
│   ├── assignment.rs                    # per-account assignment and flags in the metadata store
│   └── notify/                          # outbox writer, notifier, delivery, resync (M2b)
├── services/push_delta.rs               # pipeline before ack (acknowledge_delta after PR #510)
├── services/push_delta_proposal.rs      # pipeline before submit_delta_proposal
├── services/configure_account.rs        # pipeline after credential and signer checks; default copy
├── storage/filesystem.rs                # proposal create-if-absent
├── network/                             # account_view on NetworkClient
├── dashboard/                           # module routes, modules:disable permission
├── error.rs                             # ModuleDenied, ModuleUnavailable
├── metrics/                             # module metrics
└── builder/                             # with_module_kind, from_env (M2c), catalog in build()

crates/server/migrations/<date>_guardian_modules/       # assignments, flags, delta columns (M2a)
crates/server/migrations/<date>_guardian_module_events/ # outbox and delivery (M2b)

crates/client/src/                       # typed module errors
crates/miden-multisig-client/src/        # typed module errors on proposal, push, registration
packages/guardian-client/src/            # same in TypeScript
packages/miden-multisig-client/src/
packages/guardian-operator-client/src/   # module endpoints
examples/policy-module/                  # basic-policy as a standalone gRPC service
examples/custom-guardian/                # M2c custom build
docs/                                    # CONFIGURATION, TROUBLESHOOTING, CONCEPTS trust boundary, module author guide
docs/openapi*.json                       # regenerated with gen-openapi; CI checks drift
crates/guardian-module-sdk/README.md     # NEW: crates.io landing page, module author quick start
crates/client/README.md, packages/guardian-client/README.md            # module error codes
crates/miden-multisig-client/README.md, packages/miden-multisig-client/README.md  # refusals at creation, push, registration
packages/guardian-operator-client/README.md                             # module endpoints
```

Per AGENTS.md, published crates and packages document new public API in their own README in the same PR, OpenAPI is regenerated for every HTTP change, and `examples/demo`, `examples/web` and `examples/operator-smoke-web` are the required smoke checks ([quickstart.md](./quickstart.md), section 4).

**Structure Decision**: one new library crate for everything a module author needs, and one new `modules` subsystem inside `guardian-server`, so module code is isolated from service code and the services gain one call each. Paths under `crates/server/src/modules/` are indicative until gates G1, G2 and G4 close.

## Delivery Phases

| Phase | Content | Exit criterion |
|---|---|---|
| **P0: Prototype** (branch `251-modules-prototype`, off `main`, not for merge) | SDK skeleton, catalog with `all` and `assigned`, pipeline at push and proposal creation, `basic-policy`, one remote module, account view and projection builders, demo walk-through, latency A/B | Gates G1 to G5 closed or accepted; findings in RFC revision 20 |
| **P1: M2a** | Everything tagged M2a in the spec, starting after PR #510 merges (gate G5) | Stories 1 to 6 acceptance, SC-001 to SC-007, SC-009 to SC-011 |
| **P2: M2b** | Outbox, notifier, `pending`, retention, resync, stateful conformance | Story 7, SC-008 |
| **P3: M2c** | `ServerBuilder::from_env`, `with_module_kind`, custom-build template | Story 8 |

## Decisions made during planning

Both are now recorded in RFC 0002 revision 19 (section 3.1):

- `basic-policy` recipient allowlist denies partial and non-P2ID output notes, since their recipient is unknown ([contracts/catalog-config.md](./contracts/catalog-config.md)).
- `TxProjection` gets its own builder sharing helpers with the dashboard projector rather than reusing dashboard types (research G2 default).

## Complexity Tracking

No constitution violations to justify.
