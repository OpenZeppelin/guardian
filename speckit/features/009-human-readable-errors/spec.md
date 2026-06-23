# Feature Specification: Human-readable error messages for wallet users

**Feature Branch**: `009-human-readable-errors`  
**Created**: 2026-06-18  
**Status**: Draft  
**Input**: GitHub issue [#179](https://github.com/OpenZeppelin/guardian/issues/179) (OpenZeppelin/guardian): "Surface short, user-friendly error messages for common wallet failures instead of raw backend or transport errors." Authored by @MCarlomagno; assigned @zeljkoX, @MCarlomagno; milestone *Guardian #02 - M1*. Prior art referenced by the team: 0xMiden/wallet PRs [#273](https://github.com/0xMiden/wallet/pull/273) ("stop false 'Cannot reach the Miden node' banner") and [#247](https://github.com/0xMiden/wallet/pull/247) ("sync-mgr classification"), whose `connectivity-classify.ts` heuristic is the pattern to learn from.

## Problem

Guardian already emits a stable, machine-readable error `code` for every failure (`GuardianError::code()` in `crates/server/src/error.rs`), and the operator client already documents "branch on `code`, not the human message" (`packages/guardian-operator-client/src/types.ts`). What it does **not** provide is a message safe to show directly to a wallet end-user:

- The HTTP envelope's `error` field and the gRPC `Status` message are the `Display` string — developer/diagnostic text that embeds account IDs, commitments, nonces, and raw upstream/RPC error text (e.g. `"Commitment mismatch: expected 0xaa, got 0xbb"`, `"RPC unavailable: <raw transport error>"`). Handing that to an untrusted wallet is both unfriendly and a disclosure risk.
- The wallet-facing clients (`packages/miden-multisig-client`, `crates/client`) perform no error translation — they rethrow raw gRPC/transport errors.

Net effect: a wallet user sees raw backend or transport noise. This feature reshapes the error wire contract around a single user-safe `message`, with the server as the source of truth, and keeps the diagnostic `Display` text off the wire entirely (server logs only).

## Clarifications

### Session 2026-06-18 — post-review reshape (@zeljkoX, PR [#292](https://github.com/OpenZeppelin/guardian/pull/292))

Supersedes the additive design in the initial-draft session below. Guardian is early-phase with no external consumers locked to the error contract, so a clean breaking reshape is preferred over growing the existing flat envelope with an optional field per variant.

- Q: Additive field, or reshape the contract? → A: **Breaking reshape.** One error object, byte-identical on the HTTP body and the gRPC `Status.details`:

  ```json
  { "code": "authorization_failed", "message": "You're not an authorized signer for this account.", "meta": { "retryable": false } }
  ```

  - `code` — stable machine code (existing 37-value vocabulary, unchanged). Branch key + i18n key.
  - `message` — user-safe sentence, always present and safe to display. Only `code` is stable; wording can change.
  - `meta` — structured machine data; holds the fields that are top-level optionals today (`retryable`, `retry_after_secs`, `missing_permissions`, `paused_at`, `paused_reason`). Individual fields omitted when absent; `meta` omitted entirely when it would be empty.
- Q: What happens to the diagnostic `Display` text? → A: **Off the wire.** It is emitted to **server logs only**. The `Display` string (account IDs, commitments, `postgres://…`, raw RPC errors) is exactly what we don't want to hand an untrusted wallet; logging it instead of returning it removes the disclosure risk by construction and frees the plain field name `message` (the old diagnostic `error` field is gone).
- Q: Keep `success`? → A: **Dropped.** The HTTP status is the discriminator; gRPC never had it.
- Q: gRPC carrier? → A: `Status.code` unchanged; `Status.message` becomes the user-safe `message`; `Status.details` carries the same `{ code, message, meta }` object. Details is still required because the ~16 canonical gRPC codes can't recover our 37 stable `code` values (e.g. `authorization_failed` / `signer_not_authorized` both map to `PERMISSION_DENIED`).
- Q: Connectivity vs internal faults (raised by the Copilot review)? → A: **Explicit split.** Server-mapped connectivity faults (`network_error`, `rpc_unavailable`, `rpc_validation_failed`) get a connectivity-style `message` distinct from the single generic message used for pure internal faults (`storage_error`, `signing_error`, `configuration_error`, `data_unavailable`).

### Session 2026-06-18 — initial draft (partially superseded above)

- Q: Where does the human-readable mapping live — server or client? → A: **Server-side, single source of truth.** (Still in force.) Rationale: Constitution Principle I (bottom-up propagation) and II (HTTP/gRPC + Rust/TS parity) mean a per-consumer mapping would drift; the server already owns the stable `code`. Clients consume the message; they only need their own logic for *codeless* transport failures (see User Story 3).
- Q: Additive new field, or repurpose `error`? → A: ~~Additive `user_message`, keep `error`.~~ **Superseded:** breaking reshape to `{ code, message, meta }`, `error` removed from the wire (post-review session above).
- Q: Field name? → A: ~~`user_message`.~~ **Superseded:** `message`.
- Q: Coverage — every variant, or a curated subset? → A: **Every `GuardianError` variant** returns a `message`. (Still in force.) Internal/non-actionable variants collapse to one safe generic message.
- Q: Localization / i18n? → A: **Out of scope server-side.** (Still in force.) `code` is the stable localization key; `message` is the English default. Clients localize off `code`.

### Open questions for the team

- **OQ-2**: For internal faults, keep the existing granular `code`s (`storage_error`, `signing_error`, …) all sharing one generic `message` (this draft), or collapse them to a single `internal_error` code? Leaning keep-granular: the codes are useful in logs/metrics and cost nothing on the wire.
- **OQ-3**: Ship the starter copy catalog (Key Entities table) in this feature, or land the mechanism + a placeholder catalog and gate final wording on a UX/design review?
- **OQ-5**: Should `meta.retryable` be populated for **every** variant (cheap, lets clients drive retry UI uniformly), or only where retryability is non-obvious? The examples in this draft populate it broadly.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - A common, user-actionable failure reads as a plain sentence (Priority: P1)

A wallet user takes a normal action against a Guardian-backed account — proposing or signing a transaction, or refreshing account state — and it fails for a reason they can act on: the account is paused, they aren't an authorized signer, they're being rate-limited, or the proposal already has their signature. Instead of `"Authorization failed: signer 0x… not in policy"` they see a short sentence such as "You're not an authorized signer for this account."

**Why this priority**: This is the core of issue #179 and the primary daily wallet experience. Without it the wallet has nothing user-presentable to show on the most common failures and must either invent its own copy (drift) or surface raw text.

**Independent Test**: For each wallet-relevant `GuardianError` variant (auth, authorization, rate-limit, account-paused, proposal-conflict, insufficient-signatures), trigger the error through the HTTP and gRPC surfaces and assert the response object carries a non-empty `message` that is a complete plain-language sentence containing none of the variant's embedded identifiers, alongside the unchanged `code` and (where applicable) `meta`.

**Acceptance Scenarios**:

1. **Given** an account is paused, **When** a wallet attempts a mutating action, **Then** the error object carries `code = GUARDIAN_ACCOUNT_PAUSED`, a `message` like "This account is paused and can't approve transactions right now.", and `meta` carrying `paused_at`/`paused_reason`/`retryable: false` — with no pause metadata interpolated into the message text.
2. **Given** a signer who is not authorized for an account, **When** they attempt to sign, **Then** the object carries the authorization `code`, `message` like "You're not an authorized signer for this account.", `meta.retryable = false`, and no signer identifier in the message.
3. **Given** a rate-limited caller, **When** they retry too soon, **Then** `message` communicates "Too many requests — please try again shortly." and `meta` carries `retryable: true` and the exact `retry_after_secs` (the message stays vague; `meta` is the authoritative timing source).
4. **Given** a proposal the caller has already signed, **When** they sign again, **Then** the `message` is "You've already signed this transaction." and the stable `code` is unchanged from today.

---

### User Story 2 - Diagnostic detail never reaches the wallet (Priority: P1)

When Guardian hits an internal fault (storage error, signing error, configuration error, degraded data), the wallet user sees a single safe, generic message — never a file path, a raw RPC string, a stack trace, or a database detail. The diagnostic `Display` text is written to Guardian's logs for operators, but it is **not present on any wire surface**.

**Why this priority**: This is both a UX requirement and a security/disclosure requirement (Constitution Principle IV: stable boundary errors). Raw internal text leaking to an untrusted wallet client is a disclosure risk that exists today; removing the field from the wire eliminates it by construction.

**Independent Test**: Trigger each internal variant (`StorageError`, `SigningError`, `ConfigurationError`, `DataUnavailable`) and each server-mapped connectivity variant (`NetworkError`, `RpcUnavailable`, `RpcValidationFailed`); assert (a) the wire object contains no `error`/`success` field, (b) internal faults return the generic message and connectivity faults return the connectivity message, (c) `message` matches none of the disallowed patterns, and (d) the diagnostic `Display` text appears in the captured server log, not the response.

**Acceptance Scenarios**:

1. **Given** the storage backend fails, **When** any wallet request errors, **Then** `message` is the generic "Something went wrong on Guardian's side. Please try again.", `meta.retryable = true`, the diagnostic detail is logged server-side, and the wire object has no `error` field.
2. **Given** an upstream RPC is unreachable, **When** Guardian maps it to `RpcUnavailable`, **Then** `message` is the connectivity sentence "Guardian can't reach the network right now. Please try again." (distinct from the internal-fault generic message), `meta.retryable = true`, and the raw upstream text is logged, not returned.
3. **Given** any error, **When** the wire object is inspected, **Then** it contains exactly `code`, `message`, and (when non-empty) `meta` — no `success`, no `error`.

---

### User Story 3 - Connectivity failures get a friendly message even when Guardian was never reached (Priority: P2)

The wallet cannot reach Guardian at all — connection refused, DNS failure, timeout, TLS error, or a 5xx from an intermediary with no Guardian body. No `GuardianError` is produced because no Guardian handler ran. The wallet-facing client must still present a friendly connectivity message ("Can't reach Guardian right now. Check your connection and try again.") rather than `"Failed to fetch"` / `"transport error: …"`.

**Why this priority**: It closes the "transport errors" half of issue #179 and is exactly what wallet PR #273 solved. It's P2 because it lives in the client layer and is independent of the P1 server work. This is the only client-side message authoring this feature introduces.

**Independent Test**: With Guardian unreachable, drive a request through the TS multisig client and the Rust client; assert each surfaces a classified connectivity message and a stable connectivity category, and never surfaces the raw transport string as the primary message.

**Acceptance Scenarios**:

1. **Given** Guardian's endpoint refuses the connection, **When** the TS multisig client makes a call, **Then** the thrown error exposes a friendly connectivity `message` and a `network`/`unreachable` category — not the raw `"Failed to fetch"`.
2. **Given** a request times out, **When** the client classifies it, **Then** it is categorized as connectivity (not as a semantic Guardian error) and the message advises retrying.
3. **Given** the client receives a well-formed Guardian error object (server reachable), **When** it renders the message, **Then** it uses the server's `message` (User Story 1) and does **not** run the connectivity heuristic — the heuristic is a fallback for codeless failures only.

---

### User Story 4 - One stable, parity-preserving error object for branching and localization (Priority: P2)

Client authors (wallet, operator dashboard, Rust integrators) need to branch on failures and localize messages without string-matching English text. The stable `code` is the join key and localization key; `message` is the English default; `meta` is structured machine data. Rust and TypeScript clients expose the same `{ code, message, meta }` for an equivalent server error, and HTTP and gRPC carry the identical object.

**Why this priority**: It is the contract that lets the wallet localize (matching its `t('key')` model) and prevents the fragile message-string matching that wallet `connectivity-classify.ts` was forced into.

**Independent Test**: For a representative error produced on both HTTP and gRPC, assert identical `{ code, message, meta }` across surfaces and across the Rust and TS clients; assert a client given an unknown future `code` falls back gracefully (uses the server `message`, else a generic message) without throwing.

**Acceptance Scenarios**:

1. **Given** the same logical error, **When** observed via the HTTP body and via the gRPC `Status.details`, **Then** both expose the identical `{ code, message, meta }`, and `Status.message` equals `message`.
2. **Given** the same logical error, **When** observed via the Rust client and the TS multisig client, **Then** both expose the same `code`, `message`, and `meta` (Constitution parity).
3. **Given** a `code` the client predates, **When** it handles the error, **Then** it shows the server-provided `message`; if that is somehow absent, it shows a generic fallback — never an empty or raw message.

---

### Edge Cases

- **Breaking change, contained**: This reshapes the existing error wire contract. Every in-repo consumer (operator-client, Rust client, multisig client, examples, and their pinned error-shape tests) is updated in the same change (Constitution I, bottom-up). No external consumer is assumed locked to the old shape (see Assumptions).
- **`message` text is not a contract**: wording MAY change between releases without a version bump; only `code` is stable. Clients branching on message text is explicitly unsupported.
- **`meta` is omitted when empty**: an error with no structured side-data (e.g. a plain validation failure) serializes as `{ code, message }` with no `meta` key. Individual `meta` fields are likewise omitted when absent.
- **Rate-limit timing**: `message` may say "try again shortly" but `meta.retry_after_secs` is the authoritative timing source; the two must not contradict (message stays vague, meta stays exact). The HTTP `Retry-After` header behaviour is preserved.
- **Internal vs server-mapped connectivity**: `network_error`/`rpc_unavailable`/`rpc_validation_failed` are connectivity-style (Guardian reached, the *network behind it* didn't answer) and are distinct from `storage_error`/`signing_error`/`configuration_error`/`data_unavailable` (Guardian's own fault). Both keep their granular `code`; only the `message` text differs by group.
- **gRPC consumers that ignore `Status.details`** still get `Status.code` (unchanged) and `Status.message` (now the user-safe message) — never the old diagnostic string.
- **Codeless server responses**: a 5xx from a proxy in front of Guardian (no Guardian object) is a connectivity case (User Story 3), handled client-side.
- **Empty/oversized**: `message` is always non-empty, a short single sentence; it never embeds a truncated dump of the underlying error.

## Requirements *(mandatory)*

### Functional Requirements

**Server — message authoring (Story 1, 2)**

- **FR-001**: `GuardianError` MUST expose a `message()` accessor returning a short, plain-language, end-user-safe string for **every** variant. The set of `code()` values and their HTTP/gRPC status mappings MUST remain unchanged by this feature.
- **FR-002**: `message()` output MUST be safe-by-construction: it MUST NOT contain account IDs, commitments, nonces, signer IDs, file paths, URLs, raw upstream/RPC error text, or any value interpolated from the variant's payload fields. It is always non-empty and a single short sentence.
- **FR-003**: The diagnostic `Display` text MUST NOT appear on any wire surface (HTTP body or gRPC). It MUST be emitted to server logs for operators. (Sanitization is achieved by removal, not by filtering a returned field.)
- **FR-004**: Server-mapped connectivity faults (`network_error`, `rpc_unavailable`, `rpc_validation_failed`) MUST map to a connectivity-style `message` that is distinct from the single generic `message` used for pure internal faults (`storage_error`, `signing_error`, `configuration_error`, `data_unavailable`). User-actionable variants MUST map to a category-appropriate `message` (see Key Entities catalog).
- **FR-005**: `message` text is explicitly **not** part of the stable wire contract; only `code` is stable. Documentation on the error type/clients MUST state that consumers branch and localize on `code`, never on `message`.

**Server — unified wire object (Story 4)**

- **FR-006**: The HTTP error body and the gRPC `Status.details` MUST both carry the identical object `{ code, message, meta }`. `meta` is structured machine data carrying any of `retryable`, `retry_after_secs`, `missing_permissions`, `paused_at`, `paused_reason` that apply to the variant; absent fields are omitted and `meta` is omitted entirely when it would be empty.
- **FR-007**: The HTTP error body MUST NOT include the legacy `success` or `error` fields. This is a deliberate breaking change to the envelope shape.
- **FR-008**: For gRPC, `Status.code` MUST remain as today, `Status.message` MUST be the user-safe `message`, and `Status.details` MUST carry the `{ code, message, meta }` object. (`details` is required because the canonical gRPC codes cannot recover the 37 stable `code` values.)
- **FR-009**: The HTTP and gRPC surfaces MUST return the same `code`, `message`, and `meta` for the same logical error (Constitution II parity / invariant "HTTP and gRPC preserve the same error meanings").
- **FR-010**: The HTTP OpenAPI error schema and `crates/server/proto/guardian.proto` MUST be updated and the committed specs regenerated per the mandatory Contract-Change Workflow (`AGENTS.md` §4): `cargo run --features evm --bin gen-openapi -- docs`.

**Clients — consumption and parity (Story 3, 4)**

- **FR-011**: The Rust client (`crates/client`) MUST expose the server-provided `code`, `message`, and `meta` on its error type so Rust integrators can present and branch on them without re-parsing the body/status.
- **FR-012**: The TypeScript multisig client (`packages/miden-multisig-client`) MUST expose `code`, `message`, and `meta` on the errors it surfaces, and the operator client's error type (`GuardianOperatorHttpErrorData` in `packages/guardian-operator-client`) MUST be replaced by the new `{ code, message, meta }` shape (camelCase `meta` fields per the package convention). Both packages' parsers and pinned tests MUST be updated.
- **FR-013**: For **codeless transport/connectivity failures** (no Guardian object), each wallet-facing client MUST classify the failure and supply a generic connectivity `message` plus a stable connectivity category; it MUST NOT surface the raw transport string as the primary message. When a Guardian object IS present, the client MUST use the server `message` and MUST NOT run the connectivity heuristic.
- **FR-014**: The Rust and TypeScript clients MUST remain behaviorally aligned: the same logical error yields the same `code`, `message`, and `meta` and an equivalent connectivity category in both (Constitution II).

**Scope and process**

- **FR-015**: This is a coordinated breaking change. All affected layers MUST be updated in one change per `AGENTS.md` §4: server error type → proto + OpenAPI (regenerated) → Rust client → TS clients (esp. operator-client) → multisig SDK → examples, with the matching smoke harnesses (`examples/web` / `examples/smoke-web`; `examples/demo`; `examples/operator-smoke-web`) exercised.
- **FR-016**: The `code` vocabulary and the HTTP/gRPC status mappings MUST NOT change. This feature reshapes the envelope and adds messaging; it does not re-taxonomize errors.

### Key Entities *(include if feature involves data)*

- **Error object**: `{ code, message, meta }`. Identical on the HTTP error body and the gRPC `Status.details`. The only error shape on the wire.
- **`code`** (existing vocabulary, unchanged): stable machine-readable code. The join key for branching and the localization key for client i18n.
- **`message`**: short single-sentence end-user-safe string, English, authored server-side per `GuardianError` variant. Always present; safe to display verbatim. Not part of the stable contract (wording may change).
- **`meta`**: structured machine side-data; carries `retryable`, `retry_after_secs`, `missing_permissions`, `paused_at`, `paused_reason` when applicable. Omitted entirely when empty.
- **Generic internal message**: the single safe string for pure internal faults (`storage_error`, `signing_error`, `configuration_error`, `data_unavailable`) and the client's last-resort fallback for unknown codes.
- **Connectivity message (server-mapped)**: the distinct sentence for `network_error`/`rpc_unavailable`/`rpc_validation_failed` (Guardian reached; the network behind it did not answer).
- **Connectivity category (client-side)**: a small stable enum (e.g. `network` | `unreachable` | `timeout`) produced by the client's transport-error classifier for codeless failures, modeled on wallet `connectivity-classify.ts`. Carries its own generic `message`.
- **Starter copy catalog (illustrative, non-binding — pending UX review, OQ-3)**: representative mapping from existing `code` → proposed `message` and `meta.retryable`:

  | `code` | Category | Proposed `message` | `meta.retryable` |
  |---|---|---|---|
  | `GUARDIAN_ACCOUNT_PAUSED` | account state | "This account is paused and can't approve transactions right now." | false |
  | `authentication_failed` | auth | "Your session has expired. Please sign in again." | false |
  | `authorization_failed`, `signer_not_authorized` | authz | "You're not an authorized signer for this account." | false |
  | `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` | authz | "You don't have permission to do that." | false |
  | `rate_limit_exceeded` | throttling | "Too many requests — please try again shortly." | true (+`retry_after_secs`) |
  | `insufficient_signatures` | proposal flow | "This transaction still needs more signatures." | false |
  | `proposal_already_signed` | proposal flow | "You've already signed this transaction." | false |
  | `conflict_pending_delta`, `conflict_pending_proposal`, `pending_proposals_limit` | proposal flow | "There's already a pending change for this account. Finish or cancel it first." | false |
  | `proposal_not_found`, `account_not_found`, `state_not_found`, `delta_not_found` | not found | "We couldn't find that. It may have been completed or removed." | false |
  | `account_already_exists` | conflict | "This account already exists." | false |
  | `commitment_mismatch`, `invalid_commitment`, `invalid_delta`, `invalid_proposal_signature`, `invalid_account_id`, `invalid_input` | validation | "That request couldn't be processed. Please try again." | false |
  | `unsupported_for_network`, `unsupported_evm_chain` | capability | "That action isn't supported for this account's network." | false |
  | `network_error`, `rpc_unavailable`, `rpc_validation_failed` | connectivity (server-mapped) | "Guardian can't reach the network right now. Please try again." | true |
  | `storage_error`, `signing_error`, `configuration_error`, `data_unavailable` | internal | "Something went wrong on Guardian's side. Please try again." | true |

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: 100% of `GuardianError` variants return a non-empty `message` (enforced by an exhaustive test over the enum).
- **SC-002**: The error wire object contains exactly `code`, `message`, and (when non-empty) `meta` — no `success`, no `error` — and 0% of `message` strings match any disallowed-content pattern (hex IDs/commitments, file paths, `http(s)://` URLs, interpolated payload values, upstream "error:" fragments). The diagnostic `Display` text is present in server logs and absent from responses. Verified by a scanning test across all variants.
- **SC-003**: For every category in the starter catalog, a wallet user can determine their next action (retry, sign in, get authorized, wait, contact owner) from `message` (and `meta`) alone — validated in the smoke harness.
- **SC-004**: For a representative error produced on both surfaces, the HTTP body and the gRPC `Status.details` carry identical `{ code, message, meta }`, and `Status.message` equals `message`.
- **SC-005**: Server-mapped connectivity codes (`network_error`, `rpc_unavailable`, `rpc_validation_failed`) return the connectivity `message`; pure internal-fault codes (`storage_error`, `signing_error`, `configuration_error`, `data_unavailable`) return the generic internal `message`; the two strings are distinct (the Copilot-raised split is testable).
- **SC-006**: For an equivalent server error, the Rust client and the TS multisig client expose the same `code`, `message`, and `meta` (parity test in each client).
- **SC-007**: With Guardian unreachable, the TS multisig client and the Rust client each surface a friendly connectivity `message` and a connectivity category, and never surface the raw transport string as the primary message.
- **SC-008**: Every in-repo consumer of the old envelope (operator-client + tests, Rust client, multisig client, examples) is updated to the new shape; the full build + targeted test suites pass with no reference to `success`/`error` on the error path.

## Assumptions

- **Guardian is early-phase with no external consumer locked to the current error contract**, so a breaking reshape is acceptable and cheaper now than later (the basis for choosing reshape over additive — @zeljkoX, PR #292).
- The stable `code` vocabulary and the HTTP/gRPC status mappings in `crates/server/src/error.rs` are correct and frozen for this feature; this work reshapes the envelope and layers messaging, it does not re-taxonomize errors.
- Consumers (incl. the wallet) own localization, keyed on `code`, consistent with the wallet's existing `t('key')` i18n model; the server ships English defaults only.
- The operator dashboard / operator-client is updated as part of this change (its error parser moves to `{ code, message, meta }`); it is not assumed to keep reading the old shape.
- "Wallet users" denotes end-users of any wallet that embeds a Guardian client (the multisig client SDK), not the 0xMiden/wallet specifically — that repo does not consume Guardian today; it is the source of the *pattern* (connectivity classification), not a direct integration.
- Final message wording is subject to a UX/design review; the starter catalog is a working draft (OQ-3).

## Out of scope

- Server-side localization / i18n. The server returns English `message`; clients localize off `code`.
- Final UX copywriting and tone. The catalog above is illustrative and pending review.
- Wallet (or any consumer) UI rendering of these messages — a consumer concern.
- Introducing new `GuardianError` variants, or changing any existing `code` value, HTTP status, or gRPC `Code` mapping.
- Operator dashboard UI changes beyond updating the operator-client error parser to the new shape.
- Richer machine-readable remediation beyond `meta` and a single human sentence (e.g. deep links, structured action hints) — a possible follow-up.
