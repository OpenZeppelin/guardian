# Feature Specification: Human-readable error messages for wallet users

**Feature Branch**: `009-human-readable-errors`  
**Created**: 2026-06-18  
**Status**: Draft  
**Input**: GitHub issue [#179](https://github.com/OpenZeppelin/guardian/issues/179) (OpenZeppelin/guardian): "Surface short, user-friendly error messages for common wallet failures instead of raw backend or transport errors." Authored by @MCarlomagno; assigned @zeljkoX, @MCarlomagno; milestone *Guardian #02 - M1*. Prior art referenced by the team: 0xMiden/wallet PRs [#273](https://github.com/0xMiden/wallet/pull/273) ("stop false 'Cannot reach the Miden node' banner") and [#247](https://github.com/0xMiden/wallet/pull/247) ("sync-mgr classification"), whose `connectivity-classify.ts` heuristic is the pattern to learn from.

## Problem

Guardian already emits a stable, machine-readable error `code` for every failure (`GuardianError::code()` in `crates/server/src/error.rs`), and the operator client already documents "branch on `code`, not the human message" (`packages/guardian-operator-client/src/types.ts`). What it does **not** provide is a short message safe to show directly to a wallet end-user:

- The HTTP envelope's `error` field and the gRPC `Status` message are the `Display` string — developer/diagnostic text that embeds account IDs, commitments, nonces, and raw upstream/RPC error text (e.g. `"Commitment mismatch: expected 0xaa, got 0xbb"`, `"RPC unavailable: <raw transport error>"`, `"Insufficient signatures: required 3, got 1"`).
- The wallet-facing clients (`packages/miden-multisig-client`, `crates/client`) perform no error translation — they rethrow raw gRPC/transport errors.

Net effect: a wallet user sees raw backend or transport noise. This feature adds a curated, end-user-safe message layer with the server as the single source of truth.

## Clarifications

### Session 2026-06-18 (proposed resolutions — pending confirmation with @MCarlomagno / @zeljkoX)

- Q: Where does the human-readable mapping live — server or client? → A: **Server-side, single source of truth.** A new `user_message()` on `GuardianError`. Rationale: Constitution Principle I (bottom-up propagation) and II (HTTP/gRPC + Rust/TS parity) mean a per-consumer mapping would drift; the server already owns the stable `code` and is the natural authoring point. Clients consume the message; they only need their own logic for *codeless* transport failures (see User Story 3).
- Q: New field, or repurpose `error`? → A: **Additive new field `user_message`.** `error` (developer detail) and `code` (stable machine code) are unchanged. The envelope already extends additively (precedent: `retryable`, `paused_at`, `missing_permissions`), so existing parsers see no change.
- Q: Coverage — every variant, or a curated "common failures" subset? → A: **Every `GuardianError` variant** returns a `user_message`. Internal / non-actionable variants collapse to one safe generic message. Guarantee to clients: `user_message` is always present and always safe to display.
- Q: Localization / i18n? → A: **Out of scope server-side.** `code` is the stable localization key; `user_message` is the English default/fallback. Clients (e.g. the wallet) localize off `code`. This mirrors the wallet's own i18n model (keys, not literals).
- Q: gRPC parity? → A: `user_message` and `code` are surfaced on the gRPC `Status` (details JSON), extending the existing `GUARDIAN_ACCOUNT_PAUSED` details pattern to all errors, so HTTP and gRPC carry the same meaning (Constitution invariant: "HTTP and gRPC preserve the same error meanings").
- Q: Transport/connectivity failures where no `GuardianError` was ever produced (connection refused, DNS, timeout, TLS)? → A: The server cannot author a message for a request that never reached it. Clients MUST classify these and show a generic connectivity message — the pattern borrowed from wallet PR #273 `connectivity-classify.ts`. This is the only client-side message authoring this feature introduces.

### Open questions for the team

- **OQ-1**: Final wire field name — `user_message` (this draft) vs `hint` vs `display_message`.
- **OQ-2**: For internal errors, expose a granular `code` (so clients can localize per-cause) or collapse all internal causes to a single `internal_error` code? This draft keeps the existing granular codes and maps several of them to one generic *message*.
- **OQ-3**: Ship a starter copy catalog in this feature (Key Entities table below), or gate all final wording on a UX/design review and ship only the mechanism + a placeholder catalog?
- **OQ-4**: Should `user_message` be emitted for *all* surfaces (operator dashboard included) or only the wallet-facing multisig/per-account surfaces? This draft emits it on the shared envelope for all surfaces (cheapest, parity-preserving); the operator dashboard may simply ignore it.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - A common, user-actionable failure reads as a plain sentence (Priority: P1)

A wallet user takes a normal action against a Guardian-backed account — proposing or signing a transaction, or refreshing account state — and it fails for a reason they can act on: the account is paused, they aren't an authorized signer, they're being rate-limited, or the proposal already has their signature. Instead of `"Authorization failed: signer 0x… not in policy"` they see a short sentence such as "You're not an authorized signer for this account."

**Why this priority**: This is the core of issue #179 and the primary daily wallet experience. Without it the wallet has nothing user-presentable to show on the most common failures and must either invent its own copy (drift) or surface raw text.

**Independent Test**: For each wallet-relevant `GuardianError` variant (auth, authorization, rate-limit, account-paused, proposal-conflict, insufficient-signatures), trigger the error through the HTTP and gRPC surfaces and assert the response carries a non-empty `user_message` that is a complete, plain-language sentence and that contains none of the variant's embedded identifiers.

**Acceptance Scenarios**:

1. **Given** an account is paused, **When** a wallet attempts a mutating action, **Then** the response carries `code = GUARDIAN_ACCOUNT_PAUSED` and a `user_message` like "This account is paused and can't approve transactions right now." with no internal pause metadata embedded in the message.
2. **Given** a signer who is not authorized for an account, **When** they attempt to sign, **Then** the response carries the authorization `code` and a `user_message` like "You're not an authorized signer for this account." (and no signer identifier in the message).
3. **Given** a rate-limited caller, **When** they retry too soon, **Then** the `user_message` communicates "Too many requests — try again shortly." and the existing `retry_after_secs` field is still present and authoritative for the exact timing.
4. **Given** a proposal the caller has already signed, **When** they sign again, **Then** the `user_message` is "You've already signed this transaction." and the stable `code` is unchanged from today.

---

### User Story 2 - Internal failures never leak internals to the wallet UI (Priority: P1)

When Guardian hits an internal fault (storage error, signing error, configuration error, degraded data), the wallet user must see a single safe, generic message — never a file path, a raw RPC string, a stack trace, or a database detail. The diagnostic detail remains available to operators/logs via the unchanged `error` field, but it is never the message the wallet presents.

**Why this priority**: This is both a UX requirement and a security/disclosure requirement (Constitution Principle IV: stable boundary errors). Raw internal text leaking to an untrusted wallet client is a disclosure risk that exists today.

**Independent Test**: Trigger each internal variant (`StorageError`, `SigningError`, `ConfigurationError`, `DataUnavailable`, `NetworkError`) and assert the `user_message` equals the generic safe string, contains no path/URL/hash/raw-error fragment, while the `error` field still carries the diagnostic detail for logging.

**Acceptance Scenarios**:

1. **Given** the storage backend fails, **When** any wallet request errors, **Then** `user_message` is the generic "Something went wrong on Guardian's side. Please try again." and the diagnostic text appears only in `error`.
2. **Given** an upstream RPC returns a raw transport error, **When** Guardian maps it to `RpcUnavailable`, **Then** `user_message` is a connectivity-style sentence with the raw upstream text stripped, while `error` retains the raw text.
3. **Given** any internal variant, **When** the `user_message` is scanned, **Then** it matches none of the disallowed patterns (hex commitments/IDs, file paths, `http(s)://` URLs, "error:" fragments from upstream).

---

### User Story 3 - Connectivity failures get a friendly message even when Guardian was never reached (Priority: P2)

The wallet cannot reach Guardian at all — connection refused, DNS failure, timeout, TLS error, or a 5xx from an intermediary. No `GuardianError` is produced because no Guardian handler ran. The wallet-facing client must still present a friendly connectivity message ("Can't reach Guardian right now. Check your connection and try again.") rather than `"Failed to fetch"` / `"transport error: …"`.

**Why this priority**: It closes the "transport errors" half of issue #179 and is exactly what wallet PR #273 solved. It's P2 because it lives in the client layer and is independent of the P1 server work.

**Independent Test**: With Guardian unreachable, drive a request through the TS multisig client and the Rust client; assert each surfaces a classified connectivity message and a stable connectivity category, and never surfaces the raw transport string as the primary message.

**Acceptance Scenarios**:

1. **Given** Guardian's endpoint refuses the connection, **When** the TS multisig client makes a call, **Then** the thrown error exposes a friendly connectivity `user_message` and a `network`/`unreachable` category — not the raw `"Failed to fetch"`.
2. **Given** a request times out, **When** the client classifies it, **Then** it is categorized as connectivity (not as a semantic Guardian error) and the message advises retrying.
3. **Given** the client receives a well-formed Guardian error envelope (server reachable), **When** it renders the message, **Then** it uses the server's `user_message` (User Story 1) and does **not** run the connectivity heuristic — the heuristic is a fallback for codeless failures only.

---

### User Story 4 - Stable, parity-preserving contract for branching and localization (Priority: P2)

Client authors (wallet, operator dashboard, Rust integrators) need to branch on failures and localize messages without string-matching English text. The stable `code` is the join key and localization key; `user_message` is the English default. Rust and TypeScript clients expose the same `(code, user_message)` for an equivalent server error.

**Why this priority**: It is the contract that lets the wallet localize (matching its `t('key')` model) and prevents the fragile message-string matching that wallet `connectivity-classify.ts` was forced into.

**Independent Test**: For a representative error produced on both HTTP and gRPC, assert identical `(code, user_message)` across surfaces and across the Rust and TS clients; assert a client given an unknown future `code` falls back gracefully (uses the server `user_message`, else a generic message) without throwing.

**Acceptance Scenarios**:

1. **Given** the same logical error, **When** observed via HTTP and via gRPC, **Then** both expose the same `code` and the same `user_message`.
2. **Given** the same logical error, **When** observed via the Rust client and the TS multisig client, **Then** both expose the same `code` and `user_message` (Constitution parity).
3. **Given** a `code` the client predates, **When** it handles the error, **Then** it shows the server-provided `user_message`; if that is also absent (older server), it shows a generic fallback — never an empty or raw message.

---

### Edge Cases

- **Additive compatibility**: A consumer that reads only `error`/`code` today must see zero behavioral change. `user_message` is purely additive; no field is removed, renamed, or repurposed.
- **Message text is not a contract**: `user_message` wording MAY change between releases without a version bump; only `code` is stable. Clients branching on message text is explicitly unsupported (and a test asserts the docs say so).
- **Sanitization**: `user_message` MUST contain no account IDs, commitments, nonces, signer IDs, file paths, URLs, or raw upstream error fragments. The `error` field retains these for diagnostics.
- **Rate-limit timing**: `user_message` for `RateLimitExceeded` may say "try again shortly" but MUST NOT be the authoritative timing source — `retry_after_secs` remains that. The two must not contradict (the message stays vague; the field stays exact).
- **gRPC carrier**: gRPC consumers that ignore `Status::details` still get the gRPC `Code` + `Display` message exactly as today; the additive details JSON does not break them.
- **Codeless server responses**: a 5xx from a proxy/load balancer in front of Guardian (no Guardian envelope) is a connectivity case (User Story 3), handled client-side.
- **Empty/oversized**: `user_message` is always non-empty and bounded to a short single sentence; it never embeds a truncated dump of the underlying error.
- **Operator dashboard**: the operator surface already branches on `code`; it MAY ignore `user_message`. Adding the field must not change any dashboard response a current operator-client test pins.

## Requirements *(mandatory)*

### Functional Requirements

**Server — message authoring (Story 1, 2)**

- **FR-001**: `GuardianError` MUST expose a `user_message()` accessor returning a short, plain-language, end-user-safe string for **every** variant. The set of `code()` values and their HTTP/gRPC status mappings MUST remain unchanged by this feature.
- **FR-002**: `user_message()` output MUST be safe-by-construction: it MUST NOT contain account IDs, commitments, nonces, signer IDs, file paths, URLs, raw upstream/RPC error text, or any value interpolated from the variant's payload fields. (Contrast with `Display`, which may.)
- **FR-003**: Internal and non-user-actionable variants (`StorageError`, `SigningError`, `ConfigurationError`, `DataUnavailable`, and the internal portion of `NetworkError`/`RpcUnavailable`) MUST map to a single shared generic message (e.g. "Something went wrong on Guardian's side. Please try again."). User-actionable variants MUST map to a category-appropriate message (see Key Entities catalog).
- **FR-004**: The `user_message` text is explicitly **not** part of the stable wire contract. Only `code` is stable. Documentation on the envelope/types MUST state that clients branch and localize on `code`, never on `user_message`.

**Server — wire surfacing (Story 4)**

- **FR-005**: The HTTP error envelope (`ErrorResponse` in `crates/server/src/error.rs`) MUST include `user_message` as an additive field. All existing fields (`success`, `code`, `error`, `retry_after_secs`, `missing_permissions`, `retryable`, `paused_at`, `paused_reason`) MUST be unchanged in name, type, and population rules.
- **FR-006**: The gRPC `Status` produced from `GuardianError` MUST carry `user_message` and `code` (via `Status::details` JSON), generalizing the existing `GUARDIAN_ACCOUNT_PAUSED` details blob so that **all** errors carry a `{ code, user_message, … }` detail. The gRPC `Code` and the `Display` message MUST remain as today for consumers that ignore details.
- **FR-007**: The HTTP and gRPC surfaces MUST return the same `code` and the same `user_message` for the same logical error (Constitution II parity / invariant "HTTP and gRPC preserve the same error meanings").
- **FR-008**: The HTTP OpenAPI error schema and `crates/server/proto/guardian.proto` MUST be updated and the committed specs regenerated per the mandatory Contract-Change Workflow (`AGENTS.md` §4): `cargo run --features evm --bin gen-openapi -- docs`.

**Clients — consumption and parity (Story 3, 4)**

- **FR-009**: The Rust client (`crates/client`) MUST expose the server-provided `code` and `user_message` on its error type (`ClientError`) so Rust integrators can present and branch on them without re-parsing the envelope/status.
- **FR-010**: The TypeScript multisig client (`packages/miden-multisig-client`) MUST expose `code` and `user_message` on the errors it surfaces, and the operator client (`packages/guardian-operator-client`) MUST carry `userMessage` on `GuardianOperatorHttpErrorData` (camelCase per its existing convention) additively.
- **FR-011**: For **codeless transport/connectivity failures** (no Guardian envelope/status), each wallet-facing client MUST classify the failure and supply a generic connectivity `user_message` plus a stable connectivity category; it MUST NOT surface the raw transport string (`"Failed to fetch"`, `"transport error: …"`, etc.) as the primary message. When a Guardian envelope/status *is* present, the client MUST use the server `user_message` and MUST NOT run the connectivity heuristic.
- **FR-012**: The Rust and TypeScript clients MUST remain behaviorally aligned: the same logical error yields the same `code` and an equivalent `user_message` and category in both (Constitution II).

**Scope and compatibility**

- **FR-013**: All changes MUST be additive on every wire surface. No existing field is removed, renamed, or has its population rules changed; no `code` value or HTTP/gRPC status mapping changes.
- **FR-014**: The change MUST be validated end-to-end via the matching smoke harnesses (`examples/web` / `examples/smoke-web` for the TS multisig flow; `examples/demo` for the Rust flow), per `AGENTS.md` §4 step 5.

### Key Entities *(include if feature involves data)*

- **`user_message`**: A short (single-sentence) end-user-safe string, English, authored server-side per `GuardianError` variant. Additive on HTTP envelope and gRPC `Status` details. Not part of the stable wire contract (wording may change); safe to display verbatim in a wallet UI.
- **`code`** (existing, unchanged): The stable machine-readable error code. The join key for client branching and the localization key for client-side i18n.
- **`error`** (existing, unchanged): Developer/diagnostic detail (the `Display` string), may embed identifiers and raw upstream text. For logs/operators, never the primary wallet message.
- **Generic fallback message**: The single safe string used for all internal/non-actionable variants and as the client's last-resort fallback for unknown codes / absent `user_message`.
- **Connectivity category** (client-side): A small stable enumeration (e.g. `network` | `unreachable` | `timeout`) produced by the client's transport-error classifier for codeless failures, modeled on wallet `connectivity-classify.ts`. Carries its own generic `user_message`.
- **Starter copy catalog (illustrative, non-binding — pending UX review, OQ-3)**: representative mapping from existing `code` → proposed `user_message`:

  | `code` | Category | Proposed `user_message` |
  |---|---|---|
  | `GUARDIAN_ACCOUNT_PAUSED` | account state | "This account is paused and can't approve transactions right now." |
  | `authentication_failed` | auth | "Your session has expired. Please sign in again." |
  | `authorization_failed`, `signer_not_authorized` | authz | "You're not an authorized signer for this account." |
  | `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` | authz | "You don't have permission to do that." |
  | `rate_limit_exceeded` | throttling | "Too many requests — please try again shortly." |
  | `insufficient_signatures` | proposal flow | "This transaction still needs more signatures." |
  | `proposal_already_signed` | proposal flow | "You've already signed this transaction." |
  | `conflict_pending_delta`, `conflict_pending_proposal`, `pending_proposals_limit` | proposal flow | "There's already a pending change for this account. Finish or cancel it first." |
  | `proposal_not_found`, `account_not_found`, `state_not_found`, `delta_not_found` | not found | "We couldn't find that. It may have been completed or removed." |
  | `account_already_exists` | conflict | "This account already exists." |
  | `commitment_mismatch`, `invalid_commitment`, `invalid_delta`, `invalid_proposal_signature`, `invalid_account_id`, `invalid_input` | validation | "That request couldn't be processed. Please try again." |
  | `unsupported_for_network`, `unsupported_evm_chain` | capability | "That action isn't supported for this account's network." |
  | `rpc_unavailable`, `rpc_validation_failed`, `network_error` | connectivity (server-mapped) | "Guardian can't reach the network right now. Please try again." |
  | `storage_error`, `signing_error`, `configuration_error`, `data_unavailable` | internal | "Something went wrong on Guardian's side. Please try again." |

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: 100% of `GuardianError` variants return a non-empty `user_message` (enforced by an exhaustive test over the enum).
- **SC-002**: 0% of `user_message` strings match any disallowed-content pattern (hex IDs/commitments, file paths, `http(s)://` URLs, interpolated payload values, upstream "error:" fragments) — verified by a scanning test across all variants.
- **SC-003**: For every category in the starter catalog, a wallet user can determine their next action (retry, sign in, get authorized, wait, contact owner) from the `user_message` alone, without seeing the `error` field — validated in the smoke harness.
- **SC-004**: For a representative error produced on both surfaces, HTTP and gRPC return byte-identical `code` and `user_message`.
- **SC-005**: Every response field returned by the error surfaces prior to this feature is still present with identical name/type/population; a pinned "legacy parser" test (reads only `error`/`code`) passes unchanged.
- **SC-006**: For an equivalent server error, the Rust client and the TS multisig client expose the same `code` and an equivalent `user_message`/category (parity test in each client).
- **SC-007**: With Guardian unreachable, the TS multisig client and the Rust client each surface a friendly connectivity `user_message` and a connectivity category, and never surface the raw transport string as the primary message.

## Assumptions

- The stable `code` vocabulary and the HTTP/gRPC status mappings in `crates/server/src/error.rs` are correct and frozen for this feature; this work layers messaging on top, it does not re-taxonomize errors.
- The wallet (and other consumers) own localization, keyed on `code`, consistent with the wallet's existing `t('key')` i18n model; the server ships English defaults only.
- The operator dashboard already branches on `code` and needs no behavioral change; it may ignore `user_message`.
- "Wallet users" denotes end-users of any wallet that embeds a Guardian client (the multisig client SDK), not the 0xMiden/wallet specifically — that repo does not consume Guardian today; it is the source of the *pattern* (connectivity classification), not a direct integration.
- Final message wording is subject to a UX/design review; the starter catalog is a working draft (OQ-3).

## Out of scope

- Server-side localization / i18n. The server returns English `user_message`; clients localize off `code`.
- Final UX copywriting and tone. The catalog above is illustrative and pending review.
- Wallet (or any consumer) UI rendering of these messages — a consumer concern.
- Introducing new `GuardianError` variants, or changing any existing `code` value, HTTP status, or gRPC `Code` mapping.
- Operator dashboard UI changes.
- Structured remediation/action hints beyond a single human sentence (e.g. machine-readable "retryable", deep links) — `retryable`/`retry_after_secs` already cover the cases needed today; richer remediation is a possible follow-up.
