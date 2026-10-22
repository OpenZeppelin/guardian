---
name: verify-proposal-lifecycle
description: Model-check the multisig proposal lifecycle (create, pin to the queue tail, execute, Guardian ack, landing, canonicalization and cleanup) across the Guardian server and both SDKs with TLA+/TLC. Use when a diff touches push_delta_proposal, push_delta, proposal deletion in the canonicalization worker, the candidate submission supersede, the Rust execute_proposal / get_guardian_ack_signature / proposal nonce label, or the TypeScript prepareProposalExecution / assertExecutesOnPinnedBase / resolveProposalNonce, or when check.sh reports DRIFT.
---

# Verify the proposal lifecycle

`model/ProposalLifecycle.tla` models one multisig account. Two cosigner
devices (each running the Rust or the TypeScript SDK) create proposals
through Guardian and execute them. Each execution is pushed to Guardian
for its acknowledgement, the transaction lands on chain, and the
canonicalization worker promotes, parks or reconciles the resulting
candidate. TLC explores every interleaving of those steps.

An account state is modeled as the path of summaries that produced it,
so its nonce is the path's length. Applying summary `s` to state `b`
gives `Append(b, s)`, and a proposal's id is its summary
(`delta_proposal_id` is the tx_summary commitment).

The candidate queue's own races (locks, READ COMMITTED, lease loss) are
checked by `verify-candidate-queue`. Here the worker is reduced to the
outcomes that model proves safe: promote a candidate once the chain
reaches it, park one that has not landed, and reconcile a parked row
once the chain reaches it.

## What is checked

| Property | In plain words | Harm if broken |
|---|---|---|
| `ExecutesOnPinnedBase` | A proposal Guardian pinned to the state its summary was built on lands only from that state. | An intent the cosigners approved for one state runs on another: a proposal made concurrently with a sibling on the same base runs after the sibling, or one built behind a candidate that never landed runs without it. `spec/processes.md` promises SDKs keep a proposal from executing anywhere but on the tail. |
| `AckedForLandingBase` | Every landed transaction carries a Guardian ack issued for the state it landed from. | Guardian queued a different transition from the one on chain; its candidate never verifies and the stored state falls behind the chain. |
| `GuardianCatchesUp` | With the worker's promote, park and reconcile steps taken whenever they stay enabled (`LiveSpec`), Guardian's stored state ends up equal to the chain and stays there. A liveness property: unlike `LandedStepRecorded`, it fails when a recorded row is one reconciliation never promotes. | Guardian stays behind the chain for good. |
| `LandedStepRecorded` | Every on-chain step Guardian has not caught up with is still in a delta row (candidate, retained, abandoned, archived superseded, or canonical). | Neither the worker nor reconciliation can ever promote past that step: Guardian stays behind the chain, and every push gets `400 commitment_mismatch`. |

Each property runs for every SDK pairing (`rust rust`, `ts ts`,
`rust ts`) at queue depth 1 and 2.

## Does my change touch this?

Yes if the diff changes a function listed in `mirrored.txt` (Rust and
TypeScript). The `check.sh` drift check hashes each one. In particular:

- What a device pushes as `prev_commitment` at execution, compared with
  the state its transaction actually executes on: Rust
  `get_guardian_ack_signature` pushes the local account commitment; TS
  `prepareProposalExecution` pushes the served `prevCommitment` after
  `assertExecutesOnPinnedBase`.
- Which proposals an SDK will execute: Rust refuses
  `proposal.nonce <= account.nonce()` and, through
  `ensure_executes_on_pinned_base`, any proposal whose pinned base is not
  its local commitment; TS refuses any proposal whose pinned base is not
  its local commitment.
- How Guardian pins and labels: `push_delta_proposal` pins to the queue
  tail and checks the label only when a queue exists. `push_delta`
  checks label = applied nonce only behind a queued candidate.
- Which rows exist after a park: `retain_candidate` /
  `finalize_abandoned_candidate`, and `submit_candidate`'s supersede of a
  retained or client-abandoned row at the same nonce.

## Run it

From the repo root:

```bash
.agents/skills/verify-proposal-lifecycle/check.sh          # quick tier, under a minute
.agents/skills/verify-proposal-lifecycle/check.sh --deep   # adds 4 proposals, about 9 min
```

A FAIL verdict names the property TLC reports violated
(`FAIL:ExecutesOnPinnedBase`; any temporal property is `FAIL:temporal`),
and an expected failure in `expected.txt` must name it too, so a
different violation in a cell that is expected to fail still shows as a
`DIFF`. `--cell` exits 0 on PASS, 1 on FAIL and 2 on ERROR.
`--accept-drift` refuses to record a mirrored function it cannot find
(renamed or removed): fix `mirrored.txt` first. Set `TLC_WORKERS` to cap
TLC's worker threads (default: all cores).

It needs `java` and `python3`. The first run downloads `tla2tools.jar`
1.7.4 to `~/.cache/guardian-tla/` and checks its SHA-256.

Each cell prints `ok` or `DIFF` against `expected.txt`. Exit code 0
means every cell matched and nothing drifted. 1 means a cell changed
verdict. 3 means cells matched but mirrored code changed. 2 is a setup
problem. The baseline is all-PASS: P1 and P2 are fixed. A
`rust * ExecutesOnPinnedBase FAIL` means P1 is back; a
`LandedStepRecorded` or `GuardianCatchesUp` FAIL means P2 (or another
way of stranding Guardian behind the chain) is.

To see a full counterexample (every variable in every state):

```bash
.agents/skills/verify-proposal-lifecycle/check.sh --cell rust ts 2 ExecutesOnPinnedBase
```

## Reading a result

A `DIFF` from PASS to FAIL prints the counterexample as model actions.
Translate it with the action map, then decide:

- **Real bug:** each step is something the code does, in that order.
  Report it in code terms; do not edit `expected.txt` or the property to
  make it pass.
- **Modeling error:** a step the code cannot take (for example a device
  pushing a base it never reads, or a worker outcome the candidate-queue
  model rules out). Fix the action to match the code, cite it, rerun.

A PASS turning into FAIL is a regression or a new finding: read the trace
before touching `expected.txt`. `ERROR` means
TLC could not run the model; its first error lines are printed.

## When the change is intended

1. Map the changed code to its action (table below) and make the action
   do what the new code does.
2. A new guard or a new request between two existing ones is a new
   condition or a new action, not a rewrite of a neighbour.
3. Run `check.sh --deep`. A property that now fails is a finding about
   the change.
4. Run `check.sh --accept-drift` once the model matches the code; update
   `mirrored.txt` first if you add or rename a mirrored function.

## Action map

| Model action | Code |
|---|---|
| `Sync` | Rust `client/account.rs` `sync`; TS `multisig.ts` `syncState` (adopt the chain state or Guardian's canonical state) |
| `Propose` | Rust `client/proposals.rs` `propose_transaction_with_options` (label `account.nonce() + 1`); TS `resolveProposalNonce` (store nonce + 1, `Date.now()` through 0.18.0); server `services/push_delta_proposal.rs` `push_delta_proposal` (pin to tail; refuse a full queue or a label other than tail + 1 when a queue exists) |
| `ExecuteRust` | Rust `execute_proposal` (sync, refuse `proposal.nonce <= account.nonce()`, refuse a pinned base other than the local commitment via `ensure_executes_on_pinned_base`), `helpers.rs` `get_guardian_ack_signature` (push with the local commitment), `finalize_transaction` (execute on the local account) |
| `ExecuteTs` | TS `prepareProposalExecution` → `assertExecutesOnPinnedBase`, `pushDelta` with the served `prevCommitment`; `executeProposal` → `submitAtTip` on the store account |
| `Admits`, `Admit` | server `services/push_delta.rs` `push_delta` and `storage::gate_candidate_submission` / `submit_candidate` (tail position, full queue, label rule behind a queue, supersede of a parked row at the nonce) |
| `Land`, `Expire` | the chain: lands from the state it was built on; the multisig replay map refuses a summary that already executed; a transaction may expire |
| `Promote` | `jobs/canonicalization/processor.rs` `canonicalize_verified_delta` (promote, delete the proposal whose id is the summary) |
| `Park` | `retain_candidate`, `finalize_abandoned_candidate`, `handle_orphaned_candidate` (park, delete the matching proposal; an orphaned head is parked without a chain read) |
| `Reconcile` | `jobs/canonicalization/reconciliation.rs` `reconcile_account`, `reconcile_chain_advance`; archived rows via `pull_superseded_deltas` and `PromotableKind::Superseded` (`storage/postgres.rs` `restore_superseded`, `storage/filesystem.rs` `restore_superseded_holding_lock`) |

## Scope and assumptions

- One account, two devices, at most 3 proposals (4 with `--deep`),
  queue depth 1 and 2. Labels are honest nonces or timestamps.
- A summary built on one state reproduces on another state where its
  effects apply (vault and nonce changes are relative), so cosigner
  signatures, the Guardian ack and both SDKs' binding re-execution accept
  it there. Intents that fail on the other state only remove behaviours.
  Signature collection and thresholds are not modeled (enforced on
  chain).
- Retention is on and nothing expires through the TTL. A transaction
  either lands or expires before `retained_ttl_seconds`.
- Out of scope: switch_guardian (TS `servedSwitchProposal` deliberately
  skips the pin check when the old Guardian does not serve it; Rust checks
  a switch's pinned base against the pre-switch Guardian that serves it,
  like any other proposal), offline import/export, custom
  proposals, EVM proposals, the pending-proposal limit, and every queue
  race covered by `verify-candidate-queue`.
- Guardian cannot know which state a proposer built a summary on: it pins
  to the queue tail. A proposer whose local state is ahead of the tail
  (its own candidate was parked) gets a proposal pinned to a state it did
  not build on. `ExecutesOnPinnedBase` only judges correctly pinned
  proposals, so this case is noted here rather than checked.

Guards the model shows are load-bearing: TS `assertExecutesOnPinnedBase`
(without it both passing properties fail), and Rust pushing the local
commitment rather than the proposal's pin (acking the pin breaks
`AckedForLandingBase`).

## Findings in the baseline

**P1 (fixed). The Rust SDK executed a proposal off its pinned base.**
Rust `execute_proposal` used to refuse only `proposal.nonce <=
account.nonce()` and never compared the proposal's `prev_commitment` with
its own state, while TS refuses unless they are equal. Rust now refuses
too: `execute_proposal` and `prepare_custom_execution` call
`ensure_executes_on_pinned_base` (`MultisigError::ProposalBaseMismatch`),
covered by `execute_proposal_refuses_a_proposal_pinned_to_another_state`
in `client/switch_recovery_tests.rs`. The counterexamples the model found
before the fix:

1. Depth 2, honest labels. A device executes proposal A from state S
   (candidate on S). Behind it, proposal B is created pinned to A's
   post-state with label n+2. A's candidate is parked (it never lands).
   A Rust device holding S executes B: `n+2 > n` passes, it pushes with
   `prev_commitment = S`, and Guardian admits it (queue empty, so the
   label is not checked). B lands from S without A.
2. Depth 1, a legacy timestamp label. Proposals A and B are both made on
   state S. A executes and lands. A Rust device now at A's post-state
   executes B: the timestamp label passes the nonce guard, Guardian
   admits it at the head, and B lands on top of A although its cosigners
   approved it against S.

With the check, `ExecuteRust` requires the synced local state to equal
the pin, and every `ExecutesOnPinnedBase` cell passes.

**P2 (fixed). A resubmission at a parked candidate's nonce deleted the
only row a late-landing transaction needs.** Counterexample before the
fix: proposals A and B are both made on state S with label
n+1. A executes; its candidate is parked (abandon, retry exhaustion)
while its transaction is still in flight. B executes with the same label:
`submit_candidate` supersedes A's retained or client-abandoned row and
queues B. A's transaction then lands. The chain moved S to S+A, but
Guardian has no row for that step: B's candidate diverges and is parked,
and reconciliation finds nothing that starts at S and reaches the chain.
Guardian stays at S for good. `spec/processes.md` keeps abandoned rows
precisely so a late landing can be recovered, and the supersede (which
the abandon flow needs, to resubmit at the same nonce) removes that row.
The fix archives the superseded row (`superseded_deltas`, migration
2026-10-19-000001) instead of deleting it, and the reconcile pass
promotes an archived row with `PromotableKind::Superseded` when the chain
shows it landed, replacing the parked row at its nonce. In the model,
`Admit` moves the replaced row to `archived` and `Reconcile` promotes from
it. Removing either step fails `GuardianCatchesUp`, and so did the old
delete. Restoring over a canonical row is refused in the code, but the
model's base check already rules that case out, so that guard is defense
in depth here. While adding `GuardianCatchesUp`, `Park` was corrected to
match `handle_orphaned_candidate`: a head that no longer chains from the
stored state is parked without any chain observation.
