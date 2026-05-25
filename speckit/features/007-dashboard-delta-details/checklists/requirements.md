# Specification Quality Checklist: Dashboard delta activity feed and detail view

**Purpose**: Validate specification completeness and quality before proceeding to planning
**Created**: 2026-05-24
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

- Story 3 reference key was settled in the 2026-05-24 clarification session: `{account_id, nonce}` (FR-007–FR-009a). The Miden on-chain `TransactionId` was considered and rejected — Guardian sits one layer below the on-chain transaction and the persisted `TransactionSummary` deliberately omits the fee asset that `TransactionId` hashes over.
- FR-006 was loosened during code-review pass: the listing endpoints continue to surface the existing `candidate`/`canonical`/`discarded` triplet (pending lives on the proposal queue), not a wire-level "canonical only" filter.
- Per-account operator ACL is **not** in scope for v1; the spec's "Operator authorization scope (v1)" edge case documents the deliberate gap and SC-008 reflects it.
- Persisted `delta_payload` has two on-disk shapes (raw `TransactionSummary` JSON from `push_delta`; `{tx_summary, metadata, signatures}` wrapper from multisig commits). The decoder handles both — see `research.md` Decision 10.
- **2026-05-25 architecture pivot**: the original "decode-on-read, no schema migration" plan was found to lose multisig `proposal_type` (the TS client unwraps the proposal payload before calling `pushDelta`). The implementation now adds a `metadata JSONB` column on `deltas`, derives metadata at push time, and replaces the top-level `category` / `kind` / `summary` / `proposal_type` fields on the listing wire shape with a single optional `metadata: DeltaMetadata` object. `kind` is dropped (redundant with `metadata.proposal?.proposal_type`); legacy `proposal_type` field is dropped (recoverable from same path). See spec.md §Clarifications 2026-05-25 and research.md Decisions 2 / 3 (revised) for the authoritative new design.
