# SDK API Contract: Custom Proposal Producer

This feature adds **no HTTP/gRPC endpoints** (server unchanged, FR-010). The contract is the **SDK surface** added to both clients, and it is **symmetric** (Constitution II, FR-009, FR-019). Signatures below are illustrative; the binding rules and error semantics are normative. The SDK never builds or interprets a custom transaction — it assembles validated advice; the integration applies that advice to its own transaction and submits it through the SDK's thin `submit_transaction` / `submitTransaction` helper (present in both SDKs).

## Shared concepts
- **Transaction request bytes**: serialized `TransactionRequest` bytes (Rust `&[u8]`, TS `Uint8Array`). Not stored by the SDK or server (FR-015); the integration owns its recipe and supplies it at create and execute-prep.
- **Label**: `proposal_type`, normalized in both SDKs to a lowercase `snake_case` token (trim + lowercase, then require `[a-z0-9_]+`); MUST NOT be a built-in/modeled label (FR-021). The server still accepts any non-empty string; normalization is SDK-side.
- **Binding**: `commitment(summary(deserialize(transaction_request_bytes))) == proposal.id`, enforced in `prepare_custom_execution` before any ack/submit (FR-007/FR-020/FR-023).
- **Advice**: the cosigner signatures + GUARDIAN acknowledgment, keyed for the transaction advice map — type-agnostic, returned by the SDK for the integration to apply.

## Rust — `MultisigClient` (`crates/miden-multisig-client/src/client/proposals.rs`)

```rust
/// Create a proposal from a producer-built transaction (issue #266).
/// `transaction_request_bytes`: serialized TransactionRequest bytes; `proposal_type`: free-form,
/// non-empty, not a modeled label.
pub async fn propose_custom_transaction(&mut self, transaction_request_bytes: &[u8], proposal_type: &str) -> Result<Proposal>;

/// For a threshold-met custom proposal: binding-check the transaction request bytes, fetch the
/// GUARDIAN ack, and return the advice to inject into the integration's own
/// transaction. Does NOT submit.
pub async fn prepare_custom_execution(
    &mut self,
    proposal_id: &str,
    transaction_request_bytes: &[u8],
) -> Result<Vec<SignatureAdvice>>;   // SignatureAdvice = (Word, Vec<Felt>)

/// Submit an integration-built transaction (advice already injected via
/// `request.advice_map_mut().extend(advice)`).
pub async fn submit_transaction(&mut self, request: TransactionRequest) -> Result<()>;
```

Integration execute flow (Rust): `let advice = client.prepare_custom_execution(id, &transaction_request_bytes).await?; let mut req = deserialize_transaction_request(&transaction_request_bytes)?; req.advice_map_mut().extend(advice); client.submit_transaction(req).await?;`

## TypeScript — `Multisig` (`packages/miden-multisig-client/src/multisig.ts`)

```ts
createCustomProposal(transactionRequestBytes: Uint8Array, proposalType: string, nonce?: number): Promise<Proposal>;

// Binding-check, fetch ack, return advice. Does NOT submit.
prepareCustomExecution(proposalId: string, transactionRequestBytes: Uint8Array): Promise<AdviceMap>;

// Submit an integration-built transaction (advice already injected via the builder).
submitTransaction(request: TransactionRequest): Promise<void>;
```

Integration execute flow (TS): `const advice = await multisig.prepareCustomExecution(id, transactionRequestBytes); const finalReq = myBuilder.extendAdviceMap(advice).build(); await multisig.submitTransaction(finalReq);`

## Behavioral contract (both SDKs)
- `propose_custom_transaction` MUST: normalize the label (trim + lowercase) and reject it if empty, not `[a-z0-9_]+`, or a modeled label (FR-001/FR-021); deserialize the bytes (else decode error, FR-016); derive the summary via `execute_for_summary`; package with the label + producer signature; push via the authenticated guardian client; return the proposal. The bytes are **not stored**.
- `prepare_custom_execution` MUST: reject a non-custom proposal; if not ready → not-ready error with **no side effects** (FR-008/FR-023); deserialize the bytes (else error); derive the summary and assert `commitment == proposal_id` (else binding-mismatch error, **before** the ack request, FR-007/FR-020/FR-023); fetch the GUARDIAN ack; return the advice. It MUST NOT submit.
- `execute_proposal`/`executeProposal` on a custom proposal MUST return a clear error pointing to `prepare_custom_execution`.
- `submit_transaction`/`submitTransaction` is a thin helper that submits an integration-built request (advice already applied) through the SDK's account client; it does not build, interpret, or re-validate the transaction.
- Built-in `propose_transaction`/`execute_proposal` and `list`/`sign` are unchanged (FR-005/FR-011).

## Error contract (stable boundary errors — Constitution IV)

| Condition | Rust | TS | Side effects |
|---|---|---|---|
| Label empty or not `[a-z0-9_]+` (after trim+lowercase) | `InvalidConfig` | thrown error | none registered |
| Label is built-in/modeled (after normalize) | `UnsupportedTransactionType` | thrown error | none registered |
| Transaction request undeserializable | `InvalidConfig` ("failed to decode transaction request") | thrown error | none |
| Create tx invalid for state | existing push/`verify_delta` error | equivalent | none registered |
| Prepare: non-custom proposal | `UnsupportedTransactionType` | thrown error | none |
| Prepare: not ready | `ProposalNotReady { collected, required }` | thrown error | **no ack** |
| Prepare: binding mismatch | `InvalidConfig` ("...does not match the signed proposal commitment") | thrown error | **no ack** |
| `execute_proposal` on custom | `UnsupportedTransactionType` (→ use prepare) | thrown error | none |

## Parity assertions (SC-004)
A shared fixture (transaction request bytes + label) MUST yield, in both SDKs: the same proposal `id`/commitment from `propose_custom_transaction`, and the same accept/reject outcome for each row of the error table. Both SDKs expose the same `submit_transaction`/`submitTransaction` helper; only the advice-injection mechanism differs by language (Rust mutates the request's advice map in place; the immutable wasm request is rebuilt via a builder).

## Out of contract (v1)
- No pre-built-summary creation input (FR-014).
- No transaction-request persistence anywhere — server or client (FR-015); the integration re-supplies it.
- The SDK does not autonomously build or interpret a custom transaction; the integration builds it and applies the advice. Final submission goes through the thin `submit_transaction`/`submitTransaction` helper (symmetric in both SDKs) or the integration's own Miden client.
