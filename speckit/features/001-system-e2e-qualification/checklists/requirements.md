# Specification Quality Checklist: Black-box system E2E and Miden multisig qualification

**Purpose**: Validate specification completeness and quality before proceeding to planning
**Created**: 2026-09-15
**Feature**: [spec.md](../spec.md)

## Content Quality

- [x] No implementation details (languages, frameworks, APIs)
- [x] Focused on user value and business needs
- [x] Written for non-technical stakeholders
- [x] All mandatory sections completed

## Requirement Completeness

- [x] No [NEEDS CLARIFICATION] markers remain
- [x] Artifact-mode scope settled (published SDK packages in or out)
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

Validation pass 1 (2026-09-15): three [NEEDS CLARIFICATION] markers open at
FR-026 (target network), FR-034 (treasury key custody), FR-041 (release
gating). All other items passed.

Validation pass 2 (2026-09-15): all three resolved by the requester and
recorded in the spec's Clarifications section. Propagated into FR-002,
FR-026/a/b/c/d, FR-034/a/b, FR-041/a/b, User Stories 1, 2 and 8, Edge Cases,
Key Entities, Success Criteria, Assumptions and Dependencies. Checklist
complete.

Validation pass 3 (2026-09-15): reprioritised on requester input that the
TypeScript SDK carries external consumers and therefore more value at risk.
The TypeScript canary became User Story 1 at P1 and the Rust canary User Story
2 at P1; the remaining stories renumbered. Added FR-023a (TypeScript coverage
at least as complete as Rust), FR-023b (exercise consumer artifacts in the
consumer runtime), FR-023c (stale-artifact detection), FR-038a (embedded
transport retry carve-out), SC-011 and SC-012. Checklist still complete.

Validation pass 4 (2026-09-15): external spec review applied. Added image
source as a first-class dimension (FR-002, FR-005, FR-007, FR-007a, FR-015b,
Key Entities), the two image/SDK pairings and the trigger and rotation set
(FR-025 through FR-025e), run-conclusion semantics including per-network
independence (FR-003a, FR-003b), unfiltered PR coverage and required-check
status (FR-015, FR-015a), on-chain residual balance policy (FR-009, FR-033a,
SC-007), release recording and decision ownership (FR-040, FR-041a), TypeScript
runtime as a dimension (FR-023d), parity comparison semantics (User Story 6),
and SC-013 through SC-015. Added a Delivery Sequencing section. Checklist
still complete.

Two review points were not applied as proposed, with reasons:

- The review's premise that the requester asked to qualify the published GHCR
  image is not what the request said; the request asked for the latest features
  from the branch. Published-image qualification was nonetheless added, because
  the underlying technical point (build-from-ref and pull-published are
  different products) is correct and the earlier draft conflated them.
- The review put the nightly schedule on the published `latest` tag. Verified
  against the publishing workflow: it triggers only on published releases and
  manual dispatch, and `latest` is gated on a full release, so it can lag the
  default branch by weeks. A nightly against it would mostly measure network
  drift, not the branch regressions the requester asked to catch. The schedule
  therefore uses the branch pairing (FR-025b), and the published artifact is
  qualified on publication, pre-release, and on a slower cadence (FR-025c,
  FR-025d).
- The review's FR-023b rewrite assumed the TypeScript consumer runtime is
  necessarily the browser. Verified otherwise: the package declares a
  server-side engine requirement and its Miden dependency ships a dedicated
  server-side entry point. Runtime became a scenario dimension (FR-023d), broad
  matrix in the cheaper runtime, browser-specific risks covered separately.

Validation pass 5 (2026-09-15): second external review applied. Two of its
findings were verified against the code and are real defects I introduced:

- FR-017 required a mixed-scheme multisig shape. Verified at
  `packages/miden-multisig-client/src/account/storage.ts:14` and
  `crates/miden-multisig-client/src/transaction/configuration/config.rs:18`:
  both builders assign one configured scheme to every signer, so the shape is
  unbuildable in either SDK, not just TypeScript as the review stated. Now a
  recorded capability gap (FR-017a).
- FR-018 asserted a proposal reaching a terminal status through Guardian.
  Verified at `docs/MULTISIG_SDK.md:710`: proposals are removed once
  canonicalized, so the wait would hang. Replaced with a four-part completion
  assertion (FR-018, FR-018a).

Also added: coverage matrix and qualification-claim rules (FR-003c to FR-003f),
minimum action matrix with balance assertions (FR-019a, FR-019b), cross-SDK
handoff (FR-023e), publication permission not amending the result (FR-041c),
serialized treasury mutations and orphan recovery (FR-009a, FR-033b). Feature
retitled to name its scope as Miden system qualification.

Validation pass 6 (2026-09-15): the open artifact-mode decision was resolved by
the requester in favour of adding the published pairing. Added FR-025a (third
pairing), FR-025f (install outside the workspace so path linking cannot
substitute local packages), FR-025g (record digest, versions, integrity hashes,
Miden versions), FR-025h (image and SDK skew check, separate pipelines),
FR-025i (post-publication, pre-release, at least weekly), plus the artifact-set
entity, SC-014a, a dependency on registry availability, and a sequencing slot.
Published SDK packages removed from Out of Scope; downstream consumer projects
remain out. Checklist complete, no open items.

Validation passes 7 and 8 (2026-09-15): Phase 0 research corrected requirements
that were wrong or unbuildable against the real codebase and the live networks.
Verified defects and their fixes:

- FR-014 was unsatisfiable. The server dials its chain RPC endpoint eagerly at
  startup and will not boot without a responder, so a profile leaving it unset
  never starts. Split into FR-014, FR-014a (local stand-in), FR-014b.
- FR-013a was understated: the ephemeral acknowledgement identity breaks
  account registration outright, not just the restart assertion.
- FR-007 would have passed vacuously. The Docker build context excludes
  version-control metadata, so an image built without the commit build argument
  reports "unknown". Added FR-007b.
- FR-012 would have asserted an untruth: message wording is documented as
  unstable, and transport-boundary errors do not carry the envelope on gRPC.
  Added FR-012a, FR-012b, FR-012c.
- FR-021 implied offline proposal creation was general. It is guardian-
  migration only. Split into FR-021 and FR-021a.
- Rate limiting would have produced fake failures (FR-008a).
- Anchor pruning: proposals must execute inside the network's historical
  window or become permanently unexecutable. Added FR-018b, FR-018c, FR-018d.
- Treasury durability and concurrency: invalidated by chain reset and by SDK
  pin bump; replay protection is per signer. Added FR-033b through FR-033e.
- Scheme policy may exclude schemes under test (FR-026g).
- Per network and SDK availability, since the two SDKs use different
  transports (FR-026e, FR-026f).
- TypeScript findings: Node viability proven by execution; published node entry
  point is missing exports a consumer needs (FR-023f); browser determinism gate
  retained (FR-023g).

Target network scope resolved by the requester: both networks are full targets.
The devnet transport refusal is treated as a temporary outage and absorbed at
run time; the retention-window constraint is structural and shrinks devnet's
required set rather than its availability.

Retained judgement calls, flagged for the reviewer rather than silently
absorbed:

- SC-002 states a wall-clock budget rather than a user-facing metric. For a
  pull-request gate, run duration is the property that decides whether the gate
  is adoptable, so it is the honest measure.
- SC-006 was rewritten to measure failure *misclassification* rather than raw
  failure rate. On shared public networks a low absolute failure rate is not
  achievable, and asserting one would have made the criterion unmeetable
  rather than demanding.
- FR-038 ("never retry a state-changing submission") cannot be satisfied on
  the TypeScript path, because its bundled Miden client retries submissions
  below the level this project controls. Rather than weaken FR-038 for
  everyone, FR-038a records the condition and requires unknown-outcome
  submissions to be resolved by observation. Worth a reviewer's attention: it
  is a real limit on what the TypeScript canary can prove.
- The spec names domain concepts (multisig shape, signature scheme, proposal
  lifecycle, HTTP and gRPC surfaces, database-backed storage) because they are
  the product's own vocabulary, not implementation choices. Test runners,
  container orchestration and workflow syntax are deliberately absent and
  belong in the plan.
