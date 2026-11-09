---
name: verify-module-counting
description: Model-check how a stateful Guardian module (a spending cap) counts signed transactions, per RFC 0002 sections 3.3 and 3.4, with TLA+/TLC. Use before changing the notify outbox, the delivery watermark, `pending`, NotifyGap or resync, the notifier lease, or the candidate lifecycle a module's count depends on (candidate admission, promotion, Retained and client-abandoned rows, reconcile); when editing RFC 0002 sections 3.3 or 3.4; when implementing M2b; or when check.sh reports DRIFT.
---

# Verify stateful module counting

`model/ModuleCounting.tla` models one account guarded by one stateful
module: a spending cap that keeps counters and a nonce watermark updated
from `Notify`, and decides each `Evaluate` on its counters plus the
`pending` list the Guardian sends. TLC explores every interleaving of
`push_delta` requests (snapshot, evaluate, store), cosigners executing any
signed delta on chain, canonicalization (promote, retain, reconcile,
expire), two replicas delivering `Notify` and `NotifyGap` under the
notifier lease (calls and replies delayed, duplicated or lost, a replica
that lost the lease still sending), retention drops and operator resync.
One model step is one storage transaction, one module write, one message
delivery or one on-chain execution.

M2b is not implemented: the notifier, the `module_events` outbox,
`pending` and the watermark exist only in the RFC
(`docs/rfcs/0002-guardian-modules.md` sections 3.3 and 3.4, revision 23).
The model checks that design, on top of the candidate lifecycle that
already exists in `crates/server`. The protocol fields it depends on
(`CommitEvent.nonce`, `Gap.resume_from_nonce`, `PendingTransaction.nonce`)
freeze with protocol version 1 at M2a.

## What is checked

| Property | In plain words | Harm if broken |
|---|---|---|
| `NoOvershoot` | The account never spends more on chain than the cap. | The limit the module exists to enforce is exceeded. |
| `NoDoubleCount` | No evaluation counts a delta both in the module's counters and in `pending`. | A payment that fits is refused. |
| `NoDoubleApply` | The counters hold each applied delta once, however often `Notify` is redelivered. | Payments that fit are refused. |
| `CountsQueueAndCommitted` | An evaluation whose delta is admitted counted every delta that was committed or queued at its snapshot and still is (RFC 3.4, "never both and never neither"). | The cap is decided on a total that misses signed spend. |
| `CountsRecoverable` | The same for deltas parked as `Retained` or client-abandoned. | Expected to fail by design (F3 below); `NoOvershoot` shows it is safe. |

The module in the model re-seeds on `NotifyGap`: it rebuilds its counters
from its own sources (here, the chain) below `resume_from_nonce`, as RFC
3.3 allows. A module that resets to zero instead accepts the dropped
history by design, and F1 and F2 do not apply to it.

## What the model found in revision 22

Revision 23 of the RFC adopted the fixes; the faults are now mutations
`m6` and `m7`, so the check fails if the design slides back.

**F1 (now `m6`): resync reopened evaluation before the module had handled
`NotifyGap`.** Cap 2, every amount 1. Deltas 1 and 2 land, delta 1 is
promoted and its event written but not acknowledged; retention drops the
backlog; an operator resyncs, which cleared `resync_required` while the
Gap was still in flight; a `push_delta` is evaluated on the module's
pre-gap counters (0) plus `pending` {2} plus 1 = 2, allowed; delta 3
lands, spending 3. Revision 23: the pair stays refused until the module
acknowledges `NotifyGap`.

**F2 (now `m7`): an event from a replica that lost the lease overtook
`NotifyGap`.** Replica l2 takes the lease while l1 still believes it holds
it; l2 resyncs (`resume_from_nonce = 2`); delta 2 is promoted after the
resync; l1 reads the moved cursor and sends event 2, which reaches the
module before l2's Gap and raises the watermark to 2; the Gap then fails
"only if that raises the watermark" and is acknowledged without the
re-seed; delta 1 is never counted and delta 3 overshoots. Revision 22 said
every event a stale leader has in flight predates the resync (corrected
in its Appendix B). Revision 23: the Gap obligation is stored with the
cursor, read in the same statement, and no replica sends an event while it
is open.

**F3 (cell `recoverable`, by design): `pending` omits signed deltas that
can still land.** A delta parked as `Retained` or client-abandoned (issue
#345) is neither queued nor committed, yet the reconcile pass can still
promote it. It does not overspend (`NoOvershoot` holds in every counting
cell, including unequal amounts): a parked delta is never the tail, so
every later delta conflicts with it on chain, and each landed delta was
evaluated with all its ancestors counted. RFC 3.4, "Recoverable rows",
states this argument; if a change lets a delta build on a parked row, the
argument breaks.

## Does my change touch this?

Yes if the diff changes anything in `mirrored.txt` (check.sh lists drift),
RFC 0002 sections 3.3 or 3.4, or adds a path that:

- makes a delta committed (writes canonical state) without writing its
  `module_events` row in the same transaction (mutation `m5`);
- reads the candidate queue and the unacknowledged events for `pending` in
  separate transactions (`m3`);
- lets a delta land on chain that the Guardian signed without evaluating
  the account's modules on its base;
- moves the stored state out of band (for example `configure_account`
  re-onboarding), which the model does not cover (see scope).

## Run it

From the repo root:

```bash
.agents/skills/verify-module-counting/check.sh              # quick tier, about 8 s
.agents/skills/verify-module-counting/check.sh --deep       # adds 4 deltas and mixed amounts, about 3 min
.agents/skills/verify-module-counting/check.sh --mutations  # every broken variant must fail, about 10 s
.agents/skills/verify-module-counting/check.sh --cell m7     # one cell, full TLC output and trace
```

It needs `java` and `python3`. `tla2tools.jar` 1.7.4 is cached in
`~/.cache/guardian-tla/` and checksum-verified (downloaded on first run),
shared with the other `verify-*` skills. Exit codes: 0 every cell matches
`expected.txt` and no drift; 1 a verdict changed; 2 setup error; 3 cells
match but mirrored source drifted. `TLC_WORKERS` caps TLC's threads.

## Reading a result

`ok` lines match `expected.txt`, including the expected `recoverable`
failure.
A `DIFF` prints the counterexample, one step per line with what changed.
Delta ids index `txs`; `pending` is what the request's snapshot listed.

- Walk the steps against the action map. If every step is something the
  design (or the code, once it exists) can do, it is a real bug: report it
  in RFC and code terms, and leave `expected.txt` and the properties alone.
- If a step is impossible, the model is wrong: fix that action, cite the
  source, and rerun. Two modeling errors were found and fixed this way
  while building the model: liveness judged by delta identity (a delta
  superseded at its nonce looked "missed") and a snapshot set that
  excluded a promoted delta whose event was not written yet.
- After changing the model to match intended source, run
  `check.sh --accept-drift`.

## Action map

Rust paths are relative to `crates/server/src/`; "RFC" is
`docs/rfcs/0002-guardian-modules.md`.

| Model action | Source |
|---|---|
| `Snapshot` | `services/push_delta.rs` `push_delta` (`CandidateChain::load_for_admission`) plus RFC 3.4 "Guardian side": queue, unacknowledged events and `resync_required` in one repeatable-read transaction. The client's base is the tail it knows |
| `SnapshotQueue` | mutation `m3` only: the queue read in a second transaction |
| `Evaluate` | RFC 3.4 "Module side": counters and watermark from one consistent read, pending entries above the watermark, plus the delta; `on_error = deny` under `resync_required` and until the resync's Gap is acknowledged (RFC 3.3) |
| `Store` | `storage/mod.rs` `gate_candidate_submission` (tail, room) under the account lock, `storage/postgres.rs` `submit_candidate` (deletes a `retained` or client-abandoned row at the same nonce) |
| `Execute` | a cosigner submits any signed delta; Miden executes it only on its parent state |
| `Promote` | `storage/postgres.rs` `promote_candidate` (`PromotableKind::Candidate`) plus RFC 2.3/3.3: the `module_events` row in the same transaction |
| `WriteEvent` | mutation `m5` only: the event row in a later transaction |
| `Retain` | `jobs/canonicalization/processor.rs` `retain_candidate`; successors parked as `RetainReason::Orphaned` |
| `Reconcile` | `jobs/canonicalization/reconciliation.rs` `reconcile_chain_advance` / `reconcile_recoverable_path` through `promote_candidate` (`PromotableKind::Retained`, client-abandoned) |
| `Expire` | retained-row TTL (reconciliation step 1) |
| `Acquire`, `Notice` | `coordination/postgres/lease.rs` `try_acquire`; the old holder keeps sending until it notices |
| `SendNotify`, `SendGap` | RFC 3.3 notifier: read the cursor and the Gap obligation in one statement; while the obligation is open send only the Gap, otherwise the next event |
| `Receive` | RFC 3.3 module rules (the SDK `StatefulModule` helper): apply above the watermark, counters and watermark in one write; Gap re-seeds and sets `resume_from_nonce - 1` only if that raises the watermark |
| `ReceiveAck` | RFC 3.3: fenced cursor advance, conditional on the cursor position; the lease holder clears the Gap obligation once the module acknowledged the Gap |
| `LoseNotify`, `LoseAck` | timeouts and transport errors; the replica sends again |
| `DropBacklog` | RFC 3.3 retention (`GUARDIAN_MODULE_EVENT_RETENTION`): backlog dropped, `resync_required` |
| `OperatorResync` | RFC 2.2/3.3 `POST /dashboard/modules/{id}/resync`: one fenced write moving the cursor past every committed event and storing the Gap obligation (`gap_resume_from_nonce`, speckit data model) |

## Scope and assumptions

- Bounds: quick 3 deltas, cap 2, depth 2, two concurrent requests, two
  replicas; deep up to 4 deltas, cap 3, amounts {1, 2}. A pass covers these
  bounds only.
- One account, one module instance, one lifetime cap (no time windows,
  `signed_at` not modeled), candidate mode only. Optimistic mode is out
  (RFC 3.4 already says limits are exact only in candidate mode).
- Every delta needs the Guardian's ack, and a delta executes on chain only
  on its parent state, so two deltas on one base exclude each other.
  `SwitchGuardian` (executes without an ack) is out of scope.
- Retention lasts days and a request seconds, so no request is in flight
  across a retention drop.
- A replica awaits each call before sending the next; late duplicates
  reach the module from the other replica. `NotifyGap` is delayed or
  duplicated but not lost: it is retried until acknowledged. A module
  answering `UNIMPLEMENTED` (counts as acknowledged) is not modeled.
- A whole queue is retained in one step; real passes park the head first,
  and the orphans stay listed (and counted) until the next pass, which is
  only more conservative.
- Not modeled: the backlog cap, `require_fresh_state_ms`, the activation
  fence, assignment and unassignment of the module (the RFC does not say
  where delivery restarts when an instance is assigned again after
  removal), `configure_account` moving the stored state out of band, and
  the execution worker as a second ack path.
- The lease fence on cursor writes is not load-bearing here: the write is
  also conditional on the cursor position, so dropping the fence alone
  breaks nothing. Keep both anyway.
- Of the existing Rust, no single guard is load-bearing on its own for
  these properties: the limit rests on the M2b design and on Miden
  executing a delta only on its parent state. Drift in a mirrored function
  usually means "check the model still says the same", not a bug. Example:
  `Reconcile` requires the row's parent to be the stored state, which
  `reconcile_chain_advance` enforces twice (the `obsolete_base` skip and
  the `recomputed_commitment_mismatch` check); removing one leaves the
  model unchanged. What would matter is a new state in which a signed
  delta can still land while being neither queued nor committed, or a
  new way to build on such a delta (F3's argument depends on parked
  deltas never being the tail).

## Mutations

`check.sh --mutations` runs each; every one must fail:

- `m1` pending counted without the watermark check: `NoDoubleCount`.
- `m2` NotifyGap re-seeds even when it does not raise the watermark (a
  duplicated Gap rewinds the module): `NoOvershoot`.
- `m3` queue and unacknowledged events read in two transactions:
  `NoOvershoot`.
- `m4` Notify applied without the watermark check: `NoDoubleApply`.
- `m5` the `module_events` row written outside the promotion transaction:
  `NoOvershoot`.
- `m6` resync clears the refusal before the module acknowledges
  `NotifyGap` (revision 22, F1): `NoOvershoot`.
- `m7` only the replica that ran the resync knows of the Gap obligation
  (revision 22, F2): `NoOvershoot`.

## When M2b is implemented

Add the notifier, outbox write, `pending` builder and `StatefulModule`
helper functions to `mirrored.txt`, point the `Promote`, `Snapshot`,
`Send*`, `Receive*` and `OperatorResync` rows above at them, check that
each model step is still one transaction in the code, and run
`check.sh --accept-drift`. The conformance suite's resync cases (RFC 3.4)
correspond to `m6` and `m7`.
