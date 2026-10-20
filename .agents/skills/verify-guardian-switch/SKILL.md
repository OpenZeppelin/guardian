---
name: verify-guardian-switch
description: Model-check the account status lifecycle around a guardian switch (issues #305, #434) and an operator pause (#181) with TLA+/TLC. It checks that a release is always true of the state the server holds, that only /configure re-onboarding reactivates a released account, that a switch the server can see is released for good every time, and that no ack returned after a pause or release executes on chain. Use when a diff touches services/release_on_switch.rs, services/account_status.rs, the pause/unpause services, set_released_if_state / clear_released_if_state / set_pause / clear_pause, configure_account's stored-state checks, replace_state and release clear, the status read in submit_candidate or the optimistic commit, jobs/release_sweep/, the release hook in canonicalization or delta_commit, the status check in push_delta or push_delta_proposal, or when check.sh reports DRIFT.
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
| `PausedAckNeverExecutes` | An ack returned after the account was paused never executes on chain. | The operator's pause, the safety lever, does not stop the next transaction. Held only after the F1 fix. |
| `NoAckWhileInactive` | No ack is returned at all while paused or released. | Stricter form of the two above. |
| `SwitchIsReleased` | When the account has left for good and the server can see the switch (public storage, or every switch went through a delta or proposal held here), it is eventually released and stays released. The consequent is `<>[]released`, so one release earlier in the run (before a re-onboarding and a second switch) does not satisfy it. | Stale reads and dead proposals for a switched account. Held for private accounts only after the F2 and F3 fixes. |

## Findings

**F1 (medium, fixed): a pause did not fence a `push_delta` already in
flight.** `push_delta` checks the status on the metadata loaded at the
start of the request (`services/push_delta.rs`,
`ensure_account_active_metadata`), then reconstructs, applies, signs the
ack and submits the candidate; `submit_candidate` never read
`paused_at`. Trace (mutation `m10`):

```
PushRead    push passes the status check
Pause       operator pauses; the pause call returns
PushCommit  candidate admitted, ack returned to the client
ChainLand   the transaction executes on chain
```

`storage/postgres.rs` `submit_candidate` now reads `paused_at`,
`paused_reason` and `released_at` in the `FOR UPDATE` that locks the
account row, the row `set_pause` and `set_released_if_state` write, and
refuses with `CandidateSubmission::Paused` / `Released` (mapped to
`GUARDIAN_ACCOUNT_PAUSED` / `GUARDIAN_ACCOUNT_RELEASED` in
`services/delta_commit.rs`). The filesystem backend and optimistic mode
re-read the status right before the write, without a lock shared with
the pause, so there the window narrows but stays open; the model covers
Postgres candidate mode only. `push_delta_proposal` and
`sign_delta_proposal` keep the check-then-act shape: they store
proposals and signatures, not acks, so a late write there executes
nothing on its own.

**F2 (low, fixed): `/configure` could roll the stored state back, and a
private account then never released.** `submit_state` was an
unconditional upsert, and the request carries the state, so any older
state still bound to this server could replace a promoted switch; the
hook's release then refused (StateMoved) and nothing could prove the
switch again. Two checks close it:

- Stale state (mutation `m9`): when the submitted nonce is below the
  stored one, `configure_account` asks the chain whether it reached the
  stored state (`stored_state_landed`: at the head, or a committed
  transaction ended at it) and refuses with `400 commitment_mismatch` if
  so; an unreadable chain refuses with `rpc_unavailable` (fail closed). A
  stored state that never landed (optimistic mode, ahead of the chain)
  can still be replaced by the older chain state. A stored row without a
  nonce (written before the server stored nonces) has it decoded from the
  stored state (`stored_head`), with the commitment from the same read;
  only a stored state that does not decode skips the check. The replay
  floor (`consume_auth_timestamp`) runs first, so a replayed stale
  request is refused before any chain read.
- Race (mutation `m6` drops both): the write goes through
  `StorageBackend::replace_state`, only while the stored commitment read
  at the start is unchanged (Postgres: one conditional UPDATE; filesystem:
  under `delta_write_lock`).

A stale state at the *same* nonce as a lagging stored state is not
refused: it can clear a release until the sweep proves the switch again
(`ReleasedAckNeverExecutes` shows acks meanwhile cannot execute).

**F3 (low, fixed; also issue #504): the sweep matched candidates only
against the stored state.** An own delta retained after its retries, then
landing late, followed by a switch proposal built on it (recorded against
the stored state: a summary does not name its base) or a switch delta
parked behind it: applied to the stored state neither reproduces the
switch's post-state, and reconcile promotes only a path ending at the
chain head. `switch_candidates` now builds every base
(`candidate_bases`: the stored state plus the replayed end state of each
retained or abandoned delta chaining from it, each matching the
commitment it was admitted with), applies proposals to every base and
deltas to their own. Mutation `m7` restores the one-hop match.

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
.agents/skills/verify-guardian-switch/check.sh --cell m10
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

Every finding is fixed and folded into the normal actions; its mutation
keeps the check failing if the code slides back. A new finding goes in
the same way: an expected FAIL until the fix lands, then a mutation.

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
| `PushCommit` | `ack_delta`, then `storage/postgres.rs` `submit_candidate`: the status read under the account row lock (`CandidateSubmission::refused_while_inactive`), then `storage/mod.rs` `gate_candidate_submission`; the response carries the ack |
| `Promote` | `jobs/canonicalization/processor.rs` promotion through `storage/postgres.rs` `promote_candidate` (CAS on the base commitment) |
| `Retire` | `handle_diverged_candidate` (Retained), a stale base, or `max_retries` exhausted after the grace period |
| `ReconcilePromote` | `jobs/canonicalization/reconciliation.rs` `reconcile_chain_advance`: a recoverable path that ends at the chain head |
| `HookRelease`, `HookSkip` | `services/release_on_switch.rs` `release_if_guardian_switched` from `canonicalize_verified_delta`; the write may fail |
| `SweepRead` | `jobs/release_sweep/sweep.rs` `visit`: `check_stored_state` / `promoting_delta_nonce` (Delta), `visit_diverged` with `switch_candidates` and `precompute_switch` (ProposalMatch, RecoverableDelta), `fetch_on_chain_guardian_binding` with `classify_key` (ChainSweep) |
| `SweepWrite`, `SweepWriteFails` | `release_switched_account` through `metadata/postgres.rs` `set_released_if_state` |
| `CfgRead` | `services/configure_account.rs` `configure_account`: the existing metadata read, the stored head read (`stored_base`) and the binding validation of the submitted state, any state the chain held that binds the account to this server |
| `CfgSubmit` | `storage/postgres.rs` `replace_state` over the commitment read (the following `metadata.set` never touches lifecycle columns) |
| `CfgRefused` | `stored_state_landed` refuses an older state, or `replace_state` returns `StateMoved`: `400 commitment_mismatch`, nothing written |
| `CfgClear` | `metadata/postgres.rs` `clear_released_if_state` |
| `Pause`, `Unpause` | `services/pause_account.rs` `pause` and `services/unpause_account.rs` `unpause` through `set_pause` / `clear_pause` |

## Scope and assumptions

- One account, two concurrent pushes, one or two `/configure` calls, up
  to two pauses, 5 to 7 states, nonces up to 3 or 4. A push builds either
  a plain delta or a switch delta.
- Candidate queue depth 1, the default. A switch delta queued behind
  another candidate (issue #504) needs depth 2 and is not modeled here;
  `a_switch_delta_parked_behind_a_retained_delta_is_evidence` in
  `jobs/release_sweep/sweep.rs` covers it.
- The chain read is exact: the sweep's `confirmations`, the lagging-node
  checks (`chain_behind_stored`) and `divergence_confirmations` guard
  against stale reads, which the model does not produce.
- A sweep visit's reads are one step and its write a later one. The
  reads in the code are separate statements, but the only write is
  conditional on the stored state it read, so splitting them adds
  nothing the write's condition does not already cover.
- `/configure` submits any state the chain has held that binds the
  account to this server (`validate_guardian_commitment`), the head or a
  stale one; a state never on chain is not modeled. Replay floors and
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
- `m6` `/configure` writes its state unconditionally: it can roll the
  stored state back over a promoted switch, and a private account never
  releases (`SwitchIsReleased`).
- `m7` the sweep matches candidates only against the stored state: a
  switch built past a late-landing retained delta is never released on a
  private account (`SwitchIsReleased`).
- `m8` no release once a `/configure` re-onboarded a released account:
  the second switch away is never released (`SwitchIsReleased`; the
  earlier `<>released` consequent passed this).
- `m9` `/configure` accepts an older state over a landed stored state: a
  stale wallet rolls back a promoted switch (`SwitchIsReleased`).
- `m10` `submit_candidate` does not read the status under the lock: an
  ack returned after a pause executes (`PausedAckNeverExecutes`).
