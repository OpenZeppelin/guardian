# Feature Specification: Self-Contained consume_notes Proposal Verification

**Feature Key**: `006-consume-notes-metadata`
**Suggested Branch**: `006-consume-notes-metadata` (manual creation optional)
**Created**: 2026-05-14
**Status**: Draft
**Input**: User description: "Make `consume_notes` proposal verification self-contained by carrying the data needed to rebuild the transaction request inside the signed proposal metadata, eliminating the per-device local-store dependency that currently prevents cosigners from verifying and signing legitimate proposals when the referenced notes are not present on their device. Tracks GitHub issue #229."

## Context

Guardian's multisig proposal model relies on a single integrity check
performed by every cosigner before signing: re-derive the transaction
request from the proposal's signed metadata, recompute the transaction
summary, and confirm it matches the summary attached to the proposal.
This check is the only thing standing between a cosigner and
blind-signing whatever the proposer attached. Today it runs for every
proposal type — `p2id`, `add_signer`, `remove_signer`,
`change_threshold`, `switch_guardian`, `update_procedure_threshold`,
and `consume_notes` — at multiple call sites in both the Rust and the
TypeScript multisig clients.

For every proposal type **except `consume_notes`**, the metadata is
self-contained: the signed fields are enough to deterministically
rebuild the request without consulting any external state. For
`consume_notes`, the metadata carries only the note identifiers, and
the rebuild step has to fetch the underlying note objects from the
cosigner's per-device local Miden note store. If those records are
not present on the verifying device — regardless of whether the
proposal itself is legitimate — verification fails and the cosigner
cannot proceed to sign or to execute.

The current behavior is a regression from the original `consume_notes`
implementation, which built the transaction request from note
identifiers alone. A later correctness fix made the request rebuild
require the full note object (rather than the identifier hash) so that
the Miden transaction kernel can actually execute the transaction. The
proposal metadata schema was not updated at the same time, so the
local-store dependency silently leaked from the execution path into
the verification path that runs before every cosigner signature.

The practical consequence is that a structurally valid `consume_notes`
proposal can become permanently un-verifiable for legitimate cosigners
whose local Miden store does not contain the referenced notes. This
happens in several routine situations: a cosigner whose local
synchronization cursor has already advanced past the note's block
through unrelated prior activity, a cosigner who wiped their local
store (a common workaround for unrelated genesis-digest issues), and
any cosigner attempting to verify a private-note proposal after the
note transport service has pruned the note metadata blob. None of
these conditions can be recovered with additional client-side
synchronization, tag registration, or transport fetches.

Audit finding M-08 (May 2026 audit) identified the same class of bug
in a different proposal type — P2ID note serial-number derivation that
diverged between SDKs and produced mismatched commitments — and was
remediated by making the rebuild deterministic from signed metadata
alone. This feature applies the same remediation philosophy to
`consume_notes`: extend the signed metadata so the verification rebuild
can be performed from the proposal alone, on any device, by any
cosigner, regardless of local-store state. The same change must apply
across both the Rust and the TypeScript multisig clients so cross-SDK
verification of the same proposal produces identical commitments.

This feature touches the multisig proposal metadata contract, the
proposal creation flow, the proposal verification flow, the proposal
execution flow, and the corresponding example harnesses on both the
Rust and the TypeScript multisig clients. It does not change Guardian
server storage semantics, the operator dashboard surface, or any other
proposal type's behavior beyond the shared metadata-versioning
mechanism this feature introduces.

## Scope *(mandatory)*

### In Scope

- Extend the `consume_notes` proposal metadata so the verification
  rebuild can run from signed metadata alone. The metadata must carry
  enough information to reconstruct the same transaction request the
  proposer built, without consulting the cosigner's local Miden note
  store or any network resource.
- Introduce an explicit proposal-metadata version discriminator so the
  new and old `consume_notes` metadata shapes can coexist for as long
  as legacy proposals are in flight, and so the verification path can
  unambiguously decide which shape it is reading.
- Update the proposal creation flow on both Rust and TypeScript
  multisig clients so newly created `consume_notes` proposals carry
  the new metadata shape.
- Update the proposal verification flow on both Rust and TypeScript
  multisig clients so verification of a new-shape `consume_notes`
  proposal succeeds without any local-store read, on any device, by
  any cosigner.
- Update the proposal execution flow on both Rust and TypeScript
  multisig clients so execution of a new-shape `consume_notes`
  proposal also succeeds without any local-store read.
- Define a transitional period during which legacy `consume_notes`
  proposals (created before this feature ships) continue to verify
  and execute by the old code path. This preserves any in-flight
  multisig signing across the upgrade.
- Define the end-of-life behavior for legacy `consume_notes`
  proposals: after a documented cut-over, the legacy code path is
  removed and only the new metadata shape is accepted.
- Add a cross-SDK interoperability test that asserts the TypeScript
  and Rust multisig clients produce identical transaction summary
  commitments when rebuilding the same new-shape `consume_notes`
  proposal from the same signed metadata, mirroring the audit M-08
  remediation pattern.
- Add a binding-integrity test that asserts a `consume_notes`
  proposal whose embedded note data does not match its declared note
  identifiers fails verification rather than silently rebuilding a
  different transaction.
- Add an end-to-end test in both the Rust CLI demo and the TypeScript
  smoke-web harness that reproduces the per-device local-store
  failure mode against the old shape and proves it does not reproduce
  against the new shape.

### Out of Scope

- Changes to any proposal type other than `consume_notes`. The
  metadata-versioning mechanism is introduced for `consume_notes`
  specifically; other proposal types are not migrated to versioned
  metadata as part of this feature.
- Changes to Guardian server proposal storage, replay protection,
  signature collection, or canonicalization semantics. The server
  persists proposal metadata as opaque payload from the multisig
  client's perspective and does not interpret the new fields.
- Changes to the operator dashboard read surface. The new metadata
  shape is internal to the multisig client; dashboard proposal entries
  continue to expose only the fields defined in feature
  `005-operator-dashboard-metrics`.
- An upstream fix to the Miden SDK to expose a "fetch note by
  identifier at a historical block" API. Such an API would not close
  the transport-pruned private-note case and is not required for this
  feature.
- An upstream fix to the Miden note transport service's retention
  policy. The new metadata shape removes the dependency on transport
  retention for verification purposes.
- A general "embed arbitrary external state in proposal metadata"
  pattern. Only the specific data needed to rebuild a `consume_notes`
  transaction request is embedded.
- A change to how cosigners discover or claim notes outside of the
  proposal flow. Note discovery, tag registration, and synchronization
  are unaffected.
- A "force-resync" or local-store recovery utility for legacy
  proposals. Legacy proposals continue to verify by the old code path
  during the transitional window; after cut-over, any legacy
  proposal that is still in flight must be re-proposed in the new
  shape by its proposer.

## User Scenarios & Testing *(mandatory)*

The behavior changes here are entirely inside the multisig client
libraries and their example harnesses. Validation is primarily through
unit tests of the metadata-version dispatch, cross-SDK rebuild
equivalence tests, and end-to-end tests in the existing demo and
smoke-web harnesses.

### User Story 1 - Cosigner Can Verify A Proposal Without The Note Locally (Priority: P1)

As a cosigner who has been invited to sign a `consume_notes` proposal
on a device whose local Miden store does not contain the referenced
notes, I can verify the proposal's integrity and proceed to sign,
without first having to synchronize, refetch private notes, or
otherwise reconcile my local store.

**Why this priority**: This is the primary failure the feature exists
to fix. Without it, legitimate cosigners cannot sign legitimate
`consume_notes` proposals, and the only workaround is to re-propose
from a device that happens to hold the note — which itself can fail
once any cosigner's local state advances or is wiped.

**Independent Test**: On a device with a freshly initialized local
Miden store containing none of the notes referenced by a proposer's
in-flight `consume_notes` proposal, import the proposal, run
verification, and confirm verification succeeds and produces the same
transaction summary commitment the proposer produced.

**Acceptance Scenarios**:

1. **Given** a cosigner whose local store does not contain any of the
   notes referenced by an in-flight new-shape `consume_notes`
   proposal, **When** the cosigner runs proposal verification,
   **Then** verification succeeds, the rebuilt transaction summary
   commitment equals the proposer's, and the cosigner can produce a
   signature for the proposal.
2. **Given** a cosigner whose local store has been wiped between
   proposal creation and verification, **When** the cosigner reloads
   the proposal and runs verification, **Then** verification succeeds
   without requiring any prior synchronization or fetch.
3. **Given** a `consume_notes` proposal referencing notes whose
   transport-side records have been pruned by the note transport
   service since proposal creation, **When** the cosigner runs
   verification, **Then** verification succeeds because the rebuild
   does not consult the transport.
4. **Given** a `consume_notes` proposal whose embedded note data does
   not match its declared note identifiers, **When** the cosigner
   runs verification, **Then** verification fails with an explicit
   integrity error rather than silently producing a different
   transaction summary commitment.

---

### User Story 2 - Proposer Can Re-Verify Their Own Proposal After A Local Wipe (Priority: P1)

As a proposer who originally created a `consume_notes` proposal and
then wiped my local Miden store (deliberately or as a side effect of
an unrelated workaround), I can re-open my own pending proposal and
re-verify it without having to recover the original note records.

**Why this priority**: This is the second-most-common failure
observed in practice. Host applications routinely clear the local
Miden store on app reload as a workaround for unrelated
cross-network issues; today, that workaround silently breaks the
proposer's ability to verify their own proposal.

**Independent Test**: Create a `consume_notes` proposal with the new
shape, clear the local Miden store, reload the app, and confirm the
proposal still verifies and can still be signed/executed from the
same device.

**Acceptance Scenarios**:

1. **Given** the proposer has created a new-shape `consume_notes`
   proposal and subsequently cleared their local store, **When** the
   proposer reloads the proposal, **Then** verification succeeds and
   the proposer can sign and submit a signature for their own
   proposal.

---

### User Story 3 - Cross-SDK Proposal Verification Produces Identical Commitments (Priority: P1)

As a cosigner using one multisig client (e.g. the Rust CLI) to verify
a `consume_notes` proposal that was created on the other multisig
client (e.g. the TypeScript browser SDK), I observe the same
transaction summary commitment that the proposer's client observed,
so my signature is for the same message and the proposal can
ultimately execute.

**Why this priority**: This is the cross-SDK instance of the same
invariant audit M-08 already remediated for P2ID. Without it,
multi-client multisig deployments cannot reliably co-sign
`consume_notes` proposals even after the local-store dependency is
removed.

**Independent Test**: Fix a deterministic `consume_notes` proposal
input (note set, salt, account state), build the new-shape metadata
on the Rust client, hand the proposal to the TypeScript client, run
verification on both sides, and confirm both clients produce the
same transaction summary commitment.

**Acceptance Scenarios**:

1. **Given** a new-shape `consume_notes` proposal built on the Rust
   client, **When** the TypeScript client verifies the same
   proposal, **Then** both clients produce identical transaction
   summary commitments.
2. **Given** a new-shape `consume_notes` proposal built on the
   TypeScript client, **When** the Rust client verifies the same
   proposal, **Then** both clients produce identical transaction
   summary commitments.

---

### User Story 4 - Legacy In-Flight Proposals Continue To Resolve After Upgrade (Priority: P2)

As an operator running a Guardian deployment with `consume_notes`
proposals already in flight at the moment the upgraded multisig
clients ship, I can continue to verify, sign, and execute those
legacy proposals through the transitional window using the
multisig client versions that match my deployment, without having
to re-create them.

**Why this priority**: Without a transitional path, every in-flight
`consume_notes` proposal at upgrade time becomes a stuck signing
session that the multisig protocol cannot resolve.

**Independent Test**: With both upgraded clients deployed, present a
proposal carrying the legacy metadata shape and confirm verification
takes the legacy code path, succeeding only on devices that have
the notes locally — i.e. the bug is still reachable for legacy
proposals, by design, because the legacy shape lacks the embedded
data, but the path is not removed.

**Acceptance Scenarios**:

1. **Given** an in-flight `consume_notes` proposal with the legacy
   metadata shape and a cosigner whose local store has the
   referenced notes, **When** the cosigner runs verification on the
   upgraded client, **Then** verification succeeds via the legacy
   code path.
2. **Given** an in-flight `consume_notes` proposal with the legacy
   metadata shape and a cosigner whose local store does not have
   the referenced notes, **When** the cosigner runs verification on
   the upgraded client, **Then** verification fails with an
   explicit error that names the legacy-shape limitation, so the
   cosigner understands the failure is not a bug in the upgraded
   path.

---

### User Story 5 - Legacy Proposals Are Rejected After The Documented Cut-Over (Priority: P2)

As a multisig client maintainer enforcing the documented cut-over
date, I can configure newly-shipped client versions to reject the
legacy metadata shape outright, so the codebase no longer has to
carry the local-store-dependent code path.

**Why this priority**: Carrying both shapes indefinitely doubles the
verification surface to maintain and audit. The transitional window
exists to drain in-flight legacy proposals, not to live forever.

**Independent Test**: With a post-cut-over client, present a
legacy-shape proposal and confirm verification refuses to dispatch
into the legacy path and returns an explicit "unsupported metadata
version" error.

**Acceptance Scenarios**:

1. **Given** a multisig client released after the documented
   cut-over, **When** it is asked to verify a legacy-shape
   `consume_notes` proposal, **Then** it refuses verification with an
   explicit unsupported-version error rather than attempting the
   legacy local-store read.

---

### User Story 6 - Proposal Submission Fails Closed When Payload Is Oversized (Priority: P3)

As a proposer creating a `consume_notes` proposal whose embedded
note data, in aggregate, exceeds the documented per-proposal
metadata size limit, I receive an explicit, actionable error at
proposal-creation time rather than a silently-truncated proposal or
an opaque downstream failure.

**Why this priority**: New-shape metadata is materially larger than
legacy metadata. Without an explicit size guard, edge cases (many
notes in one proposal, or notes with large scripts) could produce
proposals that fail later in the pipeline in ways that are hard for
the proposer to diagnose.

**Independent Test**: Attempt to create a `consume_notes` proposal
whose serialized new-shape metadata exceeds the documented limit and
confirm the multisig client refuses to construct the proposal with a
clear error.

**Acceptance Scenarios**:

1. **Given** a `consume_notes` proposal whose serialized new-shape
   metadata would exceed the documented per-proposal limit, **When**
   the proposer asks the multisig client to create the proposal,
   **Then** the client refuses with an error that names the limit
   and the actual size, before any signature collection begins.

---

## Requirements *(mandatory)*

### Functional Requirements

#### Metadata shape and versioning

- **FR-001**: The multisig client MUST define a new `consume_notes`
  proposal metadata shape that carries, in addition to the existing
  note identifiers, enough data to deterministically rebuild the same
  transaction request the proposer built, without consulting any
  per-device local Miden note store and without any network call.
- **FR-002**: The new metadata shape MUST include a discriminator
  field that explicitly identifies the shape's version, so the
  verification path can route to the correct rebuild logic without
  guessing. The new shape uses **version `2`**; the integer is
  reserved for the self-contained, notes-embedded form defined by
  this spec.
- **FR-003**: The discriminator field MUST be present on
  newly-created `consume_notes` proposals and MUST be interpreted as
  the legacy shape when absent, so legacy proposals already in flight
  at upgrade time are unambiguously identified. Legacy proposals
  either omit the discriminator entirely or carry the value `1`;
  both are treated identically as v1.
- **FR-004**: The new metadata shape MUST preserve the existing
  note-identifier field so dashboards, logs, and human operators can
  continue to refer to a `consume_notes` proposal by the same
  identifiers, and so the binding-integrity check in FR-007 has both
  sides of the relation it asserts.

#### Verification and rebuild

- **FR-005**: When verifying a new-shape `consume_notes` proposal,
  the multisig client MUST rebuild the transaction request entirely
  from the signed metadata. The rebuild MUST NOT read from the local
  Miden note store, MUST NOT contact the note transport service, and
  MUST NOT contact any Miden node.
- **FR-006**: The rebuild MUST be deterministic: the same
  new-shape metadata MUST always produce the same transaction
  summary commitment, on any device, on any client (Rust or
  TypeScript), at any time, regardless of local-store contents.
- **FR-007**: When verifying a new-shape `consume_notes` proposal,
  the multisig client MUST assert that the note identifiers carried
  in metadata correspond to the embedded note data: substituting any
  embedded note for a different note, or substituting any identifier
  for one that does not match its embedded note, MUST cause
  verification to fail with an explicit integrity error rather than
  silently producing a different transaction summary commitment.
- **FR-008**: When verifying a legacy-shape `consume_notes`
  proposal during the transitional window, the multisig client MUST
  preserve the prior behavior: it MAY read the referenced notes
  from the local store, and it MUST fail with a clear,
  legacy-shape-specific error message when those notes are absent,
  so the failure is distinguishable from the new-shape verification
  path.
- **FR-009**: When verifying a `consume_notes` proposal whose
  metadata version is recognized by the client but explicitly
  marked as unsupported (e.g. after the documented cut-over removes
  the legacy path), the multisig client MUST refuse verification
  with an explicit unsupported-version error rather than silently
  succeeding, silently failing, or attempting a partial rebuild.

#### Proposal creation

- **FR-010**: When creating a `consume_notes` proposal, the
  multisig client MUST produce a proposal whose metadata is in the
  new shape and whose embedded note data is sufficient for the
  verification path described by FR-005 through FR-007.
- **FR-011**: When creating a `consume_notes` proposal, the
  multisig client MUST refuse to construct a proposal whose
  serialized new-shape metadata would exceed the documented
  per-proposal metadata size limit, returning an explicit error that
  names the limit and the actual size at proposal-creation time.
  The limit is **`262_144` bytes (256 KiB)**, enforced symmetrically
  on the Rust and TypeScript SDKs against the wire-encoded metadata
  fragment.
- **FR-012**: The proposer's local Miden note store MAY be consulted
  during proposal creation to source the note data being embedded.
  Proposal creation is the one moment in the lifecycle when the
  proposer is expected to have the notes locally; this is unchanged
  from current behavior.

#### Execution

- **FR-013**: When executing a new-shape `consume_notes` proposal,
  the multisig client MUST rebuild the transaction request from the
  signed metadata using the same logic the verification path uses,
  so the executed transaction is bit-identical to the one the
  cosigners signed against.
- **FR-014**: The execution path MUST NOT consult the local Miden
  note store for new-shape proposals. The executing cosigner's
  local-store state MUST NOT affect whether the executed
  transaction matches the signed transaction summary commitment.

#### Cross-client parity

- **FR-015**: The Rust multisig client and the TypeScript multisig
  client MUST produce identical transaction summary commitments
  when rebuilding the same new-shape `consume_notes` proposal from
  the same signed metadata. Any divergence MUST be treated as a
  bug in this feature, by the same standard audit M-08 applied to
  P2ID.
- **FR-016**: The new metadata shape MUST be serialized identically
  by both clients so a proposal created on one client and submitted
  to the Guardian server is bit-identically reconstructed by the
  other client when it reads the proposal back.

#### Backwards compatibility and cut-over

- **FR-017**: Multisig client versions shipped as part of this
  feature MUST accept both the legacy and the new `consume_notes`
  metadata shapes for verification and execution, dispatching by
  the discriminator from FR-002 and FR-003. This is the
  transitional window.
- **FR-018**: A documented cut-over MUST be defined at which
  multisig client versions stop accepting the legacy
  `consume_notes` metadata shape. The cut-over MUST be expressed as
  a client-version threshold (e.g. "starting with multisig client
  version X.Y.Z+1") so operators can plan upgrades.
- **FR-019**: Multisig client versions shipped at or after the
  documented cut-over MUST refuse legacy-shape `consume_notes`
  verification per FR-009, with no fallback to the local-store
  read.
- **FR-020**: The transitional window MUST be long enough to allow
  a Guardian deployment with `consume_notes` proposals in flight at
  the moment the new clients ship to either reach signature
  threshold and execute, or to be re-proposed in the new shape, on
  realistic operator timelines. The exact duration is a release-
  planning concern but MUST be documented before the cut-over
  client ships.

#### Errors and observability

- **FR-021**: All new error conditions introduced by this feature
  (note-data binding mismatch, unsupported metadata version,
  oversized metadata, legacy-shape note-not-found) MUST be
  reported with explicit, code-pinned error identifiers so
  consuming applications and test harnesses can branch on them
  without string-matching the message.
- **FR-022**: The new error identifiers MUST be stable across
  Rust and TypeScript multisig clients: the same condition MUST
  surface the same identifier on both, so cross-client tests and
  dashboards can rely on one taxonomy.

### Contract / Transport Impact

- The Guardian server proposal-storage and proposal-retrieval HTTP
  and gRPC surfaces are unchanged in shape. The server stores and
  returns proposal metadata as it does today; the new fields are
  carried inside that metadata payload from the server's
  perspective.
- The multisig client public API for creating, verifying, signing,
  and executing `consume_notes` proposals is preserved at the call-
  site level; callers that build their input from note identifiers
  continue to do so. New error identifiers per FR-021 are added.
- No new operator dashboard endpoints are introduced. Dashboard
  proposal entries continue to expose only the fields defined in
  `005-operator-dashboard-metrics`.
- No new Guardian server feature flags are introduced. The new
  metadata shape is multisig-client-defined.

### Field Glossary

- `proposal_type` — existing proposal-type discriminator;
  remains `consume_notes` for both legacy and new shapes.
- `note_ids` — list of canonical note identifiers being consumed
  by the proposal; present in both legacy and new shapes.
- metadata version discriminator — the new field defined by FR-002
  identifying the metadata shape. Absent on legacy proposals (FR-003),
  present on new-shape proposals.
- embedded note data — the new field(s) defined by FR-001 carrying
  the data required to deterministically rebuild the transaction
  request without a local-store read.
- per-proposal metadata size limit — the documented upper bound on
  serialized new-shape metadata per FR-011.

### Data / Lifecycle Impact

- No new persistent entities are introduced on the Guardian server.
- The multisig client's in-flight proposal representation gains the
  new metadata fields; serialization across the existing transport
  is preserved per FR-016.
- The proposal lifecycle (creation, signature collection, threshold,
  execution, canonicalization) is unchanged in shape. Only the
  verification and execution rebuild steps inside the lifecycle
  change.

## Edge Cases *(mandatory)*

- **Verifier with empty local store**: A cosigner whose local Miden
  store is empty MUST be able to verify a new-shape `consume_notes`
  proposal. This is the central failure mode the feature exists to
  fix.
- **Proposer with wiped local store after creation**: A proposer who
  wipes their local store after creating a new-shape proposal MUST
  still be able to verify and sign their own proposal.
- **Private note pruned by transport**: A new-shape proposal MUST
  verify even after the note transport service has pruned the
  referenced note records, because verification does not consult
  the transport.
- **Mismatched embedded note vs. identifier**: A new-shape
  proposal whose embedded note data does not match its declared
  note identifiers MUST fail verification with an explicit binding
  error per FR-007. Verification MUST NOT silently rebuild a
  different transaction.
- **Legacy proposal during transitional window with notes present**:
  A legacy-shape proposal on a device that does have the referenced
  notes locally MUST continue to verify by the legacy path.
- **Legacy proposal during transitional window with notes absent**:
  A legacy-shape proposal on a device that does not have the
  referenced notes locally MUST fail with a legacy-shape-specific
  error that distinguishes it from the new-shape path.
- **Legacy proposal after cut-over**: A legacy-shape proposal
  presented to a post-cut-over client MUST be refused with an
  unsupported-version error per FR-009.
- **Cross-client verification**: The same new-shape proposal MUST
  rebuild to the same transaction summary commitment on both the
  Rust and the TypeScript multisig clients.
- **Oversized new-shape metadata**: A `consume_notes` proposal whose
  serialized new-shape metadata exceeds the documented limit MUST
  be refused at proposal-creation time per FR-011, not after
  signatures have been collected.
- **Multiple notes in one proposal**: A new-shape proposal that
  consumes multiple notes MUST embed enough data per note to satisfy
  FR-005 for every referenced note, and MUST be rejected by FR-011
  only when the aggregate exceeds the documented limit.
- **Empty note set**: The existing requirement that a
  `consume_notes` proposal contains at least one note is unchanged;
  attempts to create a proposal with no notes continue to be
  rejected at creation time.
- **Unknown metadata version on a future client**: A client that
  encounters a metadata version it does not recognize MUST refuse
  verification per FR-009 rather than fall back to the legacy
  local-store path.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A cosigner whose local Miden store contains none of
  the notes referenced by a new-shape `consume_notes` proposal can
  verify the proposal and produce a valid cosigner signature in
  100% of the seeded validation runs across both Rust and
  TypeScript multisig clients.
- **SC-002**: A proposer who wipes their local Miden store after
  creating a new-shape `consume_notes` proposal can re-verify and
  sign their own proposal from the same device in 100% of seeded
  validation runs.
- **SC-003**: For a fixed deterministic input set (notes, salt,
  account state), the Rust and TypeScript multisig clients produce
  identical transaction summary commitments when rebuilding the
  same new-shape `consume_notes` proposal in 100% of cross-client
  validation runs.
- **SC-004**: Any new-shape `consume_notes` proposal whose embedded
  note data does not match its declared note identifiers fails
  verification with the explicit binding error in 100% of seeded
  validation runs; no such mismatched proposal silently produces a
  different transaction summary commitment.
- **SC-005**: A legacy-shape `consume_notes` proposal verified on a
  transitional-window client with the referenced notes locally
  succeeds in 100% of seeded validation runs; verified on a
  transitional-window client without the notes locally, it fails
  with the legacy-shape-specific error in 100% of seeded
  validation runs.
- **SC-006**: A legacy-shape `consume_notes` proposal verified on
  a post-cut-over client is refused with the unsupported-version
  error in 100% of seeded validation runs and never falls back to
  a local-store read.
- **SC-007**: Every new error condition introduced by this feature
  surfaces with the same stable, code-pinned identifier on both
  the Rust and the TypeScript multisig clients in 100% of seeded
  validation runs.
- **SC-008**: The Rust CLI demo's `consume_notes` walkthrough and
  the TypeScript smoke-web harness's `consume_notes` walkthrough
  each include an automated step that reproduces the local-store
  failure mode on the legacy shape and confirms it does not
  reproduce on the new shape.
- **SC-009**: 0 successful verifications of a new-shape
  `consume_notes` proposal involve any read from the local Miden
  note store, any call to the note transport service, or any call
  to a Miden node, across validation runs.

## Assumptions

- The Miden transaction kernel will continue to require the full
  note object (recipient/assets/inputs/script) at execution time;
  the rebuild step cannot be reverted to the original
  identifier-only construction even after this feature ships.
- The per-proposal metadata size limit is set at 256 KiB (FR-011),
  high enough to accommodate the common-case `consume_notes` proposal
  (small numbers of notes with standard P2ID-style scripts) without
  triggering the cap.
- The Guardian server's proposal storage column type is large
  enough to hold the new-shape metadata for the common case; if
  not, a separate plan-phase decision will resize it. The server
  itself does not interpret the new fields.
- The transitional window is operator-driven: deployments are
  expected to drain or re-propose in-flight legacy `consume_notes`
  proposals before upgrading past the cut-over client version.
  Guardian itself does not enforce drain.
- Cross-SDK verification equivalence is asserted by a test that
  fixes a deterministic input set. Non-determinism in the rebuild
  (e.g. unseeded RNG) is treated as a bug per FR-006 and FR-015.
- The new metadata fields are signed by the same mechanism that
  signs the rest of the proposal metadata today; no new signing
  scheme is introduced. The binding from cosigner signature to
  the embedded note data is inherited from the existing summary-
  commitment chain.
- The proposer is the trusted source of note data at proposal-
  creation time; FR-007 protects cosigners from a proposer who
  later lies about what they consumed, but does not protect
  cosigners from a proposer who creates a proposal against a
  legitimate note they do not actually wish to consume. The
  multisig threshold remains the authority on whether the
  proposal proceeds.

## Dependencies

- Guardian's existing multisig proposal model, signature
  collection, threshold, and execution semantics, which this
  feature extends but does not redefine.
- Audit finding M-08 and its remediation pattern in PR #148, which
  serve as the precedent and template for the cross-client rebuild
  equivalence requirement.
- The Miden SDK's note serialization surface, which the embedded
  note data in FR-001 uses. The exact serialization is a
  plan-phase concern, constrained by FR-016.
- The Rust CLI demo (`examples/demo`) and the TypeScript smoke-web
  harness (`examples/smoke-web`), which are extended with the
  reproduction steps in SC-008.
- GitHub issue #229, which this feature resolves.
