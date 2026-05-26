# Feature Specification: Dashboard delta activity feed and detail view

**Feature Branch**: `007-dashboard-delta-details`  
**Created**: 2026-05-24  
**Status**: Draft  
**Input**: User description: "Expose richer canonical delta data on the operator dashboard so operators can see what each transaction did, not just commitment hashes. Enrich the existing delta list endpoints with derived activity fields, and add a new delta detail endpoint with decoded notes, asset changes, and storage/account changes."

## Clarifications

### Session 2026-05-24

- Q: What is the stable reference key for a canonical delta? → A: The composite `{account_id, nonce}`. The detail URL is `/dashboard/accounts/{account_id}/deltas/{nonce}` with `nonce` as a base-10 string in the URL segment. Uniqueness is already enforced at the database level (`UNIQUE(account_id, nonce)` on the `deltas` table, see `crates/server/migrations/2026-01-01-000001_initial_schema/up.sql:22`) and the value is persisted, so no computation or schema change is required. The Miden on-chain `TransactionId` was considered and rejected for v1: Guardian sits one layer below the on-chain transaction (it stores the signed `TransactionSummary` that cosigners sign, not the `ProvenTransaction` that a downstream client later submits with a declared fee), so the on-chain `TransactionId` is not derivable from data Guardian persists — see `0xMiden/miden-base` `transaction_id.rs`, `proven_tx.rs`, `tx_summary.rs`. Adopting the on-chain `TransactionId` would require the submitter to report it to Guardian on canonicalization, which is a protocol change deferred beyond this feature.
- Q: How granular should the action-kind enumeration on the wire be? → A: Hybrid shape: a stable closed `category` enum plus an optional `kind` string. (Superseded — see 2026-05-25 below.)

### Session 2026-05-25

- Q: Should listing entries decode `TransactionSummary` at read time, or pre-compute metadata at push time? → A: Pre-compute at push time. A new `metadata JSONB` column is added on `deltas`; the `push_delta` service decodes the `TransactionSummary` once (it already does so for `verify_delta` / `apply_delta`), looks up the matching `delta_proposals` row when present, builds a typed `DeltaMetadata` blob, and persists it. Dashboard listings read the column directly — no per-request decode. The architecture also gives future policy evaluation a single point at which the "what is this delta" derivation is available before the delta is accepted.
- Q: What is the wire/persisted shape of `metadata`? → A: Flat top-level for **derived** fields (`category`, `asset`, `counterparty`, `note_counts`) plus an optional nested `proposal` block carrying operator-stated intent lifted from the matching `delta_proposals` row. Derived fields represent **what the delta did** (on-chain truth); the proposal block represents **what the operator declared they intended to do** (multisig only). The two are deliberately separate so audit / policy evaluation can compare intent vs. effect.
- Q: Is the top-level `kind` field still needed? → A: No — dropped. For multisig deltas it was always redundant with `metadata.proposal.proposal_type`; for single-key push it was always `null`. Removing it simplifies the wire shape; consumers wanting the fine-grained label read `metadata.proposal?.proposal_type`.
- Q: Should the legacy `proposal_type` wire field on `DashboardDeltaEntry` be preserved? → A: No — dropped. Its value is recoverable from `metadata.proposal?.proposal_type` and the original carried no information not already present in the new shape.
- Q: For single-key push deltas (no matching proposal), should we surface `asset` and `counterparty` by deep-decoding the output notes? → A: Yes (Knob 2). The first output note's first asset and its kind (fungible vs. non-fungible) are surfaced in `metadata.asset`. `metadata.counterparty` stays `null` for single-key push because the note's `metadata.sender()` is the account itself, not a useful counterparty value.
- Q: Should the typed metadata blob be nested under a `metadata` key on listing rows, or flattened to L1? → A: Flattened to L1. The persisted column is still a typed `DeltaMetadata` blob, but the listing wire shape spreads `category`, `proposal_type` (the lightweight tag), `assets` (array of every extractable asset summary, deterministic order), `counterparty`, and `note_counts` directly onto the entry. The full `proposal` block (typed payload by proposal type) lives only on the **detail** endpoint, where it sits at L1 alongside `category`. Wherever older prose below references `metadata.X` (e.g. `metadata.category`, `metadata.proposal.proposal_type`), the actual wire field is the L1 equivalent (`category`, `proposal_type`); skip-when-empty / skip-when-absent applies to every enrichment field uniformly.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Scannable activity feed for canonical deltas (Priority: P1)

A Guardian operator opens the dashboard to monitor transactions across the accounts they are responsible for. They scan a global feed and per-account feed of canonical deltas — the state changes that have already been applied — and need to understand what happened at a glance: which account, when, what kind of action it was, and roughly which assets, recipients, or notes were involved. They should not have to drill into a detail page or read commitment hashes to recognize "Sent 100 MAST to 0x…", "Consumed 2 notes", or "Switched Guardian".

**Why this priority**: This is the primary daily use case for the dashboard activity feed. Today the list endpoints only return commitments, nonces, and timestamps — operators cannot tell a transfer from a Guardian switch without opening another tool. Without P1, the dashboard cannot serve as a monitoring surface. Every later capability (detail view, filtering, alerts) depends on the listing being readable first.

**Independent Test**: Seed an account with a mix of canonical deltas covering each known action kind (asset transfer, note consumption, note creation, account/storage change, Guardian switch, custom/unknown). Call the per-account and global delta listing endpoints. Verify every entry carries an action-kind label and the action-kind-appropriate summary fields, that the existing per-endpoint ordering (per-account: `nonce DESC`; global: `status_timestamp DESC`) is preserved, and that an operator can hand-write a one-line human summary for each entry using only the returned fields.

**Acceptance Scenarios**:

1. **Given** an account has a canonical delta that sent fungible assets to an external recipient, **When** the operator lists canonical deltas for that account, **Then** the response includes the delta's stable reference key, account id, canonical timestamp, status, action kind "asset transfer", asset/amount summary, recipient summary, and input/output note counts.
2. **Given** an account has a canonical delta that consumed input notes and created output notes, **When** the operator lists canonical deltas, **Then** the entry's action kind reflects the dominant operation and the input/output note counts are populated.
3. **Given** an account has a canonical delta whose payload does not match any known action shape, **When** the operator lists canonical deltas, **Then** the entry returns action kind "custom" (or "unknown") rather than failing or being omitted, and the entry remains listable.
4. **Given** a delta payload is partially malformed and a derived activity field cannot be safely extracted, **When** the listing is rendered, **Then** the missing field is absent (or null) but the entry itself is still returned with its stable key and status.
5. **Given** an existing dashboard client built against the current listing shape, **When** the enriched response is returned, **Then** all previously-returned fields remain present so existing clients continue to read them.

---

### User Story 2 - Delta detail view with decoded transaction effects (Priority: P2)

An operator clicks a row in the activity feed (or follows a link from an alert) and lands on a detail view for a single canonical delta. They want to see the full human-meaningful effect of that transaction: which input notes were consumed (with asset/amount/recipient/sender where applicable), which output notes were created, how the account's vault and storage changed, and the account-level fields that changed. Raw cryptographic blobs are acceptable as secondary debug information but must not be the primary surface.

**Why this priority**: The detail view answers the second-level question ("ok, what *exactly* did this transaction do?") and is required for any operator triage workflow. It is P2 because the activity feed (P1) is usable on its own as a monitoring surface, and the detail view adds depth once an entry has been picked out.

**Independent Test**: Take a canonical delta covering a mix of input notes, output notes, vault changes, and storage changes. Call the detail endpoint with the delta's stable reference key for that delta's account. Verify the response contains the same stable reference key, the decoded input note list, the decoded output note list, the asset/vault change list, the storage/account-field change list, and the raw transaction summary only under a clearly-marked optional/debug field.

**Acceptance Scenarios**:

1. **Given** a canonical delta exists for an account, **When** the operator requests the delta detail using the delta's stable reference key for that account, **Then** the response includes the delta reference, decoded input notes (each with asset, amount, sender/recipient where applicable), decoded output notes (same shape), vault changes, and storage/account changes.
2. **Given** the detail endpoint is called with a delta reference that does not belong to the account in the path, **When** the request is processed, **Then** the response indicates the delta is not found for that account rather than returning a delta from a different account.
3. **Given** a delta has no input notes (e.g., a Guardian switch), **When** the detail is requested, **Then** the input notes list is returned as empty (not missing, not erroring) and the other change sections are populated.
4. **Given** a note's script is unusually large or expensive to decode, **When** the detail is requested, **Then** the script is either omitted or returned under a clearly-optional field, while the asset/recipient/amount fields are always returned.
5. **Given** the underlying transaction summary cannot be fully decoded into the dashboard contract, **When** the detail is requested, **Then** the request still succeeds for the parts that could be decoded and clearly indicates which sections were not decodable; the raw transaction summary remains available under the optional debug field.

---

### User Story 3 - Per-account nonce as the stable delta reference (Priority: P2)

The dashboard, alerts, audit trails, and operator tooling all need to refer to one specific canonical delta unambiguously — to deep-link from an alert email to its detail page, to compare two notifications, and to record "operator X looked at delta Y at time Z". Each canonical delta exposed in the activity feed and detail view is addressable by the composite key `{account_id, nonce}`, with the URL form `/dashboard/accounts/{account_id}/deltas/{nonce}`.

**Why this priority**: This is foundational glue between Story 1 and Story 2. Without a stable key, the dashboard cannot link from a feed entry to its detail, and external systems cannot record references that survive redeploys. `{account_id, nonce}` is uniqueness-enforced at the database level (`UNIQUE(account_id, nonce)` on `deltas`), monotonically increasing per account, requires no protocol or schema change, and is already returned on the current listing endpoints — so the change is wholly additive on the wire.

**Independent Test**: Take a delta's `nonce` (under the path's `account_id`) from a listing response, use it to fetch the delta detail, and confirm the detail returns the same delta and the same `(account_id, nonce)` pair. Restart the server and repeat — the same delta must be addressable by the same key.

**Acceptance Scenarios**:

1. **Given** a canonical delta appears in a listing response, **When** the detail endpoint is called with the same `{account_id, nonce}` pair, **Then** the same delta is returned and the response's `account_id` and `nonce` equal the listing entry's values.
2. **Given** a delta is referenced by its `{account_id, nonce}` in an external record (e.g., an alert URL), **When** the dashboard later resolves that reference, **Then** the key still maps to the same delta regardless of any non-state-changing redeploys or process restarts.
3. **Given** the detail URL transports `nonce` as a base-10 string, **When** the dashboard parses it back, **Then** it accepts only canonical decimal `u64` (no leading zeros except for `0` itself, no negative numbers, no hex), rejecting malformed segments with a structural-error response distinct from "not found".

---

### Edge Cases

- **Single-key push deltas have no matching proposal.** The push-time pipeline still produces `metadata` — `category` derived from `TransactionSummary` topology, `asset` from the first output note when present, `counterparty` left `null`. The `proposal` block is absent. EVM deltas whose `delta_payload` is not a `TransactionSummary` persist `metadata: NULL`.
- **Proposal metadata is free-form JSON at the wire boundary.** The push-time pipeline parses it into a typed `ProposalMetadata` struct; unknown keys are dropped, malformed shapes produce `metadata.proposal = absent` (the derived block is unaffected). The persisted typed blob is what downstream readers see.
- **Listing pagination is unaffected by enrichment cost.** All `TransactionSummary` decode happens at push time. Listings are pure column reads; per-row cost on the listing endpoint is dominated by JSON parsing of the typed metadata blob, which is small (kilobytes max).
- **A delta exists in a listing but not the detail endpoint (race).** A listing is served from a snapshot then a detail is requested moments later for an evicted/archived delta — the detail endpoint should return a clear "not found for this account" response, not 500 or leak fields from another account.
- **The detail endpoint is called with a reference key that is well-formed but unknown.** Behaves identically to "not found for this account" — no info leak about whether it exists under a different account.
- **The detail endpoint is called with a reference key whose format is invalid** (e.g., negative, non-decimal, leading zeros other than `"0"`, hex prefix, or non-parseable). The endpoint rejects the request with a client-error response distinct from "not found".
- **Status transitions after listing.** A delta listed as canonical can in principle be reorganized (e.g., demoted from canonical) before the detail is fetched; the detail must return whatever the current status is and not assume canonical.
- **An asset or recipient field cannot be safely extracted from an otherwise-known payload shape.** The action-kind label is still set, the unavailable summary fields are absent or null, and the entry is not dropped.
- **Operator authorization scope (v1).** Today the global delta feed has no per-account ACL filter — any authenticated operator with dashboard read access sees deltas for all configured accounts (see `list_global_deltas` signature at `crates/server/src/services/dashboard_global_deltas.rs:147`). This feature does not add per-account ACL scoping; doing so is tracked separately. The detail endpoint's uniform-404 outcome (FR-017) therefore unifies two cases in v1: unknown delta and unknown account. The "operator unauthorized for this specific account" case does not exist as a separate code path until per-account ACL is added.
- **Wire-format compatibility.** Adding the enrichment fields to the existing listing endpoints must not break previously-returning fields; existing clients continue to read what they already read.

## Requirements *(mandatory)*

### Functional Requirements

**Listing enrichment (Story 1)**

- **FR-001**: The per-account canonical delta listing and the global canonical delta listing MUST return, for each entry, a stable reference key, the account id, the canonical timestamp, the delta status, and all fields that the same endpoint returned prior to this change.
- **FR-002**: Each listing entry MUST spread the dashboard-ready activity fields directly at L1 (no nested `metadata` envelope on the wire): `category`, `proposal_type`, `assets`, `counterparty`, `note_counts`. All five fields are optional. When present, `category` MUST be a value of the closed enumeration `asset_transfer`, `note_consumption`, `note_creation`, `account_storage_change`, `guardian_switch`, `custom` — and MUST be non-null. The whole field set is absent for rows that pre-date the push-time derivation pipeline (historical multisig deltas whose proposals were already deleted) and for EVM deltas. Adding a new `category` value is a wire-contract change; the `proposal_type` string is intentionally open-ended so the multisig client can introduce new proposal types without forcing a wire bump. (The persisted server-side `metadata` JSONB column is an implementation detail; it is not returned as a nested object on listing rows.)
- **FR-002a**: When the persisted server-side `metadata.proposal` block is present (multisig deltas), `category` MUST be derived deterministically from `proposal_type`. At minimum: `p2id` → `asset_transfer`; `consume_notes` → `note_consumption`; `switch_guardian` → `guardian_switch`; `add_signer`, `remove_signer`, `change_threshold`, `update_procedure_threshold` → `account_storage_change`. Unknown `proposal_type` values map to `custom` while preserving the proposal intent block for the detail endpoint.
- **FR-002b**: When no matching proposal is found at push time (single-key push, EVM, or any payload whose proposal lookup misses), `category` MUST be inferred from the decoded `TransactionSummary` topology — input/output note counts plus the account-state delta. Single-key push deltas additionally surface a single-element `assets` array from the first output note's first asset when present (Knob 2). `counterparty` for single-key push stays absent because the note's sender field is the account itself.
- **FR-002c**: Derivation MUST happen at **push time**, not at canonicalization or at read time. The `push_delta` service decodes the `TransactionSummary` once (it already does so for `verify_delta` / `apply_delta`), runs the build pipeline, and persists the typed blob in the `deltas.metadata` JSONB column alongside the candidate row. Canonicalization just flips the status — no metadata work. Dashboard listings read the column directly and spread its fields to L1.
- **FR-003**: When enrichment is present on a listing row, it MUST carry `note_counts.input` and `note_counts.output` derived from the decoded `TransactionSummary`. `note_counts` MAY be omitted when both values are zero. `assets` and `counterparty` MUST be populated when the source data (proposal metadata for multisig, output notes for single-key) yields them; otherwise they MUST be absent (FR-004).
- **FR-004**: When a sub-field cannot be safely extracted at push time (malformed proposal metadata, undecodable `TransactionSummary`, etc.), the push pipeline MUST persist whatever can be derived and leave the rest absent. A delta whose `TransactionSummary` cannot be decoded at all (e.g. EVM delta) persists `metadata: NULL` server-side and listings still return the entry with the pre-existing fields (`nonce`, `status`, commitments) intact and no enrichment fields. Listing endpoints MUST NOT do any decoding or fall-back inference at read time — the persisted column is the source of truth.
- **FR-005**: Listing responses MUST preserve the existing endpoint-specific ordering and pagination behaviour with no regression. Per-account listing orders by `nonce DESC` (`nonce` is per-account monotonic, so this is "newest-first" by construction; see `crates/server/src/services/dashboard_account_deltas.rs`). Global listing orders by `status_timestamp DESC` with `(account_id, nonce)` as the tiebreaker (`crates/server/src/services/dashboard_global_deltas.rs:6`). Neither ordering is changed by this feature.
- **FR-006**: Listing endpoints continue to surface the existing lifecycle statuses on the `deltas` table — `candidate`, `canonical`, and `discarded` — and MUST NOT include `pending` proposal data in the activity feed (pending lives on the proposal queue endpoint). The dashboard UI is responsible for any "show only canonical" presentation filtering. The global listing endpoint preserves its existing optional `status` query parameter; the per-account endpoint preserves its current parameter set (`limit`, `cursor`). The phrase "canonical delta" elsewhere in this spec refers to the conceptual subject (a delta that *will be* the canonical record for a given `(account_id, nonce)`), not a wire-level filter.

**Delta reference key (Story 3)**

- **FR-007**: Each canonical delta MUST be addressable by the composite `{account_id, nonce}`. `nonce` is the per-account monotonically-increasing `u64` already persisted on the `deltas` row; uniqueness within an account is enforced at the database level. No new persistence is introduced by this feature.
- **FR-008**: The `nonce` of a delta returned by the listing endpoints MUST equal the `nonce` returned by the detail endpoint for that same delta, under the same `account_id`.
- **FR-009**: The `nonce` value MUST be stable across server restarts, non-state-changing redeploys, and status transitions (Candidate → Canonical → Discarded). `nonce` is assigned once when the delta row is inserted and never rewritten.
- **FR-009a**: The detail endpoint URL path MUST be `/dashboard/accounts/{account_id}/deltas/{nonce}` with `{nonce}` rendered as a canonical base-10 `u64` (no leading zeros except for the value `0`, no negative numbers, no hex prefix). The `account_id` in the path is the authorization-scoping key; the `{nonce}` segment disambiguates which delta within that account.

**Detail view (Story 2)**

- **FR-010**: The system MUST expose a new endpoint to fetch the detail of a single canonical delta belonging to a specified account, addressed by the account id and the delta reference key.
- **FR-011**: The detail response MUST include the delta reference key, the account id, the canonical timestamp, the delta status, the decoded input notes, the decoded output notes, the asset/vault changes, and the storage/account-field changes.
- **FR-012**: Each decoded note (input or output) MUST include, when applicable, the asset(s) and amount(s) carried, the sender/recipient identifier, and a stable identifier for the note itself. The note's MAST script is NOT exposed in v1 (US2 scope decision, 2026-05-25) — re-introducing it would be a coordinated wire-contract change.
- **FR-013**: Asset/vault changes MUST be expressed as a structured list of per-asset before/after or delta values (whichever is most natural for fungible vs non-fungible holdings), not as a free-form blob.
- **FR-014**: Storage/account-field changes MUST be expressed as a structured list of changed fields/slots with their post-change values (`after`). The optional `before` field is reserved for a future enhancement that replays account storage at `prev_commitment`; v1 detail responses omit `before` because a `TransactionSummary` delta carries only post-change slot values.
- **FR-015**: The raw underlying transaction summary is NOT returned by the detail endpoint by default. Callers may opt in with `?include=raw`, in which case the response includes a base64-encoded `raw_transaction_summary` field for debugging. The default response shape is unchanged.
- **FR-016**: If a portion of the underlying transaction summary cannot be decoded into the dashboard contract, the detail response MUST still succeed for the portions that could be decoded and MUST indicate which sections were not decodable.
- **FR-017**: A detail request for an unknown `(account_id, nonce)` pair, or whose `account_id` is unknown to the server, MUST return a single uniform "not found" outcome. Per-account operator ACL scoping is not in scope for v1 (see the "Operator authorization scope (v1)" edge case); when that scoping is added in a future feature, the "operator unauthorized for this account" case MUST share the same response shape.
- **FR-018**: A detail request whose reference key is structurally invalid MUST be rejected with a client-error outcome distinct from the "not found" outcome.

**Scope and compatibility**

- **FR-019**: Proposals (i.e., pending, not-yet-canonical delta intents) are explicitly out of scope for this feature; no changes to proposal listing/detail surfaces are required.
- **FR-020**: The Rust and TypeScript Guardian operator/client surfaces consuming these endpoints MUST receive the enriched listing fields and the new detail capability with no silent behaviour drift between them.
- **FR-021**: Adding the new fields and the new endpoint MUST NOT remove or rename any field currently returned by the listing endpoints.

### Key Entities *(include if feature involves data)*

- **Canonical delta**: A state change that has been applied to an account and reflected in the canonical chain of states. Carries an account reference, a canonical timestamp, a status, commitment fields (kept for debugging), and a payload describing what changed. The dashboard surfaces canonical deltas as "transactions".
- **Delta reference key**: The composite `{account_id, nonce}`. `account_id` carries the existing account identifier shape; `nonce` is the per-account `u64` already persisted on the `deltas` row, unique within an account by database constraint (`UNIQUE(account_id, nonce)`). Rendered as a base-10 string in URL segments and JSON. The Miden on-chain `TransactionId` is deliberately not adopted in this feature; see the Clarifications session entry for the rationale.
- **DeltaMetadata**: The typed blob persisted in the `deltas.metadata` JSONB column at push time. Carries two layers: (1) **derived** fields populated from the decoded `TransactionSummary` and refined with proposal hints (`category`, `asset`, `counterparty`, `note_counts`), and (2) an optional **proposal** block carrying operator-stated intent lifted from the matching `delta_proposals` row. Absent (column NULL) only for EVM deltas and pre-feature-007 historical rows.
- **Action category**: A closed, stable enumeration on `metadata.category` describing what a delta did at the coarsest useful level: `asset_transfer`, `note_consumption`, `note_creation`, `account_storage_change`, `guardian_switch`, `custom`. Signing-scheme-agnostic — single-key push and multisig deltas use the same values.
- **Proposal metadata**: An optional block at `metadata.proposal` carrying the operator-stated intent fields from `ProposalMetadataPayload` in the multisig client (`proposal_type`, `description`, `salt`, `required_signatures`, plus per-type fields like `recipient_id`/`faucet_id`/`amount` for `p2id`, `note_ids`/`consume_notes_*` for `consume_notes`, `target_threshold`/`signer_commitments` for signer ops, `new_guardian_*` for guardian-switch, `target_procedure` for procedure-threshold updates). Lifted at push time and never modified afterward. Distinct from the derived block — the proposal block reflects what was *declared*, not what was *observed*.
- **Decoded note**: A structured projection of an input or output note carried by a transaction, exposing asset/amount, sender/recipient, and a stable note identifier. The note's MAST script is not exposed in v1.
- **Vault change**: A structured projection of the account's vault state change for one asset, expressing the before/after or delta value for that asset.
- **Storage/account change**: A structured projection of a single changed account-level field or storage slot. v1 carries `slot_name` and post-change `after` only; `before` is omitted until prev-commitment state replay is implemented.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Given any canonical delta returned by the listing endpoints, an operator (or downstream renderer) can produce a one-line human summary using only the returned fields, for at least 95% of deltas in a representative production-like sample, without consulting the detail endpoint or any other data source.
- **SC-002**: Every canonical delta returned in the listing endpoints either carries the enrichment fields (`category` plus the optional `proposal_type`, `assets`, `counterparty`, `note_counts`) flattened to L1, or omits them all. The server MUST NOT fabricate placeholder values (e.g. `category: "custom"` + `note_counts: {0,0}`) for rows where derivation cannot run — those zero values would contradict the actual on-chain activity for historical consume_notes / p2id deltas whose source data we no longer have. Absence of the enrichment block is reserved for exactly three cases:
  1. EVM deltas whose `delta_payload` is not a `TransactionSummary`.
  2. Pre-feature-007 historical rows whose source proposal was already deleted.
  3. Rows whose `TransactionSummary` is corrupted/undecodable AND have no matching proposal to lift from.
  When `category` is present it is always a value of the closed enum (never null). `proposal_type` may be absent (and is absent for single-key push by design). `assets` (array of all extracted asset summaries), `counterparty`, and `note_counts` follow the same skip-when-empty rule. Clients MUST render the absent-enrichment state as a distinct UX (e.g. "metadata unavailable") rather than as zero-valued fields.
- **SC-003**: For 100% of canonical deltas appearing in a listing response, the same delta is retrievable from the detail endpoint using the listing entry's reference key, and the detail response's reference key equals the listing's value.
- **SC-004**: Listing endpoints serving a representative dashboard page (page size matching current dashboard usage) complete within the same latency budget as the current commitment-only listing — no perceptible regression to the operator opening the activity feed.
- **SC-005**: For a delta detail response, an operator can identify every input note's asset/amount/counterparty (when applicable), every output note's asset/amount/counterparty (when applicable), every asset-vault change, and every storage/account-field change without reading any optional/debug field.
- **SC-006**: A malformed or unrecognized payload field reduces the affected delta to fewer populated summary fields but does not cause the listing endpoint to fail, does not drop the affected entry, and does not affect unrelated entries in the same listing.
- **SC-007**: No previously-returned field on the existing listing endpoints is removed or renamed; existing clients continue to read the fields they already read.
- **SC-008**: A detail request that resolves to "no such delta under this account" returns the same not-found outcome shape regardless of whether the cause is an unknown nonce on a known account or an unknown account; no field-level difference distinguishes the two. (Per-account operator ACL is not in scope for v1; see FR-017.)

## Assumptions

- "Canonical deltas" are the only state-change records this feature surfaces. Pending proposal state, proposal lifecycle metadata, and proposal-level decisions are explicitly out of scope and remain on the existing proposal endpoints.
- The set of action kinds enumerated in FR-002 is closed for the wire contract but extensible over time; adding a new label is a contract change to be handled the same way as any other dashboard contract change.
- "Operator" here means an authenticated Guardian operator authorized via the existing per-operator authorization model; this feature does not introduce a new authorization concept.
- The existing dashboard treatment of `nonce`, `prev_commitment`, and `new_commitment` is preserved as debug-tier data — these fields continue to be returned for engineering use but the primary dashboard UX is built around the new derived fields.
- The Rust and TypeScript operator/client surfaces both consume these endpoints and must remain behaviourally identical with respect to the new fields.

## Out of scope

- Proposal listing or detail enrichment of any kind.
- Rendering / formatting human-readable transaction summaries. The dashboard is responsible for turning the structured fields into strings; the server only returns structured fields.
- New filtering or sorting modes on the listing endpoints beyond what is already supported.
- Historical migration / backfill of any new field; if a delta's payload does not carry enough information to fill a derived field, the field is absent or null.
- Per-operator alerting on activity-feed events. This feature is a read API only.
