# RFCs

Design decisions written for readers outside the team that made them — upstream
protocol partners, integrators, and anyone reading the repository publicly.

## Index

| # | Title | Status | Feature |
|---|---|---|---|
| [0001](./0001-server-side-transaction-execution.md) | Guardian executes, proves and submits transactions | Accepted for implementation | [#254](https://github.com/OpenZeppelin/guardian/issues/254) |

## What belongs here

An RFC when a decision needs review by people who do not read our working
artifacts: a choice between architectures, a change to a trust boundary, a claim
about upstream behavior we want verified, or a request for something from an
upstream project.

Not an RFC when the decision only concerns this repository's internals. Those
live in [`speckit/features/`](../../speckit/features/) — specs, plans, task
breakdowns, research logs — which is where the working detail behind an RFC
stays. An RFC is the reviewable surface over those artifacts, not a replacement
for them, and it links back to them.

## Conventions

**Numbering** is a zero-padded `NNNN-kebab-title.md`, assigned when the document
is opened for review and never reused or renumbered.

**Status** is one of:

| Status | Meaning |
|---|---|
| Draft | Not yet open for comment |
| Open for comment | A decision is being sought; the document states by when, if there is a deadline |
| Accepted for implementation | The decision stands and work may proceed; comments still welcome and can still change it |
| Implemented | Shipped; the RFC is a historical record |
| Withdrawn | Superseded or abandoned, with the reason stated in the document |

**Claim tags.** Where an RFC asserts something about upstream or our own code,
tag how far it was verified — **[RAN]** (verified by executing code), **[READ]**
(verified against dependency source), **[INFERRED]** (reasoned, not directly
verified) — and cite `file:line` so a reviewer can check it independently. Pin
the dependency versions the claims are made against; upstream moves.

**Corrections stay visible.** When a claim turns out to be wrong, say so in the
document and keep the withdrawal readable rather than quietly editing it away.
A reviewer who spent time on the original claim needs to see that it changed,
and the reasoning that produced a wrong conclusion is usually worth keeping.

## Review

RFCs are reviewed as pull requests, so corrections land as line comments on the
citation that makes the claim. Merging records the decision.
