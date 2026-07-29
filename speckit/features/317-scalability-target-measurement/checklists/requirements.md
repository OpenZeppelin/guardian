# Specification Quality Checklist: Guardian Scalability Target — Measurement Definition

**Purpose**: Validate specification completeness and quality before proceeding to planning
**Created**: 2026-07-28
**Feature**: [spec.md](../spec.md)

## Content Quality

- [x] No implementation details (languages, frameworks, APIs)
- [x] Focused on user value and business needs
- [x] Written for non-technical stakeholders
- [x] All mandatory sections completed

## Requirement Completeness

- [x] No [NEEDS CLARIFICATION] markers remain
- [x] Requirements are testable and unambiguous
- [x] Success criteria are measurable
- [x] Success criteria are technology-agnostic (no implementation details)
- [x] All acceptance scenarios are defined
- [x] Edge cases are identified
- [x] Scope is clearly bounded
- [x] Dependencies and assumptions identified

## Feature Readiness

- [x] All functional requirements have clear acceptance criteria
- [x] User scenarios cover primary flows
- [x] Feature meets measurable outcomes defined in Success Criteria
- [x] No implementation details leak into specification

## Notes

All three original clarifications are now resolved. Checklist passes in full.

- **Concurrency semantics** → resolved in FR-003/FR-003a–c. A concurrent user is a
  connected client at a declared per-user rate, initially one `get_state` per user
  per 10 seconds (2,000 reads/s at 20,000 readers). In-flight concurrency is a
  measured output, never the definition; the in-flight-saturation reading survives
  as a separately-labelled stress ceiling. Chosen because the target is an
  operational statement about clients, and because 2,000 reads/s sits in the same
  order as the 1,409 reads/s April sustained at p95 926ms — a target that is
  neither already met nor unreachable. The rate is versioned with the target so
  changing it is visible.
- **Account-population fidelity** → resolved in FR-016a–c. Real provisioning path
  for every account load reaches (~20,100 at target load); bulk provisioning for
  the remaining ~80,000 that exist to create scale. Bulk accounts must be
  sample-verified equivalent in stored state, and a run may not claim an
  account-dimension verdict if its real-path subset is smaller than what its load
  touched. Chosen because it keeps fidelity exactly where behaviour is observed
  while making the population feasible to build.
- **Authoritative environment** → resolved in FR-017a/b. The deployed,
  testnet-backed, production-shaped deployment measured by the distributed harness
  is authoritative; local runs are the labelled fast loop. Production already
  defaults to testnet, so the April setup is reproducible as-is. Verdicts are
  pinned to the deployment shape they were measured on.
- Priority changed after review: re-running the existing February and April
  profiles against current code (User Story 1, FR-A01–FR-A07) now precedes new
  profile work, because the code has moved since April — canonicalization work
  landed and the delta-path lock is gone — so the April numbers describe a server
  that no longer exists. The re-baseline is a replay of existing assets, not a
  build.
- Content-quality items pass with one deliberate exception to the "no
  implementation details" spirit: the Assumptions section names the two existing
  benchmark harness locations. This is scope-bounding (extend, don't replace),
  not design, and it prevents the plan phase from proposing a third harness.
- Requirement wording avoids naming a measurement tool, transport library, or
  report format; it constrains what must be observable, not how.
