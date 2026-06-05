# Specification Quality Checklist: Standards-Based Database TLS Certificate Verification

**Purpose**: Validate specification completeness and quality before proceeding to planning
**Created**: 2026-06-04
**Feature**: [spec.md](../spec.md)

## Content Quality

- [~] No implementation details (languages, frameworks, APIs) — *deliberate,
  scoped exception:* the spec names the two existing TLS stacks (synchronous
  libpq migrations vs. asynchronous rustls pools) because reconciling them is a
  load-bearing requirement (FR-007) that the plan must budget for. Postgres
  `sslmode`/`sslrootcert` terms are protocol standards, not Guardian internals.
  These references are confined to FR-007, FR-001b/c, FR-003a and Assumptions;
  user stories and success criteria remain implementation-agnostic.
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
- [~] No implementation details leak into specification — *same scoped
  exception as Content Quality line above:* the two existing TLS stacks (sync
  libpq migrations vs. async rustls pools) are named where reconciling them is a
  load-bearing requirement (FR-007, FR-001b/c, FR-003a, Assumptions). User
  stories and success criteria remain implementation-agnostic.

## Notes

- The `sslmode`/`verify-ca`/`verify-full` terms are Postgres *protocol/standard*
  vocabulary (the user explicitly asked the feature to "follow standards"), not
  Guardian implementation choices, so their use does not violate the
  "no implementation details" criterion. Mapping these standard levels to
  concrete code/config is deferred to `/speckit.plan`.
- One scope decision (embed a vendor CA bundle vs. stay fully provider-neutral)
  was resolved in favor of provider-neutral per the user's "not just AWS"
  direction and recorded in Assumptions / Out of Scope rather than left as a
  blocking clarification.
- Post-review precision pass (2026-06-04) tightened the spec without changing
  scope: FR-001a pins the verification level to the standard `sslmode` token
  (full-ladder parse, no substring check); FR-003 names `sslrootcert`
  (`<path>` / `system`) as the trust-anchor knob; FR-007 + a new edge case +
  an Assumption now make the **two TLS stacks** (sync libpq migrations vs.
  async rustls pools) explicit; the local-TLS edge case distinguishes
  `verify-ca` vs `verify-full`; SC-003 reclassifies the managed-provider legs
  as manual smoke (AGENTS.md §6) with only the local TLS leg automated.
- Second review pass (2026-06-04) applied Postgres-semantics precision fixes:
  SC-001 now scopes hostname-mismatch rejection to `verify-full` only (verify-ca
  has no hostname check); FR-001b defines supported vs recognized `sslmode`
  values and the `allow`/`prefer` fallback decision; FR-001c normalizes libpq's
  `require`+rootcert ⇒ verify-ca promotion across both stacks; FR-003a captures
  that `sslrootcert=system` forces verify-full and needs libpq ≥16 (Bookworm
  ships PG15); FR-005a requires credential redaction in errors; FR-009a pins the
  AWS CA delivery + rotation mechanism.
- Two design decisions are deliberately deferred to `/speckit.plan` (flagged
  inline, not blocking the spec): the `allow`/`prefer` handling (FR-001b) and the
  `require`+rootcert normalization rule (FR-001c). Both have a recommended
  default in-spec.
- The "No implementation details" item is intentionally marked partial (`[~]`)
  with rationale above, rather than falsely green.
- Third review pass (2026-06-04, post-plan) resolved a CRITICAL + 5 HIGH + 4
  MEDIUM set: **RDS Proxy uses ACM/Amazon Trust Services roots, not RDS CA** →
  AWS trust anchor is now a COMBINED bundle (RDS + ATS), FR-009a; **delivery
  resolved** (FR-009b: mounted at deploy, image stays CA-free); **FR-007 scoped**
  to DATABASE_URL inputs in the controlled image + **FR-007a** (preflight before
  migrations, explicit `sslrootcert` to neutralize libpq's `~/.postgresql/root.crt`
  fallback, absent→disable); **FR-002a** canonical strict-SAN hostname semantics
  (CN-only refused) + cross-stack DNS/IP/mismatch/CN tests; **Context reframed**
  to "standard parameter names + documented fail-closed policy subset"; **P3
  acceptance** scenarios made explicit on both paths; **three URL values** (raw /
  normalized_sync / sanitized_async) replace the "untouched URL" wording;
  **parsing rules** for duplicate/empty/encoded/DSN/multi-host; **FR-004** reworded
  (accept-any unreachable from verifying modes, permitted only for encrypt-only);
  **FR-005** now rejects partially-parseable bundles (entire bundle must parse).
- Items marked incomplete require spec updates before `/speckit.clarify` or
  `/speckit.plan`. All other items pass.
