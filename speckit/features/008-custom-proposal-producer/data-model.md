# Data Model: Custom Proposal Producer API

This feature introduces **no new persisted/wire data** (server unchanged, FR-010). The entities below are SDK-level inputs/outputs and the invariants that bind them. Field names are conceptual; concrete Rust/TS shapes are in `contracts/sdk-api.md`.

## Entities

### TransactionRequestBytes *(new input; central contract — FR-019)*
The producer-built transaction, supplied to the SDK as opaque serialized bytes.

| Field | Type (conceptual) | Notes |
|---|---|---|
| `bytes` | binary (Rust `Vec<u8>` / TS `Uint8Array`; base64 at JSON boundaries) | Serialized `miden_client::TransactionRequest`. |

- **Contains** (inside the serialized request): script, transaction inputs, input/output notes, replay-protection **salt**, and any producer advice the custom logic needs.
- **Excludes**: pre-built summary; already-proven/executed transaction; cosigner signatures; Guardian acknowledgment (the SDK injects signature/ack advice at execute).
- **Validation**:
  - MUST deserialize into a valid `TransactionRequest` (else FR-016 error).
  - MUST locally execute to yield a `TransactionSummary` (else creation fails, US1.3 / FR-002).
  - At execute, MUST re-derive a summary whose commitment equals the proposal id (FR-007/FR-020).
- **Lifecycle**: produced by the integration → supplied at `propose_custom` (SDK derives the summary, does not store the transaction request bytes) → the integration keeps its own recipe → re-supplied at `prepare_custom_execution` for the binding check. Not stored by the SDK or server (FR-015).

### CustomProposalLabel
The free-form proposal type string for a producer proposal.

| Field | Type | Notes |
|---|---|---|
| `proposal_type` | string | Lowercase `snake_case` token (`[a-z0-9_]+`), same shape as built-in labels. |

- **Normalization & validation** (in both SDKs, FR-001):
  - Trimmed and lowercased; the result MUST be non-empty and match `[a-z0-9_]+` (single token, no whitespace/hyphens/punctuation) → else rejected with a clear error.
  - MUST NOT equal a built-in/modeled label (`add_signer`, `remove_signer`, `change_threshold`, `update_procedure_threshold`, `switch_guardian`, `consume_notes`, `p2id`) → else rejected with guidance to the typed API (FR-021).
  - The server still accepts any non-empty string (unchanged); normalization is enforced by the SDKs, not the server.
- Preserved end-to-end after normalization (FR-003); buckets to the `Custom`/`custom` SDK type for behavior (#266).

### Proposal *(existing; reused unchanged)*
The pending multisig proposal as already modeled by the SDK after #266.

| Field | Type | Notes |
|---|---|---|
| `id` / `commitment` | hex | Deterministic = `tx_summary` commitment; the value cosigners sign. |
| `tx_summary` | summary | Derived by the SDK from the transaction request bytes; what the server validates and what is signed. |
| `transaction_type` | enum | `TransactionType::Custom` / `'custom'` for custom proposals (no enum change this feature). |
| `metadata.proposal_type` | string | The raw `CustomProposalLabel`, preserved. |
| `signatures` | list | Collected cosigner signatures over the commitment. |
| `status` | enum | `pending → ready` (threshold met) → `finalized`; unchanged lifecycle. |

### ProposalBinding *(invariant, not stored)*
The relationship that keeps collected signatures valid at execution.

- **Rule**: `commitment(execute_for_summary(deserialize(transaction_request_bytes)))` **==** `proposal.id`.
- Enforced in `prepare_custom_execution` **before** any ack request (FR-007/FR-020/FR-023). On violation → binding-mismatch error, no side effects.

### Advice (cosigner signatures + GuardianAck) *(SDK output)*
Type-agnostic VM advice the SDK assembles and **returns** to the integration.

- Cosigner signatures (keyed by signer commitment over the tx_summary commitment) + the Guardian acknowledgment.
- The integration injects it into its own rebuilt transaction (Rust: `advice_map_mut().extend(...)`; TS: `builder.extendAdviceMap(...)`); it **does not change** the committed summary (so the binding holds).

## Relationships

```
TransactionRequestBytes ──derive──▶ TransactionSummary ──commitment──▶ Proposal.id
   (integration-owned)                                                ▲
        │ supplied at create + execute-prep (binding)                 │ cosigners sign
        ▼                                                             │
  prepare_custom_execution ──returns──▶ Advice (sigs + ack) ◀── Proposal.signatures + GUARDIAN
        │
        ▼
  integration injects Advice into its rebuilt tx ──▶ submit on-chain (integration's client)
```

## State Transitions (proposal, unchanged lifecycle)

```
                 propose_custom (request bytes + label)
   [none] ───────────────────────────────────▶ pending
                                                  │  cosigners sign (existing flow)
                                                  ▼
                                                ready  (threshold met)
                                                  │  prepare_custom_execution (binding OK) → advice
                                                  │  integration injects advice + submits
                                                  ▼
                                              finalized (on-chain; account state advances)
```

Failure transitions (no state change, no side effects — FR-023):
- `propose_custom` with built-in label, undeserializable transaction request, or non-executable/invalid-for-state tx → **rejected**, nothing registered.
- `prepare_custom_execution` on a non-custom proposal, not-ready, undeserializable transaction request, or binding mismatch → **rejected**, no ack request.
