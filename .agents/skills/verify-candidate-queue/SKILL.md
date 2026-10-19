---
name: verify-candidate-queue
description: Model-check the canonicalization candidate queue (issue #17) with TLA+/TLC before merging a change to candidate admission, promotion, retention, abandon, reconciliation, the pending-candidate flag, or the account lock. Use when a diff touches crates/server/src/jobs/canonicalization/, storage submit_candidate / promote_candidate / discard_candidate / update_candidate_status / request_candidate_abandon, gate_candidate_submission or classify_chain_position, clear_pending_candidate_if_none or list_with_pending_candidates, or push_delta's label rule, or when check.sh reports DRIFT.
---

# Verify the candidate queue

`model/CandidateQueue.tla` is a TLA+ model of one account's candidate
queue. TLC explores every interleaving of client pushes (honest nonce
labels and timestamp labels), abandon requests, the chain landing
acknowledged transactions, the worker's full, fast-promotion and
reconcile passes, and a promotion that outlives its pass's lease. One
model step is one Postgres transaction, one statement outside a
transaction, or one `delta_write_lock` section on the filesystem
backend. Every `.await` between two storage calls in the worker is a
separate step.

## What is checked

| Property | In plain words | Harm if broken |
|---|---|---|
| `CanonicalIsOnChain` | The stored state is one the chain held, and every `canonical` row is a step the chain took, with a payload that produces its recorded commitment. | `/state` and `get_delta_since` serve a history the chain never had; clients build on it. |
| `CanonicalMovesForward` | The stored state only ever advances along the chain. | A rollback: Guardian serves and acks on an older state. |
| `NoCompetingCandidates` | No two queued candidates build on the same base. | Guardian holds two acknowledged, conflicting transactions; one can never land. |
| `QueuedAccountIsListed` | An account with a queued candidate has `has_pending_candidate = true`, so the full pass visits it. | The account wedges: the full pass never sees it, and at depth 1 every push gets `409 conflict_pending_delta`. |

`check.sh` runs the first three together (group `safety`) and the last one
alone (group `listed`). Each group runs per backend (`postgres`,
`filesystem`) and per queue depth (`MaxPending`).

## Does my change touch this?

Yes if the diff changes any function listed in `mirrored.txt`. The
`check.sh` drift check hashes each one and lists any that changed, so
when unsure, run it. Things the model treats as one step, and that
therefore must stay one step in the code:

- `submit_candidate`: gate, supersede, insert, and flag set under the
  account lock (Postgres `lock_account_metadata`, filesystem
  `delta_write_lock`).
- `promote_candidate`: lease fence, exact-kind row gate, base gate
  (`states.commitment = prev_commitment`), and in-transaction flag clear.
- `update_candidate_status` and `discard_candidate`: locked
  read-check-write.

The request-path checks in `services/push_delta.rs` and
`services/candidate_chain.rs` are deliberately not modeled, apart from
the label rule. The model lets a client build on any commitment it has
ever seen, so the storage gate is the only authority. A change that
moves a rule from the storage gate into the request path alone is
exactly the kind of change this model will flag.

## Run it

From the repo root:

```bash
.agents/skills/verify-candidate-queue/check.sh          # quick tier, about 5 min
.agents/skills/verify-candidate-queue/check.sh --deep   # adds depth 1, about 8 min
```

A FAIL verdict names the property TLC reports violated
(`FAIL:QueuedAccountIsListed`; any temporal property is `FAIL:temporal`),
and an expected failure in `expected.txt` must name it too, so a
different violation in a cell that is expected to fail still shows as a
`DIFF`. `--cell` exits 0 on PASS, 1 on FAIL and 2 on ERROR.
`--accept-drift` refuses to record a mirrored function it cannot find
(renamed or removed): fix `mirrored.txt` first. Set `TLC_WORKERS` to cap
TLC's worker threads (default: all cores).

It needs `java` (any recent JDK; `brew install openjdk`) and `python3`.
The first run downloads `tla2tools.jar` 1.7.4 to `~/.cache/guardian-tla/`
and checks its SHA-256.

Each cell prints `ok` or `DIFF` against `expected.txt`. Exit code 0 means
every cell matched and nothing drifted. 1 means a cell changed verdict. 3
means the cells matched but mirrored Rust changed (see below). 2 is a
setup problem. Today's baseline is not all-PASS: the filesystem `listed`
cells fail on finding F2 below, and `expected.txt` records that.

To see a full counterexample (every variable in every state):

```bash
.agents/skills/verify-candidate-queue/check.sh --cell postgres 2 listed
```

## Reading a result

A `DIFF` from PASS to FAIL prints the counterexample as a list of model
actions. Translate it with the action map below, then decide:

- **Real bug:** every step is something the code can do, in that order,
  with those atomicity boundaries. Report it with the trace in code
  terms. Do not change `expected.txt` or weaken the property to make it
  pass.
- **Modeling error:** some step is impossible in the code, for example
  the model splits a transaction the code runs in one lock, or lets a
  worker act without a read the code performs. Fix the model action to
  match the code, cite the Rust, and rerun.

A FAIL turning into PASS on a filesystem `listed` cell means F2 was fixed.
Confirm the fix is real, then update `expected.txt` and the findings
section here.

`ERROR` means TLC could not run the model: a parse error, or an action
that leaves a variable unspecified. The first lines of TLC's error are
printed.

## When the change is intended

1. Map the changed Rust to its model action (table below) and edit the
   action so it does what the new code does: same reads, same writes,
   same lock scope.
2. If the change adds a step (a new storage call or `.await` between two
   existing ones), add it as a separate action. Don't fold it into a
   neighbour.
3. Run `check.sh --deep`. A property that now fails is a finding about
   the change, not a reason to edit the property.
4. Once the model matches the code, run `check.sh --accept-drift` to
   re-record `fingerprints.txt`. If you add or rename a mirrored
   function, update `mirrored.txt` first.

## Action map

Paths are relative to `crates/server/src/`.

| Model action | Code |
|---|---|
| `Gate`, `Unchained`, `Position` | `storage/mod.rs` `gate_candidate_submission`, `first_unchained_candidate`, `classify_chain_position` |
| `ClientBuild`, `ClientLabel` | `services/push_delta.rs` `push_delta` (delta built on some base; label rule behind a queued candidate) |
| `ClientSubmit`, `ClientCommit` | `storage/postgres.rs` `submit_candidate` (one transaction; visible at COMMIT) |
| `ClientSubmit`, `ClientFsFlag` | `storage/filesystem.rs` `submit_candidate` (row write, then `set_has_pending_candidate`, both under `delta_write_lock`) |
| `Abandon` | `storage/*` `request_candidate_abandon` |
| `Land` | the chain: only a transaction Guardian acknowledged, only from its base |
| `FullList`, `FullPull` | `jobs/canonicalization/processor.rs` `process_full_pass`, `process_account` (incl. stale-flag heal) |
| `FullIter`, `FullStored`, `FullPredQ`, `FullNext` | `process_candidate_list`, `predecessor_still_queued` |
| `PfcRead`, `PfcObserve` | `process_full_candidate`, `landed_through_successor` (retry, grace and divergence counters abstracted) |
| `ParkFlip`, `ParkRefused` | `park_candidate` → `retain_candidate`, `finalize_abandoned_candidate` → `update_candidate_status` |
| `RelRead` | `release_account_if_queue_empty` → `storage.has_pending_candidate` |
| `ClrPg` | `metadata/postgres.rs` `clear_pending_candidate_if_none` (locks the metadata row, then the conditional UPDATE) |
| `ClrFs` | `metadata/mod.rs` default `clear_pending_candidate_if_none` (plain clear) |
| `Promote`, `PromoteWrite` | `canonicalize_verified_delta` → `storage/*` `promote_candidate`; the `superseded` arm is `storage/postgres.rs` `restore_superseded` / `storage/filesystem.rs` `restore_superseded_holding_lock` |
| `LoseLease`, `ZombieWrite`, `ZombieSeesNewHolder` | `worker.rs` lease loss mid-pass; `storage/postgres.rs` `lease_fence_is_current` after `lock_account_metadata` (ordering proved in `verify-worker-lease`) |
| `FastPull` … `FastNext` | `process_recent_pass`, `process_recent_page`, `process_recent_candidate` |
| `RcPull` … `RcPathNext`, `SelectPath`, `Walk`, `Try`, `HopOrder` | `jobs/canonicalization/reconciliation.rs` `reconcile_account`, `reconcile_chain_advance`, `select_recoverable_path` (backtracking), `reconcile_recoverable_path`, TTL expiry via `remove_delta`; archived rows come from `pull_superseded_deltas` tagged `superseded` |
| `archive`, `RcPrune` | `superseded_deltas` (migration 2026-10-19-000001): `submit_candidate` archives the parked row it supersedes; `prune_superseded_deltas` drops rows past the TTL |

## Scope and assumptions

- One account; two concurrent clients; at most 3 deltas built
  (`MaxDeltas`); queue depth 1 (`--deep`) and 2. A pass covers this bound,
  not every queue length or client count.
- The chain only moves by a transaction this Guardian acknowledged
  (MultisigGuardian). Guardian switches, `configure_account` and other
  out-of-band state writes are not modeled.
- A delta's post-state commitment is the true result of applying it, so
  the client's claimed commitment always matches the recomputation.
  Mismatched claims and RPC errors (the worker defers) are not modeled.
- Retry, grace-period and divergence-confirmation counters are
  abstracted: any observation that could ever lead to parking may park.
  This over-approximates when parking happens.
- Retention is on (`retained_ttl_seconds > 0`), so a supersede archives
  the replaced row. The retention-off delete paths are not modeled.
- One `delta_write_lock` section is one step, including crash points
  inside it. The filesystem backend is development-only and is not
  crash-atomic: its restore of an archived delta deletes the archived
  copy before the state and delta files are rewritten, so a crash or
  write failure in between can lose that copy. Postgres does the whole
  restore in one transaction.
- At most one lease holder at a time. A lost lease can leave one
  promotion in flight (Postgres only), and it may still commit, but only
  before the new holder's first step that takes the account lock. That
  ordering is proved separately by `.agents/skills/verify-worker-lease`
  (`WritesInTokenOrder`); rerun that skill when the lease or the fence
  changes.
- Proposals, `release_on_switch`, metrics and auth refresh are out of
  scope.

Guards the model shows are load-bearing: removing the competing-base
refusal, accepting an at-base chain read as landed, skipping the fast
pass's chain check, or widening the promote kind gate each breaks a
property. Removing the promote base gate alone does not, because the
lease fence covers it. Letting a stale promotion land after the new
holder's own locked steps does: it can stamp a retained row that was
superseded and retained again at the same nonce.

## Findings in the baseline

**F1 (fixed). Postgres: the worker's flag clear could wipe a concurrent
submission's flag.** `clear_pending_candidate_if_none` used to be a lone
`UPDATE ... WHERE has_pending_candidate AND NOT EXISTS (candidate)`.
Under READ COMMITTED, when it waited on a submission's row lock it
re-checked only that row afterwards, while the `NOT EXISTS` subquery kept
its pre-wait snapshot. A clear running during a `submit_candidate`
transaction therefore set the flag to `false` with a candidate queued,
and the full pass never visited the account again. The fix locks the
metadata row first, in one transaction, so the UPDATE's snapshot is taken
after any in-flight submission commits. The regression test is
`metadata::postgres::tests::clear_pending_candidate_waits_for_an_in_flight_submission`
(it fails without the lock). If a change brings back a `postgres listed
FAIL`, this race, or one like it, is back.

**F2. Filesystem: the plain flag clear can wipe a concurrent
submission's flag** (`filesystem * listed FAIL`). It is the same sequence
as F1, except that the clear is an unconditional `set_has_pending_candidate(false)`
outside `delta_write_lock`. `metadata/mod.rs` documents this as an
accepted residual window for single-process backends. It is kept as a
finding because the consequence is the same wedge.
