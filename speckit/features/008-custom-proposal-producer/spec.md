# Feature Specification: Custom Proposal Producer API

**Feature Branch**: `008-custom-proposal-producer`  
**Created**: 2026-06-01  
**Status**: Draft  
**Input**: User description: "Custom proposal producer API (Layer 2 of issue #266): bring-your-own-transaction SDK seam (propose_custom + prepare_custom_execution) so an integration that owns a custom proposal type can create and execute custom-type proposals through the Guardian multisig SDKs, mirroring the existing create/sign/execute flow."

## Context *(why this feature exists)*

Guardian's multisig custody follows a three-step lifecycle: a proposal is **created**, cosigners **sign** it, and once the signing threshold is met it is **executed** on-chain. The coordination server is deliberately type-agnostic — its integrity guarantees (validating the transaction summary against the account's current state, counting cosigner signatures, and the Guardian acknowledgment) do not depend on what kind of operation a proposal represents.

A prior change (issue #266) made the **coordination and signing** half of that lifecycle work for *any* proposal type, including types the SDK does not model (e.g. an agglayer bridge note, an arbitrary dApp transaction). Such proposals are bucketed as "custom", carry their original label for display, and can be listed, reviewed, signed, exported, and imported through the standard SDK.

What is still missing is the **producer** half. An integration that owns a custom proposal type cannot **create** or **execute** it through the SDK — those two ends require constructing the underlying transaction, which only the owning integration knows how to do. Today such an integration must drop down to low-level transport APIs and reimplement authentication, packaging, signature handling, and on-chain submission by hand. This feature adds a supported "bring-your-own-transaction" seam so the create and execute ends become first-class, mirroring the existing typed flow, while the SDK retains ownership of the security-critical, type-agnostic machinery.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Produce a custom-type proposal (Priority: P1)

An integration that owns a custom proposal type (the "producer") has built its own transaction. It hands the SDK that transaction in serializable form (the transaction is fully serializable to bytes, so the SDK can accept, transport, and later re-run it as an opaque blob without understanding what it does) together with a free-form type label, and asks the SDK to create a proposal from it. The SDK derives the canonical transaction summary, packages it with the producer's signature, and registers it with Guardian as a pending proposal — without the producer having to handle authentication or wire formatting.

**Why this priority**: This is the entry point of the whole lifecycle. Without a supported way to create a custom proposal, the coordination/signing capability delivered by #266 has no first-class producer and remains reachable only through low-level workarounds.

**Independent Test**: Provide a valid transaction and a custom label (e.g. `"b2agg"`); confirm a pending proposal appears in Guardian, carries the custom label, and is visible to and signable by cosigners through the existing SDK flow.

**Acceptance Scenarios**:

1. **Given** a loaded multisig account and a valid producer-built transaction, **When** the producer creates a proposal with a custom label, **Then** the proposal is registered as pending and carries the custom label verbatim.
2. **Given** that pending custom proposal, **When** other cosigners list and sign it through the standard SDK, **Then** their signatures are accepted and counted toward the threshold exactly as for built-in proposal types.
3. **Given** a transaction that is not valid for the account's current state, **When** the producer attempts to create a proposal, **Then** creation fails with a clear error and nothing is registered.

---

### User Story 2 - Execute a signed custom-type proposal (Priority: P2)

Once a custom proposal has collected enough cosigner signatures, the party holding the transaction recipe (typically the producer) calls the SDK's prepare-execution step, re-supplying the matching transaction. The SDK confirms the transaction reproduces the same summary commitment the cosigners signed, fetches the Guardian acknowledgment, and returns the validated advice (the collected cosigner signatures plus the ack). The integration folds that advice into its own rebuilt transaction and submits it on-chain through its own client. The SDK never submits the custom transaction itself, but the producer never has to assemble signatures, request the acknowledgment, or reimplement authentication.

**Why this priority**: Completes the lifecycle. Creation plus signing without execution leaves custom proposals stuck; execution is required to deliver end-to-end value. It is P2 only because it depends on a proposal existing (US1).

**Independent Test**: Take a custom proposal that has reached its threshold, re-supply the matching transaction to the prepare step, confirm the SDK returns advice and the integration's submission advances the account state; then re-supply a *non-matching* transaction and confirm the prepare step is refused before any acknowledgment request or submission.

**Acceptance Scenarios**:

1. **Given** a custom proposal that has met its signing threshold and the matching transaction, **When** the producer calls prepare-execution and submits the advice-laden transaction, **Then** the transaction lands on-chain and the account state advances.
2. **Given** a custom proposal at threshold but a transaction that does **not** reproduce the signed summary, **When** the producer calls prepare-execution, **Then** it is rejected with a clear binding error **before** any acknowledgment is requested or anything is submitted on-chain.
3. **Given** a custom proposal that has **not** met its threshold, **When** the producer calls prepare-execution, **Then** it is refused with a "not ready" indication and no side effects.
4. **Given** a custom proposal exported and re-imported (offline flow), **When** a producer calls prepare-execution **and** supplies the serialized transaction request, **Then** it returns advice as in the online case; **and when** prepare-execution is attempted **without** the transaction request bytes, **Then** it is refused with a clear error and no side effects.

---

### User Story 3 - Cosigner experience is unchanged and consistent across SDKs (Priority: P3)

A cosigner reviewing and signing a custom proposal uses the same steps as for any built-in proposal type, and the producer capability behaves identically whether the integration is written against the Rust SDK or the TypeScript SDK.

**Why this priority**: Protects the value already delivered by #266 and the project's cross-SDK consistency guarantee. It is supporting/regression-oriented rather than new user-facing capability.

**Independent Test**: Exercise the full create→sign→execute flow for the same custom label through both SDKs and confirm equivalent behavior and outcomes; confirm built-in proposal types continue to work unchanged.

**Acceptance Scenarios**:

1. **Given** a custom proposal, **When** a cosigner lists and signs it, **Then** the steps and prompts are identical to a built-in proposal type and the cosigner reviews the raw transaction summary.
2. **Given** the same custom scenario run through the Rust SDK and the TypeScript SDK, **When** each produces, signs, and executes, **Then** the observable behavior and results match.
3. **Given** existing built-in proposal types, **When** they are created, signed, and executed, **Then** their behavior is unchanged by this feature.

---

### Edge Cases

- **Mismatched transaction at execution**: the re-supplied transaction does not reproduce the signed commitment → reject early with a binding error, before any acknowledgment request or on-chain submission.
- **Rebuilt request (fresh salt) at execution**: if the transaction request bytes the producer re-supplies to the prepare step differs from the one signed (e.g. rebuilt with a fresh salt) → different summary commitment → rejected by the FR-007 binding check before any acknowledgment request or submission. The SDK does not store the transaction request bytes (FR-015); the producer reproduces it from its own recipe, so it must preserve the salt and all inputs.
- **Invalid or non-executable transaction at creation**: the supplied transaction cannot produce a valid summary, or Guardian rejects it against current state → creation fails cleanly with nothing registered.
- **Undeserializable transaction bytes**: the supplied bytes are not a valid serialized transaction (corrupt, truncated, wrong version) → fail with a clear error at create or execute; nothing registered or submitted.
- **Empty, mis-cased, or multi-word custom label**: the label is trimmed and lowercased; the normalized result must be a non-empty single `[a-z0-9_]+` token. Empty, whitespace-containing, hyphenated, or otherwise non-conforming labels are rejected with a clear error and nothing is registered.
- **Execution before threshold**: refuse with a clear "not ready" outcome and no side effects.
- **Authentication failure / Guardian unavailable**: surfaced through the same error channels as built-in create/execute; the producer is not asked to handle auth.
- **Built-in label supplied through the producer path**: rejected with a clear error directing the caller to the typed API (see FR-021), so an opaque producer transaction can never be mis-routed to a built-in handler at parse, display, or execution time.
- **Execute called on a custom proposal without the transaction request bytes** (e.g. an imported proposal): rejected with a clear error explaining the producer must supply the serialized transaction request (see FR-022); no acknowledgment request, no submission.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The SDK MUST let a producer create a proposal from a caller-supplied transaction — provided in serializable (byte) form that the SDK treats as opaque — together with a custom proposal type label, and register it with Guardian as pending. The SDK MUST normalize the label by trimming and lowercasing it, and MUST require the normalized result to be a non-empty single token matching `[a-z0-9_]+` (lowercase `snake_case`, mirroring the built-in type labels); any other form (whitespace, hyphens, punctuation) is rejected with a clear error.
- **FR-002**: When creating a custom proposal, the SDK MUST derive the canonical transaction summary and commitment from the supplied transaction and use them as the proposal's identity, so the proposal is indistinguishable from a built-in one to cosigners and the server.
- **FR-003**: After normalization (FR-001), the SDK MUST preserve the custom label end-to-end (creation, listing, display) without collapsing it to a generic bucket value (e.g. the displayed label stays `b2agg`, not `custom`).
- **FR-004**: The SDK MUST reuse its existing authenticated Guardian coordination path for custom create and execute; producers MUST NOT have to implement authentication, signing of the request, or wire formatting themselves.
- **FR-005**: Cosigners MUST be able to list, review, and sign a custom proposal through the unchanged standard SDK flow, with signatures counted toward the threshold identically to built-in types.
- **FR-006**: The SDK MUST expose `prepare_custom_execution(proposal_id, transaction_request_bytes)` (Rust) / `prepareCustomExecution(proposalId, transactionRequestBytes)` (TS) that, for a threshold-met custom proposal, binding-checks the transaction request bytes, fetches the Guardian acknowledgment, and returns the validated advice (cosigner signatures + ack). The integration injects that advice into its own rebuilt transaction and submits it via its own client. The SDK MUST NOT itself submit a custom transaction. `execute_proposal` on a custom proposal MUST return a clear error pointing to `prepare_custom_execution`.
- **FR-007**: Before requesting the acknowledgment or submitting anything on-chain, execution MUST verify that the re-supplied transaction reproduces the exact summary/commitment the cosigners signed, and MUST fail fast with a clear binding error if it does not.
- **FR-008**: Execution MUST refuse a custom proposal that has not met its signing threshold, with a clear "not ready" outcome and no side effects.
- **FR-009**: The capability MUST be available with **symmetric behavior** in both the Rust and TypeScript SDKs: create (`propose_custom`), sign (unchanged), and execute-prep (`prepare_custom_execution`) follow the same model — the SDK assembles/validates advice; the integration rebuilds its transaction with its own recipe and submits. (Mechanical differences in how each language injects advice — Rust mutates a request's advice map, TS rebuilds via a builder — are internal to the integration's submit step and do not change the SDK API shape.)
- **FR-010**: The feature MUST NOT require changes to the coordination server; server-side acceptance of arbitrary proposal labels is already in place.
- **FR-011**: The feature MUST NOT regress existing built-in proposal types or the existing cosigner signing experience.
- **FR-012**: Documentation MUST explain the producer flow, the producer's responsibility to reproduce the exact transaction at execution, and that the SDK cannot interpret a custom transaction — so cosigners must review the raw transaction summary, not the label or description.
- **FR-013**: The end-to-end producer flow (create → sign → execute) for a custom label MUST be demonstrable through at least one example harness.
- **FR-014**: For v1, the SDK MUST require a full producer-supplied transaction (in serializable form) as the creation input and MUST derive the canonical transaction summary itself; it MUST NOT accept a pre-built transaction summary as a creation input. This keeps the SDK's executability check at creation and the binding re-verification at execution intact. (Accepting a pre-built summary is explicitly out of scope for v1.)
- **FR-015**: The serialized serialized transaction request MUST NOT be stored anywhere by the SDK or server — not embedded in proposal metadata, not persisted server-side. The integration owns its transaction recipe and supplies the transaction request bytes at create (to derive the summary) and at execute-prep (for the binding check). This keeps the server a minimal coordinator (FR-010) and means custom execution is performed by a party holding the recipe.
- **FR-016**: If the supplied bytes cannot be deserialized into a valid transaction request, the SDK MUST fail with a clear error and MUST NOT register (at create) or submit (at execute) anything.
- **FR-017**: Determinism across create→execute MUST be conveyed by the serialized transaction request itself: the replay-protection salt and all other inputs are part of it, so re-supplying it reproduces the signed summary commitment. The SDK MUST NOT generate or inject a salt for a producer-supplied transaction request — this differs from built-in types by necessity (built-in types let the SDK generate the salt, persist it in proposal metadata, and rebuild at execution; custom types have no SDK recipe to rebuild, so the transaction request bytes is the durable source of determinism).
- **FR-018**: `propose_custom` MUST return the created proposal. The producer retains its own transaction recipe (and the salt within it) to rebuild deterministically at execution; a transaction rebuilt with different inputs (e.g. a fresh salt) would produce a different summary commitment and is rejected by the FR-007 binding check.
- **FR-019** *(transaction-request contract — central definition)*: The producer-supplied **serialized transaction request** MUST be a serializable transaction *request* that fully defines the transaction to run against the account (its script, inputs, input/output notes, replay-protection salt, and any advice the custom transaction's own logic requires). It MUST NOT be a pre-built transaction summary, and MUST NOT be an already-proven/executed transaction. It MUST NOT include cosigner signatures or the Guardian acknowledgment — those are advice the SDK injects at execution. The SDK MUST treat the transaction request bytes as opaque (it MUST NOT interpret the script or semantics): it deserializes and locally executes it to derive the canonical summary at create and for the execute-time binding check. The SDK does not inject advice or submit; it returns the advice for the integration to fold into its own transaction. The injected advice (signatures + ack) MUST NOT change the summary commitment. This contract MUST be identical across the Rust and TypeScript SDKs.
- **FR-020** *(replay invariant)*: The replay invariant is **commitment-equivalence**, not byte-identity. At execution the SDK MUST accept any transaction request that re-derives a summary commitment equal to the proposal id, and MUST NOT require the bytes to be byte-identical to those supplied at creation (deserialize/reserialize may normalize bytes). The canonical serialization from FR-018 is the recommended one to persist and replay.
- **FR-021** *(built-in label routing)*: The producer (raw) path MUST reject a proposal type label that matches a built-in/modeled type, with a clear error directing the caller to the corresponding typed operation. Built-in proposals MUST be created through the typed API; the producer path is only for labels the SDK does not model. This prevents an opaque producer transaction from being mis-routed to a built-in handler at parse, display, or execution.
- **FR-022** *(export/import and offline)*: Exported and imported custom proposals MUST support review and signing identically to built-in types (label and summary round-trip; already delivered under #266). Executing a custom proposal — online or offline — additionally requires the producer-supplied serialized transaction request, which is NOT contained in the proposal or its exported form; the SDK MUST surface a clear error if asked to execute a custom proposal without the transaction request bytes. Bundling the transaction request bytes into the exported proposal is out of scope for v1 (the producer carries it separately).
- **FR-023** *(no side effects on failed execution)*: On a binding mismatch (FR-007), a not-ready proposal (FR-008), or a missing/undeserializable transaction request (FR-016/FR-022), execution MUST NOT request a Guardian acknowledgment and MUST NOT submit anything on-chain. This MUST be covered by explicit tests.

### Key Entities *(include if feature involves data)*

- **Custom proposal**: a pending multisig proposal whose type label is outside the SDK's modeled set; behaves like any proposal for coordination/signing but is produced and executed by the owning integration.
- **Producer-supplied serialized transaction request** *(central contract; see FR-019)*: a serializable transaction request that fully defines the transaction to run (script, inputs, notes, salt, producer advice). Excludes any pre-built summary, proven transaction, cosigner signatures, and the Guardian acknowledgment. The SDK treats it as opaque — deserializes and re-runs it to derive the summary — and is the input to both create and execute. Persisting it and re-supplying it (commitment-equivalent) at execute is the producer's responsibility.
- **Transaction summary / commitment**: the canonical representation of what a transaction does against the account's current state; its commitment is the proposal's identity and the value cosigners sign.
- **Proposal binding**: the invariant that the proposal's identity equals the commitment of the transaction being executed; enforced at execution to keep collected signatures valid.
- **Custom-type label**: the string identifying the proposal type, normalized to a non-empty lowercase `snake_case` token (`[a-z0-9_]+`, same shape as built-in labels) and preserved for display.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: An integrator can take a self-built transaction and complete the full create → sign → execute lifecycle for a custom proposal type using the SDK for all Guardian coordination, authentication, binding, and advice assembly — without reimplementing any lower-level transport/auth or hand-assembling signatures/acknowledgments. The integration remains responsible only for submitting the final advice-laden transaction (via the SDK's thin submit helper or its own Miden client).
- **SC-002**: 100% of execution attempts where the re-supplied transaction does not reproduce the signed commitment are rejected before any on-chain submission or acknowledgment request.
- **SC-003**: The complete custom lifecycle is demonstrated end-to-end in an example harness for a representative custom label (e.g. `"b2agg"`).
- **SC-004**: The Rust and TypeScript SDKs expose equivalent producer capability; an automated parity check confirms matching behavior with zero drift.
- **SC-005**: Existing built-in proposal types and the cosigner signing flow show no behavioral regression.
- **SC-006**: A producer integration requires no custom authentication code to create or execute a custom proposal (auth is handled entirely by the SDK).
- **SC-007**: In every failed-execution path (binding mismatch, not-ready, missing/undeserializable transaction request), no Guardian acknowledgment is requested and nothing is submitted on-chain — verified by explicit tests.
- **SC-008**: Attempting to create a proposal through the producer path with a built-in/modeled label is rejected 100% of the time with guidance to the typed API; no proposal is registered.

## Assumptions

- The coordination server already accepts arbitrary, non-empty proposal labels (delivered under #266); no further server work is required.
- Cosigner-side listing/signing of custom proposals already works (delivered under #266) and only needs to remain unbroken.
- The producer's transaction is fully serializable to bytes, so it can be supplied at create and re-supplied at execute as an opaque blob; the SDK does not need a per-type recipe to re-run it. (This is the enabling finding: the underlying transaction-request type round-trips through serialization.)
- Determinism is handled differently than the built-in flow, by necessity: built-in types persist the salt in proposal metadata and let the SDK rebuild the transaction at execution; custom types have no SDK recipe to rebuild, so the durable source of determinism is the integration's own transaction recipe, which it re-supplies at execute (preserving salt and all inputs). The replay invariant is commitment-equivalence, not byte-identity (FR-020); the binding check (FR-007) is the backstop against any mismatch.
- The creation input is always a full transaction in serializable form (not a pre-built summary) in v1; see FR-014.
- The serialized transaction is not stored by the SDK or server (FR-015); the integration owns its recipe and supplies the transaction request bytes at create and execute-prep. Consequently custom execution is performed by a party holding the recipe (typically the producer), not an arbitrary cosigner.
- "Custom" and "custom" refer to the same bucket — a proposal type the SDK does not model; the term used on user-facing surfaces follows the existing SDK vocabulary.

## Out of Scope

- A registry or plugin system that teaches the SDK to natively build/execute specific custom types (the SDK remains transaction-agnostic; the producer supplies the transaction).
- Operator-side restriction/allowlisting of which proposal types an account may submit (policy module; issues #182/#251).
- Any change to the coordination server (already handled in #266).
- Accepting a producer-supplied pre-built transaction summary as a creation input (v1 requires a full transaction; see FR-014). May be revisited in a later version.
- Storing the serialized transaction request anywhere (server metadata, client cache). v1 does not persist it (FR-015); the integration re-supplies it from its own recipe. Trade-off accepted: only a recipe-holder can execute a custom proposal (an arbitrary cosigner cannot), in exchange for a minimal server and the strongest binding (against the actual submitted transaction).
