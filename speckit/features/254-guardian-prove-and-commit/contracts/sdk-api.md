# SDK API Contract: Guardian Prove and Commit

**Last Revised**: 2026-09-30 (spec revision 11: re-verified against the Miden 0.17 release-candidate
pins on `main`. Guardian-executable creation now applies two signed expiration bounds, an
approval expiration and a 256-block transaction delta; the anchor-based reproduction obligations
are withdrawn in favor of the signed bound block)

Normative SDK surface for #254. Signatures are illustrative; the binding rules, state
vocabulary, and error semantics are normative and MUST be symmetric across both SDKs
(Constitution II, FR-033).

Two additions per layer: **client-level configuration** deciding whether proposals are
created Guardian-executable, and methods to request and observe a Guardian execution. No
existing method signature changes.

## Proposal creation and signing

Client-side preparation and execution remain the default. In Guardian-executable mode,
creation sends the client-derived summary, signatures, metadata and attached request.
Guardian does not reproduce the request at creation. The caller separately requests
execution after enough valid signatures exist; a ready proposal needs no intervening
signing round. V1 adds no combined create-and-execute method and no automatic dispatch.
The Guardian acknowledgment is separate from the effective cosigner threshold.

A future optional preparation operation could create an unsigned proposal from a request
and return its ID and derived summary. It is outside this contract, as is automatic
execution policy. Both require explicit future API design.

## Naming rule

`Guardian` appears **only on the verb that delegates**, and only where a local counterpart
exists to contrast with:

- `requestGuardianExecution` / `request_guardian_execution` keeps it, because the multisig
  SDKs already have `executeProposal` / `execute_proposal` meaning *execute locally*. Without
  the prefix the contrast would rest on the reader noticing that "request" implies delegation.
- `executionStatus`, `currentExecution` and the shared `ProposalExecution` type drop it.
  Guardian records nothing for a local execution: self-execution creates no reservation and no
  states, so there is no other kind of execution to distinguish from. The prefix would be
  contrasting with something that cannot exist.
- The **base clients** carry no prefix at all (`executeDeltaProposal`,
  `getDeltaProposalExecution`, `getCurrentExecution`): they have no local-execution
  counterpart, so nothing needs disambiguating, and every base-client call goes to Guardian
  anyway.

The type is `ProposalExecution` in both layers and both languages. Base-client method and
gRPC operation names keep the `delta_proposal` family convention already used by
`get_delta_proposal` / `push_delta_proposal`.

## Shared concepts

- **Execution mode**: client configuration set once at construction, defaulting to
  self-executed (FR-009). When set to Guardian-executable, the SDK attaches the serialized
  `TransactionRequest` it already holds at creation; the caller supplies nothing extra
  (FR-011). It is never attachable after creation, and never negotiated with the server:
  the SDK does not query server capability, and the server independently decides whether it
  offers execution (FR-009, FR-021).
- **Execution handle**: `(account_id, proposal_id)`. No opaque token; polling is
  idempotent.
- **Execution state**: exactly five values, `pending`, `proving`, `submitted`, `committed`,
  `failed` (see `execution-api.md`). Both SDKs MUST model it as a closed type and handle
  every variant exhaustively (`never` check in TS, full `match` in Rust). Adding a state is
  a breaking SDK change. SDKs MUST NOT invent additional states, and MUST NOT collapse
  these into a boolean.
- **No Miden capability required, base clients only.** Requesting execution and polling
  state MUST work from a **base client** (`crates/client`, `packages/guardian-client`) with
  no Miden connectivity, no keystore beyond the auth key, and no transaction-building
  ability (FR-034). The multisig SDKs expose the same operations for convenience, but
  constructing one still requires a Miden client, so they do **not** satisfy this guarantee
  and MUST NOT be documented as the thin-client path.

## Base clients

### Rust: `crates/client`

```rust
pub async fn execute_delta_proposal(
    &mut self,
    account_id: &AccountId,
    proposal_id: &str,
) -> Result<ProposalExecution>;

pub async fn get_delta_proposal_execution(
    &mut self,
    account_id: &AccountId,
    proposal_id: &str,
) -> Result<ProposalExecution>;

/// The account's in-flight execution, if any (FR-036).
pub async fn get_current_execution(
    &mut self,
    account_id: &AccountId,
) -> Result<Option<ProposalExecution>>;
```

### TypeScript: `packages/guardian-client`

```ts
executeDeltaProposal(accountId: string, proposalId: string): Promise<ProposalExecution>;
getDeltaProposalExecution(accountId: string, proposalId: string): Promise<ProposalExecution>;
getCurrentExecution(accountId: string): Promise<ProposalExecution | null>;
```

`execution.ts` MUST mirror the response envelope exactly (its `Server*` types, next to the strict
`fromServerExecution` decoder that reads them), including the optional `error.meta`, and every error code in `execution-api.md` MUST be added to the client's
error-code vocabulary in the same PR. That includes the revision 11 asynchronous codes
`GUARDIAN_EXECUTION_REQUEST_INVALID`, `GUARDIAN_EXECUTION_EXPIRATION_REACHED`,
`GUARDIAN_EXECUTION_CHAIN_BEHIND`, `GUARDIAN_EXECUTION_CHAIN_INCONSISTENT`,
`GUARDIAN_EXECUTION_NODE_UNAVAILABLE`, `GUARDIAN_EXECUTION_FOREIGN_ACCOUNT_UNAVAILABLE`,
`GUARDIAN_EXECUTION_INSUFFICIENT_FEE` and `GUARDIAN_EXECUTION_SEALING_FAILED`, and the closed
`meta.reason` / `meta.bound` value sets they carry, modeled as closed types in both languages.
`GUARDIAN_EXECUTION_ANCHOR_EXPIRED` and `GUARDIAN_EXECUTION_FOREIGN_INPUTS_UNSUPPORTED` do not
exist and MUST NOT be added.

## Multisig SDKs

**No existing method signature changes.** The attachment decision is client
configuration, set once at construction (FR-009). This keeps `propose_transaction` and
`propose_custom_transaction` byte-identical in shape across both SDKs, which also avoids a
parity break, since Rust has no optional parameters and a per-call option would be a
breaking signature change there but free in TS.

### Rust: `crates/miden-multisig-client`

Typed rather than a bare bool (AGENTS.md §12: typed structures, type-driven operations):

```rust
pub enum ProposalExecutionMode {
    /// Default (FR-009). Nothing extra stored; only a transaction-capable party executes.
    SelfExecuted,
    /// Attach the serialized request so GUARDIAN may execute.
    GuardianExecutable,
}

// Set once, at construction. Absent => SelfExecuted.
MultisigClientBuilder::new()
    .execution_mode(ProposalExecutionMode::GuardianExecutable)
    .build()

/// Ask GUARDIAN to prove and submit. Returns immediately with the accepted state.
pub async fn request_guardian_execution(&mut self, proposal_id: &str)
    -> Result<ProposalExecution>;

pub async fn execution_status(&mut self, proposal_id: &str)
    -> Result<ProposalExecution>;

/// What GUARDIAN is currently doing for this account, if anything (FR-036).
pub async fn current_execution(&mut self) -> Result<Option<ProposalExecution>>;
```

`propose_transaction` and `propose_custom_transaction` are **unchanged** and honour the
client's configured mode. So does `propose_transaction_with_options`, whose existing
`ProposalOptions.approval_expiration_delta` (`crates/miden-multisig-client/src/transaction/builder.rs:47-57`)
is the per-proposal override of the Guardian-executable approval default below.

### TypeScript: `packages/miden-multisig-client`

```ts
type ProposalExecutionMode = "self_executed" | "guardian_executable";

// Set once, at construction: `executionMode` is a new optional field on the existing
// `MultisigClientConfig`. Absent => "self_executed".
new MultisigClient(midenClient, { /* … */ executionMode: "guardian_executable" });

requestGuardianExecution(proposalId: string): Promise<ProposalExecution>;
executionStatus(proposalId: string): Promise<ProposalExecution>;
currentExecution(): Promise<ProposalExecution | null>;
```

Guardian mode is honoured by the **typed, request-building proposal methods**
(`createP2idProposal`, `createConsumeNotesProposal`, the signer-set and threshold methods,
`createSwitchGuardianProposal`, `createCustomProposal`): they hold the `TransactionRequest`
before pushing, so under `guardian_executable` they serialize and attach it through an
**internal request-bearing creation path**. Their public signatures are unchanged; the existing
optional `approvalExpirationDelta` (`packages/miden-multisig-client/src/transaction/options.ts:23`)
overrides the approval default. Built-in methods also apply both expiration bounds as described
below; `createCustomProposal` attaches the producer's opaque request unchanged. The
low-level `createProposal(nonce, txSummaryBase64, metadata)` receives no
`TransactionRequest` and cannot attach one; on a `guardian_executable` client it MUST
refuse with an explicit error rather than silently create a proposal the server can never
execute.

An integration that genuinely needs both modes constructs two clients. A per-call override
can be added later without breaking either SDK (TS: optional parameter; Rust: an additive
builder); it is omitted here because the cost/benefit does not vary meaningfully between
two proposals from the same integration.

Omitting `executionMode` MUST behave as `self_executed`, so existing callers are unaffected
by an SDK upgrade (FR-009).

## Behavioral contract (both SDKs)

### Expiration constants

Both SDKs MUST export the same two constants (FR-051), pinned by the cross-language fixtures:

| Constant | Value | Meaning |
|---|---|---|
| `GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA` | `28_800` blocks (about 24 h on devnet at about 3 s per block) | Default approval expiration, counted from the bound block and signed as summary user param 0. The caller may override it with 1..65,535 (`MAX_APPROVAL_EXPIRATION_DELTA`, `crates/miden-multisig-client/src/transaction/auth_args.rs:44`, `packages/miden-multisig-client/src/transaction/authArgs.ts:73`). "Never" is not available in this mode |
| Built-in transaction expiration delta | `256` blocks | Relative delta every built-in family sets, signed as the summary's `expiration_delta` and counted from the executing reference block |

How they combine: the auth procedure applies the approval expiration after the summary is built
and only ever lowers the expiration, so the proven expiration is `R + min(256, approval_exp − R)`
for reference block `R` (`miden-protocol-0.17.0-rc.7/asm/kernels/transaction-core/src/tx.masm:143-169`).
Neither SDK sets a transaction delta on `main` today, and the only `256` there is the Rust
client's stale-sync bound (`crates/miden-multisig-client/src/builder.rs:354`), so the transaction
delta constant is new work, not a rename.

### Creation under a `GuardianExecutable` client

Creation MUST:

1. Build the auth args with the approval expiration: the caller's delta if given, otherwise
   `GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA`. `SelfExecuted` keeps today's default (never).
2. For every **built-in** proposal family, set the 256-block transaction delta, then derive the
   summary exactly as today. How the builder expresses it is family-specific on 0.17: send-notes
   and no-script requests use `TransactionRequestBuilder::expiration_delta`
   (`miden-client-0.17.0-rc.4/src/transaction/request/builder.rs:305`), and Guardian-owned
   custom scripts (signer set, threshold, switch guardian) call `tx::update_expiration_block_delta`
   in the script, because the builder rejects `expiration_delta` together with a custom script
   (`.../request/builder.rs:682-687`).
3. Carry the three-word multisig auth arg and its preimage in the request, and declare the
   bound block through `block_numbers` / `withBlockNumbers`. Both SDKs already do this on `main`
   for every request (Rust `TransactionRequestBuilderExt::multisig_auth_args`,
   `crates/miden-multisig-client/src/transaction/auth_args.rs:144-160`; TypeScript
   `feeAwareTransactionRequestBuilder`); Guardian refuses a request without them as
   `GUARDIAN_EXECUTION_REQUEST_INVALID` (`auth_args_missing`, `bound_block_not_declared`) and
   never attaches fee conversion info itself (FR-057).
4. For consume-notes, pin every input note through `TransactionRequestBuilder::explicit_input_notes`
   (`miden-client-0.17.0-rc.4/src/transaction/request/builder.rs:178-190`); see below.
5. Serialize the `TransactionRequest` it holds and wrap it in the FR-014 envelope (format
   version, protocol line `0.17`, checksum).
6. Attach the envelope to the proposal payload and push as normal.

### Custom producers

For an opaque **custom producer** request the SDK preserves the supplied serialized request
exactly. `TransactionRequest` still has no expiration mutator or accessor on 0.17, so the SDK
cannot retrofit anything without rebuilding producer-owned code. The producer MUST:

- build its auth args through `MultisigClient::multisig_auth_args(salt, bound_block, delta)`
  (`crates/miden-multisig-client/src/client/mod.rs:183-205`; TypeScript through the same
  request options) with a **non-zero** approval delta, and attach them so the request carries
  the auth arg, its preimage and the declared bound block. The approval expiration is signed in
  the auth arg, not in the producer's script, so it applies to custom producers too;
- optionally set a transaction delta inside its script with `tx::update_expiration_block_delta`.
  Without one, the proven expiration falls back to the approval bound (clamped to 65,535 from
  `R`), which may exceed the server's horizon: Guardian then refuses the execution before the
  boundary with `GUARDIAN_EXECUTION_EXPIRATION_BEYOND_HORIZON` until the chain is within the horizon
  of the approval expiration. Neither SDK nor producer needs to know a deployment's horizon.

### What identity is and is not preserved (FR-012)

`TransactionSummary` on 0.17 commits to the account delta, input notes, output notes, the
**bound block** number and its commitment, the transaction's expiration delta, and the user
parameters, among them the approval expiration (param 0) and the salt
(`miden-protocol-0.17.0-rc.7/src/transaction/tx_summary.rs:23-39`). A request built without a
transaction delta signs a delta of `0` (`tx_summary.rs:115-118`). Therefore:

- Attaching `transaction_request` MUST NOT change the derived proposal identity, because the
  server derives it from `tx_summary` alone via `delta_proposal_id`, and the envelope is not part
  of the summary.
- The two expiration bounds **do** change the summary, and with it the proposal ID, relative to
  the same effects built under `SelfExecuted`: both the 256 delta and the approval expiration are
  signed (SC-033). The two modes are therefore not interchangeable for one proposal: a proposal is
  created in one mode and signed as such. Cosigners see both bounds they are signing, and no later
  party can change them.
- **`SelfExecuted` output remains byte-identical to pre-feature output.** No bound is added,
  no envelope is attached, and nothing about the payload shape changes (SC-009).

Guardian v1 MUST NOT rewrite either bound, first because the protocol would reject the result as
a binding mismatch, and also because preserving the proposal builder's exact request keeps
transaction construction in the SDK/producer boundary (FR-013), preserves the FR-014 checksum,
and avoids inventing transformation semantics for opaque custom scripts. The server's role is
enforcement: it refuses a proposal whose approval expiration is missing or reached, and an
executed transaction whose expiration is reached or outside its configured horizon.

### Bound block and tip execution

The summary binds the bound block, not the block the transaction executes against, and the
kernel resolves that block through the partial blockchain at the executing reference block.
Guardian takes the bound block from the signed summary's `block_number` and reproduces at the
chain tip (FR-056, FR-061), exactly as both SDKs already verify, sign and execute on `main`
(Rust `execute_for_summary_at_tip`, `crates/miden-multisig-client/src/transaction/mod.rs:111-127`;
TypeScript `executeForSummaryAtTip`, `packages/miden-multisig-client/src/transaction/summary.ts:148-169`;
`docs/MULTISIG_SDK.md`, "Tip execution and the bound block"). The `chain_anchor` payload field
stays SDK metadata: the SDKs keep emitting and validating it, and Guardian does not read it.

Two SDK obligations follow for a `GuardianExecutable` client:

- the request MUST declare the bound block (point 3 above). A missing declaration fails the
  SDKs locally as `BoundBlockNotDeclared` (`crates/miden-multisig-client/src/transaction/mod.rs:205-217`)
  and Guardian as `GUARDIAN_EXECUTION_REQUEST_INVALID` (`bound_block_not_declared`);
- for consume-notes it MUST pin every input note in the request through
  `explicit_input_notes`. The mode each note is consumed in also enters the summary, and
  miden-client otherwise classifies it from the executing client's store: a note absent from
  `explicit_input_notes` is authenticated only if the store holds its proof
  (`miden-client-0.17.0-rc.4/src/transaction/request/mod.rs:322-362`). Today both SDKs
  authenticate from the local store and import proofs to force authenticated mode
  (`crates/miden-multisig-client/src/transaction/consume.rs:108-193`); Guardian has no store, so
  a request that is not pinned would reproduce a different summary, and Guardian refuses it as
  `GUARDIAN_EXECUTION_REQUEST_INVALID` (`input_notes_not_pinned`).

**Parity fix (N3).** The Rust SDK takes a rebuild's bound block from `summary.block_number()`;
TypeScript takes it from `anchor.blockNum()` (`proposalRequestBinding`,
`packages/miden-multisig-client/src/multisig.ts:245-259`). Both SDKs MUST bind it to the signed
summary, as Guardian does, and treat an anchor naming a different block as a mismatch, so a TS
proposal with an inconsistent anchor fails the same way in both SDKs and on the server. The web
SDK's summary exposes the bound block only as its header commitment, so TypeScript first
requires the anchor's commitment to equal the summary's (`assertAnchorBindsSummary`) and then
reads the number from the anchor; the commitment covers the block number, so that number is the
one the summary signed.

### Size, capability and mode

The SDK MUST NOT enforce its own size limit. The limits in FR-016 are server configuration,
and capability negotiation is prohibited (FR-009), so a client-side copy could only be a
guess that drifts from the deployment it talks to. An oversized request is refused by the
server with its typed error, which the SDK surfaces unchanged. The cost is one wasted
round trip carrying the payload on an error path; the benefit is one source of truth.

Creation under a `SelfExecuted`-configured client, including any client that sets nothing,
MUST produce a payload with **no** `transaction_request` field, byte-identical to a
pre-feature proposal (FR-010, SC-009).

Neither SDK may query, cache, or branch on the server's proving capability. A client
configured `GuardianExecutable` attaches the request unconditionally; if that server does
not offer execution, it is surfaced at execution time by `GUARDIAN_PROVING_UNAVAILABLE`,
not at creation (FR-009).

### Requesting and observing an execution

`request_guardian_execution` MUST:

1. Not build, execute, or prove anything locally (FR-034).
2. Surface synchronous refusals as typed errors carrying the stable codes from
   `execution-api.md`, never as free-form strings (AGENTS.md §12).
3. Return the accepted execution state; MUST NOT poll internally or block until committed.
   Any wait-for-completion helper MUST be a separate, explicitly-named call so the
   non-blocking behavior is visible in the API (no silent fallbacks).

`execution_status` MUST return the server's state verbatim without collapsing
distinct states into a boolean, and MUST NOT treat `submitted` as either terminal success
or terminal failure: only `committed` and `failed` are terminal. A `failed` execution's
`error.code` and `error.meta` MUST be surfaced as typed values (Rust enum, TS discriminated
union), including `meta.bound` for `GUARDIAN_EXECUTION_EXPIRATION_REACHED` and `meta.reason` for
`GUARDIAN_EXECUTION_REQUEST_INVALID` and `GUARDIAN_EXECUTION_FOREIGN_ACCOUNT_UNAVAILABLE`.

Both SDKs MUST surface `newly_accepted` and `proposal_exists` rather than dropping them.
`newly_accepted` is how a caller distinguishes a fresh execution from an idempotent hit on an
existing one, and MUST NOT be inferred from HTTP status (gRPC has no 202). A `failed`
`proposal_exists` is a fact about storage, not permission: a retry is permitted only when
the state is `failed` **and** `proposal_exists` is `true`. An SDK MUST NOT present a
`submitted` execution as retryable (retry is forbidden), and MUST NOT offer a retry when the
proposal was deleted with its candidate: that would fail with proposal-not-found, and the caller
must create a new proposal.

Both SDKs MUST NOT expose any method that retries an execution reporting `submitted`: the
server refuses it (FR-030), and the SDK MUST NOT paper over that with client-side retry.

On an execution-conflict refusal, both SDKs MUST surface the blocking proposal id from the
error payload rather than discarding it, so a caller can act on the conflict without
polling every proposal (FR-036).

## Cross-language fixtures

A committed fixture set MUST pin the envelope contract so the two SDKs cannot drift
(mirroring `fixtures/miden-multisig-client/p2id-serial-vectors.json`):

- A serialized `TransactionRequest` envelope produced by each SDK for the same
  transaction inputs, asserting equal `format_version`, `protocol_line`, and `checksum`.
- The same Guardian-executable proposal built by each SDK, asserting equal approval expiration
  (user param 0), equal transaction `expiration_delta` (256), equal declared bound block, and
  equal proposal id, and that the id differs from the `SelfExecuted` build (SC-033).
- An envelope captured from a **different** protocol line, asserting both SDKs and the
  server refuse it with `GUARDIAN_EXECUTION_PROTOCOL_MISMATCH` and never attempt
  deserialization (SC-010).
- An unsupported `format_version`, asserting refusal before deserialization.
- A corrupted-checksum envelope, asserting `GUARDIAN_EXECUTION_REQUEST_CODEC`.

## Offline proposals and Guardian execution

Decided 2026-10-01. A proposal created offline (`create_proposal_offline`,
`createSwitchGuardianProposalOffline`) is always self-executed, whatever the client's execution
mode: it never reaches Guardian, so Guardian cannot execute it. Both SDKs build it without a stored
request and without the Guardian-executable bounds, and document this on the offline methods. It is
not refused on a Guardian-executable client, because the offline switch is the recovery path when
the current Guardian is unreachable. Exporting a proposal that Guardian holds does not change
Guardian's copy: the export is a self-execution and offline-signing channel, and Guardian keeps the
stored request it was created with.
