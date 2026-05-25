# Phase 0 Research — Dashboard delta activity feed and detail view

Resolves the open design knobs called out in `spec.md` + the plan's Technical Context. Each entry lists the decision, why we chose it, what else we looked at, and the source(s) used to verify.

## Decision 1 — Reference key shape: `{account_id, nonce}`

**Decision**: Compose the delta reference key from the `account_id` already in the URL path and the per-account monotonically-increasing `nonce` already persisted on the `deltas` row. URL is `/dashboard/accounts/{account_id}/deltas/{nonce}` with `nonce` as a canonical base-10 string.

**Rationale**:
- Uniqueness within an account is already enforced at the database layer (`UNIQUE(account_id, nonce)` in `crates/server/migrations/2026-01-01-000001_initial_schema/up.sql:22`). The constraint covers all three lifecycle statuses (Candidate / Canonical / Discarded), so the key continues to address a delta after status transitions.
- `nonce` is set once on insertion and never rewritten — stable across server restarts and non-state-changing redeploys (FR-009).
- Zero new persistence, zero protocol change.

**Alternatives considered**:
- **Miden canonical `TransactionId`**: initially preferred for Midenscan interop, but Guardian sits one layer below the on-chain transaction. The `TransactionSummary` Guardian persists (`crates/miden-protocol/src/transaction/tx_summary.rs`) intentionally omits the fee asset; the on-chain `TransactionId` definition (`crates/miden-protocol/src/transaction/transaction_id.rs`) requires `FEE_ASSET` as one of five hash inputs; `FungibleAsset` has no zero-constructor (`crates/miden-protocol/src/asset/fungible.rs`). Adopting it would require the submitter to report the id back to Guardian on canonicalization — a protocol change deferred beyond this feature.
- `{account_id, new_commitment}`: works but `new_commitment` is hex (64 chars) instead of a short integer, making URLs and dashboard memory noisier with no operator benefit.
- New standalone `delta_id` column: requires schema migration + back-fill; offers no value the composite doesn't.

**Sources**:
- `crates/server/migrations/2026-01-01-000001_initial_schema/up.sql:13–24`
- `crates/server/src/storage/mod.rs:34,43` (uniqueness comments)
- `0xMiden/miden-base` `crates/miden-protocol/src/transaction/transaction_id.rs`
- `0xMiden/miden-base` `crates/miden-protocol/src/transaction/tx_summary.rs`

## Decision 2 — Two-layer metadata: derived fields + optional proposal block

**Decision (revised 2026-05-25)**: Persist a single `metadata` JSONB blob per delta with two flat layers:

- **Derived layer** (always present when metadata exists): `category` (closed enum), `asset`, `counterparty`, `note_counts`. Represents what the delta actually did on-chain.
- **Proposal layer** (`proposal`, present only on multisig commits): the operator-stated intent fields from `ProposalMetadataPayload` (proposal_type, recipient/faucet/amount for p2id, note_ids for consume_notes, target_threshold/signer_commitments for admin ops, new_guardian_* for guardian-switch, etc.). Lifted verbatim at push time.

**Rationale**:
- Two concepts that were previously conflated are now distinct: derived = on-chain truth (always recoverable); proposal = operator intent (only exists when there was a proposal).
- Future policy evaluation can compare proposal.amount against asset.amount to detect declaration vs. execution mismatch — only possible because the two layers are tracked separately.
- A previous design carried a top-level `kind` field redundantly with `proposal.proposal_type`; dropped in this revision because `metadata.proposal?.proposal_type` is the single source.

**Mapping rules**:
- When `proposal.proposal_type` is set: `category` = deterministic mapping (FR-002a); `asset` and `counterparty` seeded from proposal fields for `p2id`.
- When no matching proposal: `category` inferred from `TransactionSummary` topology (FR-002b); `asset` from first output note's first asset when present; `counterparty` left null.
- Unknown `proposal_type` strings map to `category = custom` while still preserving the proposal block verbatim.

**Sources**: spec §Clarifications 2026-05-25 session, FR-002 / FR-002a / FR-002b / FR-002c; existing `VALID_PROPOSAL_TYPES` constant at `crates/server/src/services/mod.rs:181`.

**MVP scope note — `asset_swap` removed from the enum.** Earlier drafts of feature 007 listed `asset_swap` as a `category` value. Shipping it before per-output-note tag detection landed would have meant emitting a wire-contract value that's never used. The variant was removed from `DashboardDeltaCategory` and the corresponding TS / data-model / contract entries.

**Partial follow-up landed (2026-05-25): per-note tag classification at the detail level only.** `classify_note_tag(note: &Note) -> NoteTag` was added in `projection.rs` and now correctly identifies `p2id`, `p2ide`, `pswap`, `mint`, and `burn` notes on the detail endpoint's `input_notes` / `output_notes` arrays. **Deliberately NOT promoted to a `category` upgrade.** Operator decision: `pswap` deltas continue to be categorized by topology (most commonly as `asset_transfer`) rather than introducing the `asset_swap` enum value. Reasoning: a single `pswap` output note in a transaction with other notes doesn't necessarily mean the whole delta is a "swap" — the topology-driven category remains the safe default. If a future product need surfaces `asset_swap` as a meaningful operator distinction, the upgrade is a one-line change in `infer_category_from_summary` (read the first output note's tag) plus a coordinated wire-contract addition.

**Absent-metadata policy (revised 2026-05-25 after reviewer feedback).** When the push pipeline cannot derive metadata (EVM payloads, undecodable `TransactionSummary`, pre-feature-007 historical rows), `metadata` is persisted as `NULL` and omitted on the wire. The server MUST NOT fabricate `category: "custom"` with zeroed `note_counts` for these rows — doing so would lie about historical multisig commits whose actual on-chain activity is non-zero (e.g. a `consume_notes` delta that consumed 1 note would show `note_counts: {0,0}` to operators, contradicting reality). Clients render the absence as "metadata unavailable" so the missing-data signal is preserved.

## Decision 3 — Push-time derivation, persisted JSONB

**Decision (revised 2026-05-25)**: Derive `metadata` once at push time in `crates/server/src/services/push_delta.rs` and persist it to a new `metadata JSONB` column on the `deltas` table. Listing endpoints read the column directly — no decode-on-read.

The derivation pipeline lives in `crates/server/src/delta_summary/`:
- `decode.rs` — extracts `TransactionSummary` + `ProposalMetadata` from the raw payloads.
- `category.rs` — proposal-type → category mapping and on-chain topology inference.
- `projection.rs` — note counts + first-output-note deep-decode for asset/counterparty.
- `build.rs` — orchestrator that combines all the above into a typed `DeltaMetadata`.

**Rationale**:
- `push_delta` already decodes the `TransactionSummary` for `verify_delta` / `apply_delta`. Computing metadata at the same point is essentially free incremental cost (no new decode pass).
- Dashboard listings become column reads — no per-request base64 decode + binary deserialize. This was the deferred follow-up flagged in the original (read-time) design.
- Single source of truth for derivation: any future feature (policy evaluation, alerting) reads from the same persisted blob and cannot drift from the dashboard.
- Operators see the correct `category` and `proposal` block immediately when a delta lands as Candidate, not after canonicalization (which can take seconds to minutes — see `crates/server/src/builder/canonicalization.rs:21`, default 10s tick + Miden network confirmation time).

**Cost shift**:
- **Push path** gains one classifier call + one proposal lookup (DB read) per write. Both operate on data already in memory; the proposal lookup is the same one canonicalization used to do.
- **Listing path** loses all decode work. Per-entry cost on a default 50-entry page goes from 50 × `TransactionSummary::from_json` (base64 + binary deserialize with MAST bytes) to 50 × JSON parse of a small typed blob.

**Alternatives considered**:
- Derive at canonicalization. Rejected: candidate-window UX would show every fresh delta as `custom`/null until on-chain confirmation, which can be many seconds (worker tick = 10s default + Miden network confirmation + retries).
- Derive at read time (original design). Rejected: pays the decode cost on every dashboard view, scales linearly with account history size, and creates the duplication trap with future policy eval.

## Decision 4 — Vault and storage changes: signed-delta + before/after hybrid

**Decision**:
- **Vault changes**: emit per-asset entries `{ asset_id, asset_kind: "fungible"|"non_fungible", change }` where `change` is a signed-magnitude string (e.g., `"+100"`, `"-50"`) for fungible assets and `{ added: [...], removed: [...] }` lists of asset ids for non-fungible.
- **Storage / account-field changes**: emit per-slot entries `{ slot_name, after }` in v1. `slot_name` is the human-readable Miden `StorageSlotName` (e.g. `"consumed_notes"`); `after` is a hex `Word` string (64 hex + `0x` prefix) or omitted when the slot was cleared. **`before` is intentionally omitted in v1** — a `TransactionSummary` account delta carries only post-change slot values, not prior state. Populating `before` requires reading account storage at `prev_commitment` (future enhancement / prev-commitment state replay). Earlier drafts called the identifier field `slot_index`; corrected on 2026-05-25 after the first end-to-end test surfaced that `StorageSlotName` is a string identifier, not a numeric index.

**Rationale**:
- Fungible holdings naturally read as a signed delta in an activity feed ("sent 100", "received 50"); operators don't need both endpoints when the delta is the meaningful quantity.
- Non-fungible holdings need set-difference semantics; expressing as "before/after" lists or a single signed scalar is awkward, so `added` / `removed` is the cleanest shape.
- Storage slots have arbitrary semantics — operators reading the dashboard need the raw before/after pair to interpret; trying to render a "delta" of two `Word`s is meaningless.

**Sources**: `0xMiden/miden-base` `crates/miden-protocol/src/account/delta/` — `AccountDelta` already separates vault changes (signed for fungible, add/remove sets for non-fungible) from storage changes (slot index → new value).

## Decision 5 — Note scripts: omit by default, opt-in via query parameter

**Decision**: Decoded notes (input + output) always include asset/amount/sender/recipient/note id. The note script is **omitted** from the default detail response. A query parameter `?include=scripts` opts in; when included, scripts are returned as hex strings under an optional `script` field on each decoded note. The `category`/`kind`/summary fields never depend on the script (they're derived from the standard note tag + asset data already on the wire).

**Rationale**:
- The user's framing was "expose only if not too cumbersome" — scripts are bulky (MAST bytecode), they bloat both the wire and any UI rendering, and they're niche enough that defaulting off is correct.
- Opt-in keeps the v1 contract small; if a debugging surface eventually needs them, the flag is already in place.

**Alternatives considered**:
- Always include — bloats response; tests showed `TransactionSummary` payload is dominated by MAST/script bytes (memory observation 2754).
- Always omit — closes the door on debugging surfaces with no good reason.

## Decision 6 — Authorization scope: reuse existing dashboard authz; v1 has no per-account ACL

**Decision**: Both the enriched listing handlers and the new detail handler reuse the existing `dashboard::authz` middleware (cookie session, `dashboard:read` permission, `route_layer` at `crates/server/src/builder/handle.rs:111`). No new permissions, no new audit shapes.

The detail endpoint's uniform-404 outcome (SC-008) covers exactly **two** v1 cases:
- Unknown `(account_id, nonce)` on a known account (`GuardianError::DeltaNotFound`)
- Unknown `account_id` (`GuardianError::AccountNotFound`)

The two variants emit different `code` strings today, so the handler MUST normalize the response body so the two are field-level identical. This is verified by an integration test that diffs the two response bodies as `serde_json::Value`.

**Per-account ACL is explicitly out of scope for v1.** An authenticated operator with `dashboard:read` can list any configured account's deltas and fetch any delta's detail; there is no operator → account allowlist filter on either the global listing or the detail endpoint. The "operator unauthorized for *this specific* account" case does not exist as a distinct code path. When per-account ACL is added in a future feature, that third case will share the same uniform-404 shape, but introducing the ACL itself is not part of this feature. Today, an authenticated operator without `dashboard:read` at all receives a `403 InsufficientOperatorPermission` from the middleware — not a `404` from this endpoint.

**Rationale**:
- Account pausing (feature #233) and per-operator authorization (#231) already handle the existing surface; layering a feature-specific authz check would create drift.
- Promising a uniform 404 across an ACL case that doesn't exist would be a false guarantee.

**Sources**: `crates/server/src/builder/handle.rs:93–111`, `crates/server/src/dashboard/authz.rs`, `crates/server/src/dashboard/permissions.rs`, spec §Edge Cases "Operator authorization scope (v1)".

## Decision 7 — Wire-format compatibility: additive only

**Decision**: All new fields on the existing listing endpoints (`category`, `kind`, summary fields, etc.) are *added* to the response. No existing field is removed or renamed. Fields that may be null (`kind`, individual summary fields per FR-004) are serialized as `null` explicitly so downstream parsers see a stable key set.

**Rationale**: FR-021. Existing TypeScript operator client tests at `packages/guardian-operator-client/src/http.test.ts:580+` parse the current shape; they must continue to pass without modification to their assertions on pre-existing fields.

## Decision 8 — Performance verification: local integration bench

**Decision**: Add a Criterion-style bench (or a `#[tokio::test]` timing harness if Criterion is overkill) under the dashboard listing service that exercises the enriched code path against a fixture set of ~500 mixed-shape canonical deltas seeded in the filesystem backend. Compare against a baseline run that returns the pre-feature shape (no `category` / `kind` / `summary`). Acceptance bar: p95 wall-clock per default-page-size response within the same envelope as the baseline (SC-004). If the local bench shows a measurable regression, treat that as the trigger for Decision 3's follow-up (persist the projection).

**Rationale for choosing local over prod**: SC-004 is about per-page decode cost, not end-to-end system throughput. A local fixture-seeded bench is faster to iterate on, deterministic, and runnable in CI. The `run-guardian-prod-benchmarks` skill targets full-stack production scenarios and is the wrong default for a per-handler micro-bench.

**Open**: exact regression budget is intentionally framed as "no perceptible regression" in SC-004; tightening to a hard number can wait until the bench produces baseline data.

## Decision 9 — TypeScript-only consumer; no Rust operator client to update

**Decision**: Grep confirmed `crates/guardian-operator-client/` does not exist; only `packages/guardian-operator-client/` (TS) does. Constitution principle II ("Rust ↔ TypeScript parity for clients modeling the same workflow") is therefore vacuously satisfied for this feature; the only client surface to update is TS.

**Documented divergence**: spec FR-020 mentioned "Rust and TypeScript Guardian operator/client surfaces"; this is reframed in the plan's Constitution Check as a TS-only consumer surface. If a Rust operator client is ever added in the future, it would mirror the TS client's shape verbatim — the wire contract authored here is the source of truth.

## Decision 10 — Push-time decoder normalizes both `delta_payload` shapes

**Decision**: The shared `delta_summary::decode` module accepts any `&serde_json::Value` and resolves it to a `TransactionSummary` plus an `Option<MultisigMetadata>` via a single normalization step. The normalizer handles two on-disk shapes, distinguished by whether the top-level object has a `tx_summary` field.

**The two shapes** (verified in the codebase):

- **Single-key `push_delta`** path (`crates/server/src/services/push_delta.rs`): `params.delta.delta_payload` is passed *directly* to `TransactionSummary::from_json` (`crates/server/src/network/miden/mod.rs:127, :152, :306` via the network adapter; `crates/server/src/ack/miden_falcon_rpo/signer.rs:64` and `crates/server/src/ack/miden_ecdsa/signer.rs:63` for ack signing). So for direct-pushed deltas the persisted `delta_payload` **is** the `TransactionSummary` JSON itself — no wrapper, no `metadata` sibling.
- **Multisig commit** path (`crates/server/src/services/push_delta_proposal.rs:58` calls `normalize_payload`): the persisted `delta_payload` is the wrapper `{ tx_summary: <TransactionSummary JSON>, metadata: { proposal_type, ... }, signatures?: [...] }` validated by `normalize_payload` / `validate_tx_summary` at `crates/server/src/services/mod.rs:191, :210`. When the proposal is executed and committed, this wrapper is preserved verbatim as the delta's `delta_payload`.
- **EVM deltas** (out of scope for category inference beyond falling back to `custom`): not a `TransactionSummary` shape at all; the normalizer recognizes them by absence of any of the above markers and returns a sentinel that drives `category = "custom"` and `kind = null`.

**Normalization rules**:

1. If `value.get("tx_summary").is_some()` → wrapper shape. Use `value["tx_summary"]` as the input to `TransactionSummary::from_json`, and use `value.get("metadata").and_then(parse_metadata)` for `proposal_type` / `kind`. Note that `tx_summary` may itself be either the JSON shape or `{ "data": "<base64>" }`; the decoder MUST handle both (`payload.rs::ProposalPayload::new` writes the JSON shape; `validate_tx_summary` validates the base64 shape — both legitimately exist in production).
2. Else if `value.get("account_delta").is_some()` (or any other unambiguous `TransactionSummary` marker — `account_delta` is the most stable) → raw `TransactionSummary` shape. Pass `value` directly to `from_json`; `metadata` is `None`.
3. Else → unrecognized shape (likely EVM or schema drift). Return a `NormalizedPayload::Opaque` variant; classifier maps to `category = "custom"`, `kind = null`, and lists `decode_warnings = [{ section: "tx_summary", reason: "unrecognized_payload_shape" }]` in the detail view.

**Test fixtures required** (added under `crates/server/src/delta_summary/tests/fixtures.rs`):

- `MULTISIG_P2ID_WRAPPER` — wrapper with `tx_summary` as JSON, `metadata.proposal_type = "p2id"`.
- `MULTISIG_P2ID_WRAPPER_BASE64` — wrapper with `tx_summary: { data: "<base64>" }`, same metadata.
- `MULTISIG_ADD_SIGNER` — wrapper, `metadata.proposal_type = "add_signer"`.
- `MULTISIG_SWITCH_GUARDIAN` — wrapper, `metadata.proposal_type = "switch_guardian"`.
- `PUSH_DELTA_RAW_TX_SUMMARY` — direct `TransactionSummary` JSON (no wrapper).
- `EVM_PLACEHOLDER` — opaque non-TransactionSummary JSON; classifier returns `custom`.
- `MALFORMED_BASE64` — wrapper whose `tx_summary.data` is not valid base64; decoder returns `Opaque` + warning.

Each fixture has an explicit assertion in the classifier unit tests for both `(category, kind)` and (where applicable) the `summary` projection.

**Sources**:
- `crates/server/src/services/push_delta_proposal.rs:6, :58, :115`
- `crates/server/src/services/push_delta.rs:82–86`
- `crates/server/src/services/mod.rs:181, :191, :210`
- `crates/server/src/network/miden/mod.rs:127, :152, :306`
- `crates/server/src/ack/miden_falcon_rpo/signer.rs:64`
- `crates/miden-multisig-client/src/payload.rs:63, :70–77`
