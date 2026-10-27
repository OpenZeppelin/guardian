# Qualification suite

Automated end-to-end qualification of the assembled Guardian system: the
shipped server image against a real database over real sockets, driven by both
base clients and both multisig SDKs, with the multisig flows executing real
transactions on public Miden networks.

Start with [`docs/QUALIFICATION.md`](../docs/QUALIFICATION.md): what the suite
covers, how to set up an environment, how to run each profile, how to read a
result, and how to add a scenario.

## Layout

| Path | Contents |
|---|---|
| `manifest/` | Scenario definitions and the coverage matrix, as data. Both drivers read these; neither defines them. |
| `stack/` | Stack provisioning: compose files, the RPC stand-in, shell libraries, and `run.sh`. |
| `report/` | The run result JSON schema. |

The Rust driver lives in `crates/qualification-driver`. The TypeScript driver
lives in `packages/miden-multisig-client/tests/qualification`.

## Running

```bash
qualification/stack/run.sh --profile deterministic
```

See the quickstart for the live profile, treasury configuration, and how to
read the outcome classes.

## Negative controls

A suite that has never failed is not known to detect anything. These are the
checks that a deliberate defect produces a *named* failure rather than a pass,
an ambiguous error, or an outcome class that does not fail a run.

They are run by hand, against testnet, by editing the driver, running one
scenario, and reverting. They are not permanent code: a fault-injection switch
that ships with the driver is a switch that can be left on.

| Defect | Where | Expected verdict |
|---|---|---|
| Execution submits nothing | drop the `executeProposal` call in `tests/qualification/actions/live.ts` | `failed` / `product`, naming the unmoved account nonce |
| Signatures are claimed but never collected | in `collectSignatures`, increment the counter without calling `signProposal` | `failed` / `product`, naming the unmet threshold |
| Stored data does not survive an upgrade | drop the database between the seed and the swap in `run.sh --upgrade-from` | `failed` / `product`, naming the unreadable account |
| A proposal that does not extend the queue is admitted | in `crates/server/src/services/push_delta_proposal.rs`, disable the `tail_nonce + 1` refusal | `det-candidate-queue` and `live-candidate-queue-stranded-head-1of1-ecdsa`: `failed` / `product`, naming the accepted proposal and its nonce |
| A full queue judges a delta's base first | in `push_delta.rs` and `storage::gate_candidate_submission`, let an unknown base past the full-queue check | `det-candidate-queue`: `failed` / `product`, naming `commitment_mismatch` where `conflict_pending_delta` was due |
| The server does not queue chained candidates | run the queue server at `GUARDIAN_MAX_PENDING_CANDIDATES_PER_ACCOUNT=1` | `live-candidate-queue-chained-1of1-ecdsa`: `failed` / `product`, naming the refused second transfer |
| The TypeScript SDK executes a proposal pinned to a state its client does not hold | in `packages/miden-multisig-client/src/multisig.ts`, drop the `assertExecutesOnPinnedBase` calls | `live-candidate-queue-stranded-head-1of1-ecdsa` (TypeScript): `failed` / `product`, naming the executed proposal labelled past the head |
| A mixed-scheme account is reported under one scheme | in `crates/server/src/metadata/auth/mod.rs`, make `auth_scheme_label` return the GUARDIAN's scheme instead of `mixed` | `det-mixed-scheme-account`: `failed` / `product`, naming the reported `ecdsa` |
| Requests are verified under one scheme for every cosigner | in `Auth::verify`'s `Miden` arm, authorize only the Falcon cosigners | `det-mixed-scheme-account`: `failed` / `product`, naming the ECDSA cosigner that was not authenticated under its own scheme |
| The scheme gate checks only the GUARDIAN's scheme | in `configure_account.rs`, gate `[params.auth.guardian_scheme()]` instead of `schemes_in_use()` | `det-mixed-scheme-gate`: `failed` / `product`, naming the admitted Falcon approver; `det-scheme-gate` still passes, so only the mixed account exposes it |
| A GUARDIAN key stored under the wrong scheme is accepted | in `network/miden/mod.rs`, make `validate_guardian_commitment` accept any `guardian::scheme` | `det-mixed-scheme-account`: `failed` / `product`, naming the account whose GUARDIAN slot stores the ECDSA key under the Falcon scheme |
| Declared cosigner schemes are not checked against the state | in `configure_account.rs`, compare the declared and stored auth by commitments only | `det-mixed-scheme-account`: `failed` / `product`, naming the single-scheme ECDSA declaration it registered |
| Registration on a GUARDIAN that released the account is not retried while the switch transaction lands | set `REGISTRATION_ATTEMPTS` to 1 in `crates/miden-multisig-client/src/client/account.rs` and `packages/miden-multisig-client/src/multisig.ts` | `live-guardian-switch-back-2of3-falcon`, both SDKs: `failed` / `product`, naming the registration GUARDIAN refused because the account's latest transaction has not reached the chain |
| A delta push is signed by the wallet while a session is active | in `crates/client/src/client.rs`, send `push_delta` with `send_with_replay_retry`; in `packages/guardian-client/src/http.ts`, drop `'session'` from `pushDelta` and rebuild its `dist` | `live-session-execute-2of3-ecdsa`, both SDKs: `failed` / `product`, naming the one Guardian request the key managers (Rust) or wallets (TypeScript) signed while a session was active |

```bash
cd packages/miden-multisig-client
QUAL_PROFILE=live QUAL_NETWORK=testnet \
QUAL_SCENARIOS=live-execute-2of3-falcon \
QUAL_HTTP_ENDPOINT=http://127.0.0.1:3000 \
QUAL_GRPC_ENDPOINT=http://127.0.0.1:50051 \
QUAL_OUT_DIR=/tmp/nc QUAL_RUN_ID=nc-1 \
  npx vitest run --config vitest.qualification.config.ts
```

Run a control, confirm the named verdict, then revert and confirm the run
passes again. A control that fails in both directions proves nothing.

The candidate-queue controls were run against a local Miden node, since devnet
had moved past the pinned client when they were added, with the server built
from the branch, and each passed again once reverted.

The mixed-scheme controls edit the server rather than the driver and need no
chain: each was run as a deterministic scenario against a server built from the
branch with that one line changed (the gate control on a server started with
`GUARDIAN_ALLOWED_ACCOUNT_SCHEMES=ecdsa`), and each passed again once reverted.
Two data-loss controls back the post-restart pass: deleting the stored proposal
files between the passes fails `det-mixed-scheme-account` naming the proposal
that is gone, and running the post-restart pass against an empty database fails
it naming the account that is gone, while the same pass with `--upgrade-target`
registers the account and passes.

Two of these controls found real defects when they were first run, and both
fixes carry their reasoning at the code: `account-register` is a no-op on a
post-restart pass, in `scenario/account.rs`, because a scenario that rewrites
the data its assertion reads proves nothing; and both drivers compare the
account nonce across an execution, because without it an execution that
submitted nothing was indistinguishable from a slow network and reported
`environment_blocked`.
