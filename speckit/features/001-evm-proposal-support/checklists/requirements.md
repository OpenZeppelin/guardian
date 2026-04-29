# Requirements Checklist: Domain-separated EVM proposal support

**Purpose**: Validate that the feature requirements are complete, clear, and
ready for planning and implementation  
**Created**: 2026-03-18  
**Updated**: 2026-04-29
**Feature**: [spec.md](/Users/marcos/repos/guardian/speckit/features/001-evm-proposal-support/spec.md)

## Requirement Completeness

- [x] CHK001 Are the affected layers explicitly identified when lower-layer behavior changes? [Coverage] [Context] [Contract / Transport Impact]
- [x] CHK002 Are in-scope and out-of-scope boundaries documented? [Completeness] [Scope]
- [x] CHK003 Are upstream consumer validation expectations defined when contracts change? [Coverage] [User Scenarios & Testing] [Contract / Transport Impact]
- [x] CHK004 Is the domain-separated `/evm/*` route model stated explicitly? [Clarity] [Scope] [Contract / Transport Impact]
- [x] CHK005 Are superseded unified-route assumptions excluded from the current requirements? [Consistency] [Functional Requirements] [Delivery Guidance]

## Contract & Parity Clarity

- [x] CHK006 Are HTTP and gRPC changes or non-changes stated explicitly? [Clarity] [Contract / Transport Impact]
- [x] CHK007 Are Rust and TypeScript client impacts stated explicitly? [Clarity] [Scope] [Functional Requirements]
- [x] CHK008 Are storage backend parity expectations or limitations stated explicitly? [Clarity] [Data / Lifecycle Impact]
- [x] CHK009 Are error-shape and auth expectations specific enough to verify? [Measurability] [Functional Requirements] [Contract / Transport Impact] [Edge Cases]
- [x] CHK010 Are cookie-backed EVM session requirements defined without requiring JWTs? [Clarity] [User Story 1] [Functional Requirements]

## State, Auth, and Lifecycle Coverage

- [x] CHK011 Are Miden state, delta, proposal, and canonicalization boundaries preserved? [Coverage] [User Story 4] [Functional Requirements]
- [x] CHK012 Are EVM account registration and signer-authority checks covered? [Coverage] [User Story 2] [Functional Requirements]
- [x] CHK013 Are EVM proposal create/list/get/approve/executable/cancel flows covered? [Coverage] [User Story 3] [Functional Requirements]
- [x] CHK014 Are replay protection, signer handling, duplicate-signature, expiry, and finality edge cases addressed? [Edge Case] [User Stories] [Edge Cases]
- [x] CHK015 Are feature-gate semantics documented for default and enabled servers? [Clarity] [Functional Requirements] [Contract / Transport Impact]
- [x] CHK016 Are out-of-scope execution, UserOperation building, and on-chain submission responsibilities documented? [Scope] [Out of Scope] [Assumptions]

## Validation & Documentation Readiness

- [x] CHK017 Are acceptance scenarios independently testable per user story? [Acceptance Criteria] [User Scenarios & Testing]
- [x] CHK018 Are measurable success criteria defined? [Completeness] [Success Criteria]
- [x] CHK019 Are docs/examples/client updates called out when external behavior changes? [Coverage] [Scope] [Functional Requirements]
- [x] CHK020 Are assumptions and deferred topics separated from current scope? [Completeness] [Assumptions] [Deferred Topics]

## Notes

- The Speckit feature spec is aligned with the repository `spec/` docs updated
  on 2026-04-29.
- Speckit planning artifacts now describe the domain-separated `/evm/*` flow.
