# Implementation Plan: Guardian Proves and Commits Transactions

**Feature Key**: `254-guardian-prove-and-commit` | **Date**: 2026-07-28 | **Spec**: [spec.md](./spec.md)
**Branch**: `254-guardian-prove-and-commit`
**Last Revised**: 2026-09-30 (re-verified against the Miden 0.17 release-candidate pins on `main`
at `34920008`: protocol / standards / tx `0.17.0-rc.7`, `miden-client` `0.17.0-rc.4`,
`miden-node-proto-build` `0.17.0-rc.3`, web SDK `0.17.0-rc.4`. Anchored reproduction is replaced
by tip reproduction under the signed bound block (FR-056, FR-061), foreign public accounts are in
scope (FR-050), expiration has two bounds (FR-051), fee info travels in the request's auth arg
(FR-057), submission inputs are sealed before the boundary (FR-059), and FR-045 is now fourteen
steps with the boundary at step 12 and the send at step 14. The 0.16 anchored design is withdrawn,
see RFC 0001 Appendix A.3)

## Summary

Let a client hand Guardian a fully-signed proposal and have Guardian execute,
prove, and submit the transaction on the client's behalf, so a cosigner needs
no Miden dependency and no chain access to move an account forward.

**The proving architecture is ratified and is not what this plan builds.** The
Gate 0 spike produced a working `DataStore` over Guardian's own state
(`crates/server/src/network/miden/execution/` on the [`254-execution-spike`](https://github.com/OpenZeppelin/guardian/tree/254-execution-spike) branch, commit `769e2a90`; not on `main`), assembled a `PartialBlockchain`
from node RPC alone, and proved a witness through a remote prover against public
testnet, with **no new dependencies**. That spike ran on the 0.16 release candidates (protocol
rc.9, client rc.4) and is 34 commits behind `main`, so none of its tests mean anything on 0.17
until its pieces are ported forward (Workstream D0).

**Miden 0.17 changes what workstream D feeds the seam, and in the spike's favour.** The signed
`TransactionSummary` now carries a `block_number`, the bound block, and binds that block's
commitment (`miden-protocol-0.17.0-rc.7/src/transaction/tx_summary.rs:23-39`); the kernel checks
it through the partial blockchain MMR and only requires `bound_block <= ref_block`
(`miden-protocol-0.17.0-rc.7/asm/kernels/transaction-core/src/tx.masm:98-130`). Executing at the proposal's anchor is
therefore no longer required, and on devnet it is no longer viable either: the fee faucet loads
as a foreign account at the reference block and devnet prunes account state after about 50
blocks (#462). Both SDKs on `main` already verify, sign and execute at the tip (#498). So:

- The reference block `R` is the node's committed tip at attempt start, and the partial
  blockchain at forest `R` tracks the bound block and every authenticated note block (FR-061,
  FR-056). The spike's tip-reference `ChainView` (`SyncChainMmr` peaks plus
  `GetBlockHeaderByNumber(include_mmr_proof)`) is normative again.
- The `ProtocolConfig` the executor now requires is fetched and checked against the reference
  header's commitment (FR-060).
- Foreign **public** accounts (fee faucet callbacks, FEE_SPONSORSHIP pricing) are loaded lazily
  at `R` (FR-050).
- Fee conversion info is already committed in the request's three-word multisig auth arg;
  Guardian passes it through and never derives it (FR-057).
- Expiration has two signed bounds, the approval expiration and a 256-block transaction delta
  (FR-051), and a reached expiration stops the attempt before the boundary (FR-058).
- Submission requires `TransactionInputs` sealed against the validator key, prepared before the
  boundary (FR-059).

See [research.md](./research.md) and
[RFC 0001](../../../docs/rfcs/0001-server-side-transaction-execution.md) revision 17.

**Gate 0 is narrowed, not fully passed, and the residue is named.** P2ID and
configuration executed and proved under `MockChain`; `consume_notes` executed
from a prepared store, with snapshot-pinned live-RPC note-block assembly
validated independently through `SyncNotes` but the joined live flow pending;
the custom family (#266) is unrun; live **submission** is unvalidated. None of that can
falsify the architecture (same `DataStore`, same witness assembly), so it does
not block lifecycle implementation. This plan does **not** claim all four
families are validated; deferred coverage is tracked in
[validation-matrix.md](./validation-matrix.md). All Gate 0 evidence is 0.16 evidence (proof
format, VM, request size and proving timings are historical); it must be re-run on the ported
seam at the 0.17 pins, and the 0.17 additions (foreign-account loading, `ProtocolConfig`,
sealing, bound-block tracking) have no Gate 0 evidence at all yet.

What remains is **lifecycle machinery** plus the 0.17 execution-input work: the durable
reservation that makes an execution a single-owner, crash-safe, non-retryable operation. Nearly
all of the specified risk lives in the lifecycle, not in proving. The design centre is FR-045's
fourteen-step sequence and its **no-retry boundary** (FR-047, step 12), one atomic commit
that admits the candidate and persists submission evidence together, after
which no failure path may ever retry, only reconcile.

**This plan builds no new concurrency mechanism.** Guardian already has the
exact primitive FR-037 requires: a per-account row lock plus fence-validated
conditional write, committed as one transaction. Reservations extend that
pattern.

## Review decisions and implementation scope

V1 retains client preparation and an explicit execution trigger. Admission stores the
summary and request without execution; FR-007 verifies reproduction after acceptance.
The Guardian acknowledgment remains separate from the cosigner threshold. Resolve the
upstream wallet's 2-of-3 mapping without assuming a change to account authorization.

Implement FR-016 per-proposer quotas in proposal admission on both backends and test
concurrent inserts and capacity for another signer. Before implementation, settle the
count configuration and allocation rule; two per proposer is a proposed default, not a
final config contract. Use `committed` consistently for terminal execution success while
keeping the delta status `canonical` and existing error codes unchanged.

Preparation, automatic execution, dependent chains, independent proposal revalidation,
and batching remain future work. No v1 implementation tasks are added for those APIs.

## Technical Context

- **Language / runtime**: Rust 2024 edition (server + clients), TypeScript
  (base + multisig clients).
- **Server**: `crates/server`, axum HTTP + tonic gRPC, Diesel-backed Postgres
  plus the filesystem backend in `src/storage/filesystem.rs`.
- **Miden pins**: the 0.17 release candidates on `main`: `miden-protocol` / `miden-standards` /
  `miden-tx` `0.17.0-rc.7`, `miden-client` `0.17.0-rc.4`, `miden-node-proto-build`
  `0.17.0-rc.3`, web SDK `0.17.0-rc.4`. Implementation targets these pins and devnet (testnet
  still runs 0.16). **Production is gated on stable 0.17** plus the re-pin (procedure roots,
  fixtures, determinism vectors).
- **Proving**: `crates/server/src/network/miden/execution/`, always compiled since 2026-10-01
  (`miden-tx`; the `proving` Cargo feature was dropped, and execution is off at run time until a
  prover is configured); `e2e` uses the remote prover client exposed by
  `miden-client` `0.17.0-rc.4`, whose default timeout is still 10 s
  (`miden-client-0.17.0-rc.4/src/remote_prover/tx_prover.rs:43`). Production proving remains
  remote. The implementation branch starts from `main` and **ports** the spike's `blockchain.rs`,
  `store.rs` and the RPC client's `sync_chain_mmr` from `254-execution-spike` (`769e2a90`,
  0.16 rc.9) forward; it does not start from the stale spike branch.
- **Execution seam (0.17)**: `DataStore` keeps five methods plus `MastForestStore`; the only
  change is the added `ProtocolConfig` return from `get_transaction_inputs`
  (`miden-tx-0.17.0-rc.7/src/executor/data_store.rs:19-96`). `execute_transaction(account_id,
  block_ref, notes, tx_args)` takes the reference block explicitly
  (`miden-tx-0.17.0-rc.7/src/executor/mod.rs:189-194`). `ClientDataStore` is still `pub(crate)`
  (`miden-client-0.17.0-rc.4/src/store/mod.rs:65-71`), so Guardian keeps its own `DataStore`.
- **Node RPC gaps on `main`**: `miden-rpc-client` hardcodes `include_protocol_config: None` on
  `get_block_header` and `get_chain_tip` (`crates/miden-rpc-client/src/lib.rs:302,492`),
  `get_account_with_details` hardcodes `block_num: None` (`lib.rs:471`), and there is no
  `GetTransactionEncryptionKey` call or `sync_chain_mmr`. `submit_transaction` already takes the
  0.17 `ProvenTransactionSubmission` (`lib.rs:320-333`).
- **Concurrency substrate**: `LeaseFence { lease_name, holder_id, fence_token }`
  (`storage/mod.rs:149`) and the leader/lease machinery in
  `src/coordination/leader.rs`, established by `010-horizontal-scaling`.
- **Existing lifecycle owner**: `src/jobs/canonicalization/{worker,processor}.rs`.
  This feature adds no delta status values and alters no existing transition.
- **Storage**: Postgres tables `account_metadata`, `states`, `deltas`,
  `delta_proposals`, `worker_leases`. **Three** new tables
  (`execution_reservations`, `execution_submissions`, `execution_outcomes`) in one
  migration. Filesystem gains a per-account reservation file.
- **Testing**: `cargo test -p guardian-server`; Postgres and live-network tests
  gated `#[ignore]`. Every lifecycle test in this feature runs **in-process with
  no node**; proving is already validated separately.
- **Scope**: server-side execution of already-signed proposals for Miden
  multisig accounts. Foreign **public** accounts are in scope and loaded lazily at `R`
  (FR-050), because every devnet fee payment loads the callback-enabled fee faucet as a foreign
  account (`miden-protocol-0.17.0-rc.7/asm/kernels/transaction-core/src/callbacks.masm:102-123`).
  Private or unservable foreign accounts are refused pre-boundary.
- **NEEDS CLARIFICATION**: none. The one open design question at plan entry,
  FR-037's admission primitive on the filesystem backend, is resolved below as
  Decision 1.

## Key Design Decisions

### Decision 1: FR-037's admission primitive extends `discard_candidate`'s shape, two-tier by backend

FR-037 requires reservation creation and candidate admission to be decided by
**one account-scoped atomic primitive**. Guardian already has that primitive,
and the two backends already implement it at different strengths:

**Postgres** (`storage/postgres.rs:1652`) commits each canonicalization write as
one transaction that: takes `lock_account_metadata`, a per-account
`SELECT … FOR UPDATE` on `account_metadata` (`postgres.rs:825`), then validates
`lease_fence_is_current`, then performs a status-conditional write, returning
`CanonicalWrite::{Applied, StaleLease, NotCandidate}`. An unfenced call is
**refused outright** via `unfenced_write_error` (`postgres.rs:786`).

That per-account lock is already the serialization point for *every* candidate
write. Reservation creation taking the **same** lock is what makes
reservation-vs-candidate atomic in both directions, with no new mechanism:
exactly what FR-037 asks for. `submit_candidate` already rechecks under this
lock that no candidate exists and the nonce is unoccupied
(`storage/mod.rs:518-528`); admission adds one predicate to that existing
recheck.

**Filesystem** serializes writes with a single in-process mutex, `delta_write_lock`, held by
`submit_delta`, `request_candidate_abandon`, `update_delta_status`, and
`update_candidate_status`. The backend is single-replica by construction, but execution leases
can still transfer between tasks inside that process. A task that resumes after losing its lease
must therefore be fenced out just as it is on Postgres.

**Decision**: reservation admission on filesystem takes **`delta_write_lock`
itself**, not a new mutex. A separate mutex would be a correctness bug:
admission must be atomic *with respect to candidate writes*, and those are
serialized by that specific lock. Under the same hold, every execution-owned mutation compares
the supplied holder and fence with the persisted active reservation; ownership transfer updates
the holder and advances the fence atomically. A stale or unfenced execution mutation writes
nothing. Generic client abandon annotations and the `AlwaysLeader` canonicalization path keep
their existing semantics because they are not execution-owner writes.

Filesystem tests cover a stale task losing ownership, waiting for the lock, and resuming after a
new owner has claimed it. Cross-replica races remain Postgres-only. The storage-parity invariant
and exact affected operations are recorded in [data-model.md](./data-model.md).

### Decision 2: no trait defaults on the new methods

The new `StorageBackend` methods are declared **without default bodies**, placed
in the existing canonicalization-writes block, which already states the rule and
the reason (`storage/mod.rs:509-516`): a trait default silently absorbing a
dropped backend override would revert Postgres to unfenced, non-atomic writes.

This is load-bearing here. A default returning "no reservation" would make the
Postgres reservation check silently vacuous: the failure mode would be a lost
concurrency guarantee that no test names, surfacing as a double submission under
load. Requiring every impl makes a dropped override a compile error.

Consequence for effort estimates: `StorageBackend` has **five**
implementations: `PostgresService`, `FilesystemService`, `EncryptedStorage`
and `InstrumentedStorage` (both pass-through decorators), and
`MockStorageBackend`. Every new method is five impls, of which two are real.

### Decision 3: execution leases are per-account, and ownership transfers by compare-and-set

FR-038 requires reusing the `LeaseFence` type. It does **not** license reusing the
canonicalization lease, and doing so would be a serious mistake:
`worker_leases` admits **one holder per `lease_name`**
(`coordination/postgres/lease.rs:59`, `ON CONFLICT (lease_name) DO UPDATE`), and
`CANONICALIZATION_LEASE` is the single cluster-wide string `"canonicalization"`
(`coordination/mod.rs:36`). Fencing reservations against it would reduce the whole
deployment to one execution at a time, and would make every fence check answer a
question about the canonicalization worker rather than about this account's reservation.

**Decision**: execution leases use the account-scoped name `execution:{account_id}`.
Different accounts contend on different lease rows and proceed concurrently; the same
account serializes at lease acquisition, *before* any reservation row is written, so the
FR-037 admission primitive becomes a second line of defence rather than the only one.

**FR-052 adds the missing operation.** FR-028 requires post-submission ownership to transfer
to a reconciliation owner without releasing the reservation, and there was previously no
storage operation that could do it. Transfer is a **compare-and-set** against the current
holder and fence token, returning `ClaimSuperseded` rather than stealing when the caller's
expectation is stale. Release-then-reacquire is explicitly not acceptable: the gap between
the two is exactly the window that admits a second submission.

### Decision 4: the internal ack path is an extraction, not a reimplementation

FR-044 needs Guardian to acknowledge its own delta without traversing
`push_delta` (which creates a candidate, and which FR-027 must now refuse for a
reserved account). `push_delta` is linear and already has the needed seam: it
resolves and verifies through line 118, then commits through the
`DeltaCommitStrategy` abstraction (`services/push_delta.rs:120-134`).

**Decision**: extract the verify-and-acknowledge span
(`push_delta.rs:31-118`) into `services/ack_delta_internal.rs`, with
`push_delta` calling it and then committing exactly as it does today. FR-044's
"MUST NOT weaken any check" is then satisfied structurally (there is one
shared implementation of the checks) rather than by review discipline.
The internal path deliberately performs no commit and does **not** set
`has_pending_candidate`; admission happens only at FR-045 step 12.

### Decision 5: one reference block per attempt, chosen at the tip

FR-061 fixes `R` as the node's committed tip at attempt start, and everything the executor reads
is read at `R`: the reference header, the `ProtocolConfig` (FR-060), the partial blockchain, and
every lazily loaded foreign account (FR-050). A retried attempt picks a fresh `R`; nothing from a
previous attempt's chain view is reused, and the FR-039 `reference_block` is the `R` actually
proven.

**Chain assembly** is the spike's construction: peaks at forest `R` from `SyncChainMmr`, plus
`GetBlockHeaderByNumber(n, include_mmr_proof)` for each tracked block (the summary's bound
block and every authenticated input note's creation block). `GetBlockHeaderByNumber` returns
paths at the node's *current* chain length, which can be past `R`, so each path is adjusted to
forest `R` (miden-client's `adjust_merkle_path_for_forest` is crate-private, about 15 lines to
copy). Tracking the bound block is Guardian's job: the executor builds `ref_blocks` from note
blocks and `R` only (`miden-tx-0.17.0-rc.7/src/executor/mod.rs:280-281`), and lazy block-witness
loading is a TODO (`host/tx_event.rs:491-494`), so Guardian's `DataStore` adds
`request.block_numbers()` itself, as `ClientDataStore` does
(`miden-client-0.17.0-rc.4/src/store/data_store/mod.rs:335-342`). Omitting it surfaces as
`TransactionSummaryUnknownBlockNumber` (`host/mod.rs:504-516`).

**The anchor is not an execution input.** On `main`, `chain_anchor` only names the bound block
for 0.18.0-rc.1 peers and nothing executes against it (`crates/miden-multisig-client/src/payload.rs:82-86`).
Guardian reads the bound block from the signed summary alone; when an anchor is present it may
compare the anchor's header commitment with the summary's block commitment, and nothing more.

The trust root reduces to "is `R`'s header canonical": the bound block's commitment is proven by
an MMR path under `R`'s chain commitment rather than by comparing two headers from one node.
Which finality level `R` should be taken at (`SyncChainMmr` `FinalityLevel {COMMITTED, PROVEN}`,
`rpc.proto:673-713`) is an upstream question (RFC Q2); v1 uses the node's committed tip.

### Decision 6: the execution path reads the chain through `miden-client`'s `NodeRpcClient`

Decided 2026-09-30 during Phase 2B. The 0.17 node protos are fully structured (headers carry a
validator configuration, the protocol configuration is a nested message), and the calls the seam
needs (genesis-seeded `sync_chain_mmr`, headers with MMR proofs, `GetAccount` at a block, the
attested transaction encryption key, sealed submission) already exist in `miden-client`
0.17.0-rc.4 with upstream conversions and checks: `ChainMmrInfo` verifies the returned
`ProtocolConfig` against the header, `AttestedTransactionEncryptionKey::verify` checks the
attestations. The server therefore depends on `miden-client` (with `tonic`), and
`network/miden/execution/` takes an `Arc<dyn NodeRpcClient>`, so tests run against
`miden_client::testing::mock::MockRpcApi` over a `MockChain`. Guardian's own `miden-rpc-client`
is unchanged and keeps serving canonicalization. Workstream D0's "add the RPC client surface"
bullets are satisfied by this decision rather than by new calls in `miden-rpc-client`.

## Constitution Check

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Bottom-up change propagation | OK | Server contract drives the Rust base client (`guardian-client`), TS base client, and both multisig SDKs. FR-033 requires equivalent capability; FR-051 requires both SDKs to apply the shared approval-expiration default and the shared 256-block transaction delta to built-in proposals, and preserve custom-producer requests for server enforcement. Propagation is Workstream H, gated on the server contract landing first. |
| II. Transport and cross-language parity | OK | All **three** new endpoints ship on HTTP **and** gRPC (FR-034); no divergence requested and none taken. Rust/TS surfaces stay behaviorally aligned per FR-033. Contract pinned in [contracts/execution-api.md](./contracts/execution-api.md). |
| III. Append-only integrity and explicit lifecycles | OK | Adds **no** delta status values and alters no existing transition (FR-026). Reported execution state is a separate, explicitly enumerated five-value vocabulary (FR-024) mapped onto the delta lifecycle. The one persisted state (FR-041) exists because canonicalization's `remove_candidate` destroys the record it would otherwise be derived from; this is documented, not implicit. Execution mode is an explicit client-set control path, default off (FR-009), never an inferred fallback. |
| IV. Explicit auth and stable boundary errors | OK | Requester must be a cosigner; the Guardian ack gate is unchanged and still mandatory. Synchronous refusals are enumerated in FR-022 with stable codes; `failed` carries a stable code distinguishing verification / proving / submission / post-submission-discard causes (FR-024). Capability-unavailable and startup misconfiguration are explicit (FR-043). |
| V. Evidence-driven delivery | OK | Five independently testable user stories; 44 success criteria; [validation-matrix.md](./validation-matrix.md) carries the offline and live coverage tables plus the fault-injection rows. Proving is already evidenced against public testnet. |

**No unresolved violations.** Execution ownership fencing preserves the same stale-worker
semantics on both backends; only true cross-replica concurrency is Postgres-specific.

## Project Structure

### Documentation (this feature)

```text
speckit/features/254-guardian-prove-and-commit/
├── spec.md                      # FR-001 to FR-061, SCs, 5 user stories (revision 11)
├── plan.md                      # This file
├── research.md                  # Evidence log with file:line citations
├── validation-matrix.md         # Gate, propagation, coverage, fault injection
├── data-model.md                # Reservation + evidence entities (Phase 1)
├── quickstart.md                # Operator/developer walkthrough (Phase 1)
├── contracts/
│   ├── execution-api.md         # HTTP + gRPC contract, 5-state vocabulary
│   └── sdk-api.md               # Client-level execution mode, naming rule
└── tasks.md                     # 152 tasks, regenerated 2026-09-30 for Miden 0.17
```

The external review document for this feature is
[`docs/rfcs/0001-server-side-transaction-execution.md`](../../../docs/rfcs/0001-server-side-transaction-execution.md).

### Source code

```text
crates/server/src/
├── network/miden/execution/     # PORTED from 254-execution-spike@769e2a90 (0.16 rc.9) onto main
│   ├── store.rs                 # DataStore over Guardian's own state; + ProtocolConfig return,
│   │                            #   bound-block tracking, lazy get_foreign_account_inputs at R
│   ├── blockchain.rs            # tip ChainView: SyncChainMmr peaks + header MMR paths at forest R
│   ├── foreign.rs               # NEW: public foreign accounts via GetAccount(block_num = R)
│   ├── sealing.rs               # NEW: encryption key fetch, attestation check, seal (FR-059)
│   ├── tests.rs                 # offline tests (MockChain), re-run at the 0.17 pins
│   └── live_tests.rs            # live tests (devnet, #[ignore])
├── storage/
│   ├── mod.rs                   # + reservation types, outcome enums, trait methods
│   ├── postgres.rs              # + fenced reservation writes (real impl)
│   ├── filesystem.rs            # + delta_write_lock-guarded impl
│   └── encryption/decorator.rs  # + pass-through
├── metrics/storage.rs           # + pass-through (instrumented decorator)
├── testing/mocks.rs             # + mock impl
├── services/
│   ├── ack_delta_internal.rs    # NEW: extracted from push_delta (FR-044)
│   ├── execute_proposal.rs      # NEW: FR-045's 14-step sequence
│   ├── execution_status.rs      # NEW: reported-state derivation (FR-024/026)
│   └── push_delta.rs            # MODIFIED: calls extraction; FR-027 refusal
├── jobs/execution_reconcile/    # NEW: FR-040 evidence paths, FR-031 recovery
├── api/{http.rs,grpc.rs}        # + 3 endpoints on both transports (FR-034)
├── config/                      # + execution capability + prover URL (FR-043)
└── error.rs                     # + stable codes for FR-022 refusals and D3's pre-boundary codes

crates/server/migrations/2026-10-01-000001_execution_reservations/

crates/miden-rpc-client/         # + sync_chain_mmr (ported), include_protocol_config,
                                 #   GetAccount at block_num, GetTransactionEncryptionKey

crates/client/                   # Rust base client `guardian-client` (FR-034); no `miden-client` dependency, guarded by `tests/no_miden_client.rs`
packages/guardian-client/        # TS base client (FR-034); no web SDK dependency, guarded by `src/dependencies.test.ts`
crates/miden-multisig-client/    # Rust SDK: execution mode (FR-009), FR-051 defaults, pinned notes
packages/miden-multisig-client/  # TS SDK: execution mode (FR-009), FR-051 defaults, bound block (N3)
```

**Structure decision**: the proving seam stays where the spike put it, under
`network/miden/execution/`, because it is network-specific. Lifecycle machinery is
network-agnostic and goes in `storage/`, `services/`, and `jobs/`, matching how
canonicalization is already split. The reconciliation worker is a **sibling** of
`jobs/canonicalization/`, not a modification of it: it consumes canonicalization
outcomes but owns a different question (did a submitted transaction land), and
folding it in would entangle two lifecycles that Principle III wants kept
distinct.

## Workstreams

### A: Storage, the reservation and the admission primitive

- Types in `storage/mod.rs`: `ExecutionReservation`, `SubmissionEvidence`,
  `ExecutionOutcome`, `CandidateAdmission`, and outcome enums mirroring
  `CanonicalWrite`'s shape:
  `ReservationWrite::{Created, AlreadyReserved, CandidateExists, StaleLease, ClaimSuperseded}`,
  `AdmissionWrite::{Admitted, NotAuthorized, CandidateExists, StaleLease}`, and
  `ResolveWrite::{Resolved, NotAuthorized, AlreadyResolved, StaleLease}`. `CanonicalWrite`
  gains `ProtectedByExecution`. Exhaustive, no catch-all variant.
- Trait methods, **no defaults** (Decision 2): create / renew / release / load
  reservation; `claim_execution_reservation` (Decision 3's fenced compare-and-set
  ownership transfer, FR-052); `admit_execution_candidate` (the FR-045 step 12
  commit, which admits the candidate **and** persists submission evidence
  together); `resolve_execution` (the post-boundary failure resolution: discard,
  outcome and release in one transaction); `record_execution_outcome`
  (**pre-boundary failures only**: post-boundary outcomes are written by the
  extended `promote_candidate` and by `resolve_execution`); and the
  reconciliation-owed query.
- The **account-scoped** `execution:{account_id}` lease is what every fence
  validates against (Decision 3), never `CANONICALIZATION_LEASE`.
- Postgres: each as one transaction: `lock_account_metadata`, then
  `lease_fence_is_current`, then conditional write; `unfenced_write_error` on a
  missing fence.
- Filesystem: `delta_write_lock`-guarded read-modify-write (Decision 1).
- Decorators and mock: pass-through and test double.
- `submit_candidate` and `push_delta` gain the reservation predicate (FR-027),
  with the **owner-authorized exception**: admission is permitted for the
  caller presenting the matching reservation's owner identity and a valid
  fence, refused for all others. This exception is not optional: without it
  Guardian deadlocks against its own candidate.

### B: Migration

`2026-07-28-000001_execution_reservations`: table keyed by `account_id` with
owner identity, lease expiry, fence token, optional candidate nonce, submission
evidence (including the expiration block from `ProvenTransaction`), and terminal
outcome. A **partial unique index** enforces at most one active reservation per
account at the schema level, so FR-029's single-owner rule does not rest on
application logic alone. Shape in [data-model.md](./data-model.md).

### C: Internal acknowledgment path (FR-044)

Extract `services/ack_delta_internal.rs` per Decision 4; `push_delta` delegates
to it. Not reachable from any transport: this is enforced by module visibility and
asserted by a test that the router exposes no route reaching it.

### D0: Port the spike seam onto `main` (prerequisite for D)

The implementation branch starts from `main` and ports, rather than merges, the spike's
execution pieces from `254-execution-spike` (`769e2a90`, protocol 0.16 rc.9, client 0.16 rc.4,
34 commits behind): `network/miden/execution/blockchain.rs`, `network/miden/execution/store.rs`,
and `miden-rpc-client`'s `sync_chain_mmr`. The port re-targets the 0.17 pins, adds the
`ProtocolConfig` return to `get_transaction_inputs`, makes the `DataStore` track
`request.block_numbers()` (Decision 5), and re-runs the spike's offline tests before any D work
depends on it. Its tip-reference `ChainView` is the normative construction (FR-061).

Alongside the port, D0 adds the RPC client surface the seam needs and `main` lacks:

- `include_protocol_config` on the header read (today hardcoded `None`,
  `crates/miden-rpc-client/src/lib.rs:302`), so FR-060 can fetch the `ProtocolConfig` and check it
  against `header.protocol_config_commitment()`
  (`miden-tx-0.17.0-rc.7/src/executor/data_store.rs:29-44`; `rpc.proto:270-295`).
- `GetAccount(account_id, block_num = R, details)` with storage and vault witnesses
  (`rpc.proto:33,345-412`; today `get_account_with_details` sends `block_num: None`,
  `lib.rs:471`), mapping node code 5 to a pruned-block error
  (`miden-client-0.17.0-rc.4/src/rpc/errors/node/account.rs:28-45`).
- `GetTransactionEncryptionKey` with its attestations (`rpc.proto:74`,
  `miden-node-proto-build-0.17.0-rc.3/proto/types/submission.proto:51-97`).

### D: Execution service (FR-045)

`services/execute_proposal.rs` implements the fourteen steps in order:

1. select signatures and check the threshold (FR-005, FR-006)
2. structural request checks (FR-056, FR-057, FR-051), then the approval-expiry check against
   the observed tip (FR-058)
3. pick `R` and assemble the chain view and `ProtocolConfig` (FR-061, FR-060)
4. reproduce and verify the binding (FR-007)
5. acknowledgment (FR-044)
6. inject the signature and acknowledgment advice
7. execute
8. re-verify the binding and run the expiration-reached check (FR-058)
9. prove (FR-019, FR-055)
10. seal the inputs (FR-059)
11. admissibility re-check plus the horizon check (FR-048, FR-046)
12. the boundary commit (FR-037, FR-039, FR-047)
13. re-validate the fence (FR-049)
14. submit

**Step 2 decodes before it reads the chain.** After the envelope and
protocol checks it decodes the stored request and refuses, as
`GUARDIAN_EXECUTION_REQUEST_INVALID`, a request that is structurally not Guardian-executable on
0.17 (`meta.reason`):

- `bound_block_not_declared`: `request.block_numbers()` lacks the summary's `block_number`,
  mirroring the SDKs' `BoundBlockNotDeclared`
  (`crates/miden-multisig-client/src/transaction/mod.rs:209-217`).
- `auth_args_missing`: `auth_arg` is empty, or its preimage is not in the advice map. The
  guarded multisig's `resolve_auth_args` pipes the three-word `[bound_block,
  approval_expiration, 0, 0] || SALT || CONVERSION_INFO` preimage
  (`miden-standards-0.17.0-rc.7/asm/standards/auth/multisig.masm:827-854`) and aborts with
  `ERR_AUTH_ARGS_PREIMAGE_MISSING` without it, even at zero fee. Guardian never derives,
  attaches or repairs fee conversion info (FR-057): miden-client's own path commits the
  **two-word** `[SALT, CONVERSION_INFO]` preimage
  (`miden-client-0.17.0-rc.4/src/transaction/request/mod.rs:291-300`), which is wrong for this
  account, and the fee faucet, 1/1 rate, salt and bound block are all proposer commitments
  already covered by the summary through the TX_FEE output note.
- `approval_expiration_missing`: summary user param 0 is zero (FR-051).
- `input_notes_not_pinned`: consume-notes without `explicit_input_notes`
  (`miden-client-0.17.0-rc.4/src/transaction/request/builder.rs:178-190`).

It then reads the observed tip and stops with `GUARDIAN_EXECUTION_EXPIRATION_REACHED`
(`meta.bound = approval`) when the tip is at or past the approval expiration. The reservation is
released within milliseconds; creation-time acceptance (FR-009) and the no-decode rule for
incompatible lines (FR-015) are untouched.

**Step 3** takes `R` = the node's committed tip. If the node is below the summary's bound block,
the attempt stops with the retryable `GUARDIAN_EXECUTION_CHAIN_BEHIND`, mirroring the SDKs'
`ChainBehindBoundBlock` (`transaction/mod.rs:152-173`).

**Steps 4 and 7** run the executor at `R` with the auth arg passed through unchanged. Foreign
**public** accounts are loaded lazily through `DataStore::get_foreign_account_inputs(id, R)`
(`miden-tx-0.17.0-rc.7/src/executor/exec_host.rs:183`); this covers the fee faucet's asset
callback (`callbacks.masm:102-123`) and `fee::pay_fee`'s FEE_SPONSORSHIP pricing through FPI
against the target network account (`miden-standards-0.17.0-rc.7/asm/standards/fee/mod.masm:349-352`).
A private foreign account, or foreign state the node cannot serve, stops the attempt with
`GUARDIAN_EXECUTION_FOREIGN_ACCOUNT_UNAVAILABLE` (`meta.reason` = `private` or `unavailable`).
Error mapping out of reproduction and execution is specific, not generic:

- VM abort `ERR_MULTISIG_APPROVAL_EXPIRED` (`multisig.masm:884-914`) maps to
  `GUARDIAN_EXECUTION_EXPIRATION_REACHED` (`meta.bound = approval`).
- An abort because the account's native fee-asset balance cannot pay the fee maps to
  `GUARDIAN_EXECUTION_INSUFFICIENT_FEE`. Every transaction needs that balance, including the
  first.
- A summary mismatch is `GUARDIAN_EXECUTION_BINDING_MISMATCH`. It also covers fee drift (see
  Risks), so the logged diagnostic compares the TX_FEE output notes of the signed and reproduced
  summaries; the wire code does not change.

**Step 8** re-verifies the binding on the executed result and stops with
`GUARDIAN_EXECUTION_EXPIRATION_REACHED` (`meta.bound = transaction`) when the executed
transaction's expiration block is at or below the observed chain height. The same check runs
before each proving retry under FR-055.

**Step 10 seals before the boundary.** `SubmitProvenTx` takes
`ProvenTransactionSubmission{transaction, SealedTransactionInputs{key_id, ciphertext}}`
(`miden-node-proto-build-0.17.0-rc.3/proto/types/submission.proto:8-28`), and `TransactionInputs`
cannot be recovered from a `ProvenTransaction`. The worker therefore keeps the execution's
`TransactionInputs` in memory from step 7 through step 14, and at step 10 fetches the validator
encryption key, validates its attestations and seals, using miden-client's
`seal_transaction_inputs` (`miden-client-0.17.0-rc.4/src/rpc/encryption.rs:445-460`; flow in
`transaction/mod.rs:779-845`). Any failure is `GUARDIAN_EXECUTION_SEALING_FAILED`, an ordinary
fail-and-release. Doing this after the boundary would strand the account until expiration for a
transaction that was never sent. How a server should validate the attestations is an upstream
question (RFC, sibling of Q2).

**Step 11's horizon** is `proven expiration_block_num - R`, using the `R` actually proven.
The proven expiration is `R + min(256, approval_expiration - R)` for built-in families (the
approval bound only ever lowers it, `tx.masm:143-169`), and falls back to the approval bound,
clamped to 65,535 from `R`, for a custom producer with no script delta. An expiration beyond the
horizon is **refused** with `GUARDIAN_EXECUTION_EXPIRATION_BEYOND_HORIZON`; nothing waits. The
default horizon must be at least 256.

The normative orderings are encoded structurally rather than by comment:

- **Step 2 before 3**: structurally invalid or already-expired requests cost no chain assembly.
- **Steps 4 before 5**: never acknowledge a transaction that does not reproduce the signed
  summary (FR-056, FR-057).
- **Step 8 before 9**: never prove a transaction whose expiration the observed chain height
  has already reached (FR-058); the horizon bounds the distance from `R`, not staleness.
- **Steps 10 and 11 before 12**: sealing (FR-059), admissibility (FR-048) and the horizon
  (FR-046) are checked *before* the boundary; after it, FR-047 forbids the fail-and-release they
  would demand, so the account would be held until expiration for a transaction never sent.
- **Step 12 before 14**: the candidate must exist before the send, or a crash between them
  leaves a chain transaction with no candidate to promote.

The step-12 commit is the **no-retry boundary**. Implementation rule: it is one
storage call returning one outcome, and no code path may treat its failure as
retryable. Step 13 re-validates the fence and aborts **without sending and
without writing** if stale (FR-049). Step 14 submits the sealed blob prepared at step 10; the
blob is never re-sent, so it need not be durable.

### E: Reconciliation and recovery (`jobs/execution_reconcile/`)

Reconciliation owns **two** terminal paths, superseded and expired, plus FR-031's restart
rule: an execution whose durable record shows the boundary was crossed is **never** retried,
only reconciled.

**It does not own `committed`.** That belongs solely to the extended `promote_candidate`
(FR-053). Observing the account at the expected commitment is an *input* telling reconciliation
this execution is neither superseded nor expired, so it must wait for promotion; it is never a
second write of the outcome. A reconcile loop that upserted `committed` on that observation would
race the party that owns it, reintroducing the `remove_candidate` hazard FR-041 exists to
prevent.

FR-046 gives reconciliation a finite chain-height bound: measured work confirmed a transaction
with no delta defaults to `u32::MAX` (never expires), which is exactly why FR-046 refuses an
unbounded result. On 0.17 FR-051 closes this from both sides: every Guardian-executable proposal
carries a signed, non-zero approval expiration that the multisig auth procedure applies after the
summary (`multisig.masm:931-942`), which bounds custom producers too because they build the auth
arg through `MultisigClient::multisig_auth_args(salt, bound_block, delta)`
(`crates/miden-multisig-client/src/client/mod.rs:183-205`), not through their script; and
built-in families additionally sign a 256-block transaction delta. The recorded bound is still the
proven `expiration_block_num()` (FR-039). Termination still depends on eventual trustworthy
chain observation. When RPC is unavailable, reconciliation retains the reservation, retries with
capped backoff, and exposes an operator-visible outage; restoring or failing over the chain source
is recovery. Wall-clock time never authorizes release or retry.

**Terminal resolution is not a separate write.** SC-025 requires promotion and discard to
*each* atomically persist the outcome, so outcome persistence and reservation release are
**extensions of the existing fenced promote and discard primitives**, inside their
transaction, not a `record_execution_outcome` call the worker makes afterwards. A separate
call racing `remove_candidate` can find the proposal already deleted, which is the exact
failure FR-041 exists to prevent. `record_execution_outcome` survives only for pre-boundary
failures, where no candidate exists and there is nothing to race.

**Two distinct discards, and conflating them wedges the account.** Between the step-12 commit
and the send, the candidate looks ordinary to canonicalization, which could discard it while
the execution's fence is live and FR-049 still permits sending. So the **canonicalization
worker's** `discard_candidate` checks, in its own transaction, whether the candidate belongs to
an unresolved boundary-crossed execution and returns `ProtectedByExecution` if so.

But the definite-rejection, superseded, and expired paths *require* exactly that discard. They
therefore go through `resolve_execution`, which validates execution ownership and fence and
performs discard, outcome persistence, and reservation release in one transaction. The
protection is scoped to the **caller**, not to the row: a blanket refusal would leave the
account unresolvable until expiration, and the expired path could not clear it either.

### F: Reported state and status surface

`services/execution_status.rs` derives the five reported states (FR-024) from
the reservation, the delta, and the persisted terminal outcome. Pre-terminal
states are derived; only the post-submission terminal outcome is persisted
(FR-041), because `remove_candidate` deletes the candidate *and* its proposal.
`proposal_exists` (FR-042) reports presence, never retry advice.

### G: Transports and configuration

Both endpoints on HTTP and gRPC (FR-034), per
[contracts/execution-api.md](./contracts/execution-api.md). Stable error codes
in `error.rs` for every FR-022 synchronous refusal and for the asynchronous pre-boundary codes
(`GUARDIAN_EXECUTION_REQUEST_INVALID`, `GUARDIAN_EXECUTION_EXPIRATION_REACHED`,
`GUARDIAN_EXECUTION_CHAIN_BEHIND`, `GUARDIAN_EXECUTION_INSUFFICIENT_FEE`,
`GUARDIAN_EXECUTION_SEALING_FAILED`, `GUARDIAN_EXECUTION_FOREIGN_ACCOUNT_UNAVAILABLE`,
`GUARDIAN_EXECUTION_EXPIRATION_BEYOND_HORIZON`), with their `meta.reason` / `meta.bound` values.
`GUARDIAN_EXECUTION_ANCHOR_EXPIRED` and `GUARDIAN_EXECUTION_FOREIGN_INPUTS_UNSUPPORTED` are not
implemented. Config: execution capability, prover URL and the FR-046 horizon (default at least
256), with **startup validation** (FR-043), including canonicalization being enabled, since
FR-040 depends on it entirely. There is no environment variable for the canonicalization mode:
`ServerBuilder::with_canonicalization(Option<...>)` selects it and the binary always runs
Candidate (`crates/server/src/main.rs:44-56`), so the startup check guards library and embedding
use, not operator config.

**Operational note (FR-020)**: the remote-prover client's default timeout is still
10 s (`miden-client-0.17.0-rc.4/src/remote_prover/tx_prover.rs:43`), below the observed 0.16
proving times of 6.2 to 20.1 s. Those timings and the ~26 KB request size are historical (0.17
uses proof format 2, VM 0.33, and the request now also carries `block_numbers` and the auth-arg
preimage) and are re-measured on 0.17. Guardian
MUST set an explicit timeout; the default surfaces as an intermittent
"failed to prove transaction" that names no timeout. This cost half a debugging
session already and belongs in `docs/TROUBLESHOOTING.md`. Timeouts and other
transport-level prover failures are retried server-side with capped backoff
under the held reservation (FR-055). The transient classifier must match the
walked error source chain, since the outermost `Display` hides the transport
cause. A mis-set timeout therefore wastes prover capacity on retries rather than
surfacing to the caller, which makes the explicit setting no less mandatory.

**Deploy note: execution is always built in (decided 2026-10-01).** It was first an optional
`proving` Cargo feature, which made every image built without it answer the execute endpoint
with `GUARDIAN_PROVING_UNAVAILABLE` for a build-time reason, and kept the execution tests out of
a default `cargo test`. The feature was dropped: execution is compiled into every binary, as it
is into the SDKs, and stays off at run time until `GUARDIAN_TX_PROVER_URL` is set, with
`GUARDIAN_PROVING_ENABLED=false` as the explicit off switch.

### H: Clients (FR-033, FR-009, FR-051)

Client-level `ProposalExecutionMode`, default **not attached**: no new
per-call SDK methods. Four packages. SDK work in `guardian_executable` mode, identical in Rust
and TypeScript and asserted by a parity test:

- **Approval expiration (FR-051).** Apply the shared constant
  `GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA = 28_800` blocks (about 24 h on devnet) unless
  the caller passes one (1..65,535), through the existing option: Rust
  `ProposalOptions.approval_expiration_delta: Option<NonZeroU32>`
  (`crates/miden-multisig-client/src/transaction/builder.rs:47-57`), TypeScript
  `approvalExpirationDelta` (`packages/miden-multisig-client/src/transaction/options.ts:23`).
  Today the default is never. This applies to custom producers as well, since their auth arg is
  built by `multisig_auth_args`, not by their script.
- **Transaction delta (FR-051).** Built-in families set a relative **256-block** delta, a shared
  constant in both SDKs: `TransactionRequestBuilder::expiration_delta` for no-script requests,
  `tx::update_expiration_block_delta` inside Guardian-owned scripts. Neither SDK sets one today;
  the only `256` on `main` is the Rust client's stale-sync bound `max_block_number_delta(256)`
  (`crates/miden-multisig-client/src/builder.rs:354`). The builder still rejects the delta
  together with a custom script
  (`miden-client-0.17.0-rc.4/src/transaction/request/builder.rs:305,682-687`), so an opaque custom
  request is attached byte-for-byte and is bounded by the approval expiration alone (clamped to
  65,535 from `R`), which the server checks against its horizon before the boundary.
- **Pinned consume-notes (FR-056, tasks T123 and T125).** Build consume-notes with `explicit_input_notes`
  (`miden-client-0.17.0-rc.4/src/transaction/request/builder.rs:178-190`). Neither SDK uses it
  yet; the Rust SDK classifies notes from the local store and imports proofs
  (`crates/miden-multisig-client/src/transaction/consume.rs:108-193`).
- **TypeScript bound block (N3).** Take the bound block from the signed summary, not from
  `anchor.blockNum()` (`packages/miden-multisig-client/src/multisig.ts:245-259`), so a TS proposal
  with a mismatching anchor fails the same way on both SDKs. Rust already uses
  `summary.block_number()`.
- **Envelope identity (FR-014).** `protocol_line` is `"0.17"`. The envelope carries no
  `serializer_id` (decided 2026-10-01: a version allowlist protects nothing the signed summary does not already protect, and it doubled the work of every `miden-client` bump): a request from another `miden-client` fails to decode or fails
  the summary comparison, both before proving.

Proposal identity under `guardian_executable` differs from `self_executed`, because both the
256-block delta and the approval expiration are signed (SC-033).

### I: Tests and docs

Per [validation-matrix.md](./validation-matrix.md). The fault-injection rows are
the substance: both sides of the step-12 write (SC-024, SC-030), fence theft
mid-flight (SC-019, SC-031), and the self-deadlock regression (SC-028): the bug this
spec already had once, where the blanket admission rule blocked Guardian's own
candidate. The 0.17 execution-input work adds its own tests: fee-faucet callback, network-note
sponsorship pricing, pruned foreign state (node code 5), private foreign account, node behind the
bound block, unfunded account, each `GUARDIAN_EXECUTION_REQUEST_INVALID` reason, approval
expired at step 2 and as a VM abort, sealing failure before the boundary, and `ProtocolConfig`
commitment mismatch. Docs: `CONFIGURATION.md`, `TROUBLESHOOTING.md`, `spec/api.md`,
`docs/MIDEN_COMPATIBILITY.md`.

## Phasing

| Phase | Content | Gate |
|---|---|---|
| 1 | A + B: storage, migration, admission primitive | Concurrency tests pass on both backends before anything calls it |
| 2 | C: internal ack extraction | `push_delta` behavior provably unchanged (existing tests, untouched) |
| 3a | D0: port the spike seam onto `main` at the 0.17 pins, add the RPC client surface | Ported offline tests green; tip `ChainView` tracks the bound block and note blocks at forest `R` |
| 3 | D: execution service, up to and including step 12 | Fault injection on both sides of the boundary; sealing and every pre-boundary code fail-and-release |
| 4 | E + F: reconciliation, recovery, reported state | Every FR-040 path terminates after eventual trustworthy chain observation; outages remain safe and visible |
| 5 | G: transports, config, startup validation | Parity tests on both transports |
| 6 | H + I: clients, docs, full matrix | Matrix green |

Phase 1 is the gate for everything else, and its tests must be written against
the storage layer directly. If the admission primitive is wrong, every later
phase inherits a concurrency bug that integration tests will only surface
intermittently.

Phases 3 and 4 may not be reordered: the reconciliation paths are what make the
no-retry boundary survivable, so shipping the boundary without them would leave
accounts held until expiration with no resolution path.

## Validation

- **Concurrency (Postgres)**: two workers racing execution on one account:
  exactly one reservation, exactly one submission (SC-005). Repeated proving is
  permitted; a second submission is not.
- **Fault injection at the boundary**: crash immediately before the step-12 write
  fails-and-releases; crash immediately after reconciles and never retries
  (SC-024). A sealing failure at step 10 fails-and-releases and never reaches step 12.
- **Fence theft**: lease stolen between step 12 and step 14: no send, durable
  candidate left to reconciliation (SC-031); new owner resolves, original
  worker resumes without submitting (SC-019), and the handover is a fenced
  compare-and-set rather than a release (SC-035).
- **Self-deadlock regression**: Guardian admits its own candidate under its own
  reservation (SC-028).
- **Per-account leases**: two accounts execute concurrently on one replica while two
  executions for one account serialize (a test that would fail if the cluster-wide
  canonicalization lease were reused, SC-034).
- **Expiration**: both SDKs apply the shared 28,800-block approval default and the shared
  256-block transaction delta to built-in proposal families; custom producers carry the
  approval bound through the auth arg. The server refuses a zero approval expiration at step 2,
  stops at a reached approval or transaction expiration (step 2, VM abort, step 8, before each
  proving retry), and refuses a proven expiration beyond the horizon measured from the proven `R`
  before the no-retry boundary (SC-027, SC-033).
- **Tip reproduction**: a proposal executed well past its bound block (more than the devnet
  pruning window of about 50 blocks) reproduces at a fresh `R`, with the bound block proven
  through the MMR at forest `R`; a request whose `block_numbers()` omits the bound block is
  refused before any chain read.
- **Backend parity**: identical externally observable outcomes on filesystem and
  Postgres for every non-concurrent scenario. Concurrent scenarios are
  Postgres-only **by design**: see Complexity Tracking.
- **Transport parity**: all three endpoints, both transports, same semantics and
  error meanings (FR-034).

## Deferred

- **Optimistic mode**: refused. Requires canonicalization (FR-043); without it
  there is no way to establish whether a submitted transaction committed.
- **Private foreign accounts**: refused pre-boundary with
  `GUARDIAN_EXECUTION_FOREIGN_ACCOUNT_UNAVAILABLE` (`meta.reason = private`). Public foreign
  accounts are in scope (FR-050), no longer deferred.
- **Stage 2 live submission validation**: needs a funded, Guardian-registered
  devnet account on 0.17 (testnet still runs 0.16). Not a blocker: it validates the submit call
  with sealed inputs, and every lifecycle test above runs in-process with no node.
- **Upstream peaks nicety**: a small `PartialMmr`-from-tip convenience on the Miden side, or a
  public `chain_anchor_at_tip(tracked_blocks)` (private at
  `miden-client-0.17.0-rc.4/src/transaction/mod.rs:398-427`, the same unblocker the web SDK
  needs), would delete `blockchain.rs`'s seed-and-apply dance and the forest path adjustment.
  Cosmetic; the ported path is the one validated.

## Risks

- **Fee drift (N1, RFC Q9).** The fee is `(ilog2(clk + extra) + 1) × verification_base_fee`
  with the base fee from the reference block header
  (`miden-protocol-0.17.0-rc.7/asm/kernels/transaction-core/src/tx.masm:309-339`), and the TX_FEE
  amount enters the signed output-notes commitment. Guardian's reproduction at `R` therefore
  differs from the proposer's summary when the base fee changed or the cycle count crosses a
  power of two; tip execution with `bound < R` adds an MMR walk the proposer's `bound == R` run
  did not do. The SDK cosigners carry the same risk today, and devnet ran fine 70+ blocks past
  the bound block. It surfaces as `GUARDIAN_EXECUTION_BINDING_MISMATCH`, fixable by a fresh
  proposal, and the logged diagnostic compares the TX_FEE output notes so it is not mistaken for
  tampering.
- **Node behind the bound block.** A lagging or failed-over node can sit below a freshly bound
  block. This is the retryable pre-boundary `GUARDIAN_EXECUTION_CHAIN_BEHIND`, never a
  reservation held past step 3.
- **Fee balance.** Every transaction, including the first, needs a native fee-asset balance in
  the account's own vault; an unfunded account aborts during reproduction with
  `GUARDIAN_EXECUTION_INSUFFICIENT_FEE`. Live tests need a funding step before the first
  execution.
- **Pre-release pins.** Everything targets 0.17 release candidates; rc.3 and rc.4
  `TransactionRequest` bytes do not decode across each other (`block_numbers` became the first
  serialized field, `miden-client-0.17.0-rc.4/src/transaction/request/mod.rs:447-477` versus rc.3
  `:439-443`), so proposals created across an rc bump may not execute on the bumped server. They
  fail as `REQUEST_CODEC` or `BINDING_MISMATCH` before proving, and their signers can still
  execute them; production waits for stable 0.17 and the re-pin.
- **Sealing trust root.** Validator key attestations have no Guardian-side validation policy
  yet (RFC Q2 sibling); until one is decided, step 10 follows miden-client's own
  `submit_proven_transaction` flow (`miden-client-0.17.0-rc.4/src/transaction/mod.rs:779-845`),
  and the policy is a production gate.

## Complexity Tracking

| Violation | Why needed | Simpler alternative rejected because |
|---|---|---|
| A reported execution state is persisted (FR-041) rather than derived, unlike every other status in the system | Canonicalization's `remove_candidate` deletes an unrecoverable candidate **and then its matching proposal**, destroying the record a derived state would read. Without persistence, a post-submission terminal outcome is unrecoverable. | Deriving on read was the original design and is simply incorrect: it was caught by review, not by tests. Keeping the candidate alive instead would change canonicalization's existing lifecycle, which FR-026 forbids. |
