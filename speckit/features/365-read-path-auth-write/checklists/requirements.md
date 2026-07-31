# Specification Quality Checklist: Reduce Per-Read Cost of Replay-Protection Auth Writes

**Purpose**: Validate specification completeness and quality before proceeding to planning
**Created**: 2026-07-31
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

- The spec deliberately does **not** choose among the mechanisms sketched in
  issue #365 (narrow-table split, external auth-state store, coarsened
  granularity). Per the user's direction, approach selection is gated by
  FR-005: a documented, security-weighed comparison in the planning phase,
  open to alternatives beyond the issue's three options.
- Endpoint names (`get_state`, `get_delta_since`, …) and benchmark artifact
  paths appear in the spec as **contract vocabulary and evidence pointers**,
  consistent with prior specs in this repo — they identify *what* must be
  covered and *how* success is measured, not *how* to build the fix.
- Success criteria reference database time/utilisation because the feature's
  entire purpose is a storage-cost property; they are expressed as A/B deltas
  against committed baselines and remain mechanism-neutral.
- Security posture is encoded as a hard default (FR-001: guarantee preserved;
  any relaxation requires explicit approval), so no [NEEDS CLARIFICATION]
  marker was needed — the sensitive decision is surfaced as an explicit
  decision gate rather than an open question blocking the spec.

## Post-review updates (2026-07-31)

Two external reviews were folded into the spec and design artifacts:

- **FR-001 hardened (H1)**: guarantee-weakening approaches are now outside the
  feature entirely — reachable only via a spec amendment or successor feature,
  resolving the tension with FR-004 and Out of Scope. US2 scenario 4, the
  crash edge case, FR-005, and the Assumptions were aligned.
- **SC-006 numeric conflict fixed**: floor is now ≥30% headroom (arithmetically
  the same gate as SC-001's +25%); the 40% figure is a reported stretch marker,
  not a pass/fail criterion, with the observable defined concretely (H3).
- **SC-002 hardened (M1)**: paired the share-of-DB-time percentage with an
  absolute per-read auth cost measure and named the exact statements counted.
- **SC-007 added (M4)**: mixed-profile A/B with a defined 40/40/20 mix and a
  quantitative pass criterion.
- **Backend scoping (C1)**: filesystem backend documented as single-process by
  design across FR-001/SC-003, contract, and data model; two-replica
  verification is Postgres-only.
- **Harness dependency (H2)**: `benchmarks/diagnostic-stack/` recorded as an
  external dependency landing via a separate PR; baselines regenerated on
  `main` if the recorded result dirs don't ship with it; all comparisons are
  same-machine A/B.
- **US4/FR-008 precision (M2, M3)**: the retained small replay-state write is
  explicit, and `updated_at` semantics are defined as "non-authentication
  metadata mutations" (config + pause/release + pending-candidate).
- **Design pack**: filesystem `auth_state.json` fail-open-on-deletion guard,
  `configure_account.rs:150` added as a second stale-clobber site, CAS
  return-mapping test contract made explicit, test command corrected to
  `--features postgres,integration`.
