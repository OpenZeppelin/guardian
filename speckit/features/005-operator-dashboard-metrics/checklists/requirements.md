# Specification Quality Checklist: Operator Dashboard Metrics — Pagination, Info, and Activity

**Purpose**: Validate specification completeness and quality before proceeding to planning
**Created**: 2026-05-07
**Last revised**: 2026-05-10 (Phase A implementation review)
**Feature**: [spec.md](../spec.md)

## Content Quality

- [x] No implementation details (languages, frameworks, APIs)
- [x] Focused on user value and business needs
- [x] Written for non-technical stakeholders
- [x] All mandatory sections completed

## Requirement Completeness

- [x] No [NEEDS CLARIFICATION] markers remain
- [x] Requirements are testable and unambiguous
- [x] Success criteria are measurable
- [x] Success criteria are technology-agnostic (no implementation details)
- [x] All acceptance scenarios are defined
- [x] Edge cases are identified
- [x] Scope is clearly bounded
- [x] Dependencies and assumptions identified

## Feature Readiness

- [x] All functional requirements have clear acceptance criteria
- [x] User scenarios cover primary flows
- [x] Feature meets measurable outcomes defined in Success Criteria
- [x] No implementation details leak into specification

## Notes

### Resolved during initial drafting (2026-05-07)

- Activity-entity scope → split into per-account delta history (FR-013–FR-016) and per-account proposal queue (FR-017–FR-021).
- Pending-candidate detail → resolved per-status: candidate entries carry `retry_count`; proposal-queue entries carry proposer + sigs collected/required.
- Account-id lookup → dropped entirely; `GET /dashboard/accounts/{account_id}` already serves it.
- Asset / TVL → deferred (FR-024) pending state-schema or account-inspector follow-up.

### Resolved during 2026-05-08 review pass #1

- Cursor stability honest semantics (insert-stable; sort-key updates may skip/repeat) — FR-005, US1 scenario 6, US6 scenario 3.
- Cursor TTL → out of scope; cursors validated for tampering/staleness only.
- Singular "the network" field → dropped; FR-009 returns deployment environment + per-network account counts.
- `pending`-status delta entries removed from delta history endpoint (lives only in proposal queue per FR-017).
- HTTP status codes pinned (FR-028) and exposed via SC-006/SC-012.
- Backend parity relaxation for cross-account aggregates on filesystem backend (FR-029).
- `transaction_type` field dropped from v1 (deferred).
- Schema migration (`delta.status` + status timestamp → first-class indexed columns) called out as plan-phase prerequisite.
- Default page `limit` 50 / max 500 named.

### Resolved during 2026-05-08 review pass #2

- **Account list breaking change accepted**: FR-001/FR-002/FR-007 — list endpoint is now always paginated. Full-inventory unparameterized mode and `total_count` are removed; the internal dashboard consumer is updated as part of this feature. Resolves the FR-002↔FR-007 dual-mode contradiction.
- **`cursor` without `limit`**: FR-002 + Edge Cases — applies the default `limit=50`; not a no-op, not rejected.
- **`?limit=` (bare)**: FR-002 + Edge Cases — treated as omitted; default applies.
- **Per-account history and global feeds always paginated**: FR-016/FR-021/FR-038 — the dual-mode quirk does not leak onto the other endpoints; default 50, max 500 universally; explicit "no full-history download" note on the delta history endpoint.
- **Error code overload split**: FR-028 now distinguishes `400 InvalidCursor` / `400 InvalidLimit` / `400 InvalidStatusFilter`, each carrying a stable machine-readable code in the body. SC-006 and SC-012 updated.
- **EVM accounts in proposal queue**: FR-017 — explicit note that EVM accounts (`Auth::EvmEcdsa`) never appear in the proposal queue or global proposal feed in v1 because EVM proposals do not flow through `delta_proposals`.
- **`retry_count` nullability**: FR-015 — always present on candidate entries, defaults to `0` for legacy records.
- **Global proposal feed has no `status` filter**: FR-035 — every entry is in-flight by definition.
- **Filesystem-backend threshold**: FR-029 — exposed as a server config field with documented default of 1,000 accounts.
- **`latest_activity` source pinned**: Assumptions — `MAX(latest delta.status_timestamp, latest delta_proposal originating timestamp)` across all accounts.
- **Asset/TVL "permanently" softened**: Out of Scope + FR-024 — "for the foreseeable future, to be re-evaluated only after a normalized state schema or account-inspector extension exists".
- **Field glossary added**: `account_id`, `limit`, `cursor`, `next_cursor`, `total_count`, `status`, `retry_count`, `proposer_id`, `signatures_collected`/`signatures_required`, `environment`, `latest_activity`.
- **"Not yet canonical" cross-endpoint composition**: Assumptions — explicit note that the dashboard composes `/deltas?status=candidate` with the proposal queue; not a unified endpoint in v1.
- **FR numbering**: renumbered sequentially after merging FR-007/FR-008. Final range: FR-001..FR-039.

### Asset/TVL deferral (unchanged)

- Asset / balance / token-amount / TVL data is explicitly **deferred** to a follow-up feature (FR-024 enforces non-exposure for v1). Reason: Guardian's `state_json` is an opaque client-defined blob with no normalized asset schema today, and no Guardian-side account-inspector decodes asset vaults. The gh issue's "raw token/asset data per account" note is captured as a future-feature precondition.

### Resolved during 2026-05-08 review pass #4 (entry-shape enrichment)

- **Delta entries enriched** (FR-014, FR-015, US3, SC-003): added per-account `nonce`, `prev_commitment`, `new_commitment` (nullable). Replaced the opaque `record_id` with `nonce` (integer) for clarity in dashboard tables. Source fields all already exist on the `deltas` table, no schema change.
- **Proposal entries enriched** (FR-018, FR-019, US4, SC-004): added per-account `nonce`, `prev_commitment`, `new_commitment` (nullable); renamed `record_id` → `commitment` to match storage semantics (`delta_proposal.commitment` is the cryptographic identifier cosigners are signing).
- **Field glossary updated**: added `status_timestamp`, `nonce`, `prev_commitment`, `new_commitment`, `commitment`.
- **Wire shapes (data-model.md, contracts/dashboard.openapi.yaml, quickstart.md examples)** all reflect the enriched entries.
- **No schema migration** — every new wire field is read directly from existing columns on `deltas` / `delta_proposals` per `schema.rs`.

### Resolved during 2026-05-08 review pass #3 (Option A: drop migration)

- **No schema migration in v1.** Reverted Decision 1 in `research.md` from "promote `delta.status` and a status timestamp to typed indexed columns" to "no migration; sort by immutable `id`". Backfill, dual-write, and migration risk all removed.
- **Sort keys reworked**:
  - `/dashboard/accounts`: `updated_at DESC, account_id ASC` (kept; mutable; FR-005 caveat applies to this endpoint only).
  - `/dashboard/accounts/{id}/deltas`: `delta.id DESC` (immutable, fully stable).
  - `/dashboard/accounts/{id}/proposals`: `delta_proposal.id DESC` (immutable, fully stable).
  - `/dashboard/deltas`: `delta.id DESC` (immutable, fully stable).
  - `/dashboard/proposals`: `delta_proposal.id DESC` (immutable, fully stable).
- **Cursor stability caveat scoped down**: FR-005 now narrows the "skip/repeat under sort-key updates" caveat to the account list endpoint only. The four immutable-PK endpoints are fully stable under both inserts and status updates.
- **Info per-status counts and `latest_activity`**: computed at request time via `Jsonb` extraction from the existing `status` column. No typed `status_kind`/`status_timestamp` columns introduced. An expression index on `(status->>'status')` MAY be added later as a profiling-driven optimization without changing the contract.
- **Network info dropped from info response**: `per_network_account_counts` removed — instance is gated to one network family in practice (Miden default; EVM behind a feature flag), and v1 is Miden-oriented.
- **Miden orientation explicit**: spec Context, plan Summary, and research Decision 8 now state v1 targets Miden deployments; EVM-specific dashboard surfaces are deferred.

### Resolved during 2026-05-10 implementation review (Phase A: DB-backed pagination backbone)

- **Decision 1 reversed.** The "no migration" framing was rejected mid-implementation in favor of pushing pagination, sort, and filter into SQL. The original service-layer fan-out scaled poorly and moved work the database does well into Rust.
- **Phase A schema migration added** (`crates/server/migrations/2026-05-10-000001_promote_delta_status/`): typed `status_kind text NOT NULL` and `status_timestamp timestamptz NOT NULL` columns on both `deltas` and `delta_proposals`, backfilled from the existing `status` Jsonb blob. `CHECK` constraint enforces the four lifecycle statuses; composite indexes `(status_kind, status_timestamp DESC, account_id, id)` on each table support the global feed sort. `down.sql` is reversible with `IF EXISTS` clauses.
- **Backfill is mandatory.** Existing Guardian deployments already have populated rows in both tables; the up.sql backfill is required, not optional.
- **Postgres dual-write** centralized in `derive_status_columns(&DeltaStatus) -> (&'static str, DateTime<Utc>)`; every write path that touches `status` also writes the typed columns so the two representations cannot drift.
- **Sort keys reworked again** (relative to review pass #3):
  - Per-account endpoints: still sort by the immutable PK (`delta.id DESC` / `delta_proposal.id DESC`); fully stable per-account cursors.
  - `/dashboard/deltas`: now sorts by `(status_timestamp DESC, account_id ASC, id ASC)` against the composite index; mutable sort key on status transitions, FR-005 caveat applies.
  - `/dashboard/proposals`: now sorts by `(status_timestamp DESC, account_id ASC, id ASC)`; originating timestamp is immutable while the proposal is in-flight, so traversal is stable for the lifetime of an in-queue entry.
- **`StorageBackend` trait extended** with seven new methods (`list_account_deltas_paged`, `list_account_proposals_paged`, `list_global_deltas_paged(status_filter)`, `list_global_proposals_paged`, `count_deltas_by_status`, `count_in_flight_proposals`, `latest_activity_timestamp`) plus the supporting types (`DeltaStatusKind`, `AccountHistoryCursor`, `GlobalDeltaCursor`, `GlobalProposalCursor`, `DeltaStatusCounts`, `GlobalDeltaRow`, `GlobalProposalRow`). Postgres pushes everything into SQL; filesystem keeps fan-out behind the FR-029 threshold; mock supports queue-based fixtures.
- **Service layer rewired to thin pass-throughs.** Dropped the in-memory sort/filter helpers (`compare_entries`, `max_timestamp`) that the original service-layer fan-out required. Service-layer tests now focus on cursor-kind validation, EVM short-circuit, 404/503 mapping, and wire-shape correctness; sort/filter/pagination behavior is exercised by integration tests in `crates/server/tests/dashboard_*.rs`.
- **Artifacts updated**: `research.md` Decision 1 + Decision 7, `data-model.md` Persistence Changes + cursor sort table + Lifecycle rules, `plan.md` Summary + Workstreams + Phasing + Validation, `spec.md` Data/Lifecycle Impact + Assumptions, `quickstart.md` Prerequisites, `contracts/dashboard.openapi.yaml` `latest_activity` + `status_timestamp` field descriptions, `tasks.md` (added Phase A block, replaced "no schema migration" risk callout).

### Resolved during 2026-05-10 external review (Phase B: cursor + contract correctness)

External reviewer flagged several merge-blocking bugs after Phase A landed. Fixes:

- **Bug #1 (cursor `nonce` vs surrogate `id` mismatch).** Postgres storage layer was filtering on the global `id` PK while the cursor encoded the per-account `nonce`. Per-account paging silently terminated after one page on any non-newborn database. Fix: storage trait cursors split into `AccountDeltaCursor { last_nonce }`, `AccountProposalCursor { last_nonce, last_commitment }`, `GlobalDeltaCursor { last_status_timestamp, last_account_id, last_nonce }`, `GlobalProposalCursor { last_status_timestamp, last_account_id, last_nonce, last_commitment }`. Postgres SQL queries now `ORDER BY` and filter on `nonce` (per-account) and `(status_timestamp, account_id, nonce, [commitment])` (global). Phase A migration's composite indexes updated to `(status_kind, status_timestamp DESC, account_id, nonce[, commitment])`. Inline regression test seeds 23 deltas and walks every page asserting no skip/repeat.
- **Bug #2 (filesystem-only threshold applied to Postgres too).** `enforce_aggregate_threshold` now gates on `state.storage.kind() == StorageType::Filesystem`; same fix applied to `dashboard_info.rs`. Postgres deployments above 1,000 accounts no longer hit a 503 path. Added `StorageBackend::kind()` to expose backend type; mock backend defaults to `Postgres` with `with_kind(StorageType::Filesystem)` opt-in for filesystem-degraded tests.
- **Bug #3 (OpenAPI/server error shape divergence).** Server emits `{success: false, code: <snake_case>, error, retry_after_secs?}`; `dashboard.openapi.yaml` `ErrorBody` schema rewritten to match. `DashboardErrorCode` TS union and `DASHBOARD_ERROR_CODES` set updated to drop `unauthorized` and add `authentication_failed` (the actual 401 code emitted by the operator session middleware). `parseErrorBody` no-op ternary collapsed.
- **Bug #4 (integration test files claimed but missing).** Inline regression test `cursor_walks_every_page_no_skip_no_repeat` in `dashboard_account_deltas.rs` exercises end-to-end cursor traversal against the actual `FilesystemService`. Comprehensive integration tests at `crates/server/tests/dashboard_*.rs` are still pending; their absence is now reflected in `tasks.md` rather than marked `[X]`.
- **Bug #5 (proposal commitment reconstructed from `new_commitment`).** Storage trait now returns `ProposalRecord { account_id, commitment, proposal }` from `list_account_proposals_paged` and `list_global_proposals_paged`; the wire `commitment` is the storage-layer column value (filesystem: filename; Postgres: `delta_proposals.commitment`). `DashboardProposalEntry::from_record` takes the commitment as input rather than synthesizing it.
- **Bug #6 (account list now pushes pagination into SQL).** New `MetadataStore::list_paged(limit, cursor)` and `StorageBackend::pull_states_batch(account_ids)` trait methods plus migration `2026-05-10-000002_account_metadata_pagination_index` (composite index on `(updated_at DESC, account_id ASC)`). The service is now a thin pass-through: one `list_paged` query + one batched `pull_states_batch` per page request, instead of N round trips per page. SC-001 restored to 10,000 accounts; SC-007 applies uniformly across all paginated endpoints. Inline regression test `cursor_walks_every_page_no_skip_no_repeat` in `dashboard_accounts.rs` walks an 11-account fixture across 3 pages.
- **Bug #7 (EVM filter undercounts pages).** Service-layer `has_more` is now judged from raw storage count BEFORE EVM filtering; truncation happens on the raw row vector and the cursor anchor is the last surviving raw row. Pages may have fewer than `limit` entries when EVM rows are dropped, but `next_cursor` correctly continues the walk.
- **Bug #8 (`parse_status_timestamp` falls back to `Utc::now()`).** `derive_status_columns` now returns `Result<(_, _), String>` and propagates `StorageError` on malformed/empty timestamps. Writers no longer silently rewrite indexed `status_timestamp` to wall-clock now on every touch of a legacy row.
- **Bug #11 (cursor secret per-restart).** `DashboardConfig` gains an optional `cursor_secret_hex` field, sourced from `GUARDIAN_DASHBOARD_CURSOR_SECRET`. When unset, a fresh random secret is generated per process and a startup `tracing::warn!` notes that multi-replica deployments must pin a shared value. `CursorSecret::from_hex` parses the 32-byte hex envelope.
- **Bug #14 (`count_deltas_by_status` silently drops unknown `status_kind`).** Unknown kinds now emit `tracing::warn!` with the offending value; the migration's `CHECK` constraint should make this unreachable, but a future lifecycle status addition will show up in tests/ops instead of silently zeroing the counter.
- **Bug #9, #10, #12 (cursor/code naming consistency).** `Cursor::nonce_cursor` / `Cursor::proposal_cursor` replaced by `Cursor::account_deltas` / `Cursor::account_proposals` / `Cursor::global_deltas` / `Cursor::global_proposals` factory methods that take exactly the fields each kind requires. `Cursor::last_id` field renamed to `last_nonce`. Doc comments updated to match the post-fix reality.

### Still open (not blockers; tracked as follow-ups)

- **Bug #4 (full integration test suite under `crates/server/tests/`).** Inline regression tests for Bug #1 and Bug #6 cover the cursor mechanics; a full HTTP-level matrix (info endpoint, error taxonomy, global feeds with status filter) requires exposing `testing` outside `#[cfg(test)]` or building a dedicated `integration_helpers` crate.
- **Bug #16 (cross-backend parametric tests).** Same dependency as Bug #4.

### Ready state

- No `[NEEDS CLARIFICATION]` markers; no contradictions across spec/plan/data-model/research/quickstart/contracts after the four review passes, the Phase A implementation review, and the Phase B external review.
- All blocker bugs from the external review resolved.
- Spec, plan, and tasks are coherent with the as-built code in `crates/server/src/storage/{mod,postgres,filesystem}.rs`, `crates/server/src/testing/mocks.rs`, and the `dashboard_*` service modules.
