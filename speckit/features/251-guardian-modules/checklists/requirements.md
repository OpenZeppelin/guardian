# Specification Quality Checklist: Guardian Modules, a Hook Pipeline for Pluggable Policy

**Purpose**: Validate specification completeness and quality before proceeding to planning
**Created**: 2026-10-13
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

- Validation pass 1 (2026-10-13): all items marked passing, but review found six requirements that contradicted the RFC or existing behaviour, so "testable and unambiguous" did not hold: FR-007 blocked re-onboarding of paused and released accounts; FR-003 did not cover denied reconfiguration; FR-014 omitted the disabled-instance exception; FR-015 conflated secret references with resolved values; FR-064 refused during resync instead of applying the failure mode; FR-066 placed evaluation purity in M2b.
- Validation pass 2 (2026-10-13): all six resolved against `configure_account.rs` (no active check, pause kept, release cleared on re-onboarding) and RFC 0002 sections 2.1 to 3.3; evaluation purity moved to M2a as FR-054 with a conformance check; SC-008 and Story 7 qualified to modules that are reachable and not awaiting resync. Added acceptance cases: registration running mandatory and default modules before an assignment exists, paused and released reconfiguration allowed and denied, enable versus disable permissions, and a notification applied before its acknowledgement is recorded. All items pass.
- Accepted domain terms: the spec names the storage backends (Postgres, filesystem), candidate mode, the Miden protocol line and the Rust and TypeScript SDKs. These are product surfaces and deployment facts an operator must know, not implementation choices; wire formats, field names, configuration keys and code locations stay in RFC 0002.
- No clarification markers: the open decisions were resolved in RFC 0002 revisions 1 to 19 (section 9 and Appendix C).
- SC-006 (10 ms in-process overhead) and the default budget are provisional until the prototype on `251-modules-prototype` measures them.
- Validation pass 3 (2026-10-13, after the pre-prototype plan): review found a split-payment gap in the amount cap, stateful activation with queued candidates lacking module data, the filesystem proposal update path, sequential evaluation not continuing past fail-open failures, and FR-100 metrics in the wrong slice. Resolved: per-asset gross outgoing cap with split-payment and receive-and-send scenarios (Story 1), FR-067 and the activation fence (Story 7), FR-035 separating creation from updates, FR-053 continuing past fail-open failures (Story 4), orphaned and over-budget metrics moved to M2a. RFC 0002 revision 19 updated to match. All items pass.
- `plan.md` records the pre-prototype assumptions and the prototype findings (`research.md`, G1 to G5). `tasks.md` holds the P0 prototype tasks and the follow-on phases; the quickstart walk-through and an end-to-end run against a local node remain open.
