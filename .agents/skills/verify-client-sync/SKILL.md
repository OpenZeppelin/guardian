---
name: verify-client-sync
description: Model-check how the Rust multisig SDK (crates/miden-multisig-client) and the TypeScript multisig SDK (packages/miden-multisig-client) keep a cosigner device's local view of an account (its local store state and, for TypeScript, the cached signer config) on the chain and on Guardian, with TLA+/TLC. It checks that every device ends up on the chain's state once nothing is pending at the node, including after the node drops a transaction the device already applied locally, and that no TypeScript decision reads a signer config the local state no longer has. Use when a diff touches the SDKs' sync (sync / sync_from_guardian_internal, syncState / isSafeToAdoptGuardianState), how execution writes local state (finalize_transaction, rebuilds_local_state_from_delta, submit_transaction_at_tip, ProverWorkflow.submit), proposal creation or execution (execute_proposal, execute_imported_proposal, prepareProposalExecution, assertExecutesOnPinnedBase), the TS config cache (refreshConfigFromAccount), or when check.sh reports DRIFT.
---

# Verify client sync

`model/ClientSync.tla` models one private multisig account with two
cosigner devices, one per SDK, against Guardian (candidate mode, queue
depth 1) and a node that takes a submitted transaction into its mempool
and later includes it or drops it (expired, or evicted with a dropped
dependency). TLC explores every interleaving of the devices proposing,
executing and syncing (Guardian sync and chain sync), the node including
or dropping, and Guardian promoting or retiring its candidate. A state
carries a nonce, a parent and a signer config; one step is one SDK
call's decision or one server or node step.

Guardian's own TLA+ models (separate skills, not on this branch) treat a
client as an ideal actor that always knows the truth. This one models
what a device believes.

## What is checked

| Property | In plain words | Harm if broken |
|---|---|---|
| `DeviceConverges` | Once nothing is pending at the node, every device's local state ends up the chain's (under fair syncs). | A device stays on a state that never landed: its proposals are built on it and refused, and it cannot execute until the client restarts or its store is reset. |
| `NeverRollsBackPending` | A sync never rolls a device back from a transaction the node still has pending. | If it then lands, the device has already built on the older base and forks against it; a private account cannot recover the discarded post-state from chain sync. |
| `ConfigFresh` | No TypeScript decision (readiness, the target signer list of a membership proposal, the cosigner list sent at registration) reads a cached signer config the local state no longer has. | A stale signer list is written into a membership proposal (dropping a signer added since), or readiness is judged against the wrong threshold. |

## Findings (both fixed)

**C1 (fixed): a dropped Rust membership, threshold or switch transaction
left the device on a state that never landed.** `finalize_transaction`
(`client/helpers.rs`) writes those transaction types
(`rebuilds_local_state_from_delta`) with `add_or_update_account` after
`submit_proven_transaction`, not through miden-client's
`apply_transaction_update` (miden-client "persists incorrectly for private
accounts" for them). miden-client holds no record of the transaction, so
its chain sync can never undo it (`undo_account_state` undoes only
recorded, discarded transactions), and `sync_from_guardian_internal` kept
local whenever Guardian was not ahead. Trace (mutation `m1`): a
signer-config proposal executes on the Rust device, the node drops it,
Guardian retires the candidate, and the device stays on its local state
for the rest of the session (the store is per process, so a restart and
a pull recovered it; `pull_account` on the same store cannot, since
miden-client refuses a lower nonce). The TypeScript SDK applies through
`submission.apply()`, so miden-client undoes it there.

Fix: `finalize_transaction` records the submission it wrote without a
record (`UntrackedSubmission`: post-state commitment, submission height,
expiration block), and `local_state_abandoned` (`client/account.rs`) lets
sync roll local back to Guardian's state when Guardian is not ahead and
all of these hold:

- this client submitted the local state, and miden-client's own discard
  rule says that transaction is dropped by the store's sync height
  (`UntrackedSubmission::dropped_at`: expired, or still unconfirmed
  `TX_DISCARD_DELTA` = 20 blocks after submission, as miden-client
  `apply_sync_height_update` judges the transactions it does record);
- miden-client has no pending record for the account;
- the chain holds Guardian's state;
- unless the transaction expired (`UntrackedSubmission::expired_at`, only
  for a proposal created with an approval expiration), Guardian does not
  hold the local state as a delta that may still land
  (`delta_may_still_land_as`: a candidate or retained delta recording the
  local commitment, or one whose commitment does not parse, keeps local).
  An expired transaction cannot land, so Guardian is not asked.

The node-side condition is what makes the rollback safe. Guardian's
answer alone is no evidence: it can forget a delta whose transaction is
still pending (reconciliation expires a retained row past
`retained_ttl_seconds` without looking at the node, retention disabled
deletes a give-up candidate, a best-effort switch push can fail), and a
first version of this fix trusted Guardian's not-found and discarded
answers and rolled back a still-pending transaction (mutation `m3`). The
stale half of the rule is a heuristic, not proof: it is the one
miden-client already applies to every recorded transaction, so a Rust
untracked transaction and a TypeScript or Rust recorded one are judged
alike, and it is why a transaction without an expiration still waits for
Guardian's veto (in practice until `retained_ttl_seconds`, 24 hours by
default, runs out). A failed read keeps local.

The rollback builds a fresh miden-client store (both miden-client and the
store refuse a lower nonce) on the same node RPC client, puts Guardian's
state and the unconsumed input notes (as expected notes) into it, syncs
it to the chain tip, and installs it only once all of that succeeded
(`roll_back_store`), so a failed rollback keeps the current store, notes
included, for the next sync. A note whose tag is unknown keeps local
rather than be lost. Output-note records cannot be carried into a fresh store,
so the client keeps the replaced store for the session and note export reads
an earlier note from it. The replaced store file stays in the account directory
and the rollback's warning names it, so a transaction that lands after the
stale window can still be recovered from it.

The fix does not cover a switch to a new endpoint: `finalize_transaction`
registers the switched, not yet landed, state on the new Guardian, whose head
then equals local, so sync returns before the rollback check. The model leaves
the switch's rebind and registration out (see Scope).

**C2 (fixed): the TypeScript signer config cache went stale.** `threshold`
and `signerCommitments` were refreshed only from `syncState` and
`syncNetworkOnly`, not after `executeProposal` and not when miden-client's
chain sync undoes a dropped transaction. Until the next `syncState`,
`createAddSignerProposal` / `createRemoveSignerProposal` built the target
signer list from the old list (after executing "add X", a following "add
Y" targeted the old list plus Y, and executing it removed X), readiness
used the old threshold, and `registerOnGuardian` sent the old cosigner
list. Trace (mutation `m2`): execute a signer change, it lands, execute
again with no sync in between. Refreshing only after execution is not
enough, because the undo of a dropped transaction leaves the cache on the
dropped config.

Fix: every proposal creator (`createProposal` and each `create*Proposal`
method), `signProposal`, `signProposalOffline`, `importProposal`,
`syncProposals` (`reconcileProposals`), `submitTransaction` and
`registerOnGuardian` re-read the store (`refreshConfigFromStore`), and
`prepareProposalExecution` and `prepareCustomExecution` refresh the config
from the one store read each already makes for the pinned-base check, before
any signature or readiness check. Each reads after its own chain sync when it
makes one: that sync is what undoes a dropped transaction, and it can change
the signer set, not only the threshold. Proposal verification can sync too
(note authentication for consume proposals, the bound-block catch-up), so
import, signing (online and offline) and listing judge signatures and readiness
after verification, and the consume-notes creator reads after authenticating
its notes. The note-recovery listing (`syncProposalsIsolatingFailures`) uses
proposals only for their notes and does not judge readiness.

`ConfigFresh` checks the config a TypeScript decision actually uses against
the store. In the fixed model every decision reads the store in the same
step it decides, so the property holds by construction and its value is as a
regression guard (`m2` restores the cached read and fails it). The model
does not represent a second concurrent call on the same `Multisig` whose
chain sync lands between a decision's store read and its use. It also sets
`staleUse` only in `Execute`: proposal creation, signing, listing, import and
`submitTransaction` read the config the same way, and those reads are held by
unit tests in `multisig.test.ts` and by fingerprint drift on their functions
(`mirrored.txt`), not by the model.

## Rust and TypeScript differences seen while mapping (not all modeled)

From reading both SDKs. None is a model finding by itself; each is a
place the two SDKs make different decisions on the same inputs
(`AGENTS.md` §3, rule 5).

- Rust syncs (Guardian and chain) before propose, sign and execute; TS
  does not.
- Listing: Rust drops proposals with `nonce <= local nonce`; TS lists
  everything Guardian returns and blocks stale ones at execution.
- Execution: TS checks local == the pinned base and echoes Guardian's
  `prev_commitment`; Rust checks `nonce > local nonce` and pushes its
  local commitment (PR #559, not on this branch, adds the pinned-base
  refusal to Rust).
- Switch: Rust pushes the switch delta to the old Guardian before
  submitting, TS after.
- Equal-nonce divergence on sync: Rust keeps local quietly, TS throws.
- No chain commitment: Rust's overwrite guard always passes, TS refuses
  for a transacted account (its `syncState` with no local record still
  adopts unchecked; `load` does not).
- `execute_imported_proposal` (Rust) has no nonce check.

## Run it

From the repo root:

```bash
.agents/skills/verify-client-sync/check.sh            # quick tier, seconds
.agents/skills/verify-client-sync/check.sh --deep       # larger bounds, a few minutes
.agents/skills/verify-client-sync/check.sh --mutations
.agents/skills/verify-client-sync/check.sh --cell m1
```

A FAIL verdict names the property TLC reports violated (any temporal
property is `FAIL:temporal`), and an expected failure in `expected.txt`
must name it too. A cell whose properties include `DeviceConverges` runs
`FairSpec`. `--cell` exits 0 on PASS, 1 on FAIL and 2 on ERROR.
`fingerprint.py` hashes Rust (`fn`) and TypeScript (function, method or
`const` arrow) definitions; `--accept-drift` refuses to record a
function or file it cannot find. Set `TLC_WORKERS` to cap TLC's worker
threads. It needs `java` and `python3`; `tla2tools.jar` 1.7.4 is cached
in `~/.cache/guardian-tla/` and checksum-verified. Exit codes: 0 all
match, 1 a verdict changed, 2 setup error, 3 mirrored source drifted.

Both findings are fixed and folded into the normal actions; mutations
`m1` (no Rust rollback), `m2` (TS reads its cached config) and `m3` (roll
back on Guardian's word alone, without the node-side drop; `m3-expiring`
runs it with every config proposal expiring) put each back and must fail
(`check.sh --mutations`). A new finding goes in the
same way: an expected FAIL until the fix lands, then a mutation.

## Reading a result

A `DIFF` to FAIL prints the counterexample as model actions (map below);
`st` changes print as `new <id>=(nonce, parent, config, kind)`, and
every other value is a state id. If every step is something the SDKs,
Guardian and the node can do, it is a real bug; if a step is impossible
in the code, fix the action and cite the source. After changing the
model to match intended code, run `check.sh --accept-drift`.

## Action map

Rust paths are under `crates/miden-multisig-client/src/`, TypeScript
paths under `packages/miden-multisig-client/src/`.

| Model action | Rust | TypeScript |
|---|---|---|
| `SyncGuardian` | `client/account.rs` `sync_from_guardian_internal` with `is_not_ahead_of`, `local_state_abandoned` (`UntrackedSubmission::dropped_at` / `expired_at`, `delta_may_still_land_as`), `ensure_safe_to_overwrite_local_state`, then `input_notes_to_carry` and `roll_back_store` on a rollback | `multisig.ts` `syncState` with `guardianIsNotAhead`, `state/adopt.ts` `isSafeToAdoptGuardianState` |
| `RefreshConfig` | (none: the config is read from the account each time) | `multisig.ts` `refreshConfigFromAccount`, from `syncState` / `syncNetworkOnly` |
| `SyncChainUndo` | miden-client `sync_state` (sqlite store `undo_account_state`) for recorded transactions | the same, through the WASM client |
| `Propose` | `client/proposals.rs` `propose_transaction_with_options` (label local + 1) | `multisig.ts` `createProposal`, `resolveProposalNonce` |
| `Execute` | `client/proposals.rs` `execute_proposal`, `client/helpers.rs` `get_guardian_ack_signature`, `finalize_transaction` (untracked for `rebuilds_local_state_from_delta` types, recorded as an `UntrackedSubmission`) | `multisig.ts` `prepareProposalExecution` / `prepareCustomExecution` (store read, `adoptConfigFromAccount`, `assertPinnedBase`), `executeProposal`, `prover/workflow.ts` `submit` |
| `Land`, `Drop` | the node: include a pending transaction built on the chain head, or drop one | |
| `Promote`, `Retire` | Guardian's canonicalization: promote the candidate once the chain holds it; retire it as retained (also while its transaction may still be pending, on exhausted retries or a lagging read) | |
| `ReconcilePromote`, `Expire` | Guardian's reconcile pass: promote a retained delta the chain head shows landed; drop it after `retained_ttl_seconds` once its transaction is gone | |
| `Forget` | Guardian dropping a delta whose transaction is still pending: reconciliation's TTL expiry (`reconcile_expired`) does not look at the node, and with retention disabled a give-up candidate is deleted outright (`Retire` then `Forget`) | |

## Scope and assumptions

- Two devices, one per SDK; up to 5 or 6 states, nonces up to 3 or 4,
  one dropped transaction. A proposal is plain (keeps the signer config)
  or a signer-config change.
- The account is private: chain sync never installs a state, it only
  undoes recorded discarded transactions. A public account's chain sync
  can also update the tracked account; not modeled.
- A switch that changes the Guardian endpoint is not modeled: after it the
  device talks to the new Guardian, which `finalize_transaction` registers
  with the switched state. C1's rollback does not reach that case.
- Signing, approval thresholds and the summary binding are folded into
  `Execute` (a proposal executes only from the state it was built on);
  offline export and import, the guardian switch's rebind and
  registration, and note recovery are not modeled.
- Guardian is simplified to its stored state, one candidate, and
  proposals pinned to its stored state; its own invariants belong to the
  server-side skills.
- The `Retire`, `Expire` and `Forget` guards are assumptions about the
  server, not a description of it. `Spec` (the safety cells) includes
  `Forget`, so `NeverRollsBackPending` holds without trusting Guardian's
  absence. `FairSpec` (the convergence cells) leaves it out and keeps
  `Expire` to transactions the node no longer has: a forgotten transaction
  that then lands leaves Guardian behind the chain, a server-side gap no
  client sync can close (with the default 24 hour TTL it needs a
  transaction pending at the node for a day).
- Each state records whether its proposal set an expiration (`e`), and
  only states without one take Guardian's veto. A cell sets it for every
  config proposal (the `expiring` column, constant `Expiring`) instead of
  branching per proposal, which multiplied the safety cell's state space
  by more than five. Because the model reads the node's mempool, expiry and
  the stale window are both exact in it: the `-expiring` cells check that
  skipping Guardian on expiry keeps the properties, and `m3-expiring` that
  the expiry path still needs the node-side drop, not that the veto backs
  the heuristic.
- "The node no longer has it pending" stands for miden-client's discard
  rule, which the SDKs act on; a transaction that outlives the stale window
  in the mempool and then lands is outside the model, for recorded and
  untracked transactions alike.
