# Implementation Plan: Dashboard delta activity feed and detail view

**Branch**: `007-dashboard-delta-details` | **Date**: 2026-05-24 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `speckit/features/007-dashboard-delta-details/spec.md`

## Summary

Enrich the two existing dashboard delta listing endpoints (`GET /dashboard/accounts/{account_id}/deltas`, `GET /dashboard/deltas`) with derived activity fields (action `category`, optional `kind`, asset/recipient/note-count summary) and add a new per-delta detail endpoint (`GET /dashboard/accounts/{account_id}/deltas/{nonce}`) that decodes the persisted `TransactionSummary` into structured input/output notes, vault changes, and storage/account changes. Reference key is the composite `{account_id, nonce}`. No schema migration, no protocol change. Proposals out of scope.

## Technical Context

**Language/Version**: Rust 1.93.0 (server, pinned in `rust-toolchain.toml`); TypeScript 5.x (operator client, ESM)
**Primary Dependencies**: `axum`, `tokio`, `serde`, `serde_json`, `sqlx` (Postgres) + filesystem store for server; `miden-protocol` (`0xMiden/miden-base`) for `TransactionSummary` decode; `base64` for the wrapper-shape payload; existing `dashboard::cursor`, `dashboard::authz`, `services::dashboard_pagination` scaffolding.
**Storage**: Existing `deltas` table (Postgres + filesystem-backed equivalent). Schema unchanged. Reads only.
**Testing**: `cargo test` with inline `#[cfg(test)] mod tests` blocks colocated in the service / handler files (the convention used elsewhere in `crates/server`, e.g. the existing `dashboard_account_deltas.rs` test module at the bottom of the file); `vitest` for the TS operator client. No new files under `crates/server/tests/` — that directory is not the repo convention.
**Target Platform**: Server binary on Linux (containerized); TS operator client consumed by browsers (smoke-web harness) and Node.
**Project Type**: Web service (`crates/server`) + TypeScript SDK (`packages/guardian-operator-client`).
**Performance Goals**: SC-004 — enriched listing latency must show no perceptible regression vs. current commitment-only listing on a default page size. Verified by a local Criterion bench seeded with mixed-shape fixtures (Decision 8 in `research.md`), not by the prod benchmark skill.
**Constraints**: Append-only `deltas` table; read-only feature; no schema migration; no new env vars. (Constitution III is upheld trivially because nothing is written.) Listing endpoints continue to surface the existing `candidate` / `canonical` / `discarded` triplet — *not* a wire-level "canonical only" filter (FR-006 as revised). No new wire field may remove or rename existing fields (FR-021). `category` MUST be non-null on every entry (SC-002). Authorization scoping unchanged from existing dashboard behavior — v1 has no per-account ACL (documented in spec edge case "Operator authorization scope (v1)").
**Scale/Scope**: Dashboard pages default to ~50 entries; high-volume accounts may have tens of thousands of canonical deltas; the detail endpoint is a point lookup. No change to pagination behavior.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Check | Status |
|---|---|---|
| I. Bottom-Up Change Propagation | Server changes land first (enriched listing + new detail endpoint). Only the **TypeScript** operator client (`packages/guardian-operator-client`) consumes these endpoints today — no Rust operator client exists (verified by `find crates -maxdepth 2 -name Cargo.toml` returning none for `guardian-operator-client`). The TS client and its `vitest` suites are updated in lockstep. | ✅ Pass |
| II. Transport and Cross-Language Parity | Dashboard endpoints are HTTP-only by design (no `dashboard` references in `crates/server/src/api/grpc.rs`). Rust ↔ TypeScript parity applies only to consumers; only TS consumes these endpoints, so parity reduces to "TS client mirrors HTTP wire shape exactly." | ✅ Pass |
| III. Append-Only Integrity and Explicit Lifecycles | Feature is read-only. No status transitions, no fallback paths, no implicit state rewrites. Listing endpoints continue to surface the existing `candidate` / `canonical` / `discarded` triplet (FR-006 amended in spec); detail endpoint surfaces whatever current `DashboardDeltaStatus` is (edge case: status transitions after listing). | ✅ Pass |
| IV. Explicit Authentication and Stable Boundary Errors | Reuses existing cookie-session authz (`guardian_operator_session`, `crates/server/src/dashboard/config.rs:7`). Detail endpoint adds two error outcomes that map to existing variants: `GuardianError::InvalidInput(_)` for malformed `nonce` (FR-018), and a unified 404 body across `DeltaNotFound` / `AccountNotFound` (FR-017, SC-008). The body unification requires explicit shaping at the handler boundary — see Validation Matrix row "uniform 404 body". Existing listing endpoint errors unchanged. | ✅ Pass |
| V. Evidence-Driven Delivery | Per-story acceptance tests defined in the spec; validation matrix below maps each FR/SC to a concrete test. | ✅ Pass |

**System invariants check** (constitution §System Invariants):

| Invariant | Disposition |
|---|---|
| Append-only / canonical lineage | Untouched. |
| HTTP/gRPC parity | Not in scope (HTTP-only dashboard surface). |
| Rust/TS workflow parity | TS-only consumer. |
| Discarded deltas not in default flows | Listing endpoints preserve current status filter (none on per-account, optional on global); we do not change the default. |
| Per-account auth, replay protection | Untouched. |
| Filesystem and Postgres backends share semantics | Both back the same `DeltaObject` shape; the decoder operates on `serde_json::Value`, backend-agnostic. |

## Project Structure

### Documentation (this feature)

```text
speckit/features/007-dashboard-delta-details/
├── plan.md              # This file
├── spec.md              # Feature spec (post /speckit.clarify)
├── research.md          # Phase 0 output — 10 decisions, including payload-shape split (D10)
├── data-model.md        # Phase 1 output — wire entities + error map
├── quickstart.md        # Phase 1 output — local validation loop (port 3000, cookie auth)
├── contracts/           # Phase 1 output — three contracts
│   ├── http-list-account-deltas.md
│   ├── http-list-global-deltas.md
│   └── http-get-account-delta-detail.md
├── checklists/
│   └── requirements.md
└── tasks.md             # NOT YET CREATED — Phase 2 output, will be generated by /speckit.tasks after this plan is approved
```

### Source Code (repository root)

```text
crates/server/
├── src/
│   ├── api/
│   │   └── dashboard_feeds.rs                          # EXTEND: enriched response shapes; ADD list_account_delta_detail_handler; ADD inline #[cfg(test)] cases
│   ├── services/
│   │   ├── dashboard_account_deltas.rs                 # EXTEND: DashboardDeltaEntry adds category/kind/summary; inline tests added
│   │   ├── dashboard_global_deltas.rs                  # EXTEND: shares DashboardDeltaEntry projection logic
│   │   ├── dashboard_account_delta_detail.rs           # NEW: get_account_delta_detail service + inline tests
│   │   └── mod.rs                                      # re-exports the new service
│   ├── delta_object.rs                                 # Existing; unchanged
│   ├── delta_summary/                                  # NEW module
│   │   ├── mod.rs                                      # public: classify, decode_full, NormalizedPayload
│   │   ├── decode.rs                                   # delta_payload → NormalizedPayload (handles BOTH shapes per Decision 10)
│   │   ├── category.rs                                 # proposal_type → category (FR-002a); on-chain inference (FR-002b)
│   │   ├── projection.rs                               # AccountDelta + InputNotes + OutputNotes → wire shapes
│   │   └── tests/
│   │       └── fixtures.rs                             # 7 fixtures per Decision 10
│   ├── builder/handle.rs                               # WIRE: register list_account_delta_detail_handler — see §Handler wiring below for the exact line
│   └── error.rs                                        # No new variants — reuses InvalidInput / DeltaNotFound / AccountNotFound

packages/guardian-operator-client/
└── src/
    ├── http.ts                                         # EXTEND listAccountDeltas/listGlobalDeltas parsers; ADD getAccountDeltaDetail
    ├── server-types.ts                                 # ADD DashboardDeltaCategory, DeltaActivitySummary, DecodedNote, VaultChange, StorageChange, etc.
    ├── http.test.ts                                    # ADD test cases mirroring server contract tests
    └── permissions.ts                                  # Unchanged
```

**Structure Decision**: Server-side, follow the existing per-route service pattern (`crates/server/src/services/dashboard_*.rs`) — one service module per endpoint, handler in `api/dashboard_feeds.rs`. A new `delta_summary` module concentrates all `TransactionSummary` decode + category-inference logic so listing + detail share it (and tests target a single surface). All tests are colocated with the code under `#[cfg(test)] mod tests` blocks — the repo convention; `crates/server/tests/` is not used. TS-side, extend `http.ts` + `server-types.ts`; no new file needed.

### Handler wiring

The new detail endpoint registers under the existing dashboard router builder in `crates/server/src/builder/handle.rs:93–111`, immediately after the existing per-account deltas route at `:100–103`. The dashboard sub-router is mounted under `/dashboard` (see `:109–110`), so the relative path is `/accounts/{account_id}/deltas/{nonce}`.

```rust
// crates/server/src/builder/handle.rs, immediately after the existing
// .route("/accounts/{account_id}/deltas", get(list_account_deltas_handler)) at lines 100–103:
.route(
    "/accounts/{account_id}/deltas/{nonce}",
    get(list_account_delta_detail_handler),
)
```

Path extractor uses Axum's `Path<(String, String)>` to receive both segments; the handler then parses the `nonce` segment per FR-009a (canonical base-10 `u64`, rejecting hex, negatives, leading zeros, etc.) and translates the parse failure into `GuardianError::InvalidInput`. The `route_layer(from_fn_with_state(dashboard_read_authz, enforce_authz))` at `:111` already covers the new route because it is registered before the layer is applied, identical to the existing per-account deltas route.

## Implementation Phases

Story order P1 → P2 → P2 maps to phases that can be merged independently as long as the foundation phase ships first. Each phase has its own slice of tests; nothing is gated on later phases existing.

### Phase A — Foundation: `delta_summary` module + payload normalization (Decision 10)

Goal: stand up the shared decode/classify pipeline, with full unit coverage on both `delta_payload` shapes, before any wire-facing change.

- Create `crates/server/src/delta_summary/{mod,decode,category,projection}.rs` and `tests/fixtures.rs`.
- Implement `NormalizedPayload::resolve(value: &serde_json::Value)` that branches on the presence of `tx_summary`:
  - **Wrapper** (multisig commit): extract `tx_summary` (handle both JSON-inline and `{data: base64}` sub-shapes), parse `metadata.proposal_type`.
  - **Raw** (`push_delta`): treat the value as a `TransactionSummary` JSON directly.
  - **Opaque** (EVM / unknown): produce a sentinel that drives `category = "custom"`.
- Implement `classify(normalized) -> (category, kind, summary)` per the table in `data-model.md`.
- Implement `decode_full(normalized, include_scripts) -> (input_notes, output_notes, vault_changes, storage_changes, warnings)` per `data-model.md`.
- Inline unit tests assert `(category, kind, summary)` for all 7 fixtures from Decision 10.

**Exit criteria**: `cargo test -p guardian-server delta_summary` passes; the module is callable from elsewhere but not yet wired into any handler.

### Phase B — Story 1: enriched listing endpoints (P1)

Goal: ship the activity feed with `category` / `kind` / `summary` while preserving every existing field.

- Extend `DashboardDeltaEntry` (`crates/server/src/services/dashboard_account_deltas.rs:40`) with `category`, `kind`, `summary` (per `data-model.md`). Keep `proposal_type` for backwards compat (Decision 7).
- Update `DashboardDeltaEntry::from_delta` (`:88`) to call `delta_summary::classify` on the persisted `delta_payload`. On decode failure, the entry is still returned with `category = "custom"`, `kind = null`, `summary` fields nulled — per FR-004.
- **`DashboardGlobalDeltaEntry` is a *separate* flat struct** at `crates/server/src/services/dashboard_global_deltas.rs:49–62`, mapped by hand in `entry_from(...)` at `:107–118` — *not* a `..DashboardDeltaEntry` spread. Extending the wire shape therefore requires touching both structs and both projection functions. The cleanest fix is to extract a `fn build_entry_payload(delta: &DeltaObject) -> EntryPayload` helper that produces every field except `account_id`, then have both `DashboardDeltaEntry::from_delta` and `entry_from` consume it — keeping the two structs in lockstep without forcing a public-API rename.
- Extend the TS operator client's `parseDeltaEntry` (`packages/guardian-operator-client/src/http.ts:1338`) and `parseDeltaPage` (`:1254`) to read the new fields; add types in `server-types.ts`.
- Inline tests in both service files cover: each category, single-key push vs multisig, malformed payload still returns entry.
- TS tests in `packages/guardian-operator-client/src/http.test.ts` (delta-listing block starting at `:570` with the first `it(...)` at `:580`) extended to assert the new fields without weakening existing assertions.

**Exit criteria**: SC-001, SC-002, SC-006, SC-007 satisfied end-to-end through HTTP. The TS smoke test (skill `smoke-test-operator-dashboard`) still passes.

### Phase C — Story 2 + Story 3: detail endpoint with `{account_id, nonce}` key (P2)

Goal: ship the new detail endpoint and the round-trip key contract.

- Add `crates/server/src/services/dashboard_account_delta_detail.rs` with `get_account_delta_detail(state, account_id, nonce, include_scripts) -> Result<DashboardDeltaDetail>`.
- Add `list_account_delta_detail_handler` in `api/dashboard_feeds.rs`; parse the `nonce` segment per FR-009a; map parse failures to `GuardianError::InvalidInput`.
- Wire the route in the dashboard router builder (path documented in §Handler wiring).
- Normalize the 404 body so `DeltaNotFound` and `AccountNotFound` are field-level identical (SC-008). The easiest path is to route both to a single error variant at the handler boundary; the explicit approach is a `match` in the handler that maps both into the same `Response`.
- `?include=scripts` and `?include=raw` are handler-level booleans, parsed from a single `?include=` comma-list query param; default off.
- Add `getAccountDeltaDetail(accountId, nonce, opts)` to the TS operator client.
- Inline tests cover the 5 user-story-2 acceptance scenarios + the 3 user-story-3 acceptance scenarios + all detail-contract behavioral invariants.

**Exit criteria**: SC-003, SC-005, SC-008 satisfied; quickstart Story 3 round-trip succeeds.

### Phase D — Performance verification (Decision 8)

Goal: prove no perceptible regression vs. baseline (SC-004).

- Add a Criterion bench (or `#[tokio::test]` timing harness if Criterion overkill) under `crates/server/src/services/dashboard_account_deltas.rs` exercising `list_account_deltas` against ~500 mixed-shape canonical-status deltas seeded in the filesystem backend.
- Compare baseline (without `category`/`kind`/`summary` returned) vs. enriched. Acceptance: p95 within the same envelope.
- If the bench shows a measurable regression, escalate to Decision 3's follow-up (persist the projection); do not ship without a recorded baseline.

**Exit criteria**: bench output committed to the PR description; no perceptible regression vs. baseline.

## Validation Matrix

Each requirement / success criterion maps to a concrete test. Mostly inline `#[cfg(test)]` modules per the repo convention.

| Requirement | Where | Test |
|---|---|---|
| FR-001 (existing fields preserved) | TS `http.test.ts:570+ (delta-listing describe block; first `it` at :580)` | Existing per-account listing assertion left untouched; passes unmodified. |
| FR-002 / FR-002a / FR-002b (category + kind) | `delta_summary` unit | Per-fixture assertion (7 fixtures, Decision 10). |
| FR-003 (summary fields) | `dashboard_account_deltas.rs` inline | Assert asset/counterparty/note_counts on a seeded p2id delta. |
| FR-004 (malformed payload tolerated) | `delta_summary` unit + `dashboard_account_deltas.rs` inline | `MALFORMED_BASE64` fixture: entry returned, `category = custom`, summary fields null. |
| FR-005 (pagination unchanged) | `dashboard_feeds.rs` inline | Existing pagination tests left untouched; passes unmodified. |
| FR-006 (amended: lifecycle feed preserved) | `dashboard_feeds.rs` inline | Existing tests at `:289`/`:325`/`:354`/`:407` continue to pass; no new filtering added. |
| FR-007 / FR-008 / FR-009 / FR-009a (key) | `dashboard_account_delta_detail.rs` inline | Listing → detail round-trip; URL parse rejection cases (negative, hex, leading zero, non-decimal). |
| FR-010 / FR-011 (detail endpoint shape) | `dashboard_account_delta_detail.rs` inline | Full shape assertion on a p2id delta. |
| FR-012 (decoded notes) | `delta_summary::projection` unit | Per-note-tag assertion (p2id, p2ide, pswap, mint, burn, custom). |
| FR-013 / FR-014 (vault + storage changes) | `delta_summary::projection` unit | Fungible signed-delta, non-fungible add/remove, storage slot before/after. |
| FR-015 (raw debug field) | `dashboard_account_delta_detail.rs` inline | `?include=raw` round-trip; field absent without param. |
| FR-016 (partial decode warnings) | `dashboard_account_delta_detail.rs` inline | `MALFORMED_BASE64` fixture: `decode_warnings[]` present, other sections still populated. |
| FR-017 / SC-008 (uniform 404 body) | `dashboard_account_delta_detail.rs` inline | `serde_json::Value` diff of `DeltaNotFound` vs `AccountNotFound` bodies = empty. |
| FR-018 (400 on malformed nonce) | `dashboard_account_delta_detail.rs` inline | Each malformed-nonce input returns 400, body shape = existing `InvalidInput`. |
| FR-019 (proposals out of scope) | N/A — verified by code review (no proposal-side changes). |
| FR-020 / Decision 9 (TS parity) | `http.test.ts` | TS client parses every field server emits; mismatch fails fast. |
| FR-021 / SC-007 (no field removed) | TS `http.test.ts` | Existing assertions on `nonce`/`status`/`prev_commitment`/`new_commitment`/`proposal_type`/`retry_count` continue to pass. |
| SC-001 (1-liner derivable) | Manual review of fixture set + spec acceptance scenarios. |
| SC-002 (`category` non-null) | `delta_summary` unit | Property assertion: classifier never returns `None` for category. |
| SC-004 (no perceptible regression) | Phase D bench | p95 enriched within baseline envelope. |
| Decision 10 (payload shape split) | `delta_summary::tests::fixtures` | 7 fixtures, each driving a distinct branch through the decoder. |

## Documentation impact

- `docs/dashboard.md` — operator-facing reference. Add a section describing the enriched listing shape and the new detail endpoint, mirroring the wire contracts. **Required before merge** per CLAUDE.md "Output discipline" + `CONTRIBUTING.md` docs table.
- `docs/CONFIGURATION.md` — unchanged; no new env vars introduced.
- `spec/` directory (protocol spec) — unchanged; this feature does not touch the protocol surface.
- `packages/guardian-operator-client/README.md` — add the new method signatures and a short example for `getAccountDeltaDetail`.

## Complexity Tracking

> No constitution violations require justification. Table intentionally empty.

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| — | — | — |

## Next command

Once this plan is reviewed, run `/speckit.tasks` to generate `tasks.md` with dependency-ordered tasks. The phases above map cleanly to task groups: Phase A first, B and C after A, D last.
