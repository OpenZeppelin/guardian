# Data Model: Guardian Prove and Commit

**Last Revised**: 2026-09-30 (re-verified against the Miden 0.17 release-candidate pins on `main`:
protocol / standards / tx `0.17.0-rc.7`, `miden-client` `0.17.0-rc.4`, `miden-node-proto-build`
`0.17.0-rc.3`, web SDK `0.17.0-rc.4`. FR-045 is now fourteen steps: the boundary commit is step
12 and the send is step 14. `reference_block` is the tip-chosen `R` (FR-061), the expiration has
two signed bounds (FR-051), and sealed submission inputs are prepared in memory before the
boundary and never persisted (FR-059))

Entities added by #254. Wire shapes are normative in
[contracts/execution-api.md](./contracts/execution-api.md); this document covers
**storage-side** shapes, the atomic write units, and the internal→reported state
mapping.

Rust/SQL spellings below are illustrative; field meanings and atomicity
boundaries are normative.

## Design centre: the boundary is a row, not a flag

FR-047 requires "submission authorized and prepared" to be one durable,
observable transition, never an inference. The submission evidence **is** that
transition, so its presence is the boundary; there is no separate
`submission_attempted` boolean to drift out of sync with it. The name of the
column is historical/internal; evidence may exist before the network send begins.

This is why the FR-045 step 12 commit writes the candidate and the evidence
**together**: one commit makes "about to submit" a single durable fact. Any
model with two writes, or with a flag beside the evidence, reintroduces the
window this design exists to close.

## Proposal admission quotas

FR-016 adds viable count accounting by `(account_id, authenticated proposer_id)`.
Use the stored authenticated proposer identity and compare each proposal's base commitment
with the current canonical commitment. Stale proposals do not consume viable count quota.
Check per-proposer counts, account-wide counts and request-byte limits together with
insertion under the account lock or transaction on both filesystem and Postgres backends.
Concurrent creates must not oversubscribe a quota. This admission operation creates no
execution reservation. The final count allocation rule must preserve other signers'
allocated capacity; its configuration is pending design, with two per proposer suggested.

## Stored request envelope (FR-014)

The serialized `TransactionRequest` is stored with the proposal payload, inside the FR-014
envelope; the wire shape is normative in
[contracts/execution-api.md](./contracts/execution-api.md#proposal-payload-addition). Stored
example on the 0.17 line:

```json
{
  "format_version": 1,
  "protocol_line": "0.17",
  "checksum": "0x…",
  "bytes": "<base64>"
}
```

- There is no serializer identity (decided 2026-10-01: a version allowlist protects nothing the signed summary does not already protect, and it doubled the work of every `miden-client` bump). `TransactionRequest` serialization carries no
  version tag, and rc.4 added `block_numbers` as the **first** serialized field
  (`miden-client-0.17.0-rc.4/src/transaction/request/mod.rs:447-477`; rc.3 starts with
  `input_notes`, `miden-client-0.17.0-rc.3/src/transaction/request/mod.rs:439-443`), so rc.3 and
  rc.4 bytes do not decode across each other although both declare `protocol_line` `"0.17"`.
  Such a request fails to decode or fails the summary comparison, both before proving.
- Nothing in the envelope is derived from the chain. The bound block, the approval expiration
  and the auth arg are all read from the decoded request and the signed summary at FR-045 step 2;
  the stored `chain_anchor` metadata is not an execution input.

## `ExecutionReservation`

One row per account, at most one **active** at a time. Spans acceptance through
terminal state (FR-023).

| Field | Type | Required | Notes |
|---|---|---|---|
| `account_id` | string | yes | Account under execution |
| `proposal_id` | string | yes | The proposal being executed; with `account_id`, the execution handle (FR-003) |
| `attempt` | i32 | yes | 1-based; increments per retry of the same proposal. See § Attempt identity |
| `holder_id` | string | yes | Owning worker; from `LeaseFence.holder_id` (FR-038) |
| `lease_name` | string | yes | **`execution:{account_id}`**: account-scoped, never the cluster-wide canonicalization lease (FR-038) |
| `fence_token` | i64 | yes | Monotonic; from `LeaseFence.fence_token` |
| `lease_expires_at` | timestamptz | yes | Renewable (FR-023, FR-028) |
| `phase` | enum | yes | Internal phase; never on the wire (FR-025). A `CHECK` admits only the known phase names |
| `ignored_signatures` | i32 | yes | Count excluded as invalid / duplicate / non-cosigner (FR-006) |
| `created_at` / `updated_at` | timestamptz | yes | |

The reservation does not carry the candidate's nonce: the submission evidence does, written in
the same commit. The filesystem backend, which cannot commit several files atomically, records
the admission it is about to write in a private marker file instead, so a crash before the
evidence leaves the orphaned candidate identifiable.

The evidence and the outcome reference their reservation by the attempt key
(`account_id`, `proposal_id`, `attempt`) with a foreign key, and an outcome's error columns are
constrained to its state: none for `committed`, a code and a message for `failed`.

**Why not the existing pending-candidate flag**: that flag only exists *after* a
candidate is persisted, which is after proving. The whole span this feature must
protect (accept, verify, acknowledge, execute, prove) precedes it (FR-023).

### The lease is per-account

`lease_name` is `execution:{account_id}`. This matters more than it looks: `worker_leases`
admits **one holder per `lease_name`** (`ON CONFLICT (lease_name) DO UPDATE`), and
`CANONICALIZATION_LEASE` is the single cluster-wide string `"canonicalization"`. Fencing
reservations against that lease would reduce the entire deployment to one execution at a
time, and would make the fence check answer a question about the canonicalization worker
rather than about this account's reservation.

Two properties fall out of the account-scoped name, both required:

- Executions for **different** accounts proceed concurrently, contending on different lease
  rows.
- Executions for the **same** account serialize at lease acquisition, before any reservation
  row is written, so the admission primitive is a second line of defence rather than the
  only one.

FR-038 requires reusing the `LeaseFence` **type**. It forbids reusing the canonicalization
**lease**.

## Attempt identity

A pre-boundary failure is terminal *and* retryable: `state: failed` with
`proposal_exists: true` explicitly permits another attempt on the same proposal. So
`(account_id, proposal_id)` identifies a **handle**, not a single execution, and rows keyed
on the pair alone cannot represent two attempts.

`attempt` is therefore part of the identity of every execution-owned row:

- The handle on the wire stays `(account_id, proposal_id)` (FR-003). No new identifier is
  exposed; attempt numbering is internal.
- **The number is allocated inside reservation creation**, under the same account lock, as
  `max(attempt) + 1` for the handle. It is a write concern, not a read one: allocating it in
  the status/derivation layer would mean two concurrent retries could compute the same number
  before either inserted.
- A status read reports the **most recent** attempt for the handle. This is normative: an
  unqualified read of a retried proposal would otherwise be ambiguous.
- Uniqueness is `(account_id, proposal_id, attempt)`, never the pair.
- At most one attempt may be **active** per account, which the partial unique index on
  `account_id` already enforces.

An earlier revision keyed submissions and outcomes on the pair alone. In practice the
collision was hard to trigger (pre-boundary failures write no outcome row, and a
post-boundary failure deletes the proposal, so a second post-boundary attempt cannot
arise), but the schema still could not express a retry, and the contract never said which
attempt a read reports.

### Retention

Execution-owned rows are not kept forever. A daily sweep on every replica deletes an attempt
(its reservation, evidence and outcome, in foreign-key order: outcome, evidence, reservation)
only when all of these hold:

- the reservation is released (`released_at` set) and its outcome's `resolved_at` is older
  than `GUARDIAN_EXECUTION_RECORD_RETENTION_DAYS` (default 30; `0` disables the sweep, any
  other value below 2 refuses startup so records outlive the approval window);
- its proposal no longer exists for the account, **or** a newer attempt of the same proposal
  exists.

So an active attempt is never deleted, and the newest attempt of a proposal that still exists
always survives: `max(attempt) + 1` stays correct for a live handle, and the "most recent
attempt" a status read reports for a live proposal is never removed. Once every attempt of a
deleted proposal is gone, a status read returns `GUARDIAN_EXECUTION_NOT_FOUND`, as for a
proposal never executed. The sweep deletes in bounded batches
(`StorageBackend::prune_execution_records(cutoff, limit)`), each one transaction in Postgres
(oldest outcomes first through `execution_outcomes (resolved_at)`, locked rows skipped) and one
per-account rewrite of `executions.json` under the write lock on the filesystem backend. It is
idempotent, so replicas need no lease to run it.

## `SubmissionEvidence`

Written **before** the network send, inside the step-12 commit (FR-039).

| Field | Type | Required | Notes |
|---|---|---|---|
| `account_id` | string | yes | |
| `proposal_id` | string | yes | |
| `attempt` | i32 | yes | Which execution attempt this evidence belongs to; see § Attempt identity |
| `candidate_nonce` | i64 | yes | The candidate admitted in the same commit |
| `transaction_id` | string | yes | **FR-039.** The proven transaction's id |
| `expected_commitment` | string | yes | **FR-039.** The account commitment the transaction is expected to produce. FR-040's superseded rule is unimplementable without it: "moved somewhere that is neither base nor this transaction's result" requires knowing the result |
| `reference_block` | i64 | yes | **FR-039.** `R`: the node's committed tip chosen at this attempt's start, which the transaction was executed and proven against (FR-061). Not the summary's bound block and not an anchor; each attempt records its own `R` |
| `expiration_block` | i64 | yes | **FR-039.** From `ProvenTransaction::expiration_block_num()`, **not** the request's `expiration_delta` and not the signed approval expiration. The FR-046 horizon is `expiration_block - reference_block` |
| `base_commitment` | string | yes | Account state the transaction was built against; FR-040's "still at base" test |
| `committed_at` | timestamptz | yes | |

All four FR-039 fields are mandatory, and each is load-bearing for a specific
reconciliation rule rather than merely diagnostic: `expected_commitment` distinguishes
committed from superseded, `base_commitment` detects "never moved", `expiration_block` bounds
the wait, and `transaction_id` is what an operator correlates against the chain. An earlier
revision of this document carried only the expiration block, which left the superseded rule
stated but unimplementable.

**`expiration_block` source is normative.** Three values bear on it, and only the third is
stored:

- `TransactionRequest::expiration_delta` is `Option<u16>`, `None` meaning non-expiring
  (`miden-client-0.17.0-rc.4/src/transaction/request/mod.rs:128-130`). The signed
  `TransactionSummary` carries the delta as a `u16` in its metadata element, next to the bound
  block (`miden-protocol-0.17.0-rc.7/src/transaction/tx_summary.rs:37,270-282`), relative to the
  executing reference block, so it reproduces at any `R`. Built-in families sign 256 (FR-051).
- The signed approval expiration (summary user param 0) is absolute. The multisig auth procedure
  applies it after the summary is built, through `tx::update_expiration_block_delta`, which only
  ever lowers the expiration (`miden-standards-0.17.0-rc.7/asm/standards/auth/multisig.masm:931-942`;
  `miden-protocol-0.17.0-rc.7/asm/kernels/transaction-core/src/tx.masm:143-169`).
- The authoritative value is the proven transaction's own expiration block
  (`miden-protocol-0.17.0-rc.7/src/transaction/proven_tx.rs:179`): `R + min(256,
  approval_expiration - R)` for a built-in family, and the approval bound clamped to 65,535 from
  `R` for a custom producer with no script delta.

Measured on 0.16: a transaction with no delta proves with `u32::MAX`, never expires. On 0.17 a
Guardian-executable proposal cannot reach that state, because a zero approval expiration is
refused at step 2 (`GUARDIAN_EXECUTION_REQUEST_INVALID`, `meta.reason =
approval_expiration_missing`), and FR-046 still refuses any proven expiration beyond the horizon
from `R`. That is what makes FR-040's `expired` path able to fire at all. Reading a delta instead
of the proven value was a real defect in an earlier revision.

### What is deliberately not in the evidence

The submission evidence is exactly what reconciliation reads. Two execution products are held
**in memory only** by the owning worker and are never persisted:

| Value | Produced | Consumed | Why not durable |
|---|---|---|---|
| `TransactionInputs` | step 7 (execute) | step 10 (seal); retained in memory through step 14 | Cannot be recovered from a `ProvenTransaction`, so the worker keeps it from execution through submission. Nothing after step 14 needs it |
| Sealed submission inputs (`SealedTransactionInputs{key_id, ciphertext}`, `miden-node-proto-build-0.17.0-rc.3/proto/types/submission.proto:8-28`) | step 10 (seal, FR-059), before the boundary | step 14 (send) | Nothing is ever re-sent (FR-047). A crash before step 12 fails and releases; a crash after step 12 reconciles and never resubmits, so losing the blob loses nothing |

Sealing is pre-boundary for the same reason admissibility is: a key-fetch, attestation or sealing
failure after the step-12 commit would hold the account until expiration for a transaction that
was never sent. A crash or fence loss between steps 12 and 14 leaves evidence without a sent
transaction, which FR-049 already covers; the missing blob does not change that path.

Neither the summary's bound block nor any `ChainAnchor` is part of the evidence. The bound block
is read from the signed summary on every attempt; the anchor is not an execution input on 0.17.

## `ExecutionOutcome`

Persisted terminal outcome (FR-041). The **only** reported state that is stored
rather than derived.

| Field | Type | Required | Notes |
|---|---|---|---|
| `account_id` | string | yes | |
| `proposal_id` | string | yes | |
| `attempt` | i32 | yes | Which attempt resolved; see § Attempt identity |
| `state` | enum | yes | `committed` or `failed` only |
| `error_code` | string | no | Required when `failed`; from the contract's vocabulary. Pre-boundary codes written by `fail_execution` include the 0.17 set: `GUARDIAN_EXECUTION_REQUEST_INVALID`, `GUARDIAN_EXECUTION_EXPIRATION_REACHED`, `GUARDIAN_EXECUTION_CHAIN_BEHIND`, `GUARDIAN_EXECUTION_CHAIN_INCONSISTENT`, `GUARDIAN_EXECUTION_NODE_UNAVAILABLE`, `GUARDIAN_EXECUTION_INSUFFICIENT_FEE`, `GUARDIAN_EXECUTION_SEALING_FAILED`, `GUARDIAN_EXECUTION_FOREIGN_ACCOUNT_UNAVAILABLE`, `GUARDIAN_EXECUTION_EXPIRATION_BEYOND_HORIZON`, `GUARDIAN_EXECUTION_BINDING_MISMATCH` |
| `error_meta` | json | no | The code's structured `meta` where it has one: `reason` (`REQUEST_INVALID`, `FOREIGN_ACCOUNT_UNAVAILABLE`) or `bound` = `approval` / `transaction` (`EXPIRATION_REACHED`) |
| `error_message` | string | no | Human-readable |
| `resolved_at` | timestamptz | yes | |

It exists because canonicalization's `remove_candidate`
(`jobs/canonicalization/processor.rs:1092`) deletes an unrecoverable candidate
**and then its matching proposal**, destroying whatever a derived state would
read. It MUST be written atomically with, and no later than, the promotion or
deletion that determines it.

Pre-terminal states are **derived and never persisted**, so the two
representations cannot both exist and drift.

## Storage write outcomes

Exhaustive enums mirroring the existing `CanonicalWrite`
(`storage/mod.rs`), one per operation so every match site handles only the
outcomes that operation can produce. No catch-all variant: a new case must force a
compile error at every match site. As implemented in `crates/server/src/storage/execution.rs`:

```rust
pub enum ReservationWrite {          // create
    Created { attempt: u32 },
    AlreadyReserved { holder_id: String, proposal_id: String },
    CandidateExists,
    StaleLease,
}

pub enum ReservationUpdate {         // renew / advance phase
    Applied,
    StaleLease,
    NotActive,
}

pub enum ClaimWrite {                // FR-052 ownership transfer
    Claimed,
    ClaimSuperseded, // the caller's expected holder/fence no longer matches
    StaleLease,      // the claimant's own lease is not current
    NotActive,
}

pub enum AdmissionWrite {            // the step-12 boundary commit
    Admitted,
    NotAuthorized,   // caller is not this reservation's owner for this attempt (FR-037)
    CandidateExists,
    NonceOccupied,   // a settled delta holds the candidate's nonce
    StaleBase,       // the stored state left the candidate's base
    StaleLease,
}

pub enum ResolveWrite {              // fail_execution / resolve_execution
    Resolved,
    NotAuthorized,
    AlreadyResolved,
    StaleLease,
    WrongSideOfBoundary, // pre-boundary write after the boundary, or the reverse
}
```

`CandidateSubmission` (the public `push_delta` path) gains `ExecutionReserved { proposal_id }`,
which the service maps to `GUARDIAN_EXECUTION_CONFLICT` with `meta.blocking_proposal_id`. Both
backends check the reservation before the pending-candidate conflict, so a client push against a
reserved account always names the execution.

There is no standalone release operation: every release is part of `fail_execution`,
`resolve_execution` or the extended promotion, so no path can release without an outcome.

`CanonicalWrite` gains one variant, `ProtectedByExecution`, returned when the canonicalization
worker attempts to discard a candidate owned by an unresolved boundary-crossed execution. It is
exhaustively matched like every other variant, so adding it forces every existing call site to
decide what to do, which is the point.

`AlreadyReserved` carries `proposal_id` because the contract requires
`meta.blocking_proposal_id` to name the blocker (FR-036), and the storage layer is
the only place that knows it.

## The atomic write units

Four writes, each one account-scoped transaction. Nothing below may be split
into check-then-act (FR-037).

| Unit | Contents | Returns |
|---|---|---|
| **Create reservation** | Acquire the `execution:{account_id}` lease, then insert the reservation iff no candidate exists and no active reservation | `ReservationWrite` |
| **Admit candidate + record evidence** (step 12) | Persist candidate, set `has_pending_candidate`, insert evidence, **as one commit** | `AdmissionWrite` |
| **Promote candidate + resolve** | Existing fenced promotion, **extended** to upsert `ExecutionOutcome { committed }` and release the reservation in the same transaction. Owns `committed` (FR-053, FR-054) | `PromoteWrite` |
| **`resolve_execution`** | Post-boundary failure: validate execution ownership + fence, discard the candidate, **delete its matching proposal**, upsert `ExecutionOutcome`, release the reservation, **one transaction** | `ResolveWrite` |
| **`fail_execution`** | Pre-boundary failure: upsert `ExecutionOutcome` and release the reservation as **one commit**. No candidate exists, so nothing to discard | `ResolveWrite` |
| **Claim ownership** (FR-052) | Compare-and-set `holder_id` / `fence_token` on a live reservation; fails rather than steals on a stale expectation | `ReservationWrite` |
| **Renew / release** | Update expiry, or mark `released_at` | `ReservationWrite` |

### Terminal resolution is not its own *separate* write: but it is its own *operation*

SC-025 requires candidate promotion and candidate deletion to **each** atomically persist the
outcome. Two operations satisfy that, and the split matters:

- **`committed`** is owned by the extended `promote_candidate`. Promotion is what makes the
  outcome true, so nothing else may write it: a reconciliation worker that also persisted
  `committed` on observing `canonical` would be a second writer racing the first.
- **Every post-boundary failure** (definite rejection, superseded, expired) is owned by
  `resolve_execution`, which discards the candidate *and* persists the outcome *and* releases
  the reservation in one transaction.

The reason neither may be split into steps is `remove_candidate`, which deletes the candidate
*and its proposal*. A separate `record_execution_outcome` call racing that deletion can find
the record it needs already destroyed, the exact failure FR-041 exists to prevent.

Pre-boundary failures use **`fail_execution`**, which is still one commit even though it has no
candidate to discard. Persisting the outcome and releasing the reservation as two operations
would let a crash between them expose either a terminal execution still holding its account, or
a released account with no outcome to report. There is no `record_execution_outcome` that writes
an outcome without also releasing: every terminal transition is one atomic operation (FR-053).

### `resolve_execution` must delete the matching proposal

Canonicalization's `remove_candidate` deletes the candidate inside its transaction and then
derives and deletes the matching proposal **afterwards**, tolerating failure with a warning
(`processor.rs:1092-1130`). A post-boundary execution cannot inherit that looseness.

`proposal_exists: true` on a `failed` execution is precisely what FR-042's contract defines as
a permitted retry. If `resolve_execution` discarded the candidate but left the proposal, a
**definitely rejected** transaction would be advertised to the client as retryable. So proposal
deletion is part of the resolution commit, not a follow-up step.

### Promotion is explicitly authorized to release (FR-054)

Promotion runs under the **canonicalization** lease, not the account's execution lease, yet it
now releases an execution reservation, and FR-038 requires every durable mutation to validate
the execution fence. Promotion is authorized to do this while holding the canonicalization
fence, provided it takes the same per-account lock every other reservation write takes.

The justification is narrow: promotion is what makes `committed` true, and it cannot be expected to
hold a lease belonging to a worker that may have died. The per-account lock is what keeps it
safe: promotion and `resolve_execution` serialize on it, whichever commits first wins, and the
loser observes `AlreadyResolved` and writes nothing. Both results are individually correct, so
the lock decides rather than a precedence rule.

### Ordinary canonicalization discard versus execution resolution

These are different operations on the same row, and conflating them deadlocks the account.

`discard_candidate`, called by the **canonicalization worker**, MUST refuse a candidate whose
execution has crossed the boundary and not yet resolved, returning a distinct
`CanonicalWrite::ProtectedByExecution`. That protection exists because between step 12 and
step 14 the candidate looks ordinary to canonicalization, which would otherwise leave a
submitted transaction with no candidate to promote.

`resolve_execution`, called by the **owning execution or its reconciliation owner**, discards
that same candidate deliberately: it is the only operation permitted to, and it validates
execution ownership and fence to prove it is entitled.

An earlier revision expressed the protection as a blanket refusal to discard any unresolved
boundary-crossed candidate. That was wrong in a way worth naming: it also blocked the
definite-rejection, superseded, and expired paths, which *require* exactly that discard. The
account would then be unresolvable until expiration, and the expired path could not clear it
either, so the wedge was permanent. The protection must be scoped to the caller, not to the
row.

### The owner-authorized exception is required, not optional

FR-037's blanket rule (admission fails if a reservation is active) would
deadlock Guardian against itself: it holds the reservation for the account and
must then admit its *own* candidate. Admission is therefore permitted for the
caller presenting the **matching reservation's owner identity and a valid
fence**, and refused for every other caller.

The test is "is this the candidate this reservation authorized", not "does a
reservation exist". This was a genuine self-deadlock in an earlier revision;
SC-028 is its regression test.

## Backend implementations

| Concern | Postgres | Filesystem |
|---|---|---|
| Atomicity | One transaction | One in-process mutex hold |
| Serialization point | `lock_account_metadata`: per-account `SELECT … FOR UPDATE` (`postgres.rs:825`) | `delta_write_lock` (`filesystem.rs:25`) |
| Execution fencing | `lease_fence_is_current`; unfenced call **refused** via `unfenced_write_error` (`postgres.rs:786`) | Active reservation `holder_id` / `fence_token` compared under `delta_write_lock`; stale or unfenced execution writes refused |
| Single-active enforcement | Partial unique index, at the schema level | Single file per account |

**Filesystem must take `delta_write_lock` itself, not a new mutex.** Admission
must be atomic with respect to candidate writes, and that specific lock is what
serializes them (`submit_delta`, `request_candidate_abandon`,
`update_delta_status`, `update_candidate_status`). A separate mutex would permit
exactly the interleaving FR-037 forbids while appearing to be locked.

Single-process deployment removes cross-replica contention, but it does **not** remove stale
tasks: an execution lease can expire, a reconciliation task can claim the reservation, and the
original task can later resume. Every **execution-owned** filesystem mutation therefore reads
the active reservation and validates its holder and fence while holding `delta_write_lock`.
Claiming ownership changes the holder and advances the fence under that same lock. This applies
to renewal, admission plus evidence, fail/resolve, claim/release, and the pre-send validation;
a stale task returns `StaleLease` and writes nothing.

This does not retrofit execution ownership onto unrelated primitives. In particular,
`request_candidate_abandon` remains an intentionally unfenced, non-destructive client
annotation, and the single-process canonicalization elector remains `AlwaysLeader`. The new
checks protect the execution reservation whose ownership really can transfer within one
process.

Consequence: stale-task and ownership-transfer scenarios are validated on **both** backends;
true cross-replica races remain Postgres-only. All other scenarios must produce identical
observable outcomes on both.

## Migration

`crates/server/migrations/2026-10-01-000001_execution_reservations/`

```sql
CREATE TABLE execution_reservations (
    id                  BIGSERIAL PRIMARY KEY,
    account_id          TEXT        NOT NULL,
    proposal_id         TEXT        NOT NULL,
    attempt             INTEGER     NOT NULL,
    holder_id           TEXT        NOT NULL,
    lease_name          TEXT        NOT NULL,
    fence_token         BIGINT      NOT NULL,
    lease_expires_at    TIMESTAMPTZ NOT NULL,
    phase               TEXT        NOT NULL,
    candidate_nonce     BIGINT,
    ignored_signatures  INTEGER     NOT NULL DEFAULT 0,
    released_at         TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, proposal_id, attempt)
);

-- FR-029's single-owner rule enforced by the schema, not by application logic.
CREATE UNIQUE INDEX execution_reservations_one_active_per_account
    ON execution_reservations (account_id)
    WHERE released_at IS NULL;

CREATE TABLE execution_submissions (
    id                   BIGSERIAL PRIMARY KEY,
    account_id           TEXT        NOT NULL,
    proposal_id          TEXT        NOT NULL,
    attempt              INTEGER     NOT NULL,
    candidate_nonce      BIGINT      NOT NULL,
    transaction_id       TEXT        NOT NULL,
    expected_commitment  TEXT        NOT NULL,
    reference_block      BIGINT      NOT NULL,
    expiration_block     BIGINT      NOT NULL,
    base_commitment      TEXT        NOT NULL,
    committed_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, proposal_id, attempt)
);

-- Canonicalization must be able to see that a candidate belongs to a live execution
-- before discarding it (FR-049): the execution may still be between its boundary commit
-- and its send.
CREATE INDEX execution_submissions_account_nonce
    ON execution_submissions (account_id, candidate_nonce);

CREATE TABLE execution_outcomes (
    id             BIGSERIAL PRIMARY KEY,
    account_id     TEXT        NOT NULL,
    proposal_id    TEXT        NOT NULL,
    attempt        INTEGER     NOT NULL,
    state          TEXT        NOT NULL CHECK (state IN ('committed', 'failed')),
    error_code     TEXT,
    error_message  TEXT,
    error_meta     JSONB,
    resolved_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, proposal_id, attempt)
);

-- The retention sweep walks finished attempts oldest first (§ Retention).
CREATE INDEX execution_outcomes_resolved_at
    ON execution_outcomes (resolved_at);

ALTER TABLE delta_proposals ADD COLUMN request_bytes BIGINT NOT NULL DEFAULT 0;
```

`delta_proposals.request_bytes` is the decoded size of the proposal's stored request, computed
by the service before the storage encryption decorator runs, so the FR-016 aggregate can be
summed in the same transaction that checks it. The filesystem backend keeps the same numbers in
`proposal_request_bytes.json` beside the account's proposals and counts an entry only while its
proposal exists and is viable, pruning the rest on the next admission. Both backends check the
viable count and the byte aggregate and insert as one step under the account lock
(`StorageBackend::admit_delta_proposal`). A proposal and its stored request are one row (or one
file), so FR-017 cleanup needs no separate path.

The partial unique index is deliberate: FR-029 forbids two concurrent
executions for one account across all replicas, and a database constraint holds
that even if a service-layer check is ever bypassed or refactored away.

`execution_submissions`' uniqueness on `(account_id, proposal_id, attempt)` is a second
structural guard on the no-retry boundary: a second attempt to cross it *for the same
attempt* fails on the constraint rather than on a code path.

### The post-boundary candidate must be protected from canonicalization

Between the step-12 commit and the send, the candidate is an ordinary candidate as far as
canonicalization is concerned, so the canonicalization worker could discard it while the
execution's fence is still live and FR-049 still permits the send. That would leave a
submitted transaction with no candidate to promote, the precise state FR-045's step-12-before-14
ordering exists to prevent.

**Discard MUST therefore consult `execution_submissions` for the account and nonce inside its
own transaction**, and MUST NOT discard a candidate whose execution has crossed the boundary
and not yet resolved. The index above exists for that lookup. This is a change to an existing
primitive, not a new one, and it is why the discard write unit above is listed as extended.

## Internal phase → reported state

Internal phases (FR-025) never appear on the wire; each maps onto exactly one of
the five reported values (FR-024).

| Internal `phase` | FR-045 steps | Reported | Boundary crossed |
|---|---|---|---|
| `accepted` | before 1 | `pending` | no |
| `verified` | 1 to 4 (signatures, structural request checks, approval-expiry check, `R` and chain view, reproduction) | `pending` | no |
| `acknowledged` | 5 to 6 | `pending` | no |
| `executed` | 7 to 8 (execute, re-verify binding, expiration-reached check) | `pending` | no |
| `proving` | 9 | `proving` | no |
| `proved` | 10 to 11 (seal, admissibility and horizon re-check) | `proving` | no |
| `submission_committed` | 12 to 13 | `submitted` | **yes** |
| `sent` | 14 | `submitted` | **yes** |
| `reconciling` | after 14 | `submitted` | **yes** |
| `resolved` | terminal | from `ExecutionOutcome` | either |

Sealing (step 10) happens while the reported state is still `proving`, and a sealing failure is an
ordinary pre-boundary `failed` with `proposal_exists: true`. The `TransactionInputs` and the
sealed blob live only in the worker's memory from `executed` through `sent`; no phase implies they
were persisted (see § What is deliberately not in the evidence).

`submission_committed` versus `sent` is the distinction FR-031 requires on
restart: both report `submitted` and neither may be retried, but only
`submission_committed` may not yet have reached the network. Both are resolved
by reconciliation, never by resubmission.

Because the boundary is the evidence row's existence, this column is
diagnostic: recovery reads the evidence, not the phase. A phase that disagreed
with the evidence would be a bug, and the evidence wins.

## Reconciliation inputs (FR-040)

Reconciliation needs no transaction-status lookup, and Miden still exposes no status-by-id RPC.
It resolves from observations already available. On 0.17, `SyncTransactions(block_range,
account_ids)` returns `TransactionRecord{block_num, header, ...}` (`rpc.proto:90,798-841`), and
Guardian's RPC client already has `sync_transactions` (`crates/miden-rpc-client/src/lib.rs:518`),
so an inclusion observation is available as a faster input for the committed path. Like the
expected-commitment observation below, it is an input to promotion and to status derivation,
never a reconcile-owned write; the expiration bound remains the backstop.

**Reconciliation owns exactly two terminal paths.** `committed` is **not** one of them:

| Reconcile-owned path | Observation | Outcome |
|---|---|---|
| **Superseded** | Account moved to a commitment that is neither `base_commitment` nor `expected_commitment` | `failed` / `GUARDIAN_EXECUTION_CANDIDATE_DISCARDED` |
| **Expired** | Chain passed `expiration_block` with the account still at `base_commitment` | `failed` / `GUARDIAN_EXECUTION_EXPIRED` |

Plus FR-031 restart recovery, which resolves rather than retries.

`committed` is owned solely by the extended `promote_candidate` (FR-053). Observing the account at
`expected_commitment` is an **input**: it tells reconciliation this execution is not
superseded and not expired, so it must wait for promotion; it is never a second write of `committed`.
A reconciliation loop that upserted `committed` on that observation would be a second writer
racing promotion, which is the exact `remove_candidate` race FR-041 exists to prevent.

`expected_commitment` is still what makes the distinction possible at all: without it,
superseded and "waiting for promotion" both reduce to "the account is not at base", which
cannot be resolved.

The expired path supplies a finite **chain-height** bound, and it is only finite because FR-046
refuses a non-finite expiration before the boundary. It does not create a wall-clock bound when
chain observation is unavailable. During an RPC outage the execution remains `submitted`, the
reservation stays held, and reconciliation retries with capped backoff while surfacing outage
health, metrics, and logs. Operator recovery restores or fails over the chain source; it never
releases the reservation or authorizes retry without positive chain evidence. Once trustworthy
observation reaches the recorded expiration height, FR-040 resolves the execution.

The dependency chain is therefore: FR-051 (every Guardian-executable proposal signs a non-zero
approval expiration, and built-in families also sign a 256-block delta) → FR-046 (server refuses a
proven expiration beyond the horizon from `R`) → FR-039 (evidence records the proven block and
`R`) → FR-040 (expiry resolves it after eventual trustworthy chain observation).
