# Implementation Plan: Guardian Proves and Commits Transactions

**Feature Key**: `254-guardian-prove-and-commit` | **Date**: 2026-07-28 | **Spec**: [spec.md](./spec.md)
**Branch**: `254-guardian-prove-and-commit`

## Summary

Let a client hand Guardian a fully-signed proposal and have Guardian execute,
prove, and submit the transaction on the client's behalf, so a cosigner needs
no Miden dependency and no chain access to move an account forward.

**The proving architecture is ratified and is not what this plan builds.** The
Gate 0 spike landed a working `DataStore` over Guardian's own state
(`crates/server/src/network/miden/execution/`), assembled a `PartialBlockchain`
from node RPC alone, and proved a witness through a remote prover against public
testnet — with **no new dependencies**. See [research.md](./research.md) and
[RFC 0001](../../../docs/rfcs/0001-server-side-transaction-execution.md).

**Gate 0 is narrowed, not fully passed, and the residue is named.** P2ID and
configuration executed and proved under `MockChain`; `consume_notes` executed
from a prepared store, with snapshot-pinned live-RPC note-block assembly
validated independently through `SyncNotes` but the joined live flow pending;
the custom family (#266) is unrun; live **submission** is unvalidated. None of that can
falsify the architecture — same `DataStore`, same witness assembly — so it does
not block lifecycle implementation. This plan does **not** claim all four
families are validated; deferred coverage is tracked in
[validation-matrix.md](./validation-matrix.md).

What remains is **lifecycle machinery**: the durable reservation that makes an
execution a single-owner, crash-safe, non-retryable operation. Nearly all of
the specified risk lives here, not in proving. The design centre is FR-045's
eleven-step sequence and its **no-retry boundary** (FR-047) — one atomic commit
that admits the candidate and persists submission evidence together, after
which no failure path may ever retry, only reconcile.

**This plan builds no new concurrency mechanism.** Guardian already has the
exact primitive FR-037 requires: a per-account row lock plus fence-validated
conditional write, committed as one transaction. Reservations extend that
pattern.

## Technical Context

- **Language / runtime**: Rust 2024 edition (server + clients), TypeScript
  (base + multisig clients).
- **Server**: `crates/server`, axum HTTP + tonic gRPC, Diesel-backed Postgres
  plus the filesystem backend in `src/storage/filesystem.rs`.
- **Proving**: `crates/server/src/network/miden/execution/` behind the
  `proving` Cargo feature (`miden-tx`, `miden-remote-prover-client`); `e2e`
  includes `proving`. Remote prover only — Guardian never proves locally.
- **Concurrency substrate**: `LeaseFence { lease_name, holder_id, fence_token }`
  (`storage/mod.rs:149`) and the leader/lease machinery in
  `src/coordination/leader.rs`, established by `010-horizontal-scaling`.
- **Existing lifecycle owner**: `src/jobs/canonicalization/{worker,processor}.rs`.
  This feature adds no delta status values and alters no existing transition.
- **Storage**: Postgres tables `account_metadata`, `states`, `deltas`,
  `delta_proposals`, `worker_leases`. **Three** new tables —
  `execution_reservations`, `execution_submissions`, `execution_outcomes` — in one
  migration. Filesystem gains a per-account reservation file.
- **Testing**: `cargo test -p guardian-server`; Postgres and live-network tests
  gated `#[ignore]`. Every lifecycle test in this feature runs **in-process with
  no node** — proving is already validated separately.
- **Scope**: server-side execution of already-signed proposals for Miden
  multisig accounts. Foreign-account (FPI) inputs are excluded by FR-050.
- **NEEDS CLARIFICATION**: none. The one open design question at plan entry —
  FR-037's admission primitive on the filesystem backend — is resolved below as
  Decision 1.

## Key Design Decisions

### Decision 1 — FR-037's admission primitive: extend `discard_candidate`'s shape, two-tier by backend

FR-037 requires reservation creation and candidate admission to be decided by
**one account-scoped atomic primitive**. Guardian already has that primitive,
and the two backends already implement it at different strengths:

**Postgres** (`storage/postgres.rs:1652`) commits each canonicalization write as
one transaction that: takes `lock_account_metadata` — a per-account
`SELECT … FOR UPDATE` on `account_metadata` (`postgres.rs:825`) — then validates
`lease_fence_is_current`, then performs a status-conditional write, returning
`CanonicalWrite::{Applied, StaleLease, NotCandidate}`. An unfenced call is
**refused outright** via `unfenced_write_error` (`postgres.rs:786`).

That per-account lock is already the serialization point for *every* candidate
write. Reservation creation taking the **same** lock is what makes
reservation-vs-candidate atomic in both directions, with no new mechanism —
exactly what FR-037 asks for. `submit_candidate` already rechecks under this
lock that no candidate exists and the nonce is unoccupied
(`storage/mod.rs:518-528`); admission adds one predicate to that existing
recheck.

**Filesystem** serializes writes with a single in-process mutex, `delta_write_lock`, held by
`submit_delta`, `request_candidate_abandon`, `update_delta_status`, and
`update_candidate_status`. The backend is single-replica by construction, but execution leases
can still transfer between tasks inside that process. A task that resumes after losing its lease
must therefore be fenced out just as it is on Postgres.

**Decision**: reservation admission on filesystem takes **`delta_write_lock`
itself**, not a new mutex. A separate mutex would be a correctness bug —
admission must be atomic *with respect to candidate writes*, and those are
serialized by that specific lock. Under the same hold, every execution-owned mutation compares
the supplied holder and fence with the persisted active reservation; ownership transfer updates
the holder and advances the fence atomically. A stale or unfenced execution mutation writes
nothing. Generic client abandon annotations and the `AlwaysLeader` canonicalization path keep
their existing semantics because they are not execution-owner writes.

Filesystem tests cover a stale task losing ownership, waiting for the lock, and resuming after a
new owner has claimed it. Cross-replica races remain Postgres-only. The storage-parity invariant
and exact affected operations are recorded in [data-model.md](./data-model.md).

### Decision 2 — no trait defaults on the new methods

The new `StorageBackend` methods are declared **without default bodies**, placed
in the existing canonicalization-writes block, which already states the rule and
the reason (`storage/mod.rs:509-516`): a trait default silently absorbing a
dropped backend override would revert Postgres to unfenced, non-atomic writes.

This is load-bearing here. A default returning "no reservation" would make the
Postgres reservation check silently vacuous — the failure mode would be a lost
concurrency guarantee that no test names, surfacing as a double submission under
load. Requiring every impl makes a dropped override a compile error.

Consequence for effort estimates: `StorageBackend` has **five**
implementations — `PostgresService`, `FilesystemService`, `EncryptedStorage`
and `InstrumentedStorage` (both pass-through decorators), and
`MockStorageBackend`. Every new method is five impls, of which two are real.

### Decision 3 — execution leases are per-account, and ownership transfers by compare-and-set

FR-038 requires reusing the `LeaseFence` type. It does **not** license reusing the
canonicalization lease, and doing so would be a serious mistake:
`worker_leases` admits **one holder per `lease_name`**
(`coordination/postgres/lease.rs:59`, `ON CONFLICT (lease_name) DO UPDATE`), and
`CANONICALIZATION_LEASE` is the single cluster-wide string `"canonicalization"`
(`coordination/mod.rs:36`). Fencing reservations against it would reduce the whole
deployment to one execution at a time, and would make every fence check answer a
question about the canonicalization worker rather than about this account's reservation.

**Decision**: execution leases use the account-scoped name `execution:{account_id}`.
Different accounts contend on different lease rows and proceed concurrently; the same
account serializes at lease acquisition, *before* any reservation row is written — so the
FR-037 admission primitive becomes a second line of defence rather than the only one.

**FR-052 adds the missing operation.** FR-028 requires post-submission ownership to transfer
to a reconciliation owner without releasing the reservation, and there was previously no
storage operation that could do it. Transfer is a **compare-and-set** against the current
holder and fence token, returning `ClaimSuperseded` rather than stealing when the caller's
expectation is stale. Release-then-reacquire is explicitly not acceptable: the gap between
the two is exactly the window that admits a second submission.

### Decision 4 — the internal ack path is an extraction, not a reimplementation

FR-044 needs Guardian to acknowledge its own delta without traversing
`push_delta` (which creates a candidate, and which FR-027 must now refuse for a
reserved account). `push_delta` is linear and already has the needed seam: it
resolves and verifies through line 118, then commits through the
`DeltaCommitStrategy` abstraction (`services/push_delta.rs:120-134`).

**Decision**: extract the verify-and-acknowledge span
(`push_delta.rs:31-118`) into `services/ack_delta_internal.rs`, with
`push_delta` calling it and then committing exactly as it does today. FR-044's
"MUST NOT weaken any check" is then satisfied structurally — there is one
implementation of the checks, shared — rather than by review discipline.
The internal path deliberately performs no commit and does **not** set
`has_pending_candidate`; admission happens only at FR-045 step 9.

## Constitution Check

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Bottom-up change propagation | OK | Server contract drives the Rust base client (`guardian-client`), TS base client, and both multisig SDKs. FR-033 requires equivalent capability; FR-051 requires both SDKs to apply the shared finite policy to built-in proposals and preserve custom-producer requests for server enforcement. Propagation is Workstream H, gated on the server contract landing first. |
| II. Transport and cross-language parity | OK | All **three** new endpoints ship on HTTP **and** gRPC (FR-034) — no divergence requested and none taken. Rust/TS surfaces stay behaviorally aligned per FR-033. Contract pinned in [contracts/execution-api.md](./contracts/execution-api.md). |
| III. Append-only integrity and explicit lifecycles | OK | Adds **no** delta status values and alters no existing transition (FR-026). Reported execution state is a separate, explicitly enumerated five-value vocabulary (FR-024) mapped onto the delta lifecycle. The one persisted state (FR-041) exists because canonicalization's `remove_candidate` destroys the record it would otherwise be derived from — documented, not implicit. Execution mode is an explicit client-set control path, default off (FR-009), never an inferred fallback. |
| IV. Explicit auth and stable boundary errors | OK | Requester must be a cosigner; the Guardian ack gate is unchanged and still mandatory. Synchronous refusals are enumerated in FR-022 with stable codes; `failed` carries a stable code distinguishing verification / proving / submission / post-submission-discard causes (FR-024). Capability-unavailable and startup misconfiguration are explicit (FR-043). |
| V. Evidence-driven delivery | OK | Five independently testable user stories; 33 success criteria; [validation-matrix.md](./validation-matrix.md) carries the offline and live coverage tables plus the fault-injection rows. Proving is already evidenced against public testnet. |

**No unresolved violations.** Execution ownership fencing preserves the same stale-worker
semantics on both backends; only true cross-replica concurrency is Postgres-specific.

## Project Structure

### Documentation (this feature)

```text
speckit/features/254-guardian-prove-and-commit/
├── spec.md                      # 51 FRs, 33 SCs, 5 user stories
├── plan.md                      # This file
├── research.md                  # Evidence log with file:line citations
├── validation-matrix.md         # Gate, propagation, coverage, fault injection
├── data-model.md                # Reservation + evidence entities (Phase 1)
├── quickstart.md                # Operator/developer walkthrough (Phase 1)
├── contracts/
│   ├── execution-api.md         # HTTP + gRPC contract, 5-state vocabulary
│   └── sdk-api.md               # Client-level execution mode, naming rule
└── tasks.md                     # 129 tasks across 8 phases
```

The external review document for this feature is
[`docs/rfcs/0001-server-side-transaction-execution.md`](../../../docs/rfcs/0001-server-side-transaction-execution.md).

### Source code

```text
crates/server/src/
├── network/miden/execution/     # EXISTS — validated proving seam
│   ├── store.rs                 # DataStore over Guardian's own state
│   ├── blockchain.rs            # PartialBlockchain from RPC
│   ├── tests.rs                 # 7 offline tests (MockChain)
│   └── live_tests.rs            # 3 live tests (testnet, #[ignore])
├── storage/
│   ├── mod.rs                   # + reservation types, outcome enums, trait methods
│   ├── postgres.rs              # + fenced reservation writes (real impl)
│   ├── filesystem.rs            # + delta_write_lock-guarded impl
│   └── encryption/decorator.rs  # + pass-through
├── metrics/storage.rs           # + pass-through (instrumented decorator)
├── testing/mocks.rs             # + mock impl
├── services/
│   ├── ack_delta_internal.rs    # NEW — extracted from push_delta (FR-044)
│   ├── execute_proposal.rs      # NEW — FR-045's 11-step sequence
│   ├── execution_status.rs      # NEW — reported-state derivation (FR-024/026)
│   └── push_delta.rs            # MODIFIED — calls extraction; FR-027 refusal
├── jobs/execution_reconcile/    # NEW — FR-040 evidence paths, FR-031 recovery
├── api/{http.rs,grpc.rs}        # + 2 endpoints on both transports (FR-034)
├── config/                      # + execution capability + prover URL (FR-043)
└── error.rs                     # + stable codes for FR-022 refusals

crates/server/migrations/2026-07-28-000001_execution_reservations/

crates/guardian-client/          # Rust base client (FR-034)
packages/guardian-client/        # TS base client (FR-034)
crates/miden-multisig-client/    # Rust SDK — execution mode (FR-009), FR-051
packages/miden-multisig-client/  # TS SDK — execution mode (FR-009), FR-051
```

**Structure decision**: the proving seam stays where the spike put it, under
`network/miden/execution/` — it is network-specific. Lifecycle machinery is
network-agnostic and goes in `storage/`, `services/`, and `jobs/`, matching how
canonicalization is already split. The reconciliation worker is a **sibling** of
`jobs/canonicalization/`, not a modification of it: it consumes canonicalization
outcomes but owns a different question (did a submitted transaction land), and
folding it in would entangle two lifecycles that Principle III wants kept
distinct.

## Workstreams

### A — Storage: reservation and the admission primitive

- Types in `storage/mod.rs`: `ExecutionReservation`, `SubmissionEvidence`,
  `ExecutionOutcome`, `CandidateAdmission`, and outcome enums mirroring
  `CanonicalWrite`'s shape —
  `ReservationWrite::{Created, AlreadyReserved, CandidateExists, StaleLease, ClaimSuperseded}`,
  `AdmissionWrite::{Admitted, NotAuthorized, CandidateExists, StaleLease}`, and
  `ResolveWrite::{Resolved, NotAuthorized, AlreadyResolved, StaleLease}`. `CanonicalWrite`
  gains `ProtectedByExecution`. Exhaustive, no catch-all variant.
- Trait methods, **no defaults** (Decision 2): create / renew / release / load
  reservation; `claim_execution_reservation` (Decision 3's fenced compare-and-set
  ownership transfer, FR-052); `admit_execution_candidate` (the FR-045 step 9
  commit, which admits the candidate **and** persists submission evidence
  together); `resolve_execution` (the post-boundary failure resolution — discard,
  outcome, release, in one transaction); `record_execution_outcome`
  (**pre-boundary failures only** — post-boundary outcomes are written by the
  extended `promote_candidate` and by `resolve_execution`); and the
  reconciliation-owed query.
- The **account-scoped** `execution:{account_id}` lease is what every fence
  validates against (Decision 3), never `CANONICALIZATION_LEASE`.
- Postgres: each as one transaction — `lock_account_metadata`, then
  `lease_fence_is_current`, then conditional write; `unfenced_write_error` on a
  missing fence.
- Filesystem: `delta_write_lock`-guarded read-modify-write (Decision 1).
- Decorators and mock: pass-through and test double.
- `submit_candidate` and `push_delta` gain the reservation predicate (FR-027),
  with the **owner-authorized exception** — admission is permitted for the
  caller presenting the matching reservation's owner identity and a valid
  fence, refused for all others. This exception is not optional: without it
  Guardian deadlocks against its own candidate.

### B — Migration

`2026-07-28-000001_execution_reservations`: table keyed by `account_id` with
owner identity, lease expiry, fence token, optional candidate nonce, submission
evidence (including the expiration block from `ProvenTransaction`), and terminal
outcome. A **partial unique index** enforces at most one active reservation per
account at the schema level, so FR-029's single-owner rule does not rest on
application logic alone. Shape in [data-model.md](./data-model.md).

### C — Internal acknowledgment path (FR-044)

Extract `services/ack_delta_internal.rs` per Decision 4; `push_delta` delegates
to it. Not reachable from any transport — enforced by module visibility, and
asserted by a test that the router exposes no route reaching it.

### D — Execution service (FR-045)

`services/execute_proposal.rs` implements the eleven steps in order, with the
three normative orderings encoded structurally rather than by comment:

- **Steps 2 before 3** — never acknowledge a transaction that does not
  reproduce the signed summary.
- **Step 8 before 9** — admissibility (FR-048) and finite expiration (FR-046)
  are checked *before* the boundary; after it, FR-047 forbids the
  fail-and-release that FR-048 would demand, so the account would be held until
  expiration for a transaction never sent.
- **Step 9 before 11** — the candidate must exist before the send, or a crash
  between them leaves a chain transaction with no candidate to promote.

The step-9 commit is the **no-retry boundary**. Implementation rule: it is one
storage call returning one outcome, and no code path may treat its failure as
retryable. Step 10 re-validates the fence and aborts **without sending and
without writing** if stale (FR-049).

### E — Reconciliation and recovery (`jobs/execution_reconcile/`)

Reconciliation owns **two** terminal paths — superseded and expired — plus FR-031's restart
rule: an execution whose durable record shows the boundary was crossed is **never** retried,
only reconciled.

**It does not own `landed`.** That belongs solely to the extended `promote_candidate`
(FR-053). Observing the account at the expected commitment is an *input* telling reconciliation
this execution is neither superseded nor expired, so it must wait for promotion — never a
second write of the outcome. A reconcile loop that upserted `landed` on that observation would
race the party that owns it, reintroducing the `remove_candidate` hazard FR-041 exists to
prevent.

FR-046 gives reconciliation a finite chain-height bound: measured work confirmed the default is
`u32::MAX` (never expires), which is exactly why FR-046 refuses it. FR-051 makes both SDKs apply
the shared finite default to built-in proposals; opaque custom requests remain producer-owned and
must encode a finite expiration themselves. Termination still depends on eventual trustworthy
chain observation. When RPC is unavailable, reconciliation retains the reservation, retries with
capped backoff, and exposes an operator-visible outage; restoring or failing over the chain source
is recovery. Wall-clock time never authorizes release or retry.

**Terminal resolution is not a separate write.** SC-025 requires promotion and discard to
*each* atomically persist the outcome, so outcome persistence and reservation release are
**extensions of the existing fenced promote and discard primitives**, inside their
transaction — not a `record_execution_outcome` call the worker makes afterwards. A separate
call racing `remove_candidate` can find the proposal already deleted, which is the exact
failure FR-041 exists to prevent. `record_execution_outcome` survives only for pre-boundary
failures, where no candidate exists and there is nothing to race.

**Two distinct discards, and conflating them wedges the account.** Between the step-9 commit
and the send, the candidate looks ordinary to canonicalization, which could discard it while
the execution's fence is live and FR-049 still permits sending. So the **canonicalization
worker's** `discard_candidate` checks, in its own transaction, whether the candidate belongs to
an unresolved boundary-crossed execution and returns `ProtectedByExecution` if so.

But the definite-rejection, superseded, and expired paths *require* exactly that discard. They
therefore go through `resolve_execution`, which validates execution ownership and fence and
performs discard, outcome persistence, and reservation release in one transaction. The
protection is scoped to the **caller**, not to the row — a blanket refusal would leave the
account unresolvable until expiration, and the expired path could not clear it either.

### F — Reported state and status surface

`services/execution_status.rs` derives the five reported states (FR-024) from
the reservation, the delta, and the persisted terminal outcome. Pre-terminal
states are derived; only the post-submission terminal outcome is persisted
(FR-041), because `remove_candidate` deletes the candidate *and* its proposal.
`proposal_exists` (FR-042) reports presence, never retry advice.

### G — Transports and configuration

Both endpoints on HTTP and gRPC (FR-034), per
[contracts/execution-api.md](./contracts/execution-api.md). Stable error codes
in `error.rs` for every FR-022 synchronous refusal. Config: execution
capability plus prover URL, with **startup validation** (FR-043) — including
canonicalization being enabled, since FR-040 depends on it entirely.

**Operational note (FR-020)**: the remote-prover client's default timeout is
10s (`tx_prover.rs:45`), below observed proving times of 6.2–20.1s. Guardian
MUST set an explicit timeout; the default surfaces as an intermittent
"failed to prove transaction" that names no timeout. This cost half a debugging
session already and belongs in `docs/TROUBLESHOOTING.md`. Timeouts and other
transport-level prover failures are retried server-side with capped backoff
under the held reservation (FR-055) — the transient classifier must match the
walked error source chain, since the outermost `Display` hides the transport
cause — so a mis-set timeout now wastes prover capacity on retries rather than
surfacing to the caller, which makes the explicit setting no less mandatory.

**Deploy note — `proving` must reach the published image.** `proving` is an optional Cargo
feature and `Dockerfile:17` sets `ARG GUARDIAN_SERVER_FEATURES=postgres`. Without adding
`proving` there, every published-image deployment answers the execute endpoint with
`GUARDIAN_PROVING_UNAVAILABLE` forever, with nothing in the logs suggesting a build-time
cause. The feature list, the compose guides, and `docs/SERVER_AWS_DEPLOY.md` all need
updating — this is the same class of trap as the storage backend being compile-time.

### H — Clients (FR-033, FR-009, FR-051)

Client-level `ProposalExecutionMode`, default **not attached** — no new
per-call SDK methods. Four packages. FR-051: both SDKs apply the shared 256-block finite policy
to every built-in proposal family. An opaque custom request is attached byte-for-byte and must
already encode a finite expiration; the server refuses it before the boundary otherwise.

### I — Tests and docs

Per [validation-matrix.md](./validation-matrix.md). The fault-injection rows are
the substance: both sides of the step-9 write (SC-024, SC-030), fence theft
mid-flight (SC-019, SC-031), and the self-deadlock regression (SC-028) — the bug this
spec already had once, where the blanket admission rule blocked Guardian's own
candidate. Docs: `CONFIGURATION.md`, `TROUBLESHOOTING.md`, `spec/api.md`.

## Phasing

| Phase | Content | Gate |
|---|---|---|
| 1 | A + B — storage, migration, admission primitive | Concurrency tests pass on both backends before anything calls it |
| 2 | C — internal ack extraction | `push_delta` behavior provably unchanged (existing tests, untouched) |
| 3 | D — execution service, up to and including step 9 | Fault injection on both sides of the boundary |
| 4 | E + F — reconciliation, recovery, reported state | Every FR-040 path terminates after eventual trustworthy chain observation; outages remain safe and visible |
| 5 | G — transports, config, startup validation | Parity tests on both transports |
| 6 | H + I — clients, docs, full matrix | Matrix green |

Phase 1 is the gate for everything else, and its tests must be written against
the storage layer directly. If the admission primitive is wrong, every later
phase inherits a concurrency bug that integration tests will only surface
intermittently.

Phases 3 and 4 may not be reordered: the reconciliation paths are what make the
no-retry boundary survivable, so shipping the boundary without them would leave
accounts held until expiration with no resolution path.

## Validation

- **Concurrency (Postgres)**: two workers racing execution on one account —
  exactly one reservation, exactly one submission (SC-005). Repeated proving is
  permitted; a second submission is not.
- **Fault injection at the boundary**: crash immediately before the step-9 write
  fails-and-releases; crash immediately after reconciles and never retries
  (SC-024).
- **Fence theft**: lease stolen between step 9 and step 11 — no send, durable
  candidate left to reconciliation (SC-031); new owner resolves, original
  worker resumes without submitting (SC-019), and the handover is a fenced
  compare-and-set rather than a release (SC-035).
- **Self-deadlock regression**: Guardian admits its own candidate under its own
  reservation (SC-028).
- **Per-account leases**: two accounts execute concurrently on one replica while two
  executions for one account serialize — a test that would fail if the cluster-wide
  canonicalization lease were reused (SC-034).
- **Expiration**: both SDKs apply the shared finite default to built-in proposal families;
  opaque custom producers must encode a finite expiration themselves. The server refuses any
  non-finite or out-of-horizon executed result before the no-retry boundary (SC-027, SC-033).
- **Backend parity**: identical externally observable outcomes on filesystem and
  Postgres for every non-concurrent scenario. Concurrent scenarios are
  Postgres-only **by design** — see Complexity Tracking.
- **Transport parity**: all three endpoints, both transports, same semantics and
  error meanings (FR-034).

## Deferred

- **Optimistic mode** — refused. Requires canonicalization (FR-043); without it
  there is no way to establish whether a submitted transaction landed.
- **FPI / foreign-account inputs** — excluded by FR-050, refused before
  execution.
- **Stage 2 live submission validation** — needs a funded, Guardian-registered
  testnet account. Not a blocker: it validates the already-proven submit call,
  and every lifecycle test above runs in-process with no node.
- **Upstream peaks nicety** — a small `PartialMmr`-from-tip convenience on the
  Miden side would delete `blockchain.rs`'s seed-and-apply dance. Cosmetic;
  the current path is validated.

## Complexity Tracking

| Violation | Why needed | Simpler alternative rejected because |
|---|---|---|
| A reported execution state is persisted (FR-041) rather than derived, unlike every other status in the system | Canonicalization's `remove_candidate` deletes an unrecoverable candidate **and then its matching proposal**, destroying the record a derived state would read. Without persistence, a post-submission terminal outcome is unrecoverable. | Deriving on read was the original design and is simply incorrect — it was caught by review, not by tests. Keeping the candidate alive instead would change canonicalization's existing lifecycle, which FR-026 forbids. |
