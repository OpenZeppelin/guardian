# SDK API Contract: Guardian Prove and Commit

Normative SDK surface for #254. Signatures are illustrative; the binding rules, state
vocabulary, and error semantics are normative and MUST be symmetric across both SDKs
(Constitution II, FR-033).

Two additions per layer: **client-level configuration** deciding whether proposals are
created Guardian-executable, and methods to request and observe a Guardian execution. No
existing method signature changes.

## Naming rule

`Guardian` appears **only on the verb that delegates**, and only where a local counterpart
exists to contrast with:

- `requestGuardianExecution` / `request_guardian_execution` — keeps it, because the multisig
  SDKs already have `executeProposal` / `execute_proposal` meaning *execute locally*. Without
  the prefix the contrast would rest on the reader noticing that "request" implies delegation.
- `executionStatus`, `currentExecution` and the shared `ProposalExecution` type — drop it.
  Guardian records nothing for a local execution: self-execution creates no reservation and no
  states, so there is no other kind of execution to distinguish from. The prefix would be
  contrasting with something that cannot exist.
- The **base clients** carry no prefix at all (`executeDeltaProposal`,
  `getDeltaProposalExecution`, `getCurrentExecution`) — they have no local-execution
  counterpart, so nothing needs disambiguating, and every base-client call goes to Guardian
  anyway.

The type is `ProposalExecution` in both layers and both languages. Base-client method and
gRPC operation names keep the `delta_proposal` family convention already used by
`get_delta_proposal` / `push_delta_proposal`.

## Shared concepts

- **Execution mode** — client configuration set once at construction, defaulting to
  self-executed (FR-009). When set to Guardian-executable, the SDK attaches the serialized
  `TransactionRequest` it already holds at creation; the caller supplies nothing extra
  (FR-011). It is never attachable after creation, and never negotiated with the server —
  the SDK does not query server capability, and the server independently decides whether it
  offers execution (FR-009, FR-021).
- **Execution handle** — `(account_id, proposal_id)`. No opaque token; polling is
  idempotent.
- **Execution state** — exactly five values: `pending`, `proving`, `submitted`, `landed`,
  `failed` (see `execution-api.md`). Both SDKs MUST model it as a closed type and handle
  every variant exhaustively (`never` check in TS, full `match` in Rust). Adding a state is
  a breaking SDK change. SDKs MUST NOT invent additional states, and MUST NOT collapse
  these into a boolean.
- **No Miden capability required — base clients only.** Requesting execution and polling
  state MUST work from a **base client** (`crates/client`, `packages/guardian-client`) with
  no Miden connectivity, no keystore beyond the auth key, and no transaction-building
  ability (FR-034). The multisig SDKs expose the same operations for convenience, but
  constructing one still requires a Miden client, so they do **not** satisfy this guarantee
  and MUST NOT be documented as the thin-client path.

## Base clients

### Rust — `crates/client`

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

### TypeScript — `packages/guardian-client`

```ts
executeDeltaProposal(accountId: string, proposalId: string): Promise<ProposalExecution>;
getDeltaProposalExecution(accountId: string, proposalId: string): Promise<ProposalExecution>;
getCurrentExecution(accountId: string): Promise<ProposalExecution | null>;
```

`server-types.ts` MUST mirror the response envelope exactly, and every error code in
`execution-api.md` MUST be added to the client's error-code vocabulary in the same PR.

## Multisig SDKs

**No existing method signature changes.** The attachment decision is client
configuration, set once at construction (FR-009). This keeps `propose_transaction` and
`propose_custom_transaction` byte-identical in shape across both SDKs — which also avoids a
parity break, since Rust has no optional parameters and a per-call option would be a
breaking signature change there but free in TS.

### Rust — `crates/miden-multisig-client`

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
client's configured mode.

### TypeScript — `packages/miden-multisig-client`

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
`createSwitchGuardianProposal`, `createCustomProposal`) — they hold the `TransactionRequest`
before pushing, so under `guardian_executable` they serialize and attach it through an
**internal request-bearing creation path**. Their public signatures are unchanged. Built-in
methods also construct a finite-expiration transaction as described below; `createCustomProposal`
attaches the producer's opaque request unchanged. The
low-level `createProposal(nonce, txSummaryBase64, metadata)` receives no
`TransactionRequest` and cannot attach one; on a `guardian_executable` client it MUST
refuse with an explicit error rather than silently create a proposal the server can never
execute.

An integration that genuinely needs both modes constructs two clients. A per-call override
can be added later without breaking either SDK (TS: optional parameter; Rust: an additive
builder) — it is omitted here because the cost/benefit does not vary meaningfully between
two proposals from the same integration.

Omitting `executionMode` MUST behave as `self_executed`, so existing callers are unaffected
by an SDK upgrade (FR-009).

## Behavioral contract (both SDKs)

Creation under a `GuardianExecutable`-configured client MUST:

1. For every **built-in** proposal family, construct the transaction with the shared finite
   expiration policy from FR-051 (256 blocks), then derive the summary exactly as today. How the
   builder expresses that policy is family-specific on Miden 0.15: send scripts receive the
   delta while being built, no-script requests use `TransactionRequestBuilder::expiration_delta`,
   and Guardian-owned custom scripts call `tx::update_expiration_block_delta`.
2. For an opaque **custom producer** request, preserve the supplied serialized request exactly.
   Miden 0.15 exposes no mutation API on `TransactionRequest`, and its builder rejects combining
   `expiration_delta` with a custom script, so the SDK cannot generically retrofit expiration
   without rebuilding producer-owned code. The producer MUST construct the custom transaction to
   expire finitely; Guardian verifies the resulting executed/proven expiration and refuses it
   before the boundary otherwise.
3. Serialize the `TransactionRequest` it already holds and wrap it in the FR-014 envelope
   (format version, protocol line, serializer id, checksum).
4. Attach the envelope to the proposal payload and push as normal.

### What identity is and is not preserved (FR-012)

`TransactionSummary` on the pinned Miden 0.15 line commits to the account delta, input notes,
output notes, and salt. It does **not** include expiration
(`miden-protocol-0.15.3/src/transaction/tx_summary.rs:20-24,77-86`). Therefore:

- Attaching `transaction_request` MUST NOT change the derived proposal identity, because the
  server derives it from `tx_summary` alone via `delta_proposal_id`.
- For the same effects and salt, adding the built-in finite-expiration policy MUST leave the
  summary and proposal ID unchanged across `SelfExecuted` and `GuardianExecutable` modes. The
  serialized request bytes legitimately differ; the signed summary does not.
- **`SelfExecuted` output remains byte-identical to pre-feature output.** No expiration is added,
  no envelope is attached, and nothing about the payload shape changes (SC-009).

Expiration is consequently an unsigned liveness constraint, not part of the cosigner-authorized
effects. Guardian v1 still MUST NOT rewrite it: preserving the proposal builder's exact request
keeps transaction construction in the SDK/producer boundary (FR-013), preserves the FR-014
checksum, and avoids inventing transformation semantics for opaque custom scripts. The server's
role is enforcement: it refuses any executed transaction whose resulting expiration is non-finite
or outside its configured horizon.

The SDK MUST NOT enforce its own size limit. The limits in FR-016 are server configuration,
and capability negotiation is prohibited (FR-009), so a client-side copy could only be a
guess that drifts from the deployment it talks to. An oversized request is refused by the
server with its typed error, which the SDK surfaces unchanged. The cost is one wasted
round trip carrying the payload on an error path; the benefit is one source of truth.

Creation under a `SelfExecuted`-configured client — including any client that sets nothing —
MUST produce a payload with **no** `transaction_request` field, byte-identical to a
pre-feature proposal (FR-010, SC-009).

Neither SDK may query, cache, or branch on the server's proving capability. A client
configured `GuardianExecutable` attaches the request unconditionally; if that server does
not offer execution, it is surfaced at execution time by `GUARDIAN_PROVING_UNAVAILABLE`,
not at creation (FR-009).

`request_guardian_execution` MUST:

1. Not build, execute, or prove anything locally (FR-034).
2. Surface synchronous refusals as typed errors carrying the stable codes from
   `execution-api.md` — never as free-form strings (AGENTS.md §12).
3. Return the accepted execution state; MUST NOT poll internally or block until landed.
   Any wait-for-completion helper MUST be a separate, explicitly-named call so the
   non-blocking behavior is visible in the API (no silent fallbacks).

`execution_status` MUST return the server's state verbatim without collapsing
distinct states into a boolean, and MUST NOT treat `submitted` as either terminal success
or terminal failure — only `landed` and `failed` are terminal.

Both SDKs MUST surface `newly_accepted` and `proposal_exists` rather than dropping them.
`newly_accepted` is how a caller distinguishes a fresh execution from an idempotent hit on an
existing one, and MUST NOT be inferred from HTTP status (gRPC has no 202). A `failed`
`proposal_exists` is a fact about storage, not permission: a retry is permitted only when
the state is `failed` **and** `proposal_exists` is `true`. An SDK MUST NOT present a
`submitted` execution as retryable (retry is forbidden), and MUST NOT offer a retry when the
proposal was deleted with its candidate — that would fail with proposal-not-found; the caller
must create a new proposal.

Both SDKs MUST NOT expose any method that retries an execution reporting `submitted` — the
server refuses it (FR-030), and the SDK MUST NOT paper over that with client-side retry.

On an execution-conflict refusal, both SDKs MUST surface the blocking proposal id from the
error payload rather than discarding it, so a caller can act on the conflict without
polling every proposal (FR-036).

## Cross-language fixtures

A committed fixture set MUST pin the envelope contract so the two SDKs cannot drift
(mirroring `fixtures/miden-multisig-client/p2id-serial-vectors.json`):

- A serialized `TransactionRequest` envelope produced by each SDK for the same
  transaction inputs, asserting equal `format_version`, `protocol_line`, full
  `serializer_id` (including any prerelease identifier), and `checksum`.
- An envelope captured from a **different** protocol line, asserting both SDKs and the
  server refuse it with `GUARDIAN_EXECUTION_PROTOCOL_MISMATCH` and never attempt
  deserialization (SC-010).
- An envelope from the **same** protocol line but a different, unallowlisted
  `serializer_id`, asserting both SDKs and the server refuse it with
  `GUARDIAN_EXECUTION_PROTOCOL_MISMATCH` before deserialization.
- An unsupported `format_version`, asserting refusal before deserialization.
- A corrupted-checksum envelope, asserting `GUARDIAN_EXECUTION_REQUEST_CODEC`.
