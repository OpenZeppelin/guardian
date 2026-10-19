---
name: verify-multisig-push-gate
description: Model-check Guardian's multisig push gate (issue #524) with TLA+/TLC. It checks that the signature floor push_delta requires never exceeds the on-chain compute_transaction_threshold and holds the default threshold on accounts without lower overrides, and checks approval relay, sign_delta_proposal writes and the push across old and new servers and SDKs. Use when a diff touches crates/server/src/services/multisig_admission.rs, the gate call in push_delta or lookup_matching_proposal, sign_delta_proposal or update_delta_proposal, the SDK relay paths (Rust relay_collected_approvals / execute_imported_proposal, TS relayCollectedApprovals / prepareProposalExecution / executeProposal), the miden-standards pin, or when check.sh reports DRIFT.
---

# Verify the multisig push gate

Since issue #524, Guardian acknowledges a multisig delta only when the
matching pending proposal carries verified cosigner approvals over the
pushed transaction, at least as many as the lowest threshold the account
permits: its default threshold, or a lower per-procedure override
(`multisig_admission.rs` `MultisigAccount::authorize` and
`signature_floor`, called from `push_delta` before `verify_delta`).

The floor is a denial-of-service guard, not the authorization rule. The
chain enforces the exact threshold of the procedures a transaction calls,
which the summary does not show. The gate therefore never tries to match
it. It only has to never ask for more than the chain will, and to make
holding the candidate slot cost the default threshold of cosigners
wherever the account allows nothing lower.

Two models check this:

- `model/PushGateThreshold.tla` covers the count. Each state is one
  transaction (the set of account procedures it calls) together with one
  account config (default threshold and per-procedure overrides). TLC
  enumerates every pair up to a bound and compares the gate's floor with
  miden-standards `multisig.masm` `compute_transaction_threshold`.
- `model/ApprovalRelay.tla` covers getting approvals to Guardian. One
  executor relays the approvals it holds and then pushes, while other
  cosigners sign the same proposal through Guardian. TLC explores every
  interleaving, for this branch's server and SDKs and for 0.18.2.
  `sign_delta_proposal` is a read and a write, as in the code; on this
  branch the write is conditional (`update_delta_proposal_if_status`) and
  retries on a lost race, on 0.18.2 it is unconditional.

## What is checked

| Property | In plain words | Harm if broken |
|---|---|---|
| `NeverOverAsks` | Guardian never asks for more approvals than the chain will. | A transaction the chain would accept cannot be executed through Guardian. |
| `DefaultWithoutLowOverride` | On an account with no override below its default threshold, Guardian asks for the default. | Fewer cosigners than the default can hold the candidate slot with a delta the chain refuses (every other push gets `409 conflict_pending_delta` until canonicalization drops it), which is the denial of service #524 closes. |
| `ReadyExecutionAcked` | An executor holding enough approvals, from any channel, gets its push acknowledged. | A fully approved proposal cannot execute. On a switch push-back, the old Guardian keeps serving the account. |
| `AckedApprovalKept` | An approval Guardian answered 200 for stays on the stored proposal. | A cosigner is told the approval is recorded, but the gate no longer counts it. |

The threshold cells (`cells.txt`) cover a standard account (BasicWallet
plus the guarded multisig, read-only getters included) and one with an
extra component. The relay cells pick a server version, an SDK version,
where the approvals are (stored, off channel, or the proposal never
stored) and whether a cosigner signs concurrently.

## Does my change touch this?

Yes if the diff changes anything in `mirrored.txt`. `check.sh` hashes
each listed function (and the locked miden-standards version) and reports
`DRIFT` when one changes. In particular:

- **The floor**: `signature_floor` (which slots it reads, the zero filter,
  min against the default).
- **What is counted**: `authorize` (summary commitment match),
  `verified_approver_count`, `pending_signatures`, the proposal lookup in
  `push_delta`, and the tail account the gate reads
  (`ensure_tail_keeps_auth` keeps the approver set fixed behind a queued
  candidate).
- **How approvals reach the store**: `sign_delta_proposal` and its
  helpers (`load_pending_proposal`, `with_approval`, the retry loop),
  `update_delta_proposal_if_status` in each backend, and the SDK relay
  loops with the refusals they skip. A new caller that writes a pending
  proposal's approvals with plain `update_delta_proposal` brings F4 back.
- **The chain**: a miden-standards bump can change
  `compute_transaction_threshold` or `auth_tx`. Re-read them in
  `~/.cargo/registry/src/*/miden-standards-<version>/asm/standards/auth/multisig.masm`.

A change that makes the gate ask for an exact per-transaction count again
(reading the summary or `proposal_type`) is a change of design: see
"History" below for the bugs the previous attempt had.

## Run it

From the repo root:

```bash
.agents/skills/verify-multisig-push-gate/check.sh            # quick tier, about 20 s
.agents/skills/verify-multisig-push-gate/check.sh --deep     # adds threshold cells at up to 5 calls, about 45 s
.agents/skills/verify-multisig-push-gate/check.sh --mutants  # each property fails on a broken variant, about 10 s
.agents/skills/verify-multisig-push-gate/check.sh --cell relay-new-new-offchannel-late ReadyExecutionAcked
.agents/skills/verify-multisig-push-gate/check.sh --cell threshold-standard NeverOverAsks 5
```

It needs `java` and `python3`. The first run downloads `tla2tools.jar`
1.7.4 to `~/.cache/guardian-tla/` and checks its SHA-256 (the cache is
shared with the repo's other `verify-*` skills). Set `TLC_WORKERS` to cap
TLC's worker threads.

Each cell prints `ok` or `DIFF` against `expected.txt`. The exit codes
are: 0 when every cell matched and nothing drifted; 1 when a cell changed
verdict; 3 when the cells matched but mirrored code changed; 2 for a
setup problem. A FAIL names the violated invariant (`FAIL:NeverOverAsks`),
and the expected FAILs in `expected.txt` name it too. `--mutants` exits 1
if a broken variant no longer produces its stated verdict, or if a
substitution no longer matches the model (`STALE`).

## Reading a result

A threshold FAIL is a single state, printed as `state: ov = ... dflt = ...
calls = ...`. `calls` are the account procedures the transaction calls,
`ov` maps procedures to overrides (0 means none; `other_procedure` stands
for every override on a procedure the transaction does not call) and
`dflt` is the default threshold. Compute `GateRequired` (the minimum of
`dflt` and the non-zero overrides) and `ChainRequired` (the maximum over
`calls` of the override or `dflt`; `dflt` when nothing is called) by hand.

A relay FAIL is a trace of steps: `RelayRead`/`RelayWrite` are one relayed
`sign_delta_proposal` (pull, then update), `LateRead`/`LateWrite` another
cosigner's own signing, `Push` the gate's read.

Then decide which kind of FAIL it is:

- **Real bug**: the config is one an account can hold and every step is
  something the code does, in that order. Report it in code terms. Do not
  edit `expected.txt` or weaken a property to make it pass.
- **Modeling error**: a state or step the code cannot produce. Fix the
  model to match the code, cite the code in a comment, and rerun.

A cell going from PASS to FAIL is a regression or a new finding, so read
the trace before touching `expected.txt`. An expected FAIL going to PASS
means a finding was fixed (or the model lost the behavior). Check which
before recording it.

## When the change is intended

1. Map the changed code to its definition in the code map below, and
   make the definition do what the new code does. A new rule is a new
   conjunct or a new definition, not a rewrite of a neighbour.
2. Run `check.sh --deep` and `check.sh --mutants`. A property that now
   fails is a finding about the change. A mutant whose substitution no
   longer matches gets updated to break the new definition the same way.
3. If the change fixes a finding, flip its line in `expected.txt` and
   move the finding below to "fixed" with the commit.
4. Run `check.sh --accept-drift` once the models match the code. Update
   `mirrored.txt` first if you add or rename a mirrored function.

## Code map

PushGateThreshold:

| Definition | Code |
|---|---|
| `Procs` | miden-standards `asm/components/auth/guarded_multisig/guarded_multisig.masm` and `asm/components/wallets/basic_wallet/basic_wallet.masm` exports, plus `custom` for an extra component |
| `SetOverrides`, `GateRequired` | `MultisigAccount::signature_floor` (procedure-thresholds map entries, zero filtered, min with the threshold-config default) |
| `PerProc`, `ChainRequired` | `multisig.masm` `compute_transaction_threshold` (override via `get_initial_map_item`, 0 means default, max, default when nothing was called), called by `auth_tx` with `get_initial_threshold_and_num_approvers` |

ApprovalRelay:

| Definition | Code |
|---|---|
| `Init` `toRelay` | TS `relayCollectedApprovals` skips signers the served proposal holds; Rust `relay_collected_approvals` sends all and the server answers held ones `proposal_already_signed` |
| `Response` | `services/sign_delta_proposal.rs` `sign_delta_proposal` (this branch: credit to the verified key, `proposal_already_signed` when that key is held); 0.18.2 (`main` before this PR): already-signed check on the caller first, then `invalid_proposal_signature` unless the approval is the caller's |
| `RelayContinues` | the two `guardian_code()` / `GuardianHttpError.code` arms both relays skip |
| `RelayRead` / `LateRead` | `load_pending_proposal` in `sign_delta_proposal` |
| `Commits` | this branch: `update_delta_proposal_if_status` (`storage/postgres.rs` one UPDATE filtered on `status = expected`; `storage/filesystem.rs` read, compare and write under `proposal_write_lock`; the encryption decorator passes the plaintext status through); 0.18.2: `update_delta_proposal`, unconditional |
| `RelayWrite` / `LateWrite` | the committed write of `with_approval`'s row |
| `RelayRetry` / `LateRetry` | a refused conditional write, then `load_pending_proposal` again (`MAX_SIGN_ATTEMPTS` bounds it in the code; each refusal means another write committed) |
| `RelayRead` when `~Stored` | `proposal_not_found`; TS `executeProposal` switch path catches it and still pushes |
| `Push` | `push_delta` → `lookup_matching_proposal` → `MultisigAccount::authorize` against `Required` (the floor); 0.18.2 acknowledges without counting |

## Scope and assumptions

- Threshold model: 3 approvers, default and overrides over 0..3, at most
  3 (quick) or 5 (deep) distinct procedures per transaction. Overrides
  are enumerated for the called procedures plus one representative for
  all others. That is sound because the chain reads only the called ones
  and the floor only their minimum. Both properties are per-procedure
  max/min facts, so the bound loses no behaviour in practice.
- Both sides read the pre-transaction state. The chain does this through
  `get_initial_*`. The gate reads the queue tail, which is the state the
  delta executes on. Signature verification itself (Falcon, ECDSA
  recovery, dedup by key) is not modeled. It is covered by the
  `multisig_push_*` unit tests in `push_delta.rs`.
- Accepted residual, by design: a push that meets the floor but not the
  chain's requirement is acknowledged, refused on chain, and holds the
  candidate slot until canonicalization drops it. On an account with a
  low override (for example `receive_asset` at 1) that takes only that
  many cosigners. `spec/processes.md` states this.
- Relay model: one executor, at most one concurrent signer, one proposal,
  a floor of 3 on a 4-cosigner account. Proposal deletion by
  canonicalization, push retries, auth, rate limits and the candidate
  queue are out of scope (see `verify-proposal-lifecycle` and
  `verify-candidate-queue` where those exist on your branch).
- Neither model checks the Rust SDK's online `execute_proposal`. It
  executes from Guardian-served signatures, which are already stored.

## Findings in the baseline

**F4 (fixed on this branch).** `sign_delta_proposal` used to read the
proposal and write the whole row back with no lock or compare-and-set.
Counterexample with a floor of 3 on a 4-cosigner account: Guardian holds
c1. Cosigner c4 starts signing and reads `{c1}`. The executor relays c2
and c3, and both get 200, so the store holds `{c1, c2, c3}`. c4's write
then puts back `{c1, c4}`, and the push is refused with
`insufficient_signatures` (2 of 3). The fix writes with
`update_delta_proposal_if_status` and retries on the current row, so
`relay-new-new-offchannel-late` passes both relay properties; mutants M8
and M9 restore the plain write and fail them. Tests:
`test_sign_delta_proposal_keeps_a_concurrent_approval`,
`test_update_delta_proposal_if_status_refuses_a_stale_read` (filesystem),
`update_delta_proposal_if_status_admits_one_write_per_read` (Postgres,
`./scripts/test-postgres.sh`).

**Documented, not bugs.** `relay-new-old-offchannel`: 0.18.2 SDKs do not
relay, so a new server refuses their off-channel executions unless the
stored approvals alone reach the floor (`docs/MIDEN_COMPATIBILITY.md`).
`relay-new-new-unstored`: a switch the old Guardian never stored cannot
be relayed to and is refused, and the release sweep releases the account
(`docs/CONCEPTS.md`).

Guards the model shows are load-bearing (`--mutants`): honouring lower
overrides (M1), keeping the default in the minimum (M2), treating an
override of 0 as none (M3), taking the minimum rather than the maximum
(M4), the relay's `invalid_proposal_signature` skip against a 0.18.2
server when the executor's own approval is not stored (M5), relaying at
all (M6), appending rather than replacing in `sign_delta_proposal` (M7),
and the conditional write (M8, M9).

## History

The first version of the #524 gate tried to require the exact
per-transaction count by inferring called procedures from the summary
(input notes, non-fee output notes, patched slots) plus `proposal_type`.
This skill found that inference unsound in both directions: an input note
can be consumed without calling `receive_asset` (a P2ID note with no
assets), a user note with the TX_FEE script hid a send, custom components
were misclassified, calls with no trace were invisible, and the label
union refused chain-valid transactions (for example `custom`-labelled
proposals on accounts with low overrides). The floor replaced it.
