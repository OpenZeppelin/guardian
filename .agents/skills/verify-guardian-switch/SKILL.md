---
name: verify-guardian-switch
description: Model-check the account status lifecycle around a guardian switch (issues #305, #434) and an operator pause (#181) with TLA+/TLC. It checks that a release is always true of the state the server holds, that only /configure re-onboarding reactivates a released account, that a switch the server can see is eventually released, and whether an ack returned after a pause or release can execute on chain. Use when a diff touches services/release_on_switch.rs, services/account_status.rs, the pause/unpause services, set_released_if_state / clear_released_if_state / set_pause / clear_pause, configure_account's submit_state and release clear, jobs/release_sweep/, the release hook in canonicalization or delta_commit, the status check in push_delta or push_delta_proposal, or when check.sh reports DRIFT.
---

# Verify the guardian switch lifecycle

`model/GuardianSwitch.tla` models one account on Postgres in candidate
mode: its stored state, its on-chain history, the `released_at` and
`paused_at` columns, a candidate queue of depth 1 (the default), two
concurrent `push_delta` requests, the canonicalization worker with its
release hook, the reconcile pass, the release sweep, `/configure`
re-onboarding, and operator pause and unpause. TLC explores every
interleaving of those with the chain, where a transaction from a state
bound to this server lands only with its ack, a guardian rotation lands
without it, and the other guardian signs anything after a switch. One
model step is one storage transaction or one statement.

## What is checked

| Property | In plain words | Harm if broken |
|---|---|---|
| `ReleasedIsTrue` | A released account really left this server at or after the state the release was proved against, and a re-onboarding is never left released by evidence about an older state. | A re-onboarded wallet is refused every mutation (`GUARDIAN_ACCOUNT_RELEASED`) although it is bound to this server. |
| `OnlyReonboardingReactivates` | Only a `/configure` step clears `released_at`; pause and unpause never do. | An operator unpause makes a switched-away account mutable again. |
| `ReleasedAckNeverExecutes` | An ack returned after the account was released never executes on chain. | This server co-signs for an account it no longer guards. |
| `PausedAckNeverExecutes` | An ack returned after the account was paused never executes on chain. | The operator's pause, the safety lever, does not stop the next transaction. **Fails today (F1).** |
| `NoAckWhileInactive` | No ack is returned at all while paused or released. | Stricter form of the two above; holds only with the F1 fix. |
| `SwitchIsReleased` | When the account has left for good and the server can see the switch (public storage, or every switch went through a delta or proposal held here), it is eventually released. | Stale reads and dead proposals for a switched account. **Fails for private accounts (F2, F3).** |

## Findings

**F1 (medium): a pause does not fence a `push_delta` already in flight.**
`push_delta` checks the status on the metadata loaded at the start of the
request (`services/push_delta.rs`, `ensure_account_active_metadata`),
then reconstructs, applies, signs the ack and submits the candidate.
`submit_candidate` and `gate_candidate_submission` never read
`paused_at`. Trace (`check.sh --cell pause-race`):

```
PushRead    push passes the status check
Pause       operator pauses; the pause call returns
PushCommit  candidate admitted, ack returned to the client
ChainLand   the transaction executes on chain
```

The window is the whole reconstruction and apply, seconds under load.
The fix the `lockcheck` variant models: read `paused_at` and
`released_at` inside `submit_candidate`'s transaction, after
`lock_account_metadata` (the row `set_pause` updates), and refuse there.
With it every property holds, including `NoAckWhileInactive`.
`push_delta_proposal` and `sign_delta_proposal` have the same
check-then-act shape, but they store proposals and signatures, not acks,
so a late write there cannot execute anything on its own. The release
side of the same race is harmless (`ReleasedAckNeverExecutes` holds): an
ack after a release is built on a state the chain has already left.

**F2 (low): `/configure` can roll the stored state back, and a private
account then never releases.** `storage/postgres.rs` `submit_state` is an
unconditional upsert, with no lock and no nonce check. A `/configure`
that read the chain before a switch delta promoted writes its older state
over the promoted one. The hook's release then correctly refuses
(StateMoved), but nothing will ever prove the switch again: the stored
state is bound to this server, the switch delta is canonical (so not a
recoverable row), and its proposal was deleted at promotion. On a public
account the sweep's storage read still releases it. The `cfgnonce`
variant (refuse a state whose nonce is below the stored one, atomically)
removes this trace; whether `/configure` should ever accept an older
state is a design decision.

**F3 (low): a late-landing retained delta followed by a switch.** An own
delta exhausts its retries and is retained, then lands; a cosigner
creates a switch proposal here, which is recorded against the stored
state (the queue is empty, and `verify_delta` checks only the tail's
own commitment, not the base the summary was built on); the switch
executes. The sweep applies each proposal only to the stored state
(`precompute_switch`), one hop, so it never reproduces the switch's
post-state, and reconcile only promotes a recoverable path that ends at
the chain head, which is now the switch. Same impact class as issue #504.
Clients that refuse to execute a proposal pinned to a state they do not
hold (TypeScript 0.18.1 and later, Rust with PR #559) cannot produce this
trace; older clients can.

## Does my change touch this?

Yes if the diff changes any function in `mirrored.txt` (the drift check
lists them), adds a mutating entry point that must respect pause or
release, adds a writer of the `states` row, or adds a release detector.
A new release detector must write through `release_switched_account`
with the stored state it proved against. A new writer of the stored state
outside canonicalization must clear a release the way `/configure` does,
after its write, conditionally on its own state.

## Run it

From the repo root:

```bash
.agents/skills/verify-guardian-switch/check.sh              # quick tier, about 30 s
.agents/skills/verify-guardian-switch/check.sh --deep       # larger bounds, a few minutes
.agents/skills/verify-guardian-switch/check.sh --mutations  # every broken variant must fail
.agents/skills/verify-guardian-switch/check.sh --cell pause-race
```

A FAIL verdict names the property TLC reports violated
(`FAIL:ReleasedIsTrue`; any temporal property is `FAIL:temporal`), and an
expected failure in `expected.txt` must name it too, so a different
violation in a cell expected to fail still shows as a `DIFF`. A cell
whose properties include `SwitchIsReleased` runs `FairSpec`. `--cell`
exits 0 on PASS, 1 on FAIL and 2 on ERROR. `--accept-drift` refuses to
record a mirrored function it cannot find (renamed or removed): fix
`mirrored.txt` first. Set `TLC_WORKERS` to cap TLC's worker threads
(default: all cores).

It needs `java` and `python3`. `tla2tools.jar` 1.7.4 is cached in
`~/.cache/guardian-tla/` and checksum-verified (downloaded on first run).
Exit codes are the same as the other verify skills: 0 all match, 1 a
verdict changed, 2 setup error, 3 mirrored Rust drifted.

When F1, F2 or F3 is fixed, implement the fix, check that the matching
variant's action matches the code, fold it into the normal action, and
flip the cell in `expected.txt` to PASS.

## Reading a result

A `DIFF` to FAIL prints the counterexample as model actions (map below);
`st` changes print as `new <id>=(nonce, key, parent)`, and every other
value is a state id. If every step is something the code can do, it is a
real bug: report it in code terms and keep `expected.txt` and the
properties as they are. If a step is impossible in the code, the model is
wrong: fix the action, cite the Rust, rerun. After changing the model to
match intended code, run `check.sh --accept-drift`.

## Action map

Paths are relative to `crates/server/src/`.

| Model action | Code |
|---|---|
| `ChainLand` | a transaction this server acked executes on chain |
| `ChainSwitch` | the cosigners rotate the guardian key away without pushing the delta here (offline switch), optionally after creating it as a proposal here (`push_delta_proposal`) |
| `ChainForeign` | a transaction under the other guardian, including a rotation back to this server |
| `PushRead` | `services/push_delta.rs` `push_delta` up to the ack: `resolve_account`, `ensure_account_active_metadata`, the queue reads, `verify_and_apply_delta` |
| `PushCommit` | `ack_delta`, then `storage/postgres.rs` `submit_candidate` under `lock_account_metadata` with `storage/mod.rs` `gate_candidate_submission`; the response carries the ack |
| `Promote` | `jobs/canonicalization/processor.rs` promotion through `storage/postgres.rs` `promote_candidate` (CAS on the base commitment) |
| `Retire` | `handle_diverged_candidate` (Retained), a stale base, or `max_retries` exhausted after the grace period |
| `ReconcilePromote` | `jobs/canonicalization/reconciliation.rs` `reconcile_chain_advance`: a recoverable path that ends at the chain head |
| `HookRelease`, `HookSkip` | `services/release_on_switch.rs` `release_if_guardian_switched` from `canonicalize_verified_delta`; the write may fail |
| `SweepRead` | `jobs/release_sweep/sweep.rs` `visit`: `check_stored_state` / `promoting_delta_nonce` (Delta), `visit_diverged` with `switch_candidates` and `precompute_switch` (ProposalMatch, RecoverableDelta), `fetch_on_chain_guardian_binding` with `classify_key` (ChainSweep) |
| `SweepWrite`, `SweepWriteFails` | `release_switched_account` through `metadata/postgres.rs` `set_released_if_state` |
| `CfgRead` | `services/configure_account.rs` `configure_account`: the existing metadata read and the binding validation of a state bound to this server |
| `CfgSubmit` | `storage/postgres.rs` `submit_state` (the following `metadata.set` never touches lifecycle columns) |
| `CfgClear` | `metadata/postgres.rs` `clear_released_if_state` |
| `CfgRefused` | only in the `cfgnonce` variant: the proposed refusal of an older state |
| `Pause`, `Unpause` | `services/pause_account.rs` `pause` and `services/unpause_account.rs` `unpause` through `set_pause` / `clear_pause` |

## Scope and assumptions

- One account, two concurrent pushes, one or two `/configure` calls, up
  to two pauses, 5 to 7 states, nonces up to 3 or 4. A push builds either
  a plain delta or a switch delta.
- Candidate queue depth 1, the default. A switch delta queued behind
  another candidate (issue #504) needs depth 2 and is not modeled.
- The chain read is exact: the sweep's `confirmations`, the lagging-node
  checks (`chain_behind_stored`) and `divergence_confirmations` guard
  against stale reads, which the model does not produce.
- A sweep visit's reads are one step and its write a later one. The
  reads in the code are separate statements, but the only write is
  conditional on the stored state it read, so splitting them adds
  nothing the write's condition does not already cover.
- `/configure` re-onboards with the chain head, and only when the head is
  bound to this server (`validate_guardian_commitment`). Replay floors and
  the auth checks are `verify-auth-replay`'s concern.
- This server's own ack key never changes, so `own_key_mismatch` never
  arises. EVM accounts have no guardian binding and are out of scope.
- The worker lease, the candidate queue's own invariants and the proposal
  lifecycle belong to `verify-worker-lease`, `verify-candidate-queue` and
  `verify-proposal-lifecycle`; promotion here is a single step.
- Optimistic mode (filesystem) commits through `services/delta_commit.rs`
  and is not modeled; the push-side race is the same there.

## Guards the model shows are load-bearing

Each of these breaks a property (`check.sh --mutations`):

- `m1` release written without the stored-state condition: a sweep that
  proved a switch against an older state releases the freshly
  re-onboarded account (`ReleasedIsTrue`).
- `m2` `/configure` clears the release only when its first read already
  saw one: a release written between that read and `submit_state`
  survives the re-onboarding (`ReleasedIsTrue`).
- `m3` `/configure` clears before `submit_state`: a release landing
  between the two survives (`ReleasedIsTrue`).
- `m5` unpause also clears `released_at` (`OnlyReonboardingReactivates`).
