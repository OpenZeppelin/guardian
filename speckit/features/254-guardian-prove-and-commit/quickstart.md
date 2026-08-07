# Quickstart: Guardian Prove and Commit

Walks the happy path — a cosigner hands Guardian a signed proposal, Guardian
proves and submits it — plus the refusals an operator will actually hit.

Requests use the existing per-account auth scheme (`x-pubkey`, `x-signature`,
`x-timestamp`); no new mechanism (FR-002).

## 0. Prerequisites

- `guardian-server` built with the `proving` feature. Without it the capability
  is unavailable and every execution request is refused — there is no fallback
  to local proving (FR-021).
- **A reachable remote prover.** Guardian never proves locally. Set
  `GUARDIAN_TX_PROVER_URL` to `{protocol}://{host}:{port}`. Operators can run
  their own; the public testnet prover works for trials.
- **Canonicalization enabled.** Guardian execution requires it — FR-040 depends
  on it entirely to establish whether a submitted transaction landed. Optimistic
  delta-commit mode is refused at startup (FR-043).
- Run the migration `2026-07-28-000001_execution_reservations` before starting
  the server.
- A multisig account registered with this Guardian, and a proposal created by a
  **Guardian-executable-configured client** (see step 1).

### Set the prover timeout

```bash
GUARDIAN_TX_PROVER_URL=https://tx-prover.testnet.miden.io
GUARDIAN_TX_PROVER_TIMEOUT_SECS=300
```

The prover client library's own default is **10 seconds**
(`tx_prover.rs:45`), and observed proving times are 6.2–20.1s. Leaving the
default in place produces intermittent `failed to prove transaction` errors that
never mention a timeout. Guardian sets an explicit default well above 10s
(FR-020); set this variable if your prover is slower.

## 1. Create a Guardian-executable proposal

Execution mode is **client-level configuration**, not a per-call argument, and
defaults to **off** — no new SDK methods (FR-009).

```ts
const client = new MultisigClient(midenClient, {
  guardianEndpoint,
  midenRpcEndpoint,
  executionMode: "guardian_executable",   // omit for "self_executed"
});

const multisig = await client.load(accountId, signer);
await multisig.createP2idProposal(/* unchanged signature */);
```

Omitting `executionMode` behaves as `self_executed`, so an SDK upgrade alone never changes
what data leaves the integration.

Two consequences:

- The proposal carries a `transaction_request` envelope. Proposals created
  without this mode do **not**, and Guardian cannot execute them — it refuses
  with `GUARDIAN_PROPOSAL_MISSING_TRANSACTION_REQUEST` rather than trying to
  rebuild the transaction (FR-013).
- For built-in proposal families, the SDK applies the shared 256-block finite
  expiration internally (FR-051). For an opaque custom-producer request, the producer's
  script must set a finite expiration; the SDK preserves those bytes unchanged. A transaction
  whose resulting expiration is unbounded can never be Guardian-executed: the default is
  `u32::MAX` (never expires), and FR-046 refuses it because an unbounded execution would leave
  reconciliation unable to terminate. Neither SDK nor a custom producer needs to match a
  deployment's private horizon. If a finite value falls outside that horizon, the server refuses
  it before the boundary.

## 2. Collect signatures as usual

No change. Sign until the effective per-procedure threshold is met (FR-005).
Invalid, duplicate, and non-cosigner entries are **ignored**, not fatal
(FR-006) — the count is reported back as `ignored_signatures` for diagnosis.

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
  "updated_at": "2026-07-28T10:00:00Z"
}
```

Calling again while it is in flight is **idempotent**: `200 OK` with
`newly_accepted: false`. gRPC has no 202, so `newly_accepted` — not the
transport status — is the contract.

## 4. Poll

```text
GET /delta/proposal/execution?account_id=0x…&proposal_id=0x…
```

Five states, exhaustive (FR-024):

```text
pending    → proving | failed
proving    → submitted | failed
submitted  → landed | failed
```

`proving` is the multi-minute phase. `landed` and `failed` are terminal.

To find what an account is doing without polling every proposal:

```text
GET /delta/execution/current?account_id=0x…
```

Nothing in flight is a **success** with `{"execution": null}` — not a `404`. The
account exists and the query succeeded (FR-036).

## 5. Read `submitted` correctly

`submitted` means the no-retry boundary has been crossed (FR-047). The
transaction is on chain, or may be and the outcome is not yet established.

**Do not retry.** The caller contract is identical either way: wait, watch the
delta. A candidate delta always exists from this state onward, because it is
admitted atomically with the submission evidence (FR-045 step 9).

Guardian resolves it without any transaction-status lookup — Miden exposes
none — via one of three observations (FR-040): the candidate reached
`canonical` (`landed`), the account moved somewhere else (`failed`, superseded),
or the chain passed the recorded expiration block with the account still at base
(`failed`, expired). The third is why FR-046's finite-expiration rule exists;
without it this path could never fire.

Expiration is a chain-height bound, not a wall-clock deadline. If the configured Miden node is
unavailable, the execution remains `submitted`, its reservation stays held, and reconciliation
retries with capped backoff while health and metrics report the outage. Restore or fail over the
RPC source; do not release the reservation or retry the transaction manually. Resolution resumes
once Guardian can obtain trustworthy chain observations.

## 6. Retry only when told

`failed` does **not** imply retryable. Read `proposal_exists` (FR-042):

| `state` | `proposal_exists` | Retry |
|---|---|---|
| `pending`, `proving` | `true` | no — already in flight |
| `submitted` | `true` | **no** — forbidden |
| `landed` | `false` | no — succeeded; proposal deleted on promotion |
| `failed`, pre-boundary | `true` | **yes** |
| `failed`, post-boundary discarded | `false` | no — create a **new** proposal |

A post-submission failure may have had its proposal deleted along with its
candidate, because canonicalization deletes both. That is reported as a fact,
never as retry advice.

## 7. Refusals you will hit

All synchronous, none creating a reservation (FR-022):

| Code | Meaning |
|---|---|
| `GUARDIAN_PROVING_UNAVAILABLE` | No prover configured, capability off, or optimistic mode |
| `GUARDIAN_PROPOSAL_MISSING_TRANSACTION_REQUEST` | Proposal was created without execution mode — step 1 |
| `GUARDIAN_PROPOSAL_NOT_READY` | Below the effective threshold of **valid** signatures |
| `GUARDIAN_EXECUTION_CONFLICT` | Another execution holds the account; `meta.blocking_proposal_id` names it |
| `GUARDIAN_CONFLICT_PENDING_DELTA` | Account already holds a pending candidate |

While an execution reservation is active, `POST /delta` (`push_delta`) is also
refused with `GUARDIAN_EXECUTION_CONFLICT` (FR-027) — a client cannot submit a
competing transaction while Guardian is mid-proof.

## 8. Self-execution still works

Nothing above is mandatory. Clients that build, prove, and submit for themselves
are fully supported and unchanged (FR-035); execution mode is off by default.
Guardian execution is an added capability, not a migration.

## Verifying the proving path without an account

The proving architecture is validated independently and needs **no funded
account** — chain reads are read-only queries:

```bash
cargo test -p guardian-server --features proving --lib live_ -- --ignored --nocapture
```

This assembles a `PartialBlockchain` from live testnet RPC, executes a
locally-built multisig transaction against it, and proves it remotely. Only
*submission* needs a funded, Guardian-registered account. See
[validation-matrix.md](./validation-matrix.md).
