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

## Decision 2 — Action classification: hybrid `category` + optional `kind`

**Decision**: Surface both fields on every entry. `category` is a closed enum (`asset_transfer`, `asset_swap`, `note_consumption`, `note_creation`, `account_storage_change`, `guardian_switch`, `custom`), always non-null. `kind` is an open-ended string that echoes `metadata.proposal_type` when present (multisig deltas), null otherwise (single-key push, EVM).

**Rationale**:
- Stable dashboard taxonomy on `category` lets the UI switch on a fixed set for icons, colors, and filters; growing the enum is a wire-contract event.
- Open `kind` exposes the fine-grained `proposal_type` (e.g., `add_signer`, `change_threshold`) without forcing the closed enum to grow each time multisig adds a proposal type.
- Multisig admin operations land in `account_storage_change` for the category and the precise op shows up in `kind`, matching the operator mental model "the account's config changed, specifically X."

**Mapping rules**:
- When `metadata.proposal_type` is present (multisig): use the deterministic mapping table in FR-002a; copy `proposal_type` verbatim into `kind`.
- When absent (single-key push, EVM, malformed metadata): set `kind` to `null` and derive `category` from the on-chain `TransactionSummary` per FR-002b (input/output note kinds + account-delta shape). Fall back to `custom` when no rule matches with confidence.

**Alternatives considered**:
- 1:1 mirror of `proposal_type` only — leaves single-key push deltas uncategorized.
- Coarse-only — loses `add_signer` vs. `change_threshold` distinction.

**Sources**: spec §Clarifications Q2, FR-002 / FR-002a / FR-002b; existing `VALID_PROPOSAL_TYPES` constant at `crates/server/src/services/mod.rs:181`.

## Decision 3 — Where decode happens: dedicated `delta_summary` module

**Decision**: Create `crates/server/src/delta_summary/` with `decode.rs`, `category.rs`, `projection.rs`. The listing and detail handlers both call into it. Decoding is *on read*; nothing is precomputed or persisted.

**Rationale**:
- Both endpoints need the same decode pipeline. Sharing keeps inference rules in one place — the place tests target.
- Keeps `services/dashboard_*.rs` files focused on pagination + cursor + response shaping; decode complexity stays contained.
- Read-time decode aligns with the "no schema change" constraint; if production telemetry later shows the listing decode cost is meaningful, persisting the projection becomes a follow-up feature without breaking the wire contract.

**Cost note**: `TransactionSummary` deserialization is bytewise reads of `account_delta + input_notes + output_notes + salt`. The MAST/script-heavy sections are skipped unless the *detail* endpoint specifically asks for them (Decision 5 below). For listing this is light enough not to blow SC-004.

**Alternatives considered**:
- Inline decode in each service file — DRY violation, three copies of the inference table.
- Decode in `DashboardDeltaEntry::from_delta` — couples the wire-shape type to the decoder; harder to test the inference rules in isolation.

## Decision 4 — Vault and storage changes: signed-delta + before/after hybrid

**Decision**:
- **Vault changes**: emit per-asset entries `{ asset_id, asset_kind: "fungible"|"non_fungible", change }` where `change` is a signed-magnitude string (e.g., `"+100"`, `"-50"`) for fungible assets and `{ added: [...], removed: [...] }` lists of asset ids for non-fungible.
- **Storage / account-field changes**: emit per-slot entries `{ slot_index, before, after }` with `before` / `after` as hex `Word` strings or `null` for unset.

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

## Decision 10 — Persisted `delta_payload` has two shapes; decoder normalizes both

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
