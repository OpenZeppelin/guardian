# Feature Specification: Guardian Proves and Commits Transactions

**Feature Branch**: `254-guardian-prove-and-commit`
**Created**: 2026-07-27
**Last Revised**: 2026-09-30 (review revision 11: re-verified against the Miden 0.17 release-candidate pins on `main`, protocol / standards / tx `0.17.0-rc.7`, `miden-client` `0.17.0-rc.4`, `miden-node-proto-build` `0.17.0-rc.3`, web SDK `0.17.0-rc.4`. The signed summary binds a proposer-chosen bound block, so reproduction runs at the chain tip (FR-056, FR-061); foreign public accounts are in scope (FR-050); expiration has an approval bound and a transaction bound (FR-051); fee info travels in the request's auth arg (FR-057); submission inputs are sealed before the boundary (FR-059). Revision 10's anchored design is withdrawn, see RFC 0001 Appendix A.3)
**Status**: Draft
**Input**: Issue #254 (parent: #253 META - Transaction Orchestration): "Enable the Guardian to handle the full prove-and-commit lifecycle for a transaction. The user submits a signed `TransactionSummary` to the Guardian; the Guardian generates the ZK proof; the Guardian submits the proven transaction to the Miden network."

## Context *(why this feature exists)*

Guardian's multisig custody splits work between two parties. The **client** builds a
transaction, derives its canonical summary, collects cosigner signatures through
Guardian, and then — once the signing threshold is met — executes, proves, and submits
the transaction to the Miden network itself. **Guardian** is a coordinator: it validates
the supplied summary's encoding and stored-state commitment consistency, records the
proposal's base commitment, accumulates signatures, issues the
acknowledgment that satisfies the on-chain guardian gate, and observes the chain
read-only to canonicalize the resulting delta.

That split makes every executing party a full Miden client: it must assemble MASM,
maintain a synced local store, reach a Miden node, and carry the cost and latency of ZK
proof generation. In a browser that means a wasm bundle and a synced IndexedDB store; on
a server integration it means embedding the whole SDK to press one button. The party
that *decides* to execute and the party *capable of* executing are forced to be the same
party.

This feature adds a supported path where the account's cosigners can hand a
threshold-met proposal to Guardian and have Guardian carry it the rest of the way:
verify the collected signatures, reproduce the exact transaction the cosigners signed,
generate the proof, and submit it on-chain. Triggering execution becomes a single
authenticated request that any cosigner can make from anywhere, with no Miden
capability of its own.

Two properties are deliberately preserved. Self-execution is **not** deprecated or
degraded — it remains the default and the only path that requires no trust in
Guardian's liveness. And Guardian gains **no new authority over account state**: the
transaction it submits must reproduce, bit for bit, the summary commitment the
cosigners already signed, or it is refused before anything is proven or submitted. What
Guardian gains is the ability to *carry out* an already-authorized transaction, not to
author one.

This is the foundation issue of #253. Batch proving (#239), operator-delegated proving
(#180), and the canonical nonce endpoint (#191) build on the capability specified here.

## Execution Architecture *(ratified; Gate 0 narrowed — see research.md)*

Reproducing a transaction requires substantially more than the serialized transaction
request: an authenticated partial account, a reference block header, a partial
blockchain, note scripts, and vault and storage-map witnesses.
The architecture is therefore specified here rather than deferred, because its choice
determines observable restart, persistence, and multi-replica behavior.

**Status: ratified for lifecycle implementation.** The Gate 0 spike was built and run. It
established the architecture below empirically, not by reading upstream code:

- The `DataStore` seam works against Guardian's own state, with **no new dependencies**.
- `PartialBlockchain` assembly from node RPC alone was validated against public testnet,
  including cold-start peak acquisition (~0.6 s on a 1,002,185-block chain). On protocol 0.17
  this is the normative construction again (FR-061): the executor reproduces at the chain tip,
  and the partial blockchain it assembles there must track the summary's bound block and every
  authenticated note block (see the 0.17 note below). Revision 10 had demoted it to historical
  evidence in favour of the proposal's `ChainAnchor`; that design is withdrawn. The spike was
  last built on the 0.16 release candidates and must be ported before its tests count.
- The full authorized path — select signatures, verify binding, acknowledge, execute,
  prove — completed end to end, with the proof produced by a **remote prover**.

**Gate 0 is deliberately narrowed, and the residue is named rather than implied.** Of the
four proposal families, P2ID and configuration executed and proved under `MockChain`;
`consume_notes` executed from a prepared store but its live-RPC note-block path is
unexercised; the custom family (#266) is unrun and is structurally identical to the others
(an opaque script through the same seam). Live **submission** is unvalidated and needs a
funded, Guardian-registered account.

That residue does not falsify the `DataStore` seam (it is the same seam in every case), but
the consume-notes item is no longer only deferred coverage: the note consumption mode enters
the signed summary and is classified from the executing client's store, so a storeless
executor reproduces a different summary unless the request pins its notes (FR-056). It remains
tracked in `validation-matrix.md`, and the items that could still surprise (pinned-note
reproduction, foreign-account loading at the tip, sealed submission) are called out there
explicitly.

**Guardian implements `miden_tx::DataStore` directly, answering each query from the account
state it already holds plus chain data read at execution time.** For each execution attempt it
builds ephemeral state: an SMT forest over the account for witness queries, the reference block
header at the node's committed tip together with the `ProtocolConfig` that header commits to,
and a partial blockchain at that tip tracking the summary's bound block and every authenticated
note block. It runs the transaction and discards that state when the attempt ends.

Gate 0 round 1 established that the ready-made bridge from miden-client (`ClientDataStore`) is
crate-private and unusable, so implementing miden-client's `Store` trait would be pointless:
nothing public consumes it. Re-verified on `miden-client` 0.17.0-rc.4: the module is still
`pub(crate)` (`src/store/mod.rs:65-71`), the only re-export sits inside the `testing` feature
module (`src/lib.rs:345-358`), and `Store` has 61 methods of which 45 are required
(`src/store/mod.rs:191-839`). The direct `DataStore` surface is five methods plus the
`MastForestStore` supertrait (`miden-tx-0.17.0-rc.7/src/executor/data_store.rs:19-96`), and the
witness assembly is largely supplied by the public `AccountSmtForest`. See `research.md`.

**Protocol 0.17 changes the execution inputs (re-verified 2026-09-30).**

- **The summary binds a chosen block, not the reference block.** `TransactionSummary` carries a
  `block_number` and that block's commitment (`miden-protocol-0.17.0-rc.7/src/transaction/tx_summary.rs:23-39`).
  The guarded-multisig auth procedure binds the proposer's bound block through
  `create_tx_summary_with_block`, which requires the bound block to be at or before the
  reference block (`miden-standards-0.17.0-rc.7/asm/standards/auth/mod.masm:48-49,67-99`); the
  kernel resolves an older block through the partial blockchain
  (`miden-protocol-0.17.0-rc.7/asm/kernels/transaction-core/src/tx.masm:98-130`). Both SDKs on
  `main` therefore verify, sign and execute at the tip. Executing at the proposal's anchor is
  not only unnecessary but unworkable: fee payment loads the chain's fee faucet as a foreign
  account at the reference block, and devnet prunes account state after about 50 blocks
  (#462). Guardian reproduces at the tip under the signed bound block (FR-056, FR-061). The
  proposal's `chain_anchor` field stays on the wire for SDK compatibility; Guardian execution
  does not read it.
- **The executor does not ask for the bound block.** `ref_blocks` holds the input-note blocks
  and the reference block only (`miden-tx-0.17.0-rc.7/src/executor/mod.rs:280-281`), and lazy
  block-witness loading is unimplemented (`src/host/tx_event.rs:491-494`). Guardian's
  `DataStore` adds the request's declared `block_numbers()` itself, as `ClientDataStore` does
  (`miden-client-0.17.0-rc.4/src/store/data_store/mod.rs:335-342`) (FR-061).
- **The data store returns a `ProtocolConfig`** that must be the one the returned header commits
  to (`data_store.rs:29-44`). The fee asset left the block header for it (FR-060).
- **The auth procedure pays the fee inside the signed summary, and the request carries the fee
  info.** The auth arg commits to `[bound_block, approval_expiration, 0, 0] || SALT ||
  CONVERSION_INFO` (`miden-standards-0.17.0-rc.7/asm/standards/auth/multisig.masm:827-854`), and
  both SDKs set it and its preimage on every request. `miden-client`'s own fee path commits a
  two-word preimage the 0.17 procedure rejects, and leaves a request that already carries an auth
  arg untouched (`miden-client-0.17.0-rc.4/src/transaction/mod.rs:1529-1576`). Guardian commits
  nothing and repairs nothing (FR-057).
- **Foreign accounts are on the ordinary path.** Moving an asset whose faucet sets the callback
  flag opens a foreign context against that faucet
  (`miden-protocol-0.17.0-rc.7/asm/kernels/transaction-core/src/callbacks.masm:102-123`), which
  devnet's fee faucet does on every fee payment, and fee sponsorship of network notes prices
  them through foreign procedure invocation
  (`miden-standards-0.17.0-rc.7/asm/standards/fee/mod.masm:349-352`). V1 therefore loads public
  foreign accounts at the reference block (FR-050).
- **Approval expiration is a signed, absolute bound.** It aborts the VM once the reference block
  reaches it and then lowers the transaction's expiration to at most that block
  (`multisig.masm:884-914,931-942`). Guardian-executable proposals must carry one, alongside a
  built-in relative transaction delta (FR-051), and Guardian stops before proving once either is
  reached (FR-058).
- **Submission carries sealed transaction inputs** encrypted to the validator set's key
  (`miden-node-proto-build-0.17.0-rc.3/proto/types/submission.proto:8-28`). Guardian keeps the
  `TransactionInputs` from execution and seals them before the no-retry boundary (FR-059).

The consequences are normative:

- **Guardian holds no synced chain state between executions.** There is no sync loop, no
  incremental chain-following, and no long-lived per-account store.
- **No cross-execution store state can diverge**, so multi-replica deployments need no
  store coherence protocol. The durable state this feature introduces is only the execution
  reservation (FR-023), the pre-submission submission evidence (FR-039), and the persisted
  terminal outcome (FR-041), none of it chain data.
- **Restart recovery needs no store replay**: there is no store to recover, so recovery is
  purely a question of what the execution record says. It is *not* trivial for an execution
  that had already submitted (that requires chain reconciliation, FR-040), but the
  ephemeral store contributes nothing to that problem.
- **The reference block is the node's committed tip at attempt start.** The signed summary binds
  the bound block, not the reference block, so any reference block at or after the bound block
  reproduces it, subject to fee drift (see below). The bound block's commitment is then proven
  by an MMR path under the tip header's chain commitment, which is a stronger check than
  comparing two headers served by one node.
- **There is no `Store` and no embedded database.** Guardian answers `DataStore` queries
  from data it already holds plus chain reads performed at execution time: no sqlite
  dependency, no temporary directory, no persisted MMR.
- **Cold start per attempt** is the genesis block commitment (trusted configuration or a read),
  one `SyncChainMmr` call seeded at genesis for the tip peaks, one
  `GetBlockHeaderByNumber(include_mmr_proof)` per tracked block (the bound block plus each
  authenticated note block), the `ProtocolConfig` for the tip header, and one account read per
  foreign account the execution loads. The `SyncChainMmr` delta is **compact: the peak set,
  logarithmic in chain length, not proportional to it**, so a per-execution cold start is
  affordable and **no persistent MMR cache is required**. A process-local cache is permitted only
  if measured latency justifies it, and MUST NOT be durable.
- **Seeding cost is per execution** and is expected to be small relative to proving, but
  it is not free; SC-011 records it.
- **Fee drift is a known reproduction risk.** The fee is `(ilog2(clk + extra) + 1) ×
  verification_base_fee`, with the base fee taken from the reference block header
  (`miden-protocol-0.17.0-rc.7/asm/kernels/transaction-core/src/tx.masm:309-339`), and the fee
  note enters the signed output-notes commitment. If the base fee changes, or the cycle count
  crosses a power of two, between the proposer's execution and Guardian's, the reproduction
  fails the binding check. The SDK cosigners carry the same risk today. It is classified as a
  binding mismatch, not repaired, and diagnosed in logs (FR-007).

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Hand a threshold-met proposal to Guardian (Priority: P1)

A cosigner of a multisig account sees a proposal that has collected enough signatures.
Instead of executing it locally — which would require a synced Miden client and minutes
of local proving — the cosigner asks Guardian to execute it. The request is accepted
immediately and returns an execution handle. Guardian verifies the collected signatures,
reproduces the signed transaction, proves it, and submits it to Miden. The account state
advances exactly as it would have under self-execution.

**Why this priority**: This is the entire capability the issue asks for. Without it,
nothing else in #253 has a foundation. It is independently valuable on its own: it
removes the Miden-client requirement from the act of executing.

**Independent Test**: Create and sign a proposal to threshold, call the execute request
as a cosigner, poll the execution state to completion, and confirm the on-chain account
commitment matches what self-execution would have produced — and that the resulting
delta canonicalizes through the normal lifecycle.

**Acceptance Scenarios**:

1. **Given** a proposal that has met its effective signing threshold and was created as
   Guardian-executable, **When** a cosigner requests Guardian execution, **Then** the
   request is accepted immediately with an execution handle, and the transaction is
   subsequently proven and submitted without further client involvement.
2. **Given** that accepted request, **When** the caller polls the execution state,
   **Then** it observes an explicit progression through the states defined in FR-024 and
   never an ambiguous or absent state.
3. **Given** a completed Guardian execution, **When** the resulting delta is inspected,
   **Then** it has moved through the same candidate → canonical lifecycle as a
   self-executed delta, with no new or special-cased delta status values.
4. **Given** a proposal that has **not** met its effective threshold, **When** Guardian
   execution is requested, **Then** it is refused synchronously per FR-022 and nothing is
   proven or submitted.
5. **Given** a caller who is not a cosigner of the account, **When** Guardian execution
   is requested, **Then** it is refused on authentication/authorization grounds and
   nothing is proven or submitted.

---

### User Story 2 - Execution is bound to what the cosigners actually signed (Priority: P1)

The cosigners signed a specific transaction summary. Before Guardian spends any proving
resources or touches the network, it independently reproduces that transaction against
the account's current state and confirms the result reproduces the exact summary
commitment the signatures were made over. It also verifies each collected signature
against that commitment and against the account's registered cosigner set. Any
mismatch — a superseded account state or a tampered stored request — stops the execution
before it starts. Invalid or non-cosigner signature entries are ignored rather than fatal,
so one bad entry cannot strand a proposal.

**Why this priority**: Also P1, and not separable from US1 in value: US1 without this
check would let Guardian submit something other than what was authorized. This story is
what makes delegating execution safe rather than merely convenient, and it is where the
feature's entire trust argument lives.

**Independent Test**: Drive Guardian execution against (a) a proposal whose account has
advanced past the proposal's base state and (b) a stored request mutated so it no longer
reproduces the signed commitment — and confirm each is refused before any proving or
submission, with a distinguishable error per cause. Separately, confirm the signature-set
behavior: (c) a proposal carrying a non-cosigner or invalid entry **alongside enough valid
signatures** still executes, with the bad entry ignored and recorded; and (d) one whose
*valid* set is below the effective threshold is refused as not-ready — not as a signature or
binding error.

**Acceptance Scenarios**:

1. **Given** a Guardian-executable proposal whose account state has advanced past the
   state the proposal was built on, **When** Guardian execution is attempted, **Then**
   it is refused with a state-mismatch error before any proving or submission.
2. **Given** a stored transaction request that does not reproduce the signed summary
   commitment, **When** Guardian execution is attempted, **Then** it is refused with a
   binding error before any proving or submission, and the failure is distinguishable
   from a not-ready or state-mismatch failure.
3. **Given** a proposal whose *valid* signature set is below the effective threshold —
   because entries are invalid, duplicated, or from non-cosigners — **When** Guardian
   execution is attempted, **Then** it is refused as not-ready before any proving or
   submission. **And given** enough valid signatures alongside a bad entry, **Then**
   execution proceeds, ignoring and recording the bad entry (FR-006).
4. **Given** any refused Guardian execution, **When** the account is inspected,
   **Then** the account is not locked, no delta was recorded, no reservation is held, and
   the proposal remains available to be executed by its cosigners through any supported
   path.

---

### User Story 3 - Configuring the client, and self-execution staying intact (Priority: P2)

An integrator decides once, when constructing the SDK client, whether the proposals it
creates may be executed by Guardian. Proposals from a self-executed client — the default —
behave exactly as they do today: nothing extra is stored, and only a party able to build the
transaction can execute them. Proposals from a Guardian-executable client carry what
Guardian needs. No existing method signature changes, and a cosigner can always still
execute either kind locally.

The client never asks the server what it supports, and the server never rejects a proposal
over it. The two decide independently: the client decides what to attach, the server decides
whether it offers execution. Any mismatch surfaces when execution is requested, not when a
proposal is created.

**Why this priority**: P2 because it protects existing behavior rather than delivering
the new capability. It is nonetheless required for correctness: feature 008 FR-015
established that the serialized transaction request is not persisted, and this feature
must narrow that rule deliberately and visibly rather than silently reverse it.

**Independent Test**: Create the same transaction type through a default client and a
Guardian-executable client. Confirm the default client's stored payload is unchanged from
today's shape and that Guardian execution of it is refused with a clear
not-Guardian-executable error; confirm the other succeeds; confirm local execution works for
both. Then point a Guardian-executable client at a server with proving disabled and confirm
creation still succeeds with the attachment stored.

**Acceptance Scenarios**:

1. **Given** a proposal created by a self-executed client, **When** its stored
   payload is inspected, **Then** it carries no serialized transaction request and is
   shape-identical to a proposal created before this feature.
2. **Given** that proposal, **When** Guardian execution is requested, **Then** it is
   refused with a clear error stating the proposal is not Guardian-executable, and
   nothing is proven or submitted.
3. **Given** either kind of proposal at threshold, **When** a cosigner executes it
   locally through the existing flow, **Then** it succeeds exactly as before this
   feature.
4. **Given** a Guardian-executable client and a server with proving disabled, **When** a
   proposal is created, **Then** creation succeeds, the attachment is stored, and the
   mismatch surfaces only when execution is requested — as a capability-unavailable error.
5. **Given** an SDK upgraded to a version supporting this feature but with no execution
   mode configured, **When** a proposal is created, **Then** no transaction request is
   attached, so an upgrade alone never changes what data leaves the integration.
6. **Given** a Guardian-executable proposal, **When** cosigners list, review, sign, and
   export it, **Then** all of those behave identically to a proposal without the stored
   request.
7. **Given** the same scenario driven through the Rust SDK and the TypeScript SDK,
   **When** each is configured Guardian-executable and requests Guardian execution,
   **Then** the observable behavior and outcomes match.

---

### User Story 4 - One submission, even under concurrency and crashes (Priority: P1)

Two cosigners press the button at the same moment; two Guardian replicas poll the same
work; a replica dies mid-proof; the Miden node times out on submission without saying
whether it accepted the transaction. In every case there is **at most one
lease-authorized proving attempt at a time and at most one on-chain submission**, and no
situation leaves the account indefinitely locked with nothing making progress. Two proofs may
genuinely be computing at once — the prover is external and cannot be cancelled, so a stale
worker's request can still be in flight — but a stale result is rejected and never carried
forward (FR-029). Wasted prover cost is accepted; a second submission is not.

**Why this priority**: P1. This is a correctness requirement, not a robustness nicety:
a double submission is a double state transition attempt on a custody account, and an
account left locked with no progress is a custody outage. The existing pending-candidate
lock cannot provide this, because it only exists once a candidate delta has been
persisted — which is after proving.

**Independent Test**: Issue concurrent execution requests for the same proposal from
multiple callers and multiple replicas and confirm exactly one submission; kill a replica
mid-proof and confirm the reservation expires and the account becomes usable again; simulate
a submission timeout and confirm the execution reports `submitted`, retains its reservation,
and blocks retry until the chain is observed.

**Acceptance Scenarios**:

1. **Given** a Guardian-executable proposal at threshold, **When** two cosigners request
   execution concurrently, **Then** exactly one reservation is created, at most one
   lease-authorized proving attempt exists at a time, exactly one submission occurs, and the
   second request observes the first execution rather than starting a new one.
2. **Given** two Guardian replicas, **When** both observe the same queued execution,
   **Then** exactly one performs it and the other does not prove or submit.
3. **Given** an execution holding a reservation, **When** a client attempts to push a
   delta for the same account, **Then** it is refused for the duration of the
   reservation, so a client cannot obtain an acknowledgment for a competing transaction
   mid-execution.
4. **Given** a replica that dies while proving, **When** the reservation's lease expires,
   **Then** the execution is reported failed, the reservation is released, the account is
   unlocked, and the proposal remains executable.
5. **Given** a submission that times out or whose connection drops, **When** the
   execution state is inspected, **Then** it reports `submitted`, retry is refused, the
   reservation is retained, and the state resolves to `committed` or `failed` only after
   chain observation establishes which occurred.

---

### User Story 5 - Operators control whether the capability exists at all (Priority: P3)

An operator deploying Guardian decides whether this server offers prove-and-commit, and
where proving happens. A deployment that has not been given a prover refuses execution
requests explicitly rather than attempting them. An operator can disable the capability
independently of prover reachability.

**Why this priority**: P3 because it gates rather than delivers capability, but it is
required before the feature can ship to a shared deployment: proving is a resource the
operator provisions, not something the server can assume.

**Independent Test**: Start the server with no prover configured and confirm execution
requests are refused with an explicit capability-unavailable error and never attempt
proving; start it with the capability disabled and confirm the same; start it with a
reachable prover and confirm execution succeeds.

**Acceptance Scenarios**:

1. **Given** a server with no prover configured, **When** Guardian execution is
   requested, **Then** it is refused with an explicit capability-unavailable error and
   no attempt is made to prove or submit.
2. **Given** a server with the capability explicitly disabled, **When** Guardian
   execution is requested, **Then** it is refused with the same explicit error class
   regardless of prover reachability.
3. **Given** a server whose configured prover is unreachable or fails, **When** Guardian
   execution is requested, **Then** the request is still accepted, the execution
   ultimately reports a proving failure (or, when an unreachable prover is retried until the
   transaction's expiration, the reached transaction expiration), the reservation is
   released, and the account is left unlocked with the proposal still executable by other
   paths.

---

### Edge Cases

- **Account already has a pending candidate**: refuse the execution request
  synchronously; do not create a reservation and do not queue behind the lock.
- **Account already has an active reservation for a different proposal**: refuse
  synchronously. At most one execution reservation exists per account at a time.
- **Account already has an active reservation for the same proposal**: the request is
  idempotent — it returns the existing execution's handle rather than creating a second.
- **Account is paused or released**: refuse synchronously on the same grounds as any
  other mutating operation, before any work.
- **Account's guardian is switched while an execution is queued or proving**: the
  execution is failed rather than submitted; a released account never has transactions
  submitted on its behalf.
- **Reservation holder dies**: the lease expires and the execution is failed and
  released. It is not silently retried, so the account never sits locked with nothing
  progressing.
- **Proving exceeds the prover's configured timeout**: treated as a transient prover
  failure and retried under FR-055; it settles as a proving failure only when the failure
  proves permanent. When retrying runs until the transaction's expiration is reached, it
  settles `GUARDIAN_EXECUTION_EXPIRATION_REACHED` with `meta.bound: "transaction"` (SC-042),
  and the message carries the last prover error. On either terminal failure the reservation
  is released, the account unlocked, and the proposal still executable; a new request gets a
  fresh transaction window at the new tip.
- **Submission returns a definite rejection** (the node rejects the proven transaction):
  reported `failed` with a submission cause. A candidate exists by construction (FR-045 step
  12); since Guardian knows nothing committed it discards its own candidate and releases the
  reservation (FR-032).
- **Submission outcome is unknown** (timeout, dropped connection, or crash between send
  and response): reported `submitted`, the reservation is *retained*, and retry is refused
  until chain observation resolves it. This is the case where an optimistic release would
  risk a double submission. It is not distinguished on the wire because the caller's
  contract is identical to a known submission (FR-030).
- **Worker commits the boundary, goes stale, then wakes**: the pre-send fence re-check aborts
  it; it writes nothing and sends nothing, and reconciliation resolves the durable candidate
  after trustworthy observation reaches the expiration horizon (FR-049).
- **`SwitchGuardian` canonicalizes between the final admissibility read and the send**: not
  observable, so not preventable. The proof is stale against the post-switch account, the node
  rejects it, and it settles as a definite submission rejection (FR-048).
- **Stored request is not deserializable** (corrupt, truncated, or serialized by an
  incompatible Miden protocol line): refused with a version/codec error distinguishable
  from a binding mismatch; nothing proven or submitted.
- **Stored request exceeds the configured size limit at creation**: proposal creation is
  refused, rather than accepting a proposal that can never be Guardian-executed.
- **Proposal type is custom (#266)**: Guardian treats the stored request as opaque and
  needs no knowledge of what the transaction does. Its effective threshold is the account
  default per FR-005.
- **Proposal invokes a procedure with a threshold override**: readiness is evaluated
  against the override, not the account default.
- **Signatures collected offline never reach Guardian's stored proposal**: Guardian
  evaluates readiness only from the signatures its own record holds, so such a proposal
  is refused as not-ready. See FR-018.
- **Two different proposals for the same account both at threshold**: the reservation
  serializes them; the second is refused synchronously rather than queued.
- **Worker resumes after losing its lease**: the pre-submission fence check rejects it; it
  cannot submit behind the new owner's back (FR-038).
- **Worker dies after the prover returns but before recording the proof**: the retry proves
  again. Wasted prover cost, accepted; a second submission is still prevented (FR-029).
- **Account never leaves its base commitment after an unknown submission**: not evidence of
  a drop. Terminates only when the chain passes the recorded expiration block (FR-040).
- **Candidate and proposal both deleted by canonicalization**: the execution's terminal
  outcome was persisted before deletion and remains readable, flagged as no-longer-retryable
  (FR-041, FR-042).
- **One cosigner submits a garbage signature**: ignored and recorded; execution proceeds if
  the remaining valid signatures meet the threshold (FR-006).
- **Server runs in optimistic delta-commit mode**: Guardian execution is refused, and the
  misconfiguration is reported at startup (FR-043).
- **Execution requested long after the proposal was built**: reproduction runs at the tip, so
  a proposal stays executable after the node has pruned account state at its bound block; only
  its approval expiration ends it (FR-056, FR-051).
- **Approval expiration already reached, or reached while proving**: refused before proving
  with a reached-expiration error; the proposal must be re-proposed (FR-058).
- **Guardian's node is behind the proposer's bound block**: the attempt fails pre-boundary as
  retryable; the caller retries once the node catches up (FR-056).
- **Account cannot pay the fee**: reproduction aborts; reported as insufficient fee, not as a
  binding mismatch, and the account stays unlocked (FR-057).
- **Fee parameters or the cycle bucket changed since the proposer's execution**: the reproduced
  fee note differs, so the binding check fails; the proposal needs to be rebuilt. Logged with
  the differing fee notes so it is distinguishable from tampering (FR-007).
- **Transaction loads a private foreign account**: refused before proving with a
  foreign-account error naming the reason (FR-050).
- **The node's transaction encryption key cannot be fetched or its attestations do not
  validate**: refused before the boundary with a sealing error; nothing is sent (FR-059).

## Requirements *(mandatory)*

### Functional Requirements

Requirement IDs are stable and assigned in order of first authoring, then placed in the
section they belong to — so numbering is not sequential within a section. An ID always
refers to the same requirement across revisions.

#### Surface and authorization

- **FR-001**: Guardian MUST expose an authenticated, per-account operation that requests
  execution of a specific proposal, and a separate operation that reports that
  execution's state. Both MUST be available with equivalent semantics on the HTTP and
  gRPC surfaces.
- **FR-002**: The execution request MUST be restricted to cosigners of the account,
  using the existing per-account authentication and replay-protection scheme. No new
  authentication mechanism is introduced.
- **FR-003**: The execution request MUST be accepted or refused promptly and MUST NOT
  block for the duration of proving. Acceptance MUST return a handle sufficient to
  observe the execution's state.
- **FR-004**: The state-reporting operation MUST be authorized to the same cosigner set
  as the request operation, and MUST NOT disclose execution state to non-cosigners.
- **FR-036**: An in-flight execution MUST be discoverable without polling each proposal
  individually. Guardian MUST provide an account-level read reporting the account's
  current execution, if any, and the execution-conflict refusal (FR-008) MUST name the
  blocking proposal in its error payload. Neither MUST change the shape of any existing
  response, because response-shape changes propagate to both base clients and both
  multisig SDKs.

#### Proposal admission and readiness

Creation MUST NOT execute the attached request or claim request-to-summary verification.
The client derives the summary and supplies signatures; Guardian stores the proposal and
its base commitment. Reproduction belongs to FR-007 after execution acceptance. A ready
proposal still requires an explicit execution request; creation and signing MUST NOT
reserve the account or dispatch execution. No preparation endpoint is added in v1.

V1 permits competing proposals on the canonical base but no dependent in-flight chain.
A proposal whose recorded base no longer matches canonical state cannot execute in v1.
This is an admission/execution policy, not a universal property of signed summaries.

#### Readiness and binding

- **FR-005**: Before any proving or network submission, Guardian MUST verify that the
  proposal has met the **effective threshold for the procedure the proposal invokes** —
  the per-procedure override when one is configured for that procedure, otherwise the
  account default. It MUST read these from the account's own state, not from the
  client-supplied `required_signatures` metadata field. For proposals whose type is
  custom, the effective threshold is the account default. Guardian's separate
  acknowledgment MUST NOT count toward this cosigner threshold. Mapping this model to
  the upstream wallet's general 2-of-3 quorum is an open integration question; v1 does
  not change the account authorization contract.
- **FR-006**: Before any proving or network submission, Guardian MUST cryptographically
  verify each collected cosigner signature against the signed summary commitment and confirm
  its signing key belongs to the account's registered cosigner set. It MUST then **select the
  set of distinct, valid, currently-registered cosigner signatures**, count readiness over
  that set, and build the execution advice from **only** that set.
  Invalid, duplicate, and non-cosigner entries MUST be **ignored, not treated as fatal**. A
  proposal MUST NOT become permanently unexecutable because one entry is bad. This is not a
  robustness nicety: `sign_delta_proposal` stores the supplied signature without verifying it
  against the commitment, there is no mechanism to remove or replace a stored signature, and
  no endpoint deletes a Miden proposal — so a single cosigner submitting garbage would
  otherwise brick Guardian execution for that proposal with no recovery path at all.
  Guardian MUST record which entries were ignored, and why, for operator diagnosis.
  Execution MUST fail only when the *valid* set is below the effective threshold, reported as
  not-ready rather than as a signature error.
- **FR-007**: Before any proving or network submission, Guardian MUST reproduce the
  transaction from the stored serialized request, at the chain tip under the summary's signed
  bound block (FR-056, FR-061), against the account's current state and
  MUST confirm the result reproduces exactly the summary commitment the cosigners
  signed. On any mismatch it MUST refuse with a binding error. This is the same binding
  invariant as feature 008 FR-007, relocated server-side and not weakened.
- **FR-008**: Guardian MUST refuse an execution request when the account is paused,
  released, already holds a pending candidate, or already holds an active reservation for
  a different proposal — before performing any other work.

#### Opt-in and stored request

- **FR-009**: A proposal MUST be executable by Guardian only if the serialized transaction
  request was attached when the proposal was created. Specifically:
  - **Decided by client configuration** set when the SDK client is constructed — not per
    call, and not negotiated with the server.
  - **Default is not-attached.** An SDK upgrade MUST NOT change what data leaves an
    existing integration; sending full transaction requests to Guardian is an explicit
    choice the integrator makes.
  - **Not addable after creation**, so that no already-signed proposal payload is ever
    mutated.
  - **The two sides decide independently and MUST NOT be coupled.** The client decides
    whether to attach; the server's own configuration decides whether execution is offered
    (FR-021). Guardian MUST accept and store an attached request regardless of whether
    proving is enabled on that server, subject only to the FR-016 size limits — it MUST NOT
    reject the proposal and MUST NOT silently discard the attachment. A mismatch in either
    direction surfaces at execution time via the FR-010 or FR-021 error, never as a failed
    or silently-altered proposal creation. No capability negotiation, discovery round trip,
    or advertised-capability endpoint is introduced.
- **FR-010**: A proposal created with no attached request MUST have a stored payload shape
  indistinguishable from one created before this feature, and MUST be refused for Guardian
  execution with a distinct, clear error.
- **FR-011**: Configuring a client Guardian-executable MUST NOT require a new transaction-request
  argument: for every proposal type, built-in and custom, the SDK already possesses the serialized
  request at creation time, so attachment is internal to the SDK's write path. Built-in proposal
  methods also own their transaction construction and MUST apply FR-051 internally. A custom
  producer already owns its opaque transaction recipe and its auth args; it MUST bind a non-zero
  approval expiration through the SDK's auth-args helper, and MAY also set a transaction
  expiration in its own script, because the SDK cannot generically rewrite an already-built
  custom request (FR-051).
- **FR-012**: Adding a serialized transaction request to a proposal payload MUST NOT
  change the proposal's identity. Proposal identity remains derived from the transaction
  summary alone.
- **FR-013**: Guardian MUST NOT construct transactions. It MUST NOT rebuild a
  transaction request from proposal metadata, and MUST NOT contain transaction-building,
  script-assembly, or note-construction logic for any proposal type; the multisig SDKs
  remain the single source of transaction construction. Guardian MAY carry a static,
  exhaustively-handled mapping from proposal type to invoked procedure **solely** to
  evaluate the effective threshold in FR-005. A proposal whose declared type maps to a
  lower threshold than the transaction actually requires is still rejected by the
  on-chain contract, so this mapping cannot authorize an under-signed state change; it
  can only cost a wasted proof.
- **FR-014**: The stored serialized transaction request MUST be persisted under an
  explicit codec envelope carrying a format version, the Miden protocol line it was
  serialized against, and an integrity checksum over the bytes. To be implementable
  identically in Rust and TypeScript, the envelope MUST fix all four of these:
  - **Checksum algorithm**: SHA-256 over the raw (pre-base64) serialized bytes.
  - **Checksum encoding**: lowercase hex, `0x`-prefixed — the same boundary convention the
    rest of the codebase uses for hex values.
  - **Protocol-line grammar**: `MAJOR.MINOR` decimal, no prefix, no patch component and no
    pre-release suffix (e.g. `"0.17"`), taken from the Miden dependency line the serializing
    SDK was built against.
  - **Serializer identity**: the exact version, **including any prerelease**, of the
    `miden-client` (Rust) or `@miden-sdk/miden-sdk` (TypeScript, whose WASM embeds the client)
    that serialized the bytes, e.g. `"0.17.0-rc.4"`, carried alongside the protocol line. It
    names the Miden serializer, not the Guardian SDK package. The coarse `MAJOR.MINOR` line
    cannot distinguish prereleases, and `TransactionRequest` serialization carries no version
    tag of its own: `miden-client` 0.17.0-rc.4 added the declared block numbers as the first
    serialized field, so rc.3 and rc.4 bytes do not decode under each other
    (`miden-client-0.17.0-rc.4/src/transaction/request/mod.rs:447-477`, rc.3 `:439-443`).
  - **Compatibility rule**: exact string equality against the server's own protocol line, and
    the serializer identity must be admitted by the server's configured allowlist. Guardian
    MUST NOT attempt range or ordering comparisons; a differing line or an unadmitted
    serializer is refused (FR-015) rather than guessed at.
  A committed cross-language fixture MUST pin all four so the two SDKs cannot drift.
- **FR-015**: Guardian MUST refuse to execute a stored request whose envelope declares a
  Miden protocol line incompatible with the running server, with an error distinguishable
  from a binding mismatch. It MUST NOT attempt to deserialize such a request.
- **FR-016**: The stored request MUST be subject to an explicit, configurable per-request
  size limit enforced at proposal creation, and MUST be counted against a per-account
  aggregate limit so that accumulated pending proposals cannot exhaust storage. Exceeding
  either limit MUST refuse creation rather than store an unexecutable proposal.
  Guardian MUST also enforce a viable-proposal count quota per account and authenticated
  proposer, alongside the account-wide count cap. Identity MUST come from authentication,
  not client-supplied metadata. The combined count limits MUST preserve capacity allocated
  to other registered signers. Count checks and insertion MUST be atomic on both backends;
  proposals stale against the current canonical base MUST NOT consume viable count quota.
  Two proposals per proposer is a proposed default. The final configuration and allocation
  rule are an explicit design decision required before implementation. **Deferred
  2026-09-30**: the per-proposer count quota is out of scope for this feature and tracked as a
  follow-up issue; the account-wide count cap and both request-byte limits ship. Request-byte limits
  remain independent and can still refuse admission.
- **FR-017**: When a proposal is deleted, discarded, or superseded, its stored request
  MUST be removed on the same schedule as the proposal itself. This feature MUST NOT
  introduce a retention path that outlives the proposal.
- **FR-018**: Guardian MUST evaluate readiness solely from the signatures held in its own
  stored proposal record. Signatures collected through the offline export/sign flow are
  local artifacts and do not reach Guardian's record; a proposal whose Guardian-held
  signatures are below the effective threshold MUST be refused as not-ready even if
  signatures exist elsewhere. Existing export, offline signing, and local execution of an
  imported proposal MUST remain unchanged by this feature.

#### Proving and submission

- **FR-019**: Guardian MUST delegate proof generation to a configured external prover
  endpoint. It MUST NOT generate proofs in-process. Local proving is achieved by an
  operator running a prover the server is pointed at, not by an alternate code path in
  the server.
- **FR-020**: The prover request timeout MUST be explicitly configured rather than left
  at the client library's default, which is far below realistic proving times.
- **FR-021**: When no prover endpoint is configured, or when the capability is
  explicitly disabled, Guardian MUST refuse execution requests with an explicit
  capability-unavailable error. It MUST NOT fall back to any other proving or submission
  strategy, silently or otherwise.
- **FR-055 — transient proving failures are retried server-side**: Guardian MUST classify
  prover failures as transient or permanent, and MUST retry transient failures with capped
  backoff while it holds (and renews) the execution reservation — without leaving the
  `proving` state and without caller involvement. The transient class MUST include
  transport-level failures — connection errors, i/o timeouts, deadline-exceeded — not only
  well-formed prover error responses: measured under concurrent load (2026-07-29), the
  dominant prover failure family is transport-level (`connection error: i/o timeout`), so a
  classifier that recognizes only structured errors retries nothing precisely when the
  prover is the bottleneck. Retrying MUST stop — settling `failed` through the ordinary
  pre-boundary path — when the failure is permanent, when the transaction's expiration
  (known from the executed transaction before proving starts) can no longer be met within
  the FR-046 horizon, or when that expiration block is at or below the chain height observed
  at the retry (FR-058). A retry re-proves the same executed transaction, whose reference block
  `R` and expiration are fixed, while the tip keeps advancing, so without the second stop a
  transient prover error plus enough new blocks would let Guardian retry, then submit, a
  transaction the node rejects. There is deliberately no separate retry-budget configuration: the
  transaction's own finite expiration (FR-051) is the bound, so the retry span scales with
  the built-in SDK default or custom producer's chosen expiration and can never outlive what FR-046 would refuse to
  submit anyway. Retries are strictly pre-boundary; FR-047 is untouched — once the boundary
  evidence commits, nothing is ever re-proved. Caller-driven re-execution of a `failed`
  execution with `proposal_exists: true` remains available and unchanged; it is the
  recovery of last resort, not the primary response to a prover hiccup.
- **FR-043 — canonicalization is required**: Guardian execution requires the
  candidate/canonical lifecycle and MUST be refused, with the same capability-unavailable
  error class, on a server running in optimistic delta-commit mode (canonicalization
  disabled). Optimistic mode accepts deltas without on-chain verification, so it has no way
  to establish whether a submitted transaction committed — which FR-040 depends on entirely.
  This MUST be detected and reported at startup as well as per request, so a misconfigured
  deployment is visible before a caller discovers it. The shipped binary always runs the candidate lifecycle
  (`crates/server/src/main.rs:44-56`) and exposes no environment variable for the mode, so the
  startup check protects embedders that build a server through
  `ServerBuilder::with_canonicalization(None)` (`crates/server/src/builder/mod.rs:218-233`). Defining a second, weaker lifecycle for
  optimistic mode is explicitly out of scope: it would mean shipping a mode in which Guardian
  cannot tell a caller whether their transaction took effect.

#### Lifecycle, concurrency, and recovery

- **FR-022**: Refusals that are determinable at request time — not Guardian-executable,
  not ready, not a cosigner, paused, released, pending candidate, conflicting
  reservation, capability unavailable — MUST be returned **synchronously** and MUST NOT
  create a reservation or an execution record. All other failures are asynchronous
  execution states.
- **FR-023**: Guardian MUST hold a **durable per-account execution reservation** for the
  whole span from acceptance through a terminal state. The reservation MUST identify its
  owning worker, carry a renewable lease with an expiry, carry a **monotonic fence token**,
  and record the resulting candidate delta once one exists. The existing pending-candidate
  flag is insufficient because it only exists after a candidate has been persisted, which
  is after proving.
- **FR-037 — single admission primitive**: Reservation creation and candidate admission
  MUST be decided by **one account-scoped atomic primitive**, not by two independent
  checks. Within a single account-scoped lock or transaction: creating a reservation MUST
  fail if a candidate exists for that account, and admitting a candidate MUST fail if a
  reservation is active. Separate check-then-act steps are explicitly forbidden — they
  admit a race in both directions (a candidate check passing while a reservation is
  concurrently created, and the reverse).
  The codebase already has this shape: `discard_candidate` commits a status-conditional,
  fence-validated write in one operation and reports `CanonicalWrite::{Applied, StaleLease,
  NotCandidate}`. Admission MUST extend that existing pattern rather than introduce a
  second concurrency mechanism.
  **Owner-authorized exception (required, not optional).** The blanket rule above would
  deadlock Guardian against itself: a Guardian execution holds the reservation for its own
  account and must then admit its *own* candidate. Admission MUST therefore be permitted for
  the caller presenting the **matching reservation's owner identity and a valid fence**, and
  MUST be rejected for every other caller. The test is "is this the candidate this
  reservation authorized", not "does a reservation exist".
- **FR-044 — internal acknowledgment path**: Guardian MUST obtain the acknowledgment for its
  own execution through an **internal** path that does not traverse the public `push_delta`
  endpoint. Today the acknowledgment is only reachable by calling `push_delta`, which both
  creates the candidate and — under FR-027 — must now refuse a reserved account, so a
  Guardian execution could not acknowledge its own transaction. The internal path MUST apply
  the same delta verification and acknowledgment signing as `push_delta`, MUST NOT weaken any
  check, and MUST NOT be reachable from the network.
  It MUST **NOT** persist a candidate delta and MUST **NOT** set `has_pending_candidate`.
  `push_delta` conflates acknowledgment with candidate creation; here they are separate steps
  and the candidate is admitted only at FR-045 step 12, atomically with the submission
  evidence. An internal path that also created a candidate would place it before the
  admissibility check and duplicate the admission.
- **FR-045 — execution sequence**: A Guardian execution MUST perform these steps in exactly
  this order, aborting without side effects at the first failure:
  1. select the valid signature subset and confirm the effective threshold (FR-005, FR-006)
  2. decode the stored request and apply the structural checks (FR-051, FR-056, FR-057), then
     read the chain tip and refuse an approval expiration the tip has already reached (FR-058)
  3. choose the reference block `R` and assemble the chain view and `ProtocolConfig` at it
     (FR-060, FR-061)
  4. reproduce the transaction at `R` and verify the binding to the signed summary (FR-007,
     FR-056)
  5. issue the Guardian acknowledgment via the internal path (FR-044)
  6. inject the signature advice and the acknowledgment
  7. execute the authorized transaction, loading foreign public accounts at `R` (FR-050)
  8. re-verify the binding on the executed result, and refuse a transaction whose approval or
     executed expiration block the observed chain height has already reached (FR-058)
  9. prove (FR-019)
  10. seal the transaction inputs for submission (FR-059)
  11. re-check account admissibility against freshly read state (FR-048), and confirm the
     expiration is within the horizon (FR-046). Steps 10 and 11 are still **before** the
     boundary, so a failure here is an ordinary fail-and-release
  12. validate the fence, then **in one atomic commit**: admit the candidate under the matching
     reservation *and* persist the submission evidence (FR-037, FR-039). This single commit
     **is** the no-retry boundary (FR-047)
  13. **re-validate the fence**: if it is now stale, abort **without sending and without
     writing anything**, and leave the durable candidate to reconciliation (FR-049)
  14. submit
  From step 14 onward the outcome is owned by **exactly one of three parties**, decided by what
  the submission returned. This is normative, because two parties writing one outcome is the
  race FR-041 exists to prevent:
  - **Promotion (canonicalization)** owns `committed`. Promotion is what makes the outcome true,
    so it persists the outcome and releases the reservation in its own transaction, and nothing
    else may write `committed`.
  - **The execution owner** owns a **definite** submission rejection: it resolves immediately
    via the single atomic resolution operation (FR-053), freeing the account rather than
    holding it until expiration.
  - **Reconciliation** owns every **unknown** outcome, resolving it by the superseded or
    expired evidence paths (FR-040). It MUST NOT write `committed`; observing the account at the
    expected commitment is an input to promotion and to status derivation, not a second
    terminal write.
  Only an explicit application-level rejection is definite. Every ambiguous transport
  failure — timeout, dropped connection, unavailable — is unknown, because misclassifying one
  as definite would discard the candidate for a transaction that actually committed.
  Four orderings are normative, each fixing a specific failure mode:
  - **Step 4 before step 5**: the acknowledgment MUST NOT be issued for a transaction that does
    not reproduce the signed summary.
  - **Step 10 before step 12**: sealing needs the validator set's encryption key and its
    attestations from the node. A key or attestation failure after the boundary would hold the
    account until expiration for a transaction that was never sent.
  - **Step 11 before step 12**: admissibility and expiration are checked *before* the boundary.
    Checking them after would be unresolvable: FR-047 forbids fail-and-release past the
    boundary while FR-048 demands failure, so the account would be held until expiration for a
    transaction that was never sent.
  - **Step 12 before step 14**: the candidate MUST exist before the transaction is sent. If
    submission preceded admission, a crash in between would leave a transaction on chain with
    no candidate to promote, so reconciliation would have to report `committed` with nothing ever
    reaching `canonical` — contradicting FR-026 and US1. Binding the candidate and the
    evidence into one commit makes "about to submit" a single durable fact.
  This mirrors the existing client flow, where `push_delta` records the candidate before the
  client submits.
- **FR-038 — fencing**: Reservations MUST reuse the existing `LeaseFence`
  (`lease_name`, `holder_id`, `fence_token`) rather than a new mechanism. The fence MUST be
  validated **immediately before submission and on every durable mutation**, and a stale
  fence MUST abort the operation without writing. Lease possession alone is insufficient: a
  worker paused past its lease expiry could otherwise wake and submit after another worker
  has taken ownership.
  **The lease MUST be per-account, not the cluster-wide canonicalization lease.** Execution
  leases MUST use a distinct, account-scoped `lease_name` (`execution:{account_id}`), so
  executions for different accounts proceed concurrently while executions for the *same*
  account are serialized by lease acquisition itself. Reusing `CANONICALIZATION_LEASE` would
  reduce the whole deployment to one execution at a time, because `worker_leases` admits a
  single holder per `lease_name`. Reusing the `LeaseFence` *type* is required; reusing the
  canonicalization *lease* is forbidden.
- **FR-052 — reservation ownership operations**: The storage layer MUST expose explicit,
  fenced operations for acquiring, renewing, and **transferring** a reservation's ownership.
  A transfer MUST be a compare-and-set against the current holder and fence token, and MUST
  fail rather than steal when the caller's expectation is stale. Without a transfer operation
  FR-028's post-submission handover has no implementation: the reservation must not be
  released, yet a new owner must be able to take responsibility for resolving it.
- **FR-053 — every terminal transition is one atomic operation**: A terminal outcome and the
  release of its reservation MUST be committed together. Three operations, and no path may
  compose them from smaller steps:
  - **Promotion** — the existing fenced promotion, extended to persist `committed` and release
    the reservation.
  - **`resolve_execution`** — post-boundary failure: discard the candidate, **delete its
    matching proposal**, persist the outcome, release the reservation.
  - **`fail_execution`** — pre-boundary failure: persist the outcome and release the
    reservation. No candidate exists on this path, so there is nothing to discard, but the
    two writes MUST still be one commit — a crash between them would otherwise expose either a
    terminal execution still holding a reservation, or a released account with no durable
    outcome to report.
  **`resolve_execution` MUST delete the matching proposal**, not leave it to a later step.
  Canonicalization's existing discard deletes the proposal *after* its transaction commits and
  tolerates failure with a warning; a post-boundary execution cannot inherit that, because a
  surviving proposal reports `proposal_exists: true`, which FR-042's contract makes indication
  of a permitted retry. A definitely-rejected transaction advertised as retryable is the
  failure this rule prevents.
- **FR-054 — who may release a reservation**: Promotion runs under the **canonicalization**
  lease, not the account's execution lease, yet FR-038 requires every durable mutation to
  validate the execution fence. Promotion is therefore **explicitly authorized** to persist
  `committed` and release the reservation while holding the canonicalization fence, provided it
  takes the same per-account lock every other reservation write takes. The authorization is
  narrow and justified: promotion is the operation that makes the outcome true, and it cannot
  be expected to hold a lease belonging to a worker that may no longer exist.
  Promotion and `resolve_execution` MUST be serialized by that per-account lock. Whichever
  commits first wins; the loser MUST observe that the execution is already resolved and write
  nothing. Both outcomes are correct — a candidate that promoted did land, and one that was
  definitely rejected did not — so the lock decides, not a precedence rule.
- **FR-024**: The **externally reported** execution state MUST be an explicit,
  exhaustively enumerated set of exactly five values: `pending`, `proving`, `submitted`,
  `committed`, and `failed`. The set is deliberately minimal: a state exists only where it
  changes what the caller does. `failed` MUST carry a stable error code and a
  human-readable message distinguishing at least verification, proving, submission, and
  post-submission-discard causes, plus an optional `meta` object whose keys and values are a
  closed, documented set per code (`meta.reason`, `meta.bound`), so both SDKs can surface them as
  typed values.
- **FR-025**: Guardian MAY maintain a finer-grained **internal** execution record, and
  MUST do so where FR-031's recovery rule requires it (specifically, whether submission
  was authorized and prepared by crossing the no-retry boundary). Internal states MUST NOT be
  exposed on any wire surface, and every
  internal state MUST map onto exactly one of the five reported states. Operator-facing
  diagnosis of internal states belongs in logs and metrics, not the wire contract.
- **FR-026**: Reported states MUST be mapped explicitly onto the existing delta
  lifecycle, with these meanings:
  - `pending`, `proving` — **no delta exists at all**.
  - `submitted` — the no-retry boundary has been crossed (FR-047), so a candidate delta
    **always exists** and the transaction is on chain or may be. From here the existing
    canonicalization lifecycle owns the outcome, and the caller's contract is identical either
    way: wait, watch the delta, do not retry. Because the candidate is admitted atomically
    with the submission evidence (FR-045 step 12), there is no window in which this state
    exists without a candidate.
  - `committed` — the candidate reached `canonical`. Terminal success.
  - `failed` — terminal; the transaction did not take effect. Whether the proposal can be
    retried is **not** implied by this state and MUST be read from `proposal_exists`
    (FR-042): a pre-boundary failure leaves it intact, while a post-boundary failure may have
    had it deleted alongside its candidate.
  Terminal states MUST be exactly `committed` and `failed`. This feature MUST NOT add delta
  status values or alter existing delta transitions.
- **FR-041 — terminal outcomes survive candidate deletion**: A post-submission terminal
  state MUST NOT be derived from the candidate delta on read, because the candidate does not
  always survive. Canonicalization's `remove_candidate` deletes an unrecoverable candidate
  **and then deletes its matching proposal** — leaving nothing to derive from and, with the
  proposal gone, nothing to retry. Therefore:
  - Guardian MUST persist the execution's terminal outcome **atomically with, and no later
    than, the candidate deletion or promotion** that determines it, in execution-owned
    storage. This is the one place a reported state is persisted rather than derived, and it
    exists specifically because the source of truth is destroyed.
  - Pre-terminal reported states remain derived and MUST NOT be independently persisted, so
    the two representations cannot drift while both exist.
  - The delta and proposal deletion behavior itself MUST NOT change; this feature records its
    own outcome rather than altering canonicalization's cleanup.
- **FR-027**: While a reservation is active for an account, Guardian MUST refuse client
  delta pushes for that account. Without this, a client could obtain an acknowledgment
  for a competing transaction while Guardian is mid-proof, defeating FR-029.
- **FR-028**: Lease expiry MUST be handled differently depending on whether the no-retry boundary
  was crossed, and MUST NOT release every expired reservation:
  - **Before the boundary** — the execution is reported `failed` and the reservation
    is released. Expiry MUST NOT silently restart the execution; the caller retries
    explicitly.
  - **After the boundary** — the reservation MUST NOT be released, even if the network send has
    not started. Ownership transfers to
    a **reconciliation owner**, which resolves the outcome under FR-030. Releasing here
    would permit a second submission of the same transaction. The transfer MUST use the
    fenced compare-and-set operation required by FR-052; it is a change of owner on a live
    reservation, never a release followed by a re-acquire, which would open exactly the
    window that admits a second submission.
- **FR-029**: Across all replicas, concurrent or repeated execution requests for one
  proposal MUST guarantee exactly three things:
  1. **At most one *lease-authorized* proving attempt at a time.** Guardian MUST NOT claim at
     most one *active* attempt: the prover is an external service Guardian cannot cancel, so
     after a lease expires the previous worker's request may still be in flight there while the
     new owner issues its own. Two proofs can genuinely be computing concurrently.
  2. **Stale proof results MUST be rejected.** A proof returned to a worker whose fence is no
     longer current MUST NOT be carried forward — the boundary commit is fenced (FR-045 step
     12), so a stale worker cannot cross it, and FR-049 stops it sending even if it commits and
     then goes stale.
  3. **At most one on-chain submission**, which is the property that actually matters.
  A crash after the prover returns but before the result is durably recorded means the retry
  legitimately proves again. That costs prover resources; it MUST NOT cost a second
  submission. Strengthening any of this to exactly-one proof would require prover-side
  cancellation or idempotency keyed by execution id — upstream capabilities Guardian does not
  control and MUST NOT assume.
- **FR-030**: When a submission's outcome cannot be established — timeout, dropped
  connection, or crash between sending and receiving a response — Guardian MUST report
  `submitted` and MUST **retain** its reservation. Retry MUST be refused. Optimistically
  releasing the reservation is forbidden, because it risks a second submission of the same
  transaction. The unknown-outcome case is not separately reported, because the caller's
  contract is identical to a known submission; the safety property is enforced by the
  retained reservation, not by informing the caller.
- **FR-039 — submission evidence, recorded before submitting**: Guardian MUST durably
  record, **before** it sends the proven transaction, at minimum: the transaction id, the
  base account commitment, the expected resulting account commitment, the reference block
  number, and the
  **expiration block taken from the proven transaction itself**
  (`ProvenTransaction::expiration_block_num()`). Without this written first, a crash
  mid-submission leaves nothing to reconcile against.
  The expiration MUST NOT be derived from the request's `expiration_delta`: that field is
  `Option<u16>` where `None` means the transaction never expires, and executed code may
  impose a different expiration than the request asked for. The executed transaction exposes
  the resulting value; Guardian records it from the exact proven transaction that will be
  submitted, where it remains available before submission.
- **FR-046 — finite expiration is required**: Guardian MUST refuse to cross the no-retry boundary
  for a transaction
  whose expiration block falls outside a configured **reconciliation horizon**, measured as the
  proven transaction's `expiration_block_num` minus the attempt's reference block `R`, reporting
  a distinct error before that boundary. It is a refusal, never a wait: Guardian does not hold a
  reservation until a far expiration comes within range. A transaction that
  effectively never expires has no finite chain-height path out of the unknown-submission state, so
  submitting one could hold the account's reservation indefinitely. Refusing before the
  no-retry boundary gives FR-040 a finite resolution height once trustworthy chain observation
  is available.
  **This is load-bearing, not defensive.** A transaction with neither an expiration delta nor an
  approval expiration is **non-expiring**: the prologue stores the `u32::MAX` sentinel
  (`miden-protocol-0.17.0-rc.7/asm/kernels/transaction-core/src/prologue.masm:30,1395`) and the
  proven transaction reports it. Non-expiring is therefore the upstream *default*, not an edge
  case: neither SDK sets either bound today. Without this refusal every execution would be
  unbounded and FR-040's `expired` path would never fire.
- **FR-051 — delegated transactions carry two finite bounds**: Every Guardian-executable
  proposal MUST carry a signed **approval expiration**, and every built-in family MUST also carry
  a relative **transaction expiration**. Both are fixed by the proposer and signed, so neither
  Guardian nor any other party can change them after cosigners sign.
  - **Approval expiration (the signing window).** The multisig auth arg binds an absolute
    `approval_expiration` block, which the summary carries as user parameter 0
    (`miden-standards-0.17.0-rc.7/asm/standards/auth/multisig.masm:827-854,956-965`). The auth
    procedure aborts once the reference block reaches it (`:884-914`, `ERR_MULTISIG_APPROVAL_EXPIRED`)
    and afterwards lowers the transaction expiration to at most that block (`:931-942`). The SDKs
    already expose it (`ProposalOptions.approval_expiration_delta`, `approvalExpirationDelta`,
    1 to 65,535 blocks, default never). In Guardian-executable mode both SDKs MUST apply the
    shared default `GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA = 28_800` blocks (about 24 hours
    at devnet's roughly 3-second blocks) unless the caller passes another value. Custom producers
    MUST pass a non-zero value to the SDK's auth-args helper
    (`MultisigClient::multisig_auth_args`). A zero approval expiration is refused as
    `GUARDIAN_EXECUTION_REQUEST_INVALID` (`approval_expiration_missing`).
  - **Transaction expiration (the unknown-submission bound).** Built-in families MUST set a
    relative expiration delta of **256 blocks** (`GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA`),
    identical in Rust and TypeScript. The summary
    signs the delta *relative to the executing reference block*: the kernel computes it as
    `expiration_block - ref_block`
    (`miden-protocol-0.17.0-rc.7/asm/kernels/transaction-core/src/tx.masm:177-189`) and the host
    checks the summary against it (`miden-tx-0.17.0-rc.7/src/host/tx_event.rs:787-795`), so the
    same delta reproduces at any tip. No-script requests use
    `TransactionRequestBuilder::expiration_delta`
    (`miden-client-0.17.0-rc.4/src/transaction/request/builder.rs:305`); Guardian-owned scripts
    call `tx::update_expiration_block_delta`. The builder still rejects `expiration_delta`
    together with a custom script (`builder.rs:682-687`) and `TransactionRequest` has no public
    mutator, so an opaque custom request can only carry a transaction expiration its producer
    scripted.
  - **How they combine.** `update_expiration_block_delta` only ever lowers the stored expiration
    (`tx.masm:143-169`), and an asset callback may lower it further as a freshness bound on
    foreign state, so the proven expiration is at most `R + min(256, approval_expiration - R)` for a
    built-in family, and `R + min(65,535, approval_expiration - R)` for a custom request with no
    scripted delta. Guardian records the value from the proven transaction (FR-039) and applies
    the horizon to it (FR-046). A custom request without a scripted delta is therefore refused
    on horizon grounds whenever its approval window reaches beyond the horizon, which with the
    default approval window is most of that window. Custom producers that want Guardian
    execution SHOULD script `GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA` through
    `tx::update_expiration_block_delta`; the refusal is an ordinary pre-boundary failure, not a
    client defect.
  **The client's obligation is finiteness only, not conformance to a deployment's horizon.**
  The client MUST NOT be required to match the server's reconciliation horizon: US3 forbids
  capability negotiation, so the client has no way to learn that value. The SDK sets the
  built-in defaults without consulting the server, custom producers choose their own values, and
  the server alone decides whether the resulting expiration falls inside its horizon (FR-046).
  Because both bounds are signed, a Guardian-executable proposal's summary and id differ from a
  self-executed proposal with the same effects. Guardian MUST NOT rewrite either bound, for that
  reason first, and also for exact request-byte preservation, FR-014 checksum reproducibility,
  and the construction boundary in FR-013.
  A parity test MUST assert that both SDKs use the same 28,800-block approval default and the
  same 256-block transaction delta.
- **FR-040 — terminating an unknown submission**: An account still sitting at its base
  commitment is **not** evidence that a transaction was dropped — it may still land. A
  reservation MUST therefore terminate only on positive evidence, exactly one of:
  - **Committed** — the expected resulting account commitment is observed on chain, or the
    transaction is observed included; settle `committed`.
  - **Superseded** — the account is observed at a commitment that is neither the base nor
    the expected result; the transaction can no longer land; settle `failed`.
  - **Expired** — the chain height is observed strictly past the expiration block recorded
    under FR-039 while the account is still at base; the transaction can never land; settle
    `failed`. FR-046 guarantees that bound is finite and within the horizon.
  Guardian MUST NOT settle on elapsed wall-clock time alone. Expiration is the only finite
  chain-height bound, which is why FR-039 requires recording it. If chain observation is
  unavailable, Guardian MUST retain the reservation, keep reporting `submitted`, and retry
  observation with capped backoff. It MUST surface the outage through health, metrics, and logs
  so an operator can restore or fail over the chain source; operator recovery MUST NOT release
  the reservation, settle the execution, or authorize a retry without positive chain evidence.
  The termination guarantee is conditional on eventual trustworthy chain observation at or
  beyond the recorded expiration height. Guardian SHOULD use the node's per-account transaction
  records as a faster path to the committed outcome: `SyncTransactions(block_range,
  account_ids)` returns each included transaction's header and block
  (`miden-node-proto-build-0.17.0-rc.3/proto/rpc.proto:90,798-841`), and Guardian's RPC client
  already calls it (`crates/miden-rpc-client/src/lib.rs:518`). No RPC reports definitive
  absence, so the expiration bound MUST remain the backstop.
- **FR-047 — the no-retry boundary**: "Submission authorized and prepared" MUST be one durable,
  observable transition, never an inference. Committing the FR-039 evidence **is** that
  transition: once durably written, the execution MUST NEVER be re-submitted or re-proved,
  and every recovery path MUST reconcile only (FR-040). Before that write, no submission has
  occurred and recovery MUST fail-and-release. There MUST be no window in which a crash
  leaves this ambiguous — the evidence write MUST commit before the first byte of the
  submission is sent, and the fence MUST be re-validated between the write and the send so a
  stale owner cannot cross the boundary.
  The reported state MUST change to `submitted` **at that commit**, not when the network send
  begins. An observer in the post-commit / pre-send window MUST see `submitted`, because that
  is precisely the window in which retry is already forbidden. Reporting `proving` there would
  invite a retry that FR-047 forbids.
- **FR-050 — foreign public accounts are loaded at the reference block**: On 0.17 foreign
  accounts are on the ordinary execution path, not an exotic one. Moving an asset whose faucet
  sets the asset-callback flag opens a foreign context against that faucet
  (`miden-protocol-0.17.0-rc.7/asm/kernels/transaction-core/src/callbacks.masm:102-123`), which
  devnet's fee faucet does on every fee payment, and fee sponsorship of network output notes
  prices each note through foreign procedure invocation against its target account
  (`miden-standards-0.17.0-rc.7/asm/standards/fee/mod.masm:349-352`). The executor requests them
  lazily through `DataStore::get_foreign_account_inputs(id, ref_block)`
  (`miden-tx-0.17.0-rc.7/src/executor/exec_host.rs:183`). Guardian MUST answer that call for a
  **public** account from the node at the attempt's reference block `R`
  (`GetAccount` with `block_num = R` and the witnesses the executor asks for,
  `miden-node-proto-build-0.17.0-rc.3/proto/rpc.proto:33,345-412`), and MUST fail the attempt
  before any proving with `GUARDIAN_EXECUTION_FOREIGN_ACCOUNT_UNAVAILABLE` when the foreign
  account is private (`meta.reason: "private"`) or the node cannot serve its state at `R`
  (`meta.reason: "unavailable"`, including the node's pruned-block error). Because `R` is the tip,
  the node's account-history pruning window does not apply on the honest path. Foreign state is
  read per attempt and never cached across executions. This replaces revision 10's blanket
  exclusion, which on 0.17 would refuse every fee-paying transaction on devnet.
- **FR-049 — no send after losing ownership**: The fence MUST be re-validated between the
  boundary commit and the network send, and a stale fence MUST abort the send. The commit's own
  fence check is not sufficient: a worker can be current at commit time, lose its lease while
  paused, and wake to find that reconciliation has already settled and released the execution.
  Sending then would put a transaction on chain for an account whose candidate was discarded
  and whose reservation is gone. A stale worker MUST write nothing — it cannot know what the
  new owner has done — and MUST leave the durable candidate and evidence for reconciliation,
  which terminates once trustworthy observation passes the FR-046 horizon. This is the one abort that legitimately leaves a
  boundary-crossed execution without a send; FR-040's expiration path is what bounds it.
- **FR-048 — re-check admissibility immediately before the boundary**: Fencing protects only
  Guardian's own lease; it says nothing about the chain moving underneath. Immediately before
  submission Guardian MUST re-verify, against freshly read state, that the account is still
  admissible: still at the base commitment the execution was built on, still not paused or
  released, and **still guarded by this Guardian's acknowledgment key**. A `SwitchGuardian`
  that has canonicalized **as observed by this check** MUST cause the execution to fail rather
  than submit. No fence check can detect that, which is why the read is required.
  The guarantee is scoped to what this final read observes, and MUST NOT be stated absolutely:
  a switch can canonicalize in the window between the read and the network send, and no
  amount of checking closes that window. In that case the proof is stale — it was generated
  against the pre-switch account state — so the node rejects it and the execution settles as a
  definite submission rejection. The check removes every *observable* case; the chain is the
  backstop for the unobservable remainder.
- **FR-031**: On restart, an execution whose durable record shows the FR-047 boundary was
  **not** crossed MUST be reported `failed` and released. One that had crossed it MUST be
  resolved by chain observation per FR-030. This is the one place a finer internal state is
  normatively required (FR-025). No execution may be left in a state where the account is
  locked with nothing making progress.
- **FR-032**: Every refused or **pre-boundary** `failed` execution MUST leave the account
  unlocked, record no delta of its own, release its reservation, and leave the proposal
  intact and executable by any supported path. This MUST be covered by explicit tests for
  each cause in FR-024. Three carve-outs, all normative:
  - An execution reporting `submitted` retains its reservation until FR-040 resolves it.
  - A **post-boundary** execution has a candidate **by construction** (FR-045 step 12), so
    "records no delta of its own" does not apply to it. Disposal of that candidate is owned by
    canonicalization and the FR-040 evidence paths; on a definite submission rejection
    Guardian MAY discard its own candidate immediately, since it knows nothing committed.
  - A **post-boundary** `failed` execution MUST NOT promise the proposal is still executable.
    Canonicalization deletes the proposal alongside an unrecoverable candidate, so it may
    legitimately be gone. Guardian MUST report this via `proposal_exists` (FR-042) rather than
    implying a retry that would fail with proposal-not-found.
- **FR-042 — proposal presence, not retry advice**: The execution envelope MUST report
  whether the proposal **still exists**, as a fact, rather than advertising whether a retry is
  permitted. Retry permission is already implied by the reported state; conflating the two
  produced a field that claimed "may retry" for `submitted` (where retry is forbidden) and for
  `committed` (whose proposal is deleted on promotion). Where a proposal was deleted with its
  candidate, the execution's terminal record MUST remain readable and MUST report the proposal
  as absent, so a caller learns it must create a **new** proposal rather than retry the old
  one. Guardian MUST NOT recreate the proposal itself: doing so would resurrect
  an intent whose transaction already failed on chain, against a state that has moved.

#### Parity and client surfaces

- **FR-033**: The Rust and TypeScript client surfaces MUST expose equivalent capability
  with equivalent semantics for configuring execution mode, requesting execution, and observing
  execution state, including identical state and error-code vocabularies.
- **FR-034**: Requesting Guardian execution and observing its state MUST be possible with
  **no Miden capability** — no local transaction building, no Miden node connectivity, and no
  proving. A caller able to produce a valid authenticated request MUST be sufficient.
  This guarantee is scoped to the **base clients** (`crates/client`,
  `packages/guardian-client`), which speak only to Guardian and have no Miden dependency. The
  multisig SDKs MUST also expose the same operations for convenience, but they retain their
  existing Miden requirements — constructing one still requires a Miden client — so they
  MUST NOT be presented as satisfying this guarantee. The thin-client and
  no-Miden-capability scenarios in this spec are therefore demonstrated against a base
  client.
- **FR-035**: Self-execution through the existing SDK flow MUST remain fully supported
  and behaviorally unchanged for both kinds of proposal. It MUST NOT be deprecated,
  gated, or degraded by this feature.

- **FR-056 (reproduction at the tip under the signed bound block)**: This is new server
  behavior. The signed summary's `block_number` is the proposal's **bound block**; Guardian MUST
  take it from the summary, never from the proposal's `chain_anchor` field, which stays on the
  wire for SDK compatibility and which Guardian execution does not read. Before any chain read
  (FR-045 step 2), Guardian MUST refuse with `GUARDIAN_EXECUTION_REQUEST_INVALID`:
  - a request whose `block_numbers()` does not declare the bound block
    (`meta.reason: "bound_block_not_declared"`), mirroring the SDKs' `BoundBlockNotDeclared`;
  - a consume-notes request whose input notes are not pinned in the request through
    `TransactionRequestBuilder::explicit_input_notes`
    (`miden-client-0.17.0-rc.4/src/transaction/request/builder.rs:178-190`)
    (`meta.reason: "input_notes_not_pinned"`), because the signed summary binds each note's
    consumption mode and a storeless executor cannot classify notes from a store.
  These checks run at execution, not at proposal creation, so creation still never refuses over
  the attachment (FR-009) and incompatible lines are still never decoded (FR-015); the reservation
  they occupy is released within the same attempt. Guardian then reproduces at the reference
  block chosen by FR-061. When its node has not yet reached the bound block, the attempt fails
  pre-boundary with the retryable `GUARDIAN_EXECUTION_CHAIN_BEHIND`. Guardian MUST NOT execute at
  the anchor block and MUST NOT require an anchor.
- **FR-057 (the request carries the fee info; Guardian commits none)**: Every
  Guardian-executable request MUST already carry the multisig auth arg and its three-word
  preimage in the advice map, as both SDKs produce on `main`
  (`crates/miden-multisig-client/src/transaction/auth_args.rs:145-160`; TypeScript through the
  web SDK's `feeAwareTransactionRequestBuilder`). The preimage names the bound block, the
  approval expiration, the salt, and the native fee conversion info the proposer resolved, and
  the fee note the auth procedure creates from it is covered by the signed summary. Guardian MUST
  NOT derive, attach, or repair fee conversion info, and MUST NOT overwrite an auth arg: the
  `miden-client` fee path commits a two-word preimage that the 0.17 guarded-multisig procedure
  rejects (`miden-client-0.17.0-rc.4/src/transaction/request/mod.rs:291-300`, versus
  `miden-standards-0.17.0-rc.7/asm/standards/auth/multisig.masm:827-854`). A request whose auth arg
  is empty or whose preimage is absent from its advice map is refused with
  `GUARDIAN_EXECUTION_REQUEST_INVALID` (`meta.reason: "auth_args_missing"`). A reproduction that
  aborts because the account's fee-asset balance cannot pay the fee fails pre-boundary with
  `GUARDIAN_EXECUTION_INSUFFICIENT_FEE`, distinct from a binding mismatch.
- **FR-058 (a reached expiration is refused before proving)**: Guardian MUST fail the attempt
  pre-boundary with `GUARDIAN_EXECUTION_EXPIRATION_REACHED` when:
  - at FR-045 step 2, the observed chain tip is at or past the summary's approval expiration
    (`meta.bound: "approval"`);
  - reproduction or execution aborts with the auth procedure's `ERR_MULTISIG_APPROVAL_EXPIRED`
    (`meta.bound: "approval"`), which covers a tip that advanced past the expiration between
    the read and the execution;
  - after execution (step 8) and again before each proving retry (FR-055), the executed
    transaction's expiration block is at or below the chain height read from the configured node
    (`meta.bound: "transaction"`). This mirrors the client's post-execution check
    (`miden-client-0.17.0-rc.4/src/transaction/mod.rs:379-389`), which the client only applies on
    its anchored path. It is separate from FR-046, which bounds the distance ahead rather than
    the distance already travelled.
- **FR-059 (submission inputs are sealed before the boundary)**: A 0.17 submission carries the
  proven transaction together with its `TransactionInputs` sealed against the validator set's
  encryption key (`miden-node-proto-build-0.17.0-rc.3/proto/types/submission.proto:8-28`), and
  the node publishes that key with attestations through `GetTransactionEncryptionKey`
  (`rpc.proto:74`, `submission.proto:51-97`). The inputs cannot be recovered from a
  `ProvenTransaction`. Guardian MUST keep the `TransactionInputs` of the executed transaction in
  memory until submission, and MUST fetch the key, validate its attestations, and seal the inputs
  at FR-045 step 10, before the no-retry boundary. A failure there is
  `GUARDIAN_EXECUTION_SEALING_FAILED`, an ordinary fail-and-release. The sealed bytes need not be
  durable, since nothing is ever re-sent. Guardian SHOULD reuse the public
  `miden-client` sealing helper (`miden-client-0.17.0-rc.4/src/rpc/encryption.rs:445-460`) rather
  than re-implementing it.
- **FR-060 (the protocol configuration matches the reference header)**:
  `DataStore::get_transaction_inputs` returns a `ProtocolConfig` that MUST be the one the
  returned header commits to (`miden-tx-0.17.0-rc.7/src/executor/data_store.rs:29-30`). Guardian
  MUST obtain it from the node together with the reference header (`GetBlockHeaderByNumber` with
  `include_protocol_config`, `rpc.proto:270-295`, or the genesis-seeded `SyncChainMmr` response,
  which carries the target's configuration, `rpc.proto:709-713`) and MUST fail the attempt before
  reproduction with `GUARDIAN_EXECUTION_BINDING_MISMATCH` when its commitment differs from
  `header.protocol_config_commitment()`, since the execution inputs would then not authenticate. Guardian's RPC client
  hardcodes the flag off today (`crates/miden-rpc-client/src/lib.rs:302`).
- **FR-061 (reference block and chain view)**: Each attempt MUST use one reference block `R`,
  the configured node's committed tip when the attempt starts, and one partial blockchain at
  forest `R` that tracks the bound block and the creation block of every authenticated input
  note. The executor does not request the bound block itself
  (`miden-tx-0.17.0-rc.7/src/executor/mod.rs:280-281`; lazy block-witness loading is
  unimplemented, `src/host/tx_event.rs:491-494`), so Guardian's `DataStore` MUST add the request's
  declared `block_numbers()` to the tracked set, as `ClientDataStore` does
  (`miden-client-0.17.0-rc.4/src/store/data_store/mod.rs:335-342`). The peaks come from a
  genesis-seeded `SyncChainMmr` to `R` and each tracked block's path from
  `GetBlockHeaderByNumber(include_mmr_proof)`; a path returned at a later chain length MUST be
  adjusted to forest `R` before use. The bound block's commitment in the resulting partial
  blockchain MUST equal the summary's `block_commitment`; the kernel and host enforce this during
  execution (`miden-tx-0.17.0-rc.7/src/host/mod.rs:504-516`), and Guardian treats a failure as a
  binding mismatch. The chain view is built per attempt and discarded afterward.

### Key Entities *(include if feature involves data)*

- **Guardian-executable proposal**: a pending proposal created by a client configured
  Guardian-executable, and which therefore carries the serialized
  transaction request needed to reproduce the signed transaction. Identical to any other
  proposal for listing, review, signing, export, and self-execution.
- **Stored serialized transaction request**: the serializable transaction request that
  fully defines the transaction to run (script, inputs, notes, salt, proposer advice),
  wrapped in the FR-014 codec envelope. Excludes any pre-built summary, proven
  transaction, cosigner signatures, and the Guardian acknowledgment. Guardian treats the
  payload as opaque: it deserializes and re-runs it to derive a summary, never to
  interpret intent.
- **Execution reservation**: the durable, fenced, leased per-account claim
  that one execution is in flight. Spans acceptance through terminal state, blocks
  competing executions and client delta pushes, and is the only durable coordination
  state this feature introduces.
- **Execution record and state**: the observable, exhaustively enumerated progress of one
  execution, per FR-024, mapped onto the delta lifecycle by FR-025. Distinct from delta
  status, which continues to describe the resulting delta's canonicalization.
- **Ephemeral execution data store**: the per-attempt store seeded from Guardian's held account
  state plus a chain view at the node's committed tip (reference header, its `ProtocolConfig`,
  and a partial blockchain tracking the bound block and authenticated note blocks) and foreign
  public accounts read at that block, used to reproduce the transaction and discarded when the
  attempt ends. Carries no state between executions.
- **Bound block**: the block the signed summary binds (its `block_number`), chosen by the
  proposer and declared in the request's `block_numbers()`. It fixes what the cosigners signed
  against; it is not the block the transaction executes at.
- **Approval expiration**: the absolute block, bound in the auth arg and carried as the summary's
  first user parameter, at which cosigner approval lapses. Required on every Guardian-executable
  proposal (FR-051).
- **Effective threshold**: the signature count a proposal must reach — the invoked
  procedure's override when configured, otherwise the account default; the account default
  for custom types.
- **Proposal binding**: the invariant that the transaction Guardian submits reproduces
  the summary commitment the cosigners signed. The feature's entire safety argument
  rests on this check preceding all proving and submission.
- **Prover endpoint**: the external service Guardian delegates proof generation to.
  Operator-provisioned; its absence disables the capability rather than changing how it
  works.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A cosigner can take a threshold-met proposal to a committed on-chain state
  using only authenticated Guardian requests — with no local transaction building, no
  Miden node connectivity, and no local proving. Demonstrated against a **base client**
  (FR-034), which has no Miden dependency; not against a multisig SDK.
- **SC-002**: 100% of execution attempts where the reproduced transaction does not match
  the signed summary commitment are refused before any proof generation or on-chain
  submission — verified by explicit tests for the binding causes (superseded state, mutated
  request). Signature-set problems are **not** binding mismatches and are covered separately
  by SC-020 (bad entries ignored) and SC-003 (insufficient valid signatures refused as
  not-ready).
- **SC-003**: 100% of the FR-022 synchronous-refusal causes are refused at request time
  without creating a reservation or execution record.
- **SC-004**: Readiness is evaluated against the effective per-procedure threshold in
  100% of cases — verified by a test in which a procedure override differs from the
  account default and only the override governs the outcome.
- **SC-005**: Repeated and concurrent execution requests for the same proposal, and
  concurrent requests across multiple replicas, produce **at most one lease-authorized proving
  attempt at a time and at most one on-chain submission** — verified under a multi-replica
  test. A crash-induced second proof and a concurrently-in-flight stale prover request are both
  permitted (FR-029); a second submission is not, and a stale proof result is never carried
  forward.
- **SC-006**: For every pre-submission `failed` cause in FR-024, the account is left
  unlocked with no delta recorded, the reservation released, and the proposal still
  executable — verified by explicit tests.
- **SC-007**: A killed reservation holder results in lease expiry, a failed execution, a
  released reservation, and a usable account — with no manual operator intervention and
  no indefinite lock, verified by an explicit test.
- **SC-008**: A simulated unknown-outcome submission never results in a second
  submission: it reports `submitted`, retains its reservation, and refuses retry. It reaches a
  terminal state via exactly one of the FR-040 evidence paths — committed, superseded, or
  expired — with an explicit test per path, including one where the account never leaves its
  base commitment and only observation beyond the expiration bound terminates it. A separate
  outage test holds the same reservation safely while observation is unavailable and resumes
  resolution after the chain source recovers.
- **SC-009**: A proposal created by a self-executed client has a stored payload
  byte-shape indistinguishable from one created before this feature — verified by an
  explicit test, so the feature 008 FR-015 property is preserved by default.
- **SC-010**: A stored request whose envelope declares an incompatible Miden protocol
  line is refused with a version error and is never deserialized — verified by a fixture
  captured from a different protocol line.
- **SC-011**: Per-execution store seeding overhead is recorded as a committed baseline so
  regressions are detectable. **Baseline telemetry, not a pass/fail gate**: no threshold is
  asserted, because the acceptable ratio depends on prover performance that varies by deployment.
  Measured during the Gate 0 spike: setup is memory-only and negligible, while proving is 6–20 s
  remote and ~60 s local — so proving dominates by orders of magnitude, and the sqlite-versus-
  in-memory question that originally motivated this criterion is moot (there is no `Store`).
- **SC-012**: Existing self-execution, export, offline signing, and local execution of an
  imported proposal show no behavioral regression for any built-in or custom proposal
  type, verified through both example harnesses.
- **SC-013**: The Rust and TypeScript SDKs expose equivalent capability with identical
  state and error-code vocabularies; a parity check confirms zero drift.
- **SC-014**: A server with no configured prover, or with the capability disabled,
  refuses 100% of execution requests with an explicit capability-unavailable error and
  never attempts to prove or submit.
- **SC-015**: The full lifecycle — propose as Guardian-executable, sign to threshold,
  request Guardian execution, observe on-chain commitment and canonicalization — is demonstrated
  end-to-end in an example harness for both a built-in proposal type and a custom one.
- **SC-016**: The reported state vocabulary is exactly the five values in FR-024 on both
  HTTP and gRPC, and no internal execution state is observable on any wire surface —
  verified by an explicit test.
- **SC-017**: A caller refused with an execution conflict can identify the blocking
  proposal from the error alone, without polling other proposals — verified by an explicit
  test.

- **SC-018**: Reservation creation and candidate admission are decided by one atomic
  primitive: a concurrency test hammering both paths on one account never produces an
  **unauthorized** coexistence, in either order — no candidate is admitted by a caller other
  than the reservation's own owner-with-valid-fence, and no reservation is created while an
  unrelated candidate exists. A candidate coexisting with the reservation that authorized it
  is the expected steady state while `submitted` (FR-037, FR-023).
- **SC-019**: A worker that resumes after losing its lease cannot submit: a fence check
  immediately before submission rejects it, verified by a test that pauses a worker past
  expiry, transfers ownership, then resumes the original (FR-038).
- **SC-020**: A proposal carrying one invalid or non-cosigner signature alongside enough
  valid ones still executes; the invalid entry is ignored, recorded, and excluded from the
  advice map. No single cosigner can render a proposal permanently unexecutable (FR-006).
- **SC-021**: A post-submission failure whose candidate and proposal were deleted still
  reports a readable terminal outcome indicating the proposal is gone — never a silent
  disappearance and never a retry that fails with proposal-not-found (FR-041, FR-042).
- **SC-022**: A server in optimistic delta-commit mode refuses Guardian execution with the
  capability-unavailable error class and reports the misconfiguration at startup (FR-043).
- **SC-023**: Every refusal, state value, and conflict payload is observably equivalent over
  HTTP and gRPC, verified by a transport-parity test per case, not by inspection.

- **SC-024**: The no-retry boundary is observable and crash-safe: a crash after the FR-039
  evidence write never re-submits or re-proves, and a crash before it always
  fails-and-releases — verified by fault injection on both sides of the write (FR-047).
- **SC-025**: Candidate promotion and candidate deletion each atomically persist the
  execution's terminal outcome; no interleaving leaves an execution whose candidate is gone
  and whose outcome was never recorded (FR-041).
- **SC-026**: A `SwitchGuardian` canonicalizing during proving causes the execution to fail
  at the pre-submission re-check; nothing is submitted for a released account (FR-048).
- **SC-027**: A proven transaction whose expiration falls outside the reconciliation horizon
  is refused before the no-retry boundary, so no execution can enter an unbounded unknown state
  (FR-046).
- **SC-028**: Guardian can admit its own candidate and obtain its own acknowledgment while
  holding the account's reservation, while an unrelated caller's candidate admission and
  public `push_delta` remain refused — verified by explicit tests, since the naive rule
  deadlocks (FR-037, FR-044).
- **SC-029**: The envelope is byte-reproducible across languages: both SDKs produce identical
  `format_version`, `protocol_line`, full `serializer_id` (including prerelease), and checksum
  values for identical inputs. Committed fixtures also prove that an unsupported format or an
  unallowlisted serializer on the same protocol line is rejected before deserialization, using
  SHA-256 / `0x`-hex / `MAJOR.MINOR` / exact-equality as fixed in FR-014 and FR-015.
- **SC-030**: The candidate always exists before the transaction is sent: a crash injected
  between the FR-045 step 12 commit and the network send leaves a durable candidate and durable
  evidence, reports `submitted`, and is resolved by reconciliation without re-sending — so no
  execution can ever report `committed` with nothing having reached `canonical` (FR-045, FR-047).
- **SC-031**: A worker that goes stale after crossing the boundary never sends: the pre-send
  fence re-check aborts it, it writes nothing, and reconciliation resolves the durable candidate
  after trustworthy observation reaches the expiration horizon — verified by a test that commits the boundary, transfers
  ownership, then resumes the original worker (FR-049).
- **SC-032**: A transaction that loads a public foreign account (the fee faucet's asset
  callback, and a foreign procedure invocation from a script) executes with that account's state
  at the attempt's reference block; one that loads a private foreign account, or whose foreign
  state the node cannot serve, fails before any proving with
  `GUARDIAN_EXECUTION_FOREIGN_ACCOUNT_UNAVAILABLE` and the matching `meta.reason`, never executed
  against absent foreign state (FR-050).
- **SC-033**: Every built-in Guardian-executable proposal built by either SDK carries the shared
  28,800-block approval expiration and the shared 256-block transaction delta, and both SDKs
  derive the same summary and proposal ID for the same effects, salt, and bound block. Because
  both bounds are signed, that ID differs from the self-executed ID for the same effects; a test
  that keeps the two IDs equal has omitted a bound. The same summary reproduces at a reference
  block later than the proposer's (the relative delta travels with `R`), and the proven
  expiration equals `R + min(256, approval_expiration - R)`. A custom request with a non-zero
  approval expiration and a script-set delta is accepted; one with a zero approval expiration is
  refused as `GUARDIAN_EXECUTION_REQUEST_INVALID`; any transaction whose resulting expiration is
  outside the deployment horizon is refused before the boundary with the FR-046 error. Explicit
  tests cover all built-in families, custom requests with and without a scripted delta, and
  Rust/TypeScript parity (FR-046, FR-051).
- **SC-034**: Execution leases are per-account: two accounts execute concurrently on one
  replica, while two executions for the same account serialize — verified by a test that would
  fail if the cluster-wide canonicalization lease were reused (FR-038).
- **SC-035**: Post-submission ownership transfer is fenced and non-stealing: a reconciliation
  owner claims a live reservation by compare-and-set, a claim presenting a stale holder or
  fence token fails without writing, and the reservation is never released as part of the
  handover (FR-028, FR-052).
- **SC-036**: Exactly one party writes each terminal outcome: a crash injected between any two
  writes of a terminal transition never leaves a terminal execution holding a reservation, nor
  a released reservation without a durable outcome — verified by fault injection on all three
  operations (FR-053).
- **SC-037**: A post-boundary failure deletes its matching proposal in the same commit, so the
  execution reports `proposal_exists: false` and no client is told a definitely-rejected
  transaction may be retried (FR-053, FR-042).
- **SC-038**: Promotion and `resolve_execution` racing on one account produce exactly one
  terminal outcome and one release, with the loser writing nothing — verified by a concurrent
  test on Postgres (FR-054).
- **SC-039**: A status read for a proposal that exists but has never been executed returns the
  defined execution-not-found result, identically on HTTP and gRPC, rather than a synthesized
  state or a transport-specific error.
- **SC-040**: A transient prover failure — including a transport-level connection error or
  i/o timeout, not only a structured error response — does not surface to the caller: the
  execution remains `proving`, is retried under the same reservation, and completes without
  a second execution request. A permanent prover error, or exhaustion of the expiration
  bound, settles `failed` with the reservation released — verified by tests that inject
  both failure families against a stub prover (FR-055).
- **SC-041**: Reproduction runs at the tip under the signed bound block: a proposal executed
  more than 50 blocks after its bound block on devnet (past the node's account-history window)
  reproduces and executes; a request that does not declare its bound block, lacks the multisig
  auth arg or its preimage, carries a zero approval expiration, or consumes unpinned notes is
  refused with `GUARDIAN_EXECUTION_REQUEST_INVALID` and the matching reason before any chain
  read; a node behind the bound block yields `GUARDIAN_EXECUTION_CHAIN_BEHIND` with the
  reservation released; and Guardian never reads the `chain_anchor` field (FR-056, FR-057,
  FR-061).
- **SC-042**: A reached expiration is refused before proving in all three placements of FR-058
  (tip already past the approval expiration, the auth procedure's approval-expired abort, and
  executed expiration reached before a proving retry), each reporting
  `GUARDIAN_EXECUTION_EXPIRATION_REACHED` with the matching `meta.bound` (FR-058, FR-055).
- **SC-043**: A key-fetch, attestation, or sealing failure is injected at FR-045 step 10 and
  settles `GUARDIAN_EXECUTION_SEALING_FAILED` with the reservation released and nothing sent; no
  test can reach the boundary without sealed inputs (FR-059).
- **SC-044**: A `ProtocolConfig` whose commitment differs from the reference header's is refused
  before reproduction, and an account whose fee-asset balance cannot pay the fee settles
  `GUARDIAN_EXECUTION_INSUFFICIENT_FEE`, not a binding mismatch (FR-060, FR-057).

## Assumptions

- **The serialized transaction request already exists at creation time for every
  proposal type.** This is the enabling finding. Built-in types build the request before
  deriving the summary; the custom producer path (#266 / feature 008) receives it as its
  first argument. Configuring a client Guardian-executable therefore requires no new input
  for attachment. Built-in methods add both expiration bounds internally. A custom producer
  already owns the transaction recipe and its auth args, so it binds the approval expiration
  itself and scripts any transaction expiration; the SDK cannot retrofit an opaque request.
- **The stored request is verified by re-execution, not trusted.** Storing it creates no
  authority: a tampered or stale request cannot produce the signed commitment, so it
  fails closed at FR-007. This is why persisting it is safe even though Guardian is not
  a trusted author of transactions.
- **The serialized request is not sufficient on its own to execute.** Authenticated
  account state, block and blockchain data, witnesses, and note scripts come from Guardian's
  own stored state plus the Miden node at execution time; see the Execution Architecture
  section. Foreign public accounts are read from the node at the reference block (FR-050).
- **Guardian re-executes against the account's current state**, so a proposal whose
  account has advanced past its base fails the FR-007 binding check rather than
  submitting stale work. This mirrors the nonce discipline already applied when listing
  proposals.
- **Payload growth is bounded and measured.** A serialized P2ID payment transaction
  request measures 26,078 bytes (private note) / 26,087 bytes (public), measured directly
  against the SDK's builder; roughly 35 KB once base64-encoded in JSON. Configuration
  transactions are expected to be comparable or smaller, because the multisig library is
  dynamically linked into the script rather than embedded — **not yet measured**. FR-016
  makes the limits explicit rather than relying on these figures.
- **Guardian gains no new authority over account state.** It could already withhold its
  acknowledgment; it still cannot forge one. The new powers are strictly about *when*
  and *whether* an already-authorized transaction is carried out — a liveness and
  ordering surface, not an authorization one. FR-035 keeps self-execution as the escape
  hatch for exactly this reason.
- **A definite submission rejection and an unknown submission outcome are different
  events internally, but not to the caller.** Only a definite rejection permits releasing
  the reservation; conflating them is what would risk a double submission. The distinction
  therefore lives in the internal record and in the reservation's retention (FR-030), not
  in the reported vocabulary — an unknown outcome reports `submitted`, whose caller
  contract (wait, watch the delta, do not retry) is already correct for it. Reporting it
  separately would also have required a state that is simultaneously a failure and a
  potential success.
- **A state earns a place in the reported vocabulary only by changing what the caller
  does.** `pending`/`proving` differ for diagnosis but not for control flow, and proving is
  kept separate solely because it is the multi-minute phase most likely to hang.
  Post-submission discard folds into `failed` because the caller's action — retry — is
  identical to a pre-submission failure. Finer distinctions belong to logs, metrics, and
  the internal record (FR-025).
- **Chain observation is the only authority on an unknown outcome.** Guardian already
  polls the chain to canonicalize deltas; resolving an unknown submission reuses that
  observation capability rather than introducing a new one.
- **Ignoring bad signatures beats verifying at ingestion.** Guardian's existing proposal
  endpoints accept cosigner signatures without cryptographic verification, and nothing can
  remove one afterward. Selecting the valid subset at execution (FR-006) is strictly more
  robust than rejecting at ingestion: it needs no change to existing endpoints, and it is
  self-healing where ingestion checks would still leave already-stored bad entries fatal.
  Verifying at ingestion would also start rejecting payloads today's clients successfully
  submit, which this feature deliberately avoids. See Out of Scope.

## Relationship to feature 008 (Custom Proposal Producer)

Feature 008 FR-015 states that the serialized transaction request MUST NOT be stored
anywhere, by the SDK or the server. That rule is **narrowed, not reversed**, by this
feature, and the narrowing is deliberate:

- FR-015's stated rationale is keeping the server a minimal coordinator. Issue #253
  explicitly changes that goal — "the basic unit of the Guardian should be a
  transaction" — so the premise no longer holds for proposals whose proposer opts into
  Guardian execution.
- FR-015's accepted trade-off was that only a recipe-holder can execute a custom
  proposal. This feature's purpose is to remove exactly that constraint, on an opt-in
  basis.
- The security property FR-015 was protecting is **not** confidentiality of the request;
  it is minimalism plus the strongest possible binding. The binding invariant (008
  FR-007) is carried over verbatim as FR-007 here — relocated, not weakened.

Accordingly: FR-009 and FR-010 confine storage to proposals whose proposer asked for it,
and SC-009 requires a test proving that every other proposal keeps 008's property
exactly. Feature 008's spec should be annotated to point at this narrowing so the two
documents do not read as contradictory.

## Out of Scope

- **Server-side proposal preparation.** A future authenticated `prepare` operation could
  accept a request and execution context, derive the summary, store an unsigned proposal,
  and return its ID and summary. Existing signing operations would collect approvals.
  Preparation would not reserve the account or authorize execution. Client preparation
  and self-execution remain the default; no preparation API is specified in v1.
- **Automatic execution when the signing threshold is reached.** V1 requires an explicit
  request. A future opt-in account policy could queue eligible proposals after verifying
  the valid-signature threshold. It needs signer-visible policy configuration and defined
  conflict/queue behavior while preserving normal authorization and state checks.
- **Dependent chains and independent proposal revalidation.** V1 does not pipeline
  speculative account states or preserve viability after the recorded base changes.
  Future work must distinguish dependent chains from independent proposals whose signed
  summaries still reproduce after another transaction commits. Chaining needs ordering
  and parent-failure recovery; revalidation must preserve signed execution context.
- **Cryptographic verification of cosigner signatures at ingestion.** Recommended as a
  separate change: it would make Guardian's stored record trustworthy and fail earlier,
  but it alters the behavior of the existing proposal-create and proposal-sign endpoints
  and would reject payloads today's clients successfully submit. Tracked separately so it
  is not smuggled in behind #254.
- **Batch proving of multiple transactions (#239)** and **operator-delegated proving
  configuration (#180)** — both build on this capability.
- **A canonical nonce endpoint (#191)**.
- **In-process local proof generation.** Excluded by FR-019: local proving is a
  deployment topology (an operator-run prover the server points at), not a server code
  path.
- **A persistent or synced server-side chain store.** Excluded by the Execution
  Architecture decision; each execution seeds an ephemeral store at the tip.
- **Operator dashboard surfacing or control of executions.** No `/dashboard/*` changes;
  no operator permission-vocabulary changes.
- **Uploading offline-collected signatures into Guardian's stored proposal.** FR-018
  scopes readiness to Guardian-held signatures; an upload/merge path is a separate
  capability.
- **Carrying the serialized transaction request through the offline export format.**
- **Instant settlement between accounts using ephemeral notes** (a later #253
  capability).
- **EVM accounts.** This feature concerns Miden accounts only; the EVM surface continues
  to return executable data for its caller to submit.
- **Policy-based restriction of which proposal types may be Guardian-executed** (policy
  module; #182/#251).
