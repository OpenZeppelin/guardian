# Data Model: Guardian Prove and Commit

Entities added by #254. Wire shapes are normative in
[contracts/execution-api.md](./contracts/execution-api.md); this document covers
**storage-side** shapes, the atomic write units, and the internal→reported state
mapping.

Rust/SQL spellings below are illustrative; field meanings and atomicity
boundaries are normative.

## Design centre: the boundary is a row, not a flag

FR-047 requires "submission authorized and prepared" to be one durable,
observable transition, never an inference. The submission evidence **is** that
transition, so its presence is the boundary — there is no separate
`submission_attempted` boolean to drift out of sync with it. The name of the
column is historical/internal; evidence may exist before the network send begins.

This is why the FR-045 step 9 commit writes the candidate and the evidence
**together**: one commit makes "about to submit" a single durable fact. Any
model with two writes, or with a flag beside the evidence, reintroduces the
window this design exists to close.

## `ExecutionReservation`

One row per account, at most one **active** at a time. Spans acceptance through
terminal state (FR-023).

| Field | Type | Required | Notes |
|---|---|---|---|
| `account_id` | string | yes | Account under execution |
| `proposal_id` | string | yes | The proposal being executed; with `account_id`, the execution handle (FR-003) |
| `attempt` | i32 | yes | 1-based; increments per retry of the same proposal. See § Attempt identity |
| `holder_id` | string | yes | Owning worker; from `LeaseFence.holder_id` (FR-038) |
| `lease_name` | string | yes | **`execution:{account_id}`** — account-scoped, never the cluster-wide canonicalization lease (FR-038) |
| `fence_token` | i64 | yes | Monotonic; from `LeaseFence.fence_token` |
| `lease_expires_at` | timestamptz | yes | Renewable (FR-023, FR-028) |
| `phase` | enum | yes | Internal phase; never on the wire (FR-025) |
| `candidate_nonce` | i64 | no | Set at step 9 when the candidate is admitted |
| `ignored_signatures` | i32 | yes | Count excluded as invalid / duplicate / non-cosigner (FR-006) |
| `created_at` / `updated_at` | timestamptz | yes | |

**Why not the existing pending-candidate flag**: that flag only exists *after* a
candidate is persisted, which is after proving. The whole span this feature must
protect — accept, verify, acknowledge, execute, prove — precedes it (FR-023).

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
  row is written — so the admission primitive is a second line of defence rather than the
  only one.

FR-038 requires reusing the `LeaseFence` **type**. It forbids reusing the canonicalization
**lease**.

## Attempt identity

A pre-boundary failure is terminal *and* retryable: `state: failed` with
`proposal_exists: true` explicitly permits another attempt on the same proposal. So
`(account_id, proposal_id)` identifies a **handle**, not a single execution — and rows keyed
on the pair alone cannot represent two attempts.

`attempt` is therefore part of the identity of every execution-owned row:

- The handle on the wire stays `(account_id, proposal_id)` (FR-003). No new identifier is
  exposed; attempt numbering is internal.
- **The number is allocated inside reservation creation**, under the same account lock, as
  `max(attempt) + 1` for the handle. It is a write concern, not a read one: allocating it in
  the status/derivation layer would mean two concurrent retries could compute the same number
  before either inserted.
- A status read reports the **most recent** attempt for the handle. This is normative — an
  unqualified read of a retried proposal would otherwise be ambiguous.
- Uniqueness is `(account_id, proposal_id, attempt)`, never the pair.
- At most one attempt may be **active** per account, which the partial unique index on
  `account_id` already enforces.

An earlier revision keyed submissions and outcomes on the pair alone. In practice the
collision was hard to trigger — pre-boundary failures write no outcome row, and a
post-boundary failure deletes the proposal, so a second post-boundary attempt cannot
arise — but the schema still could not express a retry, and the contract never said which
attempt a read reports.

## `SubmissionEvidence`

Written **before** the network send, inside the step-9 commit (FR-039).

| Field | Type | Required | Notes |
|---|---|---|---|
| `account_id` | string | yes | |
| `proposal_id` | string | yes | |
| `attempt` | i32 | yes | Which execution attempt this evidence belongs to; see § Attempt identity |
| `candidate_nonce` | i64 | yes | The candidate admitted in the same commit |
| `transaction_id` | string | yes | **FR-039.** The proven transaction's id |
| `expected_commitment` | string | yes | **FR-039.** The account commitment the transaction is expected to produce. FR-040's superseded rule is unimplementable without it — "moved somewhere that is neither base nor this transaction's result" requires knowing the result |
| `reference_block` | i64 | yes | **FR-039.** The block the transaction was executed against |
| `expiration_block` | i64 | yes | **FR-039.** From `ProvenTransaction::expiration_block_num()` — **not** the request's `expiration_delta` |
| `base_commitment` | string | yes | Account state the transaction was built against; FR-040's "still at base" test |
| `committed_at` | timestamptz | yes | |

All four FR-039 fields are mandatory, and each is load-bearing for a specific
reconciliation rule rather than merely diagnostic: `expected_commitment` distinguishes
landed from superseded, `base_commitment` detects "never moved", `expiration_block` bounds
the wait, and `transaction_id` is what an operator correlates against the chain. An earlier
revision of this document carried only the expiration block, which left the superseded rule
stated but unimplementable.

**`expiration_block` source is normative.** `TransactionRequest::expiration_delta`
is `Option<u16>` where `None` means non-expiring; the authoritative value is the
proven transaction's own expiration block. Measured: the default is
`u32::MAX` — never expires. FR-046 refuses that, which is precisely what makes
FR-040's `expired` path able to fire at all. Reading the delta instead of the
proven value was a real defect in an earlier revision.

## `ExecutionOutcome`

Persisted terminal outcome (FR-041). The **only** reported state that is stored
rather than derived.

| Field | Type | Required | Notes |
|---|---|---|---|
| `account_id` | string | yes | |
| `proposal_id` | string | yes | |
| `attempt` | i32 | yes | Which attempt resolved; see § Attempt identity |
| `state` | enum | yes | `landed` or `failed` only |
| `error_code` | string | no | Required when `failed`; from the contract's vocabulary |
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
(`storage/mod.rs:157`). No catch-all variant — a new case must force a compile
error at every match site.

```rust
pub enum ReservationWrite {
    Created,
    AlreadyReserved { holder_id: String, proposal_id: String },
    CandidateExists,
    StaleLease,
    /// FR-052: the claim's expected holder/fence no longer matches, so ownership
    /// was not transferred. Distinct from `StaleLease` — the *caller's expectation*
    /// is stale, not the caller's lease.
    ClaimSuperseded,
}

pub enum AdmissionWrite {
    Admitted,
    NotAuthorized,   // caller is not this reservation's owner (FR-037)
    CandidateExists,
    StaleLease,
}

pub enum ResolveWrite {
    Resolved,
    NotAuthorized,   // caller does not own this execution
    AlreadyResolved,
    StaleLease,
}
```

`CanonicalWrite` gains one variant, `ProtectedByExecution`, returned when the canonicalization
worker attempts to discard a candidate owned by an unresolved boundary-crossed execution. It is
exhaustively matched like every other variant, so adding it forces every existing call site to
decide what to do — which is the point.

`AlreadyReserved` carries `proposal_id` because the contract requires
`meta.blocking_proposal_id` to name the blocker (FR-036) — the storage layer is
the only place that knows it.

## The atomic write units

Four writes, each one account-scoped transaction. Nothing below may be split
into check-then-act (FR-037).

| Unit | Contents | Returns |
|---|---|---|
| **Create reservation** | Acquire the `execution:{account_id}` lease, then insert the reservation iff no candidate exists and no active reservation | `ReservationWrite` |
| **Admit candidate + record evidence** (step 9) | Persist candidate, set `has_pending_candidate`, set `candidate_nonce`, insert evidence — **as one commit** | `AdmissionWrite` |
| **Promote candidate + resolve** | Existing fenced promotion, **extended** to upsert `ExecutionOutcome { landed }` and release the reservation in the same transaction. Owns `landed` (FR-053, FR-054) | `PromoteWrite` |
| **`resolve_execution`** | Post-boundary failure: validate execution ownership + fence, discard the candidate, **delete its matching proposal**, upsert `ExecutionOutcome`, release the reservation — **one transaction** | `ResolveWrite` |
| **`fail_execution`** | Pre-boundary failure: upsert `ExecutionOutcome` and release the reservation as **one commit**. No candidate exists, so nothing to discard | `ResolveWrite` |
| **Claim ownership** (FR-052) | Compare-and-set `holder_id` / `fence_token` on a live reservation; fails rather than steals on a stale expectation | `ReservationWrite` |
| **Renew / release** | Update expiry, or mark `released_at` | `ReservationWrite` |

### Terminal resolution is not its own *separate* write — but it is its own *operation*

SC-025 requires candidate promotion and candidate deletion to **each** atomically persist the
outcome. Two operations satisfy that, and the split matters:

- **`landed`** is owned by the extended `promote_candidate`. Promotion is what makes the
  outcome true, so nothing else may write it — a reconciliation worker that also persisted
  `landed` on observing `canonical` would be a second writer racing the first.
- **Every post-boundary failure** — definite rejection, superseded, expired — is owned by
  `resolve_execution`, which discards the candidate *and* persists the outcome *and* releases
  the reservation in one transaction.

The reason neither may be split into steps is `remove_candidate`, which deletes the candidate
*and its proposal*. A separate `record_execution_outcome` call racing that deletion can find
the record it needs already destroyed — the exact failure FR-041 exists to prevent.

Pre-boundary failures use **`fail_execution`**, which is still one commit even though it has no
candidate to discard. Persisting the outcome and releasing the reservation as two operations
would let a crash between them expose either a terminal execution still holding its account, or
a released account with no outcome to report. There is no `record_execution_outcome` that writes
an outcome without also releasing — every terminal transition is one atomic operation (FR-053).

### `resolve_execution` must delete the matching proposal

Canonicalization's `remove_candidate` deletes the candidate inside its transaction and then
derives and deletes the matching proposal **afterwards**, tolerating failure with a warning
(`processor.rs:1092-1130`). A post-boundary execution cannot inherit that looseness.

`proposal_exists: true` on a `failed` execution is precisely what FR-042's contract defines as
a permitted retry. If `resolve_execution` discarded the candidate but left the proposal, a
**definitely rejected** transaction would be advertised to the client as retryable. So proposal
deletion is part of the resolution commit, not a follow-up step.

### Promotion is explicitly authorized to release (FR-054)

Promotion runs under the **canonicalization** lease, not the account's execution lease — yet it
now releases an execution reservation, and FR-038 requires every durable mutation to validate
the execution fence. Promotion is authorized to do this while holding the canonicalization
fence, provided it takes the same per-account lock every other reservation write takes.

The justification is narrow: promotion is what makes `landed` true, and it cannot be expected to
hold a lease belonging to a worker that may have died. The per-account lock is what keeps it
safe — promotion and `resolve_execution` serialize on it, whichever commits first wins, and the
loser observes `AlreadyResolved` and writes nothing. Both results are individually correct, so
the lock decides rather than a precedence rule.

### Ordinary canonicalization discard versus execution resolution

These are different operations on the same row, and conflating them deadlocks the account.

`discard_candidate`, called by the **canonicalization worker**, MUST refuse a candidate whose
execution has crossed the boundary and not yet resolved, returning a distinct
`CanonicalWrite::ProtectedByExecution`. That protection exists because between step 9 and
step 11 the candidate looks ordinary to canonicalization, which would otherwise leave a
submitted transaction with no candidate to promote.

`resolve_execution`, called by the **owning execution or its reconciliation owner**, discards
that same candidate deliberately — it is the only operation permitted to, and it validates
execution ownership and fence to prove it is entitled.

An earlier revision expressed the protection as a blanket refusal to discard any unresolved
boundary-crossed candidate. That was wrong in a way worth naming: it also blocked the
definite-rejection, superseded, and expired paths, which *require* exactly that discard. The
account would then be unresolvable until expiration — and the expired path could not clear it
either, so the wedge was permanent. The protection must be scoped to the caller, not to the
row.

### The owner-authorized exception is required, not optional

FR-037's blanket rule — admission fails if a reservation is active — would
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
| Serialization point | `lock_account_metadata` — per-account `SELECT … FOR UPDATE` (`postgres.rs:825`) | `delta_write_lock` (`filesystem.rs:25`) |
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

`crates/server/migrations/2026-07-28-000001_execution_reservations/`

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
    state          TEXT        NOT NULL CHECK (state IN ('landed', 'failed')),
    error_code     TEXT,
    error_message  TEXT,
    resolved_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, proposal_id, attempt)
);
```

The partial unique index is deliberate: FR-029 forbids two concurrent
executions for one account across all replicas, and a database constraint holds
that even if a service-layer check is ever bypassed or refactored away.

`execution_submissions`' uniqueness on `(account_id, proposal_id, attempt)` is a second
structural guard on the no-retry boundary — a second attempt to cross it *for the same
attempt* fails on the constraint rather than on a code path.

### The post-boundary candidate must be protected from canonicalization

Between the step-9 commit and the send, the candidate is an ordinary candidate as far as
canonicalization is concerned — so the canonicalization worker could discard it while the
execution's fence is still live and FR-049 still permits the send. That would leave a
submitted transaction with no candidate to promote, the precise state FR-045's step-9-before-11
ordering exists to prevent.

**Discard MUST therefore consult `execution_submissions` for the account and nonce inside its
own transaction**, and MUST NOT discard a candidate whose execution has crossed the boundary
and not yet resolved. The index above exists for that lookup. This is a change to an existing
primitive, not a new one, and it is why the discard write unit above is listed as extended.

## Internal phase → reported state

Internal phases (FR-025) never appear on the wire; each maps onto exactly one of
the five reported values (FR-024).

| Internal `phase` | Reported | Boundary crossed |
|---|---|---|
| `accepted` | `pending` | no |
| `verified` | `pending` | no |
| `acknowledged` | `pending` | no |
| `executed` | `pending` | no |
| `proving` | `proving` | no |
| `proved` | `proving` | no |
| `submission_committed` | `submitted` | **yes** |
| `sent` | `submitted` | **yes** |
| `reconciling` | `submitted` | **yes** |
| `resolved` | from `ExecutionOutcome` | either |

`submission_committed` versus `sent` is the distinction FR-031 requires on
restart: both report `submitted` and neither may be retried, but only
`submission_committed` may not yet have reached the network. Both are resolved
by reconciliation, never by resubmission.

Because the boundary is the evidence row's existence, this column is
diagnostic — recovery reads the evidence, not the phase. A phase that disagreed
with the evidence would be a bug, and the evidence wins.

## Reconciliation inputs (FR-040)

Reconciliation needs no transaction-status lookup — Miden exposes none. It resolves from
observations already available.

**Reconciliation owns exactly two terminal paths.** `landed` is **not** one of them:

| Reconcile-owned path | Observation | Outcome |
|---|---|---|
| **Superseded** | Account moved to a commitment that is neither `base_commitment` nor `expected_commitment` | `failed` / `GUARDIAN_EXECUTION_CANDIDATE_DISCARDED` |
| **Expired** | Chain passed `expiration_block` with the account still at `base_commitment` | `failed` / `GUARDIAN_EXECUTION_EXPIRED` |

Plus FR-031 restart recovery, which resolves rather than retries.

`landed` is owned solely by the extended `promote_candidate` (FR-053). Observing the account at
`expected_commitment` is an **input** — it tells reconciliation this execution is not
superseded and not expired, so it must wait for promotion — never a second write of `landed`.
A reconciliation loop that upserted `landed` on that observation would be a second writer
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

The dependency chain is therefore: FR-051 (built-in SDK or custom producer constructs a finite
transaction) → FR-046 (server refuses otherwise) → FR-039 (evidence records the proven block) →
FR-040 (expiry resolves it after eventual trustworthy chain observation).
