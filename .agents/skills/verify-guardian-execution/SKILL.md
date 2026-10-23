---
name: verify-guardian-execution
description: Model-check Guardian execution of threshold-met proposals (issue #254, PR #510) on Postgres with TLA+/TLC before merging a change to execution reservations, the execution lease, the no-retry boundary commit, claim_send or submit, the execution reconciler's ownership, chain observation or verdicts, settle/resolve/fail writes, ProtectedByExecution, executed_candidate_landed, or commit_promoted_execution inside promote_candidate. Use when a diff touches crates/server/src/services/execute_proposal/service.rs or worker.rs, storage/postgres_execution.rs, coordination/execution_leases.rs, jobs/execution_reconcile/, the execution arms of jobs/canonicalization/processor.rs or storage/postgres.rs, or when check.sh reports DRIFT.
---

# Verify Guardian execution

`model/GuardianExecution.tla` models one account at base state `B`,
guardian-executable proposals that each move it to their own target
(`E_p1`, `E_p2`), and everything that touches their execution on the
Postgres backend:

- request handlers taking the `execution:<account>` lease and opening a
  reservation;
- the worker: prepare (fixes the reference block, so the expiration),
  heartbeat renewals, the boundary commit, `claim_send`, the send, and
  a node rejection;
- the reconciler on every replica: snapshot, Owned check, lease acquire,
  claim, chain observation, renewal, then settle, hold or resolve;
- the canonicalization worker's promote and discard of the execution's
  candidate, and its promotion of a retained client delta;
- a client `push_delta`, and the chain: transactions land only from the
  base they spend and no later than their expiration, blocks advance, and
  one move needs no ack from this Guardian (a guardian switch).

TLC explores every interleaving. One model step is one Postgres
transaction (every execution write takes `lock_account_metadata`, then
checks the fence), one statement outside a transaction (lease acquire,
renew, release), or one RPC read. Workers may stop anywhere (a crash or a
long pause); their lease and reservation deadline can then run out.

## What is checked

| Property | In plain words | Harm if broken |
|---|---|---|
| `LandedNeverFailed` | A Guardian transaction that landed is never recorded as a failed execution. | Guardian deletes the candidate and the proposal and reports `failed` while the chain applied the transaction: Guardian is behind the chain and cosigners retry an intent that already ran. |
| `CommittedMeansLanded` | An execution recorded `committed` is one whose transaction landed. | Cosigners are told the proposal ran when the chain took a different transaction. |
| `SentAfterBoundary` | Nothing is sent before the boundary commit recorded the candidate and its submission evidence. | A transaction in flight that reconciliation has nothing to reconcile against. |
| `OneLandableExecution` | At most one Guardian transaction that can still land is in flight for the account. | Two Guardian-signed transactions compete for one base; Guardian tracks at most one of them. |
| `ExecutionExcludesClientCandidate` | An execution reservation and a client candidate never hold the account together. | Guardian acknowledges two conflicting transitions on one base. |
| `StoredOnChain` | The stored state is one the chain held. | `/state` serves a state the chain never had. |
| `LandedStepRecorded` | While Guardian is behind, the step the chain took from its base (by a transaction this Guardian acknowledged) is still in a row Guardian can promote. | Guardian can never reach the chain state again: every later push fails against the stale commitment. |
| `ReservationsRelease` (liveness) | Every reservation is eventually released. | The account stays reserved: every client push gets `GUARDIAN_EXECUTION_CONFLICT`. |
| `GuardianCatchesUp` (liveness) | The stored state ends up at the chain's (short of a move this Guardian did not acknowledge). | Same as `LandedStepRecorded`, as an outcome. |

`check.sh` runs the seven invariants together (group `safety`) and the two
liveness properties together (group `live`). Liveness assumes the
reconcilers, the canonicalization worker and the chain keep going, and
that a read retried forever eventually answers from the tip. Workers get
no fairness.

## Related skills

These came in with PR #560, and this model leans on what they prove:

- `verify-worker-lease`: a fenced write that locks then checks the fence
  commits in token order. Here every execution write is one atomic step
  under that assumption, and the canonicalization worker's writes are not
  fenced against a stale holder.
- `verify-candidate-queue`: client candidates under concurrent pushes,
  promotion, parking and reconciliation. Here a client candidate is
  reduced to the outcome that model proves: promoted once the chain
  reached it.
- `verify-proposal-lifecycle`: proposals across the server and both SDKs.
  `LandedStepRecorded` and `GuardianCatchesUp` are its properties of the
  same name, applied to the execution path.

## Does my change touch this?

Yes if the diff changes any function in `mirrored.txt`; the drift check
lists the ones that changed, so when unsure, run `check.sh --drift-only`
(seconds, python3 only). `mirrored.txt` lists the modeled functions and
every helper they call that decides an outcome the model represents
(`archive_parked_delta`, `restore_superseded`, `attempt_evidence`,
`owns_candidate`, `storage::abandoned_cutoff`, `gate_candidate_submission`,
the processor's `ProtectedByExecution` arms, the chain reads), plus the
filesystem backend's copies of the E1 fix, which the model does not cover but which
must stay in parity with Postgres. A new helper on a modeled path that
decides a refusal, a verdict or a write belongs there too. No CI job runs
the drift check, so run it yourself on any change to these files. Things the
model treats as one step, which must stay one step in the code:

- `create_execution_reservation_tx`, `admit_execution_candidate_tx`,
  `renew_execution_reservation_tx`, `claim_execution_reservation_tx`,
  `settle_promoted_execution_tx`, `resolve_execution_tx`: lock, fence
  check, reads and writes in one transaction.
- `promote_candidate` with `commit_promoted_execution` inside it, and
  `discard_candidate` / `update_candidate_status` with the
  `execution_owns_candidate` refusal inside them.

A new write on the execution path must keep that shape, and a new
`.await` between two existing storage calls is a new model step.

## Run it

From the repo root:

```bash
.agents/skills/verify-guardian-execution/check.sh          # quick tier, under 1 min
.agents/skills/verify-guardian-execution/check.sh --deep   # adds 2 reconcilers, 2 proposals, liveness; 15 to 20 min
```

`check.sh --drift-only` compares the mirrored Rust against
`fingerprints.txt` without running TLC (exit 0 no drift, 3 drift).

Each cell prints `ok` or `DIFF` against `expected.txt`. A FAIL verdict
names the violated property (`FAIL:LandedNeverFailed`; a liveness failure
is `FAIL:temporal`), and an expected failure must name the same one, so a
different violation still shows as a `DIFF`. Exit codes: 0 every cell
matches and nothing drifted; 1 a cell changed verdict; 2 setup error; 3
cells match but mirrored Rust changed. Every cell passes today; the
`parked=TRUE` cells guard the fix for finding E1 below: a parked delta
whose transaction can still land sits on the base when execution starts.

It needs `java` (`brew install openjdk`) and `python3`. The first run
downloads `tla2tools.jar` 1.7.4 to `~/.cache/guardian-tla/` (shared with
the other `verify-*` skills) and checks its SHA-256. Set `TLC_WORKERS` to
cap TLC's threads.

To see a full counterexample (every variable in every state):

```bash
.agents/skills/verify-guardian-execution/check.sh --cell 1 1 TRUE safety
```

`--cell <reconcilers> <proposals> <parked> <group>` exits 0 on PASS, 1 on
FAIL, 2 on ERROR.

## Reading a result

A `DIFF` to FAIL prints the counterexample as model actions. A step shown
as `Next` is a chain landing (`Land`). Translate it with the action map
below, then decide:

- **Real bug:** every step is something the code can do, in that order,
  with those transaction boundaries. Report it in code terms. Do not edit
  `expected.txt` or weaken the property to make it pass.
- **Modeling error:** some step is impossible in the code, for example a
  write the code makes under a lock the model splits, or a read the code
  performs that the model skips. Fix the action to match the code, cite
  the Rust, and rerun.

A `FAIL:temporal` trace ends with `Back to state N`: the states from `N`
on repeat forever. Check that every repeated step is one the fairness
assumptions above allow to repeat.

`ERROR` means TLC could not run the model (a parse error, or an action
that leaves a variable unspecified); the first lines of the error are
printed.

## When the change is intended

1. Map the changed Rust to its model action (table below) and edit the
   action to do what the new code does: same reads, same writes, same
   transaction scope.
2. A new storage call or `.await` between two existing ones is a new
   action. Do not fold it into a neighbour.
3. Run `check.sh --deep`. A property that now fails is a finding about
   the change, not a reason to edit the property.
4. Once the model matches the code, run `check.sh --accept-drift` to
   re-record `fingerprints.txt`. If you add or rename a mirrored
   function, update `mirrored.txt` first.

## Action map

Paths are relative to `crates/server/src/`.

| Model action | Code |
|---|---|
| `ReqAcquire` | `services/execute_proposal/service.rs` `request_execution`: `elector.try_acquire` under a fresh `new_holder_id` (the unlocked pre-checks only refuse early and are not modeled) |
| `ReqCreate` | `storage/postgres_execution.rs` `create_execution_reservation_tx` (StaleLease, AlreadyReserved, CandidateExists, ProposalGone, Created) |
| `WPrepare` | `services/execute_proposal/worker.rs` `approach_boundary`: execute and prove fix the reference block and the expiration |
| `WHeartbeat` | `worker.rs` `Heartbeat` / `Heartbeat::advance` → `coordination/execution_leases.rs` `Renewal::attempt` → `renew_execution_reservation_tx`; before the boundary, and again (recording `Sent`) while `submit` waits for the node |
| `WFail` | any failure before the boundary (`approach_boundary`, `ensure_guarded_by_this_guardian`, `ensure_admissible`, `ensure_sendable`, a failed admission, shutdown) → `fail` → `fail_execution` = `resolve_execution_tx(.., false)` |
| `WAdmit` | `cross_boundary` → `admit_execution_candidate_tx`: moves a parked row at the nonce to `superseded_deltas` (`archive_parked_delta`, when retention is on) and deletes it, inserts the candidate and the evidence |
| `WClaimLease`, `WClaimRes` | `claim_send` → `Renewal::attempt` (lease `renew`, then `renew_execution_reservation_tx` recording `Sent`). A storage error is retried, fenced, while the lease and send window last; a retry is the same claim taken later, which the model already allows since workers get no fairness |
| `WSubmit`, `WResolveRejected`, `WResolveNotProcessed` | `submit`: Accepted and Unknown leave the transaction in flight; Rejected → `resolve_execution`, deleting the proposal; NotProcessed (a refusal before the node processed the send) → `resolve_execution` keeping the proposal (`ExecutionFailureCode::ends_proposal`) |
| `WRelease` | `run_execution`'s `release_quietly` |
| `RcList` | `jobs/execution_reconcile/mod.rs` `Reconciler::pass` (`list_active_executions`) and the Owned check at the top of `reconcile` |
| `RcAcquire`, `RcClaim` | `take_ownership` (`try_acquire`, then `claim_execution_reservation_tx` unless the fence is already ours) |
| `RcFailPre` | `reconcile` with no evidence: `fail_execution` (Abandoned / LeaseExpired), then release |
| `RcObserveFresh`, `RcObserveAny`, `Verdicts` | `observe` + `moved_on` (`observe_commitment`, `find_transaction_ending_at`); a failed or timed-out read is `pending` |
| `RcRenewLease`, `RcRenewRes` | `renew_after_observing` |
| `RcAct` | `settle_promoted_execution_tx`, `hold`, or `resolve_execution` with the verdict's code |
| `PObserveFresh`, `PObserveAny`, `ProcVerdict` | `jobs/canonicalization/processor.rs`: the chain check, `executed_candidate_landed`, the divergence path |
| `PWrite`, `CommitPromotedRes` | `canonicalize_verified_delta` → `storage/postgres.rs` `promote_candidate` (with `commit_promoted_execution`, which settles only an execution whose `expected_commitment` is the promoted state), or `discard_candidate` / `update_candidate_status` refused with `ProtectedByExecution` |
| `PReconcileRetained` | `jobs/canonicalization/reconciliation.rs` promoting a retained row (`PromotableKind::Retained`), or restoring its archived copy (`PromotableKind::Superseded`, `restore_superseded`, which never replaces a candidate at the nonce), through `promote_candidate` |
| `ClientPush` | `storage/postgres.rs` `submit_candidate` (`ExecutionReserved`, the candidate gate, the retained-row supersede) |
| `LapseWorkerLease`, `LapseWorkerRes`, `LapseOther` | time passing: a `worker_leases.expires_at` or `execution_reservations.lease_expires_at` running out |
| `Land`, `NewBlock`, `ExternalMove` | the chain |

## Scope and assumptions

- Postgres only. The filesystem backend's execution writes check the
  reservation's own deadline instead of the lease row, its in-memory
  leases do not survive a restart, and the reconciler's startup pass
  treats every reservation as abandoned there; none of that is modeled.
- One account; two request handlers (each one attempt); one or two
  reconciler replicas; one or two proposals on the same base; at most
  one client push; one move without this Guardian's ack; chain height 3;
  expiration two blocks after the reference block; one budgeted lapse of
  a reconciler's lease or deadline; fence tokens up to 3. A pass covers
  this bound, not every count.
- Time is abstracted to "a lease or deadline is live or has run out".
  Both run out independently, at any point, which over-approximates the
  real ordering of deadlines.
- Chain reads may lag by any amount but never lie, and there are no
  reorgs. A node rejection is definitive: a rejected transaction never
  lands.
- The executed transaction's result is the proposal's target whatever
  the chain did; a proposal executed on a base the chain already left can
  never land.
- `ensure_guarded_by_this_guardian`, `ensure_admissible`, `ensure_sendable`
  and the other unlocked checks only add early failures, which `WFail`
  covers at any point. The send's deadline is not modeled: a send is one
  step, and a transaction lands only by its expiration either way.
- Client candidates follow the outcome `verify-candidate-queue` proves;
  pushes, parking and abandonment races between clients are not
  re-checked here. Proposal creation, signing, the SDKs, pause, release,
  release sweep, record retention and pruning are out of scope.
- The two-reconciler, two-proposal combination takes about 14 minutes and
  is not in a tier; it passed when the skill was written. Run it with
  `check.sh --cell 2 2 FALSE safety` after a change to the reconciler.

## Guards the model shows are load-bearing

Each of these breaks a property (mutations run against this model):

- reconciler resolves `expired` one block early (`observed.block + 1 >=
  expiration_block`): `LandedNeverFailed`.
- `moved_on` settles a moved account as discarded without searching the
  history (the race b8830eaa fixed): `LandedNeverFailed`.
- `discard_candidate` without the `ProtectedByExecution` refusal:
  `LandedStepRecorded`.
- `resolve_execution_tx` without the boundary-side check:
  `LandedNeverFailed`.
- the send allowed before the boundary commit: `SentAfterBoundary`.
- `create_execution_reservation_tx` without the candidate check:
  `ExecutionExcludesClientCandidate`.
- promoting a candidate while the chain is still at its base:
  `StoredOnChain`.
- admission deleting the parked row instead of archiving it (the code
  before the E1 fix): `LandedStepRecorded`.
- `commit_promoted_execution` matching on the nonce alone, not the
  evidence's `expected_commitment`: restoring the archived row records
  the execution `committed` though its transaction never landed,
  `CommittedMeansLanded`.

Not load-bearing for these properties, alone:

- `claim_send`'s renewals: a worker that sends after losing its
  reservation is still safe, because only workers send, a late
  transaction can land only before its expiration, and the reconciler
  resolves from the chain alone. The gate saves a pointless send.
- the lease check in `admit_execution_candidate_tx`: the holder check in
  the same transaction already refuses a superseded worker.
- the reservation check in `create_execution_reservation_tx`:
  `OneLandableExecution` still holds without it, and without both checks
  at create, because admission refuses while a candidate exists. It fails
  once admission's candidate check goes too.

## Findings

**E1 (fixed). Execution admission deleted a retained client delta whose
transaction could still land.** `admit_execution_candidate_tx` deleted any
`retained` or client-abandoned row at the candidate's nonce, and nothing
refused the reservation while one existed (`has_pending_candidate` counts
only `candidate` rows). Counterexample on the old code:

1. A cosigner's transaction for the parked delta lands; the chain is at
   `C`, Guardian's stored state is still `B` (reconciliation has not run).
2. A cosigner requests Guardian execution; the reservation is created.
3. The worker prepares and crosses the boundary: admission deletes the
   retained row.

Guardian then held no row for the step `B -> C`; its own transaction
could not land, the reconciler resolved it
`GUARDIAN_EXECUTION_CANDIDATE_DISCARDED`, and the stored state stayed at
`B` for good (`LandedStepRecorded` and `GuardianCatchesUp` both failed).
It was the bug class PR #560 fixes for client resubmissions.

The fix (both backends): `admit_execution_candidate_tx` moves the parked
row to #560's `superseded_deltas` archive instead of deleting it (deleting
only when retention is off, since nothing would reconcile an archive), and
`commit_promoted_execution` settles only an execution whose evidence's
`expected_commitment` is the promoted state. If the parked transaction
lands first, reconciliation resolves the execution
`GUARDIAN_EXECUTION_CANDIDATE_DISCARDED` and then restores the archived row
(`PromotableKind::Superseded`), so Guardian catches up. A first fix refused
execution while such a row existed; it blocked Guardian execution for up to
the retention TTL and was replaced once #560 merged. Regression tests in
`storage/execution_reservation_tests.rs`:
`a_parked_delta_on_its_base_does_not_refuse_execution`,
`admission_archives_a_parked_delta_at_its_nonce`,
`a_parked_delta_that_lands_instead_is_restored_after_the_execution_resolves`
and `a_promotion_to_another_state_settles_nothing`.
