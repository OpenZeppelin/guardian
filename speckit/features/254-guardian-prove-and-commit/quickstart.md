# Quickstart: Guardian Prove and Commit

**Last Revised**: 2026-09-30 (spec revision 11: Miden 0.17 release-candidate pins on `main` and
devnet; tip execution, two expiration bounds, funded accounts)

Walks the happy path (a cosigner hands Guardian a signed proposal, Guardian
proves and submits it) plus the refusals an operator will actually hit.

Requests use the existing per-account auth scheme (`x-pubkey`, `x-signature`,
`x-timestamp`); no new mechanism (FR-002).

## 0. Prerequisites

- `guardian-server` built with the `proving` feature. Without it the capability
  is unavailable and every execution request is refused; there is no fallback
  to local proving (FR-021).
- **A reachable remote prover.** Guardian never proves locally. Set
  `GUARDIAN_TX_PROVER_URL` to `{protocol}://{host}:{port}`. Operators can run
  their own, matching the node's protocol line.
- **Canonicalization enabled.** Guardian execution requires it: FR-040 depends
  on it entirely to establish whether a submitted transaction committed. Optimistic
  delta-commit mode is refused at startup (FR-043).
- Run the migration `2026-07-28-000001_execution_reservations` before starting
  the server.
- A multisig account registered with this Guardian, and a proposal created by a
  **Guardian-executable-configured client** (see step 1).
- **A fee-asset balance on the account.** On Miden 0.17 every transaction pays a
  fee in the native fee asset, including the account's first one. An unfunded
  account fails reproduction with `GUARDIAN_EXECUTION_INSUFFICIENT_FEE`.

### Which network

This feature targets the Miden 0.17 release candidates on `main` (protocol / standards / tx
`0.17.0-rc.7`, `miden-client` and web SDK `0.17.0-rc.4`). Production is gated on stable 0.17
and the re-pin.

- **Devnet** runs node 0.17.0-rc.2 and matches this line (`docs/MIDEN_COMPATIBILITY.md`, open
  upstream items). It has no faucet front end: fund an account by calling the node's
  `RegisterAccount` RPC with `scripts/devnet-register-account.sh <account-id>`, which pays it a
  small public P2ID note in the fee asset. Sync until the note shows up; the account's first
  transaction consumes it. Devnet's fee faucet enables asset callbacks, so every fee payment
  loads it as a foreign account, which Guardian supports (FR-050).
- **Testnet** still runs Miden 0.16 and cannot execute 0.17 transactions. Use a local
  `miden-node` from the pinned line instead.

### Set the prover timeout

```bash
GUARDIAN_TX_PROVER_URL=https://tx-prover.devnet.miden.io
GUARDIAN_TX_PROVER_TIMEOUT_SECS=300
GUARDIAN_EXECUTION_EXPIRATION_HORIZON_BLOCKS=512
```

The prover client library's own default is still **10 seconds** on 0.17
(`miden-client-0.17.0-rc.4/src/remote_prover/tx_prover.rs:43`). Proving times observed on
0.16 were 6.2 to 20.1 s; they are historical and are being re-measured on 0.17. Leaving the
default in place produces intermittent `failed to prove transaction` errors that
never mention a timeout. Guardian sets an explicit default well above 10 s
(FR-020); set this variable if your prover is slower.

The horizon bounds `proven expiration − R`, `R` being the block an attempt executes against
(FR-046). Its default must be at least 256, the built-in transaction expiration, so built-in
proposals always pass.

## 1. Create a Guardian-executable proposal

Execution mode is **client-level configuration**, not a per-call argument, and
defaults to **off**; no new SDK methods (FR-009).

```ts
const client = new MultisigClient(midenClient, {
  guardianEndpoint,
  midenRpcEndpoint,
  executionMode: "guardian_executable",   // omit for "self_executed"
});

const multisig = await client.load(accountId, signer);
await multisig.createP2idProposal(/* unchanged signature; optional approvalExpirationDelta */);
```

Omitting `executionMode` behaves as `self_executed`, so an SDK upgrade alone never changes
what data leaves the integration.

Consequences:

- The proposal carries a `transaction_request` envelope (protocol line `0.17`, serializer id
  the serializing `miden-client` version, e.g. `0.17.0-rc.4`). Proposals created without this
  mode do **not**, and Guardian cannot execute them: it refuses with
  `GUARDIAN_PROPOSAL_MISSING_TRANSACTION_REQUEST` rather than trying to rebuild the transaction
  (FR-013).
- The proposal carries **two signed expiration bounds** (FR-051):
  - an **approval expiration**, the signing window: by default
    `GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA` = 28,800 blocks after the bound block (about
    24 h on devnet), overridable per proposal with 1..65,535 through `approvalExpirationDelta` /
    `ProposalOptions.approval_expiration_delta`. Once it passes, the proposal can no longer be
    executed by anyone;
  - for built-in families, a **256-block transaction expiration**, counted from the block the
    execution runs against. It bounds how long an unknown-outcome submission can hold the
    account.
  Both are in the signed summary, so a Guardian-executable proposal has a different id than the
  same transfer created by a self-executing client, and cosigners sign both bounds along with
  the effects.
- An opaque custom-producer request is preserved unchanged. The producer builds its auth args
  through `MultisigClient::multisig_auth_args` with a non-zero approval delta and may set a
  transaction delta in its script. Without a script delta its transaction expires at the
  approval bound, and if that lies beyond the deployment's horizon Guardian refuses it with
  `GUARDIAN_EXECUTION_EXPIRATION_BEYOND_HORIZON` before the boundary. Nothing waits.

Creation stores the client-derived summary and attached request; it does not execute the
request. Guardian verifies their equivalence after an explicit execution request is
accepted. If creation already includes enough valid signatures, proceed directly to step 3.
The Guardian acknowledgment does not replace a missing cosigner signature. V1 has no
server preparation endpoint or automatic execution policy.

## 2. Collect signatures as usual

No change. Sign until the effective per-procedure threshold is met (FR-005).
Invalid, duplicate, and non-cosigner entries are **ignored**, not fatal
(FR-006); the count is reported back as `ignored_signatures` for diagnosis.

Signature collection may take as long as the approval window allows. Cosigners and Guardian
all verify and execute at the chain tip under the signed bound block, so a proposal does not go
stale while it waits (FR-056).

## 3. Ask Guardian to execute

```text
POST /delta/proposal/execution
{ "account_id": "0x…", "proposal_id": "0x…" }
```

`202 Accepted` with `newly_accepted: true`:

```jsonc
{
  "account_id": "0x…",
  "proposal_id": "0x…",
  "state": "pending",
  "newly_accepted": true,
  "proposal_exists": true,
  "ignored_signatures": 0,
  "updated_at": "2026-09-30T10:00:00Z"
}
```

Calling again while it is in flight is **idempotent**: `200 OK` with
`newly_accepted: false`. gRPC has no 202, so `newly_accepted` (not the
transport status) is the contract.

## 4. Poll

```text
GET /delta/proposal/execution?account_id=0x…&proposal_id=0x…
```

Five states, exhaustive (FR-024):

```text
pending    → proving | failed
proving    → submitted | failed
submitted  → committed | failed
```

`proving` is the multi-minute phase. `committed` and `failed` are terminal.

To find what an account is doing without polling every proposal:

```text
GET /delta/execution/current?account_id=0x…
```

Nothing in flight is a **success** with `{"execution": null}`, not a `404`. The
account exists and the query succeeded (FR-036).

## 5. Read `submitted` correctly

`submitted` means the no-retry boundary has been crossed (FR-047). The
transaction is on chain, or may be and the outcome is not yet established.

**Do not retry.** The caller contract is identical either way: wait, watch the
delta. A candidate delta always exists from this state onward, because it is
admitted atomically with the submission evidence (FR-045 step 12).

Guardian resolves it through one of three observations (FR-040): the candidate reached
`canonical` (`committed`), the account moved somewhere else (`failed`, superseded),
or the chain passed the recorded expiration block with the account still at base
(`failed`, expired). The third is why FR-046's finite-expiration rule exists;
without it this path could never fire. With the built-in 256-block delta it fires within
about 13 minutes on devnet.

Expiration is a chain-height bound, not a wall-clock deadline. If the configured Miden node is
unavailable, the execution remains `submitted`, its reservation stays held, and reconciliation
retries with capped backoff while health and metrics report the outage. Restore or fail over the
RPC source; do not release the reservation or retry the transaction manually. Resolution resumes
once Guardian can obtain trustworthy chain observations.

## 6. Retry only when told

`failed` does **not** imply retryable. Read `proposal_exists` (FR-042):

| `state` | `proposal_exists` | Retry |
|---|---|---|
| `pending`, `proving` | `true` | no, already in flight |
| `submitted` | `true` | **no**, forbidden |
| `committed` | `false` | no, succeeded; proposal deleted on promotion |
| `failed`, pre-boundary | `true` | **yes** |
| `failed`, post-boundary discarded | `false` | no, create a **new** proposal |

A post-submission failure may have had its proposal deleted along with its
candidate, because canonicalization deletes both. That is reported as a fact,
never as retry advice.

## 7. Refusals you will hit

Synchronous, none creating a reservation (FR-022):

| Code | Meaning |
|---|---|
| `GUARDIAN_PROVING_UNAVAILABLE` | No prover configured, capability off, or optimistic mode |
| `GUARDIAN_PROPOSAL_MISSING_TRANSACTION_REQUEST` | Proposal was created without execution mode, see step 1 |
| `GUARDIAN_PROPOSAL_NOT_READY` | Below the effective threshold of **valid** signatures |
| `GUARDIAN_EXECUTION_CONFLICT` | Another execution holds the account; `meta.blocking_proposal_id` names it |
| `GUARDIAN_CONFLICT_PENDING_DELTA` | Account already holds a pending candidate |

Asynchronous and pre-boundary, reported as `state: "failed"` with the proposal kept, so a retry
is permitted (full list in `contracts/execution-api.md`):

| Code | Meaning |
|---|---|
| `GUARDIAN_EXECUTION_REQUEST_INVALID` | The stored request is not Guardian-executable; `meta.reason` names why (`bound_block_not_declared`, `auth_args_missing`, `approval_expiration_missing`, `input_notes_not_pinned`). Re-create the proposal with a current SDK |
| `GUARDIAN_EXECUTION_EXPIRATION_REACHED` | `meta.bound` = `approval` (the signing window passed; create a new proposal) or `transaction` (the executed transaction's expiration was reached before proving) |
| `GUARDIAN_EXECUTION_CHAIN_BEHIND` | Guardian's node has not reached the proposal's bound block yet; retry shortly |
| `GUARDIAN_EXECUTION_FOREIGN_ACCOUNT_UNAVAILABLE` | A foreign account is private (`meta.reason` = `private`) or its state is not servable at the tip (`unavailable`) |
| `GUARDIAN_EXECUTION_INSUFFICIENT_FEE` | The account cannot pay the fee; fund it (on devnet, `scripts/devnet-register-account.sh`) and retry |
| `GUARDIAN_EXECUTION_BINDING_MISMATCH` | The reproduced summary differs from the signed one. At the tip this can be fee drift (the base fee changed since signing); a fresh proposal fixes it. Server logs compare the fee notes to tell drift from tampering |
| `GUARDIAN_EXECUTION_EXPIRATION_BEYOND_HORIZON` | The proven expiration lies beyond the configured horizon |

While an execution reservation is active, `POST /delta` (`push_delta`) is also
refused with `GUARDIAN_EXECUTION_CONFLICT` (FR-027): a client cannot submit a
competing transaction while Guardian is mid-proof.

## 8. Self-execution still works

Nothing above is mandatory. Clients that build, prove, and submit for themselves
are fully supported and unchanged (FR-035); execution mode is off by default.
Guardian execution is an added capability, not a migration.

## Verifying the proving path without an account

The proving architecture is validated independently and needs **no funded
account**; chain reads are read-only queries:

```bash
cargo test -p guardian-server --features proving --lib live_ -- --ignored --nocapture
```

This assembles a `PartialBlockchain` at the tip from live devnet RPC (`SyncChainMmr` peaks plus
`GetBlockHeaderByNumber` MMR proofs for the tracked blocks, FR-061), executes a locally-built
multisig transaction against it, and proves it remotely. The spike these tests come from
(`254-execution-spike`) is still on the 0.16 release candidates and must be ported to the 0.17
pins before its results apply. Only *submission* needs a funded, Guardian-registered account.
See [validation-matrix.md](./validation-matrix.md).
