<!--
Sync Impact Report
Version change: 1.0.0 -> 1.1.0
Modified principles:
- III. Append-Only Integrity and Explicit Lifecycles (expanded fallback rule)
Added sections:
- None
Removed sections:
- None
Templates requiring updates:
- ✅ .specify/templates/plan-template.md
- ✅ .specify/templates/spec-template.md
- ✅ .specify/templates/tasks-template.md
- ✅ .specify/templates/checklist-template.md
Follow-up TODOs:
- Revisit whether observability and offline import/export compatibility should become first-class principles.
-->
# Guardian Constitution

**Version**: 1.1.0  
**Ratified**: 2026-03-18  
**Last Amended**: 2026-03-18

## Purpose

This constitution governs how feature work is specified, planned, and delivered
in Guardian. It exists to preserve protocol integrity across the
server, Rust and TypeScript client layers, multisig SDKs, and validation
examples in a multi-language codebase.

## Principles

### I. Bottom-Up Change Propagation

Any change MUST be assessed from `crates/server` upward through base clients,
multisig SDKs, and examples. A lower-layer contract or behavior change is not
complete until affected upstream consumers have been updated or explicitly
proven unaffected.

Rationale: the server is the system of record, and silent propagation gaps
create regressions that only surface after integration.

### II. Transport and Cross-Language Parity

HTTP and gRPC surfaces MUST preserve equivalent semantics for the same workflow.
Rust and TypeScript clients that model the same workflow MUST remain
behaviorally aligned. Any intentional divergence MUST be documented in the
feature spec and plan before implementation begins.

Rationale: parity is a core product property, not a cleanup task.

### III. Append-Only Integrity and Explicit Lifecycles

State, delta, and proposal flows MUST preserve append-only records with explicit
lifecycle transitions. Features MUST not introduce implicit fallback paths,
silent state rewrites, or undocumented status changes across pending,
candidate, canonical, discarded, or proposal states. Online and offline flows,
including any fallback between them, MUST remain explicit in the API flow,
return types, or user-visible control path.

Rationale: append-only lineage and explicit state machines are the backbone of
trust, replay safety, and debugging.

### IV. Explicit Authentication and Stable Boundary Errors

Per-account authentication, replay protection, signature handling, and boundary
error semantics MUST remain explicit. Changes touching auth, signature schemes,
status enums, payload shapes, or error surfaces are high-risk and MUST update
tests in the changed layer plus at least one upstream consumer.

Rationale: auth and error drift produce the most expensive cross-layer failures.

### V. Evidence-Driven Delivery

Every feature MUST define independently testable user stories, a targeted
validation plan, and any required docs or example updates when behavior changes.
High-risk areas, including auth, signatures, proposal lifecycle, canonical
status transitions, Rust/TypeScript parity, and offline import/export
compatibility, MUST receive updated validation before work is considered done.

Rationale: feature completion is based on evidence, not implementation effort.

## System Invariants

The following invariants are non-negotiable unless an explicit constitution
amendment changes them:

- State, delta, and proposal records remain append-only within per-account namespaces.
- Delta lineage remains explicit through `prev_commitment` and nonce-based ordering rules.
- HTTP and gRPC preserve the same core semantics, shapes, and error meanings.
- Equivalent Rust and TypeScript workflows preserve the same observable behavior.
- Storage backends such as filesystem and Postgres preserve the same externally
  observable semantics unless a documented backend-specific limitation is
  explicitly accepted.
- Per-account authentication remains explicit and replay-protected.
- Canonicalization lifecycle remains explicit: pending or candidate data may only
  move to canonical or discarded through documented transitions.
- Discarded deltas MUST NOT appear in default user-facing retrieval flows.
- Proposal identifiers remain deterministic, duplicate proposal signatures are
  rejected, and matching proposals are deleted when the corresponding delta
  becomes canonical.
- Local development and test work default to the filesystem backend unless a
  task explicitly requires Postgres.

## Governance

### Amendment Process

Changes to this constitution MUST be made in `speckit/constitution.md` and MUST
be accompanied by any required template updates under `.specify/templates/`.
Constitution changes are separate from feature implementation changes unless the
feature itself is explicitly revising project governance.

### Versioning Policy

Constitution versioning follows semantic versioning:

- MAJOR: removals or redefinitions that change governance expectations
- MINOR: new principles, sections, or materially expanded rules
- PATCH: clarifications, wording fixes, or non-semantic refinements

### Compliance Review

Feature specifications, plans, tasks, and analysis outputs MUST treat this
constitution as authoritative. If a feature cannot satisfy a principle, the
work MUST stop and either simplify the design or amend the constitution
explicitly before implementation continues.
