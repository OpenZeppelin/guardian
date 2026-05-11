# Tasks: Operator Dashboard Metrics — Pagination, Info, and Activity

**Feature Key**: `005-operator-dashboard-metrics`
**Spec**: [spec.md](./spec.md) | **Plan**: [plan.md](./plan.md)
**Generated**: 2026-05-08

This is a Miden-oriented v1. A Phase A schema migration promotes
`status_kind` and `status_timestamp` to typed indexed columns on both
`deltas` and `delta_proposals` (with mandatory backfill from the
existing `status` Jsonb blob); the Postgres backend pushes
pagination, status filtering, and sort entirely into SQL via these
columns, while per-account history endpoints continue to sort by the
immutable primary key. Cursor pagination on all new endpoints. Error
taxonomy pinned per FR-028. See `plan.md` for the workstream layout
and `data-model.md` for wire shapes.

## Phase 1: Setup

- [X] T001 Add `filesystem_aggregate_threshold: usize` (default `1000`) to the dashboard config struct in `crates/server/src/dashboard/config.rs`, with tests in the same module verifying default and custom values
- [X] T002 [P] Scaffold empty modules with `pub use` re-exports: `crates/server/src/dashboard/cursor.rs`, `crates/server/src/services/dashboard_pagination.rs`, `crates/server/src/services/dashboard_info.rs`, `crates/server/src/services/dashboard_account_deltas.rs`, `crates/server/src/services/dashboard_account_proposals.rs`, `crates/server/src/services/dashboard_global_deltas.rs`, `crates/server/src/services/dashboard_global_proposals.rs`, `crates/server/src/api/dashboard_history.rs`. Wire each into its parent `mod.rs`
- [X] T003 [P] Scaffold response type files in `packages/guardian-operator-client/src/server-types.ts`: add empty type aliases `DashboardInfoResponse`, `DashboardDeltaEntry`, `DashboardProposalEntry`, `PagedResult<T>`, `DashboardErrorCode` so subsequent phases fill them in

## Phase 2: Foundational (BLOCKING — must complete before any user story)

- [X] T004 Implement opaque HMAC-signed cursor codec in `crates/server/src/dashboard/cursor.rs` with `CursorKind { AccountList, AccountDeltas, AccountProposals, GlobalDeltas, GlobalProposals }` and `Cursor { kind, last_id: i64, last_account_id: Option<String>, last_updated_at: Option<DateTime<Utc>> }`; base64url envelope; signing secret read from existing dashboard config block; encode/decode + tamper detection
- [X] T005 Add unit tests in `crates/server/src/dashboard/cursor.rs` covering: roundtrip per kind, signature tamper rejection, kind-mismatch rejection, malformed base64 rejection, decoding cursors signed under a previous secret accepted during rotation window
- [X] T006 Implement pagination helpers in `crates/server/src/services/dashboard_pagination.rs`: `parse_limit(opt: Option<String>) -> Result<u32, GuardianError>` enforcing `[1, 500]` with default 50 and bare `?limit=` treated as omitted; `parse_cursor<K>(opt, expected_kind)`; `PagedResult<T> { items: Vec<T>, next_cursor: Option<String> }`; unit tests for limit boundary cases including `0`, `501`, negative, and non-integer
- [X] T007 Extend `crates/server/src/error.rs` with typed variants `InvalidCursor`, `InvalidLimit`, `InvalidStatusFilter`, `DataUnavailable`, each serializing to a stable response body shape `{"error":{"code":"<Variant>","message":"...","details":{...}}}` per FR-028; map to HTTP status codes 400/400/400/503; preserve existing variants
- [X] T008 [P] Add error-taxonomy unit tests in `crates/server/src/error.rs` (or a dedicated `error_test.rs`) covering: each new variant serializes to the expected `code` string, response status code matches the FR-028 mapping, body schema validates against `contracts/dashboard.openapi.yaml`'s `ErrorBody`
- [X] T009 [P] Add filesystem aggregate-walk helper in `crates/server/src/storage/filesystem.rs`: `count_accounts() -> Result<usize, String>` plus a generic `fan_out_aggregate<T>(threshold: usize, fold: impl Fn(...) -> T)` that short-circuits to `AggregateUnavailableReason::FilesystemThresholdExceeded` when `count_accounts() > threshold`. Return type is `Result<T, AggregateUnavailableReason>`; unit test the threshold short-circuit path
- [X] T010 [P] Add `PagedResult<T>` and `DashboardErrorCode` typed wire types to `packages/guardian-operator-client/src/server-types.ts` matching `contracts/dashboard.openapi.yaml`; export from `index.ts`
- [X] T011 [P] Add HTTP-error parsing helper in `packages/guardian-operator-client/src/http.ts`: `parseErrorBody(response: Response) -> { code: DashboardErrorCode, message: string, details?: unknown }` that branches on `code` not status alone; unit test the five-code matrix

## Phase 3: User Story 1 — Page Through Many Accounts (Priority: P1)

**Story goal**: Operators can paginate the account list with cursor.
**Independent test**: Seed >50 accounts, request first page with `?limit=50`, follow `next_cursor` to the end, verify each account appears exactly once across the traversal under quiescent inventory; verify `?limit=9999` → `400 InvalidLimit`; verify tampered cursor → `400 InvalidCursor`; verify omitted `limit` returns first 50 with cursor.

- [X] T012 [US1] Add Postgres-backed paginated account read in `crates/server/src/storage/postgres.rs`: `list_account_metadata_paged(limit: u32, cursor: Option<AccountListCursor>) -> Result<(Vec<AccountMetadata>, Option<AccountListCursor>), String>` ordering by `updated_at DESC, account_id ASC` with the cursor predicate `(updated_at, account_id) < ($cursor_updated_at, $cursor_account_id)`
- [X] T013 [US1] Add filesystem-backed equivalent in `crates/server/src/storage/filesystem.rs`: load all account metadata, sort in-memory by `(updated_at DESC, account_id ASC)`, slice by limit + cursor offset semantics
- [X] T014 [US1] Rewrite `crates/server/src/services/dashboard_accounts.rs::list_dashboard_accounts` as `list_dashboard_accounts_paged(state: &AppState, limit: u32, cursor: Option<String>) -> Result<PagedResult<DashboardAccountSummary>, GuardianError>`. Remove the unparameterized variant and the `total_count` field from the response. Reuse the existing `from_parts` constructor and the existing per-entry shape (FR-006 superset compatibility)
- [X] T015 [US1] Update HTTP handler in `crates/server/src/api/dashboard.rs::list_operator_accounts` to parse `?limit=` and `?cursor=` via the helpers from T006, call `list_dashboard_accounts_paged`, map errors via T007 variants. Remove the prior `DashboardAccountsResponse { total_count, accounts }` shape; emit `PagedResult<DashboardAccountSummary>` instead
- [X] T016 [P] [US1] Add `listOperatorAccountsPaged({ limit?, cursor? })` to `packages/guardian-operator-client/src/http.ts` returning `PagedResult<DashboardAccountSummary>`; remove the existing unparameterized `listOperatorAccounts` wrapper; update `index.ts` exports
- [X] T017 [US1] Add integration test `crates/server/tests/dashboard_paged_accounts.rs` covering US1 acceptance scenarios 1–8: explicit limit + cursor traversal, end-of-list empty page with `next_cursor: null`, tampered cursor → `400 InvalidCursor`, insert during paging (no duplicate on later page), `updated_at` bump during paging (skip/repeat allowed per FR-005 caveat), bare `?limit=` defaults to 50, `?limit=9999` → `400 InvalidLimit`, cursor without limit applies default 50
- [X] T018 [P] [US1] Add TS test cases to `packages/guardian-operator-client/src/http.test.ts` for `listOperatorAccountsPaged` happy-path and the invalid-limit / invalid-cursor error matrix
- [X] T019 [US1] Update `spec/api.md` dashboard section: replace the unparameterized list documentation with the paginated contract; document `PagedResult<T>` envelope and the `400`-subtype error codes; mark the change as a breaking-vs-`003` deviation

## Phase 4: User Story 2 — Inventory and Health Summary (Priority: P1)

**Story goal**: Operators get a one-shot inventory and lifecycle summary.
**Independent test**: Seed accounts and a known mix of delta+proposal records, request `/dashboard/info`, verify `total_account_count`, `delta_status_counts`, `in_flight_proposal_count` match the seed exactly; empty Guardian → explicit zeros and `latest_activity: null`; unauthenticated → `401`.

- [X] T020 [US2] Implement `crates/server/src/services/dashboard_info.rs::get_dashboard_info(state: &AppState) -> Result<DashboardInfo, GuardianError>` returning the wire shape from `data-model.md` `DashboardInfoResponse`. Compute `total_account_count` via existing metadata count; `delta_status_counts` via `SELECT status->>'status' AS k, COUNT(*) FROM deltas GROUP BY k`; `in_flight_proposal_count` via `SELECT COUNT(*) FROM delta_proposals WHERE status->>'status' = 'pending'`; `latest_activity` via `GREATEST(MAX((delta.status->>'timestamp')::timestamptz), MAX((delta_proposal.status->>'timestamp')::timestamptz))`; `environment` from server config; `service_status: "healthy"` if all aggregates loaded else `"degraded"` with affected aggregate names in `degraded_aggregates`
- [X] T021 [US2] Add filesystem implementation of the same aggregates using the T009 fan-out walk helper. Above the threshold, mark the affected aggregate(s) in `degraded_aggregates` and set `service_status: "degraded"`; do not full-scan
- [X] T022 [US2] Add HTTP route `GET /dashboard/info` in `crates/server/src/api/dashboard.rs` behind the operator-session middleware; serialize `DashboardInfo` directly (no envelope wrapping per data-model.md)
- [X] T023 [P] [US2] Add `getDashboardInfo() -> Promise<DashboardInfoResponse>` to `packages/guardian-operator-client/src/http.ts`; finalize the `DashboardInfoResponse` type in `server-types.ts`
- [X] T024 [US2] Add integration test `crates/server/tests/dashboard_info.rs` covering US2 acceptance scenarios 1–3: seeded inventory totals match exactly, empty Guardian returns explicit zeros + `latest_activity: null`, unauthenticated → `401`, plus a degraded-aggregate fixture (force one source to fail) verifying `degraded_aggregates` contains the affected aggregate name and `service_status: "degraded"`
- [X] T025 [US2] Update `spec/api.md` with the `/dashboard/info` endpoint and the `DashboardInfoResponse` shape

## Phase 5: User Story 3 — Per-Account Delta History (Priority: P2)

**Story goal**: Operators can drill into one account's delta history.
**Independent test**: Seed an account with deltas in candidate, canonical, and discarded statuses (and a candidate with `retry_count > 0`); request `/dashboard/accounts/{id}/deltas?limit=10`; verify newest-first by `delta.id DESC`, every entry carries `nonce`/`status`/`status_timestamp`/`prev_commitment`/`new_commitment` (nullable), candidate entries carry `retry_count`, unknown account → `404`, known account with no deltas → `200` empty page.

- [X] T026 [US3] Add Postgres paginated delta read in `crates/server/src/storage/postgres.rs`: `list_deltas_for_account(account_id: &str, limit: u32, cursor: Option<i64>) -> Result<(Vec<DeltaSummary>, Option<i64>), String>` ordered by `id DESC` with `WHERE account_id = $1 AND id < $cursor_id` (when cursor present). Returns minimal columns: `id`, `nonce`, `prev_commitment`, `new_commitment`, `status` (Jsonb)
- [X] T027 [US3] Add filesystem equivalent in `crates/server/src/storage/filesystem.rs`: per-account delta dir walk, filter by `id < cursor_id`, sort by `id DESC`, slice by limit
- [X] T028 [US3] Implement `crates/server/src/services/dashboard_account_deltas.rs::list_account_deltas(state, account_id, limit, cursor) -> Result<PagedResult<DashboardDeltaEntry>, GuardianError>`. Map storage rows to `DashboardDeltaEntry` per `data-model.md`: extract `status` and `status_timestamp` from the Jsonb `status` column; populate `retry_count` only on candidate entries (default 0); return `404 AccountNotFound` if metadata missing; return `503 DataUnavailable` if metadata exists but delta read fails. Filter out `pending`-status entries (those live in `delta_proposals` per FR-014)
- [X] T029 [US3] Add HTTP route `GET /dashboard/accounts/{account_id}/deltas?limit=&cursor=` in `crates/server/src/api/dashboard_history.rs`; URL-decode the path account ID; map errors via the FR-028 taxonomy
- [X] T030 [P] [US3] Add `listAccountDeltas(accountId, { limit?, cursor? })` wrapper to `packages/guardian-operator-client/src/http.ts`; finalize `DashboardDeltaEntry` type in `server-types.ts` with `retry_count?` and `new_commitment: string | null`
- [X] T031 [US3] Add integration test `crates/server/tests/dashboard_account_history.rs` (US3 part) covering all four acceptance scenarios + cursor traversal across multiple pages + `prev`/`new` commitment correctness on a canonical entry that chains to the next entry's `prev_commitment` + `retry_count` defaulting to 0 for legacy fixtures
- [X] T032 [P] [US3] Add TS test cases to `http.test.ts` for `listAccountDeltas` happy path + 404 + 503 + invalid-cursor matrix
- [X] T033 [US3] Document `/dashboard/accounts/{id}/deltas` in `spec/api.md` with the `DashboardDeltaEntry` shape

## Phase 6: User Story 4 — Per-Account In-Flight Proposal Queue (Priority: P2)

**Story goal**: Operators can see in-flight multisig proposals per account.
**Independent test**: Seed a multisig account with one in-flight proposal at 2/3 sigs and a single-key Miden account; request the proposal queue for both; verify the multisig response carries `commitment`/`nonce`/`proposer_id`/`originating_timestamp`/`signatures_collected: 2`/`signatures_required: 3`/`prev_commitment`/`new_commitment`; verify single-key returns empty page; verify EVM account (`Auth::EvmEcdsa`) returns empty page; verify unknown account → `404`.

- [X] T034 [US4] Add Postgres paginated proposal read in `crates/server/src/storage/postgres.rs`: `list_proposals_for_account(account_id, limit, cursor) -> Result<(Vec<ProposalSummary>, Option<i64>), String>` ordered by `id DESC`, filtered to `status->>'status' = 'pending'`. Returns `id`, `commitment`, `nonce`, `prev_commitment`, `new_commitment`, `status` (Jsonb — for `proposer_id` and `cosigner_sigs.len()`)
- [X] T035 [US4] Add filesystem equivalent in `crates/server/src/storage/filesystem.rs` walking the per-account `delta_proposals` dir
- [X] T036 [US4] Implement `crates/server/src/services/dashboard_account_proposals.rs::list_account_proposals(state, account_id, limit, cursor) -> Result<PagedResult<DashboardProposalEntry>, GuardianError>`. EVM accounts (`Auth::EvmEcdsa`) early-return empty page. Compute `signatures_required` per FR-019: for `MidenFalconRpo`/`MidenEcdsa` use `cosigner_commitments.len()`; `signatures_collected` from the `cosigner_sigs` array length on the `Pending` status variant. Populate `proposer_id` from the same Jsonb path. Map 404/503 per FR-022
- [X] T037 [US4] Add HTTP route `GET /dashboard/accounts/{account_id}/proposals?limit=&cursor=` in `crates/server/src/api/dashboard_history.rs`
- [X] T038 [P] [US4] Add `listAccountProposals(accountId, { limit?, cursor? })` wrapper to `packages/guardian-operator-client/src/http.ts`; finalize `DashboardProposalEntry` type with `new_commitment: string | null`
- [X] T039 [US4] Extend `crates/server/tests/dashboard_account_history.rs` (US4 part) with US4 acceptance scenarios 1–4 plus EVM-account empty-page and single-key Miden empty-page cases
- [X] T040 [P] [US4] Add TS test cases to `http.test.ts` for `listAccountProposals` including EVM/single-key empty + 404 cases
- [X] T041 [US4] Document `/dashboard/accounts/{id}/proposals` in `spec/api.md`; cross-reference the `commitment` and `nonce` semantics

## Phase 7: User Story 5 — Explicit Read Outcomes (Priority: P3)

**Story goal**: All new endpoints return the pinned error taxonomy with stable body codes.
**Independent test**: For every new endpoint, verify each of the FR-028 error codes is reachable and the response body's `error.code` matches the documented value; verify clients can branch on `code` not `message` or HTTP status alone.

- [X] T042 [US5] Add comprehensive error-matrix integration test `crates/server/tests/dashboard_errors.rs` exercising every endpoint × every FR-028 error category: `401 Unauthorized` (no session), `404 AccountNotFound` (path-addressed endpoints with unknown id), `400 InvalidCursor` (tampered cursor on each paginated endpoint), `400 InvalidLimit` (out-of-range on each paginated endpoint), `400 InvalidStatusFilter` (global delta feed with unknown status), `503 DataUnavailable` (forced storage read failure on per-account history endpoints). Assert the body's `error.code` for every response
- [X] T043 [P] [US5] Add a corresponding TS error-matrix test in `packages/guardian-operator-client/src/http.test.ts` asserting that the typed `DashboardErrorCode` union is correctly emitted by `parseErrorBody` for every category

## Phase 8: User Story 6 — Global Delta Feed (Priority: P3, smallest)

**Story goal**: Operators get a cross-account delta feed with optional status filter.
**Independent test**: Seed deltas across at least 3 accounts in mixed lifecycle states; request `/dashboard/deltas?limit=10` with no filter, verify newest-first by `delta.id DESC` and every entry carries `account_id`; request with `?status=candidate,canonical` and verify only those statuses appear; request with `?status=foo` and verify `400 InvalidStatusFilter`.

- [X] T044 [US6] Add Postgres global delta read in `crates/server/src/storage/postgres.rs`: `list_global_deltas(limit, cursor, status_filter: Option<Vec<DeltaStatusKind>>) -> Result<(Vec<DeltaSummary>, Option<i64>), String>` ordered by `id DESC` with `WHERE id < $cursor_id` and optional `WHERE status->>'status' = ANY($statuses)`
- [X] T045 [US6] Add filesystem equivalent honoring the FR-029 threshold short-circuit via the T009 helper. Above threshold, return a `503 DataUnavailable` with reason `FilesystemThresholdExceeded`
- [X] T046 [US6] Implement `crates/server/src/services/dashboard_global_deltas.rs::list_global_deltas(state, limit, cursor, status_filter) -> Result<PagedResult<DashboardGlobalDeltaEntry>, GuardianError>`. Parse comma-separated `?status=` into `Vec<DeltaStatusKind>`; reject unknown values with `InvalidStatusFilter`. Map storage rows to `DashboardGlobalDeltaEntry` (= `DashboardDeltaEntry` + `account_id`). Filter out `pending`-status entries (live in `delta_proposals`)
- [X] T047 [US6] Add HTTP route `GET /dashboard/deltas?limit=&cursor=&status=` in `crates/server/src/api/dashboard_history.rs`
- [X] T048 [P] [US6] Add `listGlobalDeltas({ limit?, cursor?, status? })` to `packages/guardian-operator-client/src/http.ts`; the `status` parameter accepts `string[]` and is serialized to comma-separated; finalize `DashboardGlobalDeltaEntry` type in `server-types.ts`
- [X] T049 [US6] Add integration test `crates/server/tests/dashboard_global_feeds.rs` (US6 part) covering acceptance scenarios 1–4 + filesystem-threshold-degradation case
- [X] T050 [US6] Document `/dashboard/deltas` in `spec/api.md` including the `status` filter semantics and the FR-029 degradation behavior

## Phase 9: User Story 7 — Global In-Flight Proposal Feed (Priority: P3, smallest)

**Story goal**: Operators get a cross-account in-flight proposal feed.
**Independent test**: Seed in-flight proposals across at least 3 multisig accounts; request `/dashboard/proposals?limit=10`, verify newest-first by `delta_proposal.id DESC` and every entry carries `account_id` plus all the per-account proposal fields; verify a Guardian with no in-flight proposals → `200` empty page (not `404`); verify EVM accounts do not appear.

- [X] T051 [US7] Add Postgres global proposal read in `crates/server/src/storage/postgres.rs`: `list_global_proposals(limit, cursor) -> Result<(Vec<ProposalSummary>, Option<i64>), String>` ordered by `id DESC` filtered to `status->>'status' = 'pending'`
- [X] T052 [US7] Add filesystem equivalent honoring the FR-029 threshold; above threshold return `503 DataUnavailable`
- [X] T053 [US7] Implement `crates/server/src/services/dashboard_global_proposals.rs::list_global_proposals(state, limit, cursor) -> Result<PagedResult<DashboardGlobalProposalEntry>, GuardianError>`. Reject any `?status=` query param if supplied (the global proposal feed has no status filter per FR-035); compute `signatures_required` from each row's `account_id` auth policy
- [X] T054 [US7] Add HTTP route `GET /dashboard/proposals?limit=&cursor=` in `crates/server/src/api/dashboard_history.rs`
- [X] T055 [P] [US7] Add `listGlobalProposals({ limit?, cursor? })` to `packages/guardian-operator-client/src/http.ts`; finalize `DashboardGlobalProposalEntry` type
- [X] T056 [US7] Extend `crates/server/tests/dashboard_global_feeds.rs` (US7 part) with US7 acceptance scenarios 1–4 + filesystem-threshold-degradation case + verify EVM accounts excluded
- [X] T057 [US7] Document `/dashboard/proposals` in `spec/api.md`

## Phase 10: Polish and Cross-Cutting

- [X] T058 [P] Update `spec/components.md` to add the new dashboard modules (`cursor.rs`, `dashboard_pagination.rs`, six service modules, `dashboard_history.rs`) and reflect the breaking change on the account list contract
- [X] T059 [P] Add a "Pagination shape" section to `packages/guardian-operator-client/README.md` documenting the `PagedResult<T>` envelope, the `limit` / `cursor` semantics, and the typed `DashboardErrorCode` union
- [X] T060 [P] Run the full validation matrix from `plan.md` and capture results: `cargo fmt --all -- --check`, `cargo clippy --workspace -- -D warnings`, `cargo test -p guardian-server`, `cargo test -p guardian-server --features postgres`, `cargo test -p guardian-server --features integration -- dashboard_`, `cd packages/guardian-operator-client && npm run lint && npm test && npm run build`. Fix any failures; re-run until all green
- [ ] T061 Run the operator-dashboard smoke test via the `smoke-test-operator-dashboard` skill against the new endpoints (login → list → info → per-account deltas → per-account proposals → global feeds → error matrix); record results in the PR description

## Phase A: DB-Backed Pagination Backbone (LANDED RETROACTIVELY)

`research.md` Decision 1's "no migration" framing was reversed mid-implementation in favor of pushing pagination, sort, and filter into SQL. This phase captures the work that landed; tasks T026/T034/T044/T051 above were then rewired to delegate to the new trait methods.

- [X] A1 Add migration `crates/server/migrations/2026-05-10-000001_promote_delta_status/{up,down}.sql`: add `status_kind text` and `status_timestamp timestamptz` to both `deltas` and `delta_proposals`, backfill from `status->>'status'` and `(status->>'timestamp')::timestamptz`, `SET NOT NULL`, add `CHECK (status_kind IN ('pending','candidate','canonical','discarded'))`, create composite indexes `(status_kind, status_timestamp DESC, account_id, id)` on each table; `down.sql` reverses with `IF EXISTS` clauses
- [X] A2 Update Diesel `crates/server/src/schema.rs` with the two new columns on both `deltas` and `delta_proposals` table macros; update `DeltaRow`, `ProposalRow`, `NewDelta`, `NewProposal` in `crates/server/src/storage/postgres.rs` to carry the typed fields
- [X] A3 Postgres dual-write in `crates/server/src/storage/postgres.rs`: introduce `derive_status_columns(&DeltaStatus) -> (&'static str, DateTime<Utc>)` and call it from `submit_delta`, `submit_delta_proposal`, `update_delta_proposal`, `update_delta_status` so the typed columns cannot drift from the `status` Jsonb blob
- [X] A4 Add new `StorageBackend` trait methods in `crates/server/src/storage/mod.rs` plus the supporting types: `DeltaStatusKind`, `AccountHistoryCursor`, `GlobalDeltaCursor`, `GlobalProposalCursor`, `DeltaStatusCounts`, `GlobalDeltaRow`, `GlobalProposalRow`. New methods: `list_account_deltas_paged`, `list_account_proposals_paged`, `list_global_deltas_paged(status_filter)`, `list_global_proposals_paged`, `count_deltas_by_status`, `count_in_flight_proposals`, `latest_activity_timestamp`
- [X] A5 Implement the new trait methods on Postgres with full SQL pushdown: per-account paths sort by the immutable PK; global feeds use the composite-cursor predicate `(status_timestamp, account_id, id) < ($cursor_ts, $cursor_account, $cursor_id)` against the new composite indexes
- [X] A6 Implement the new trait methods on filesystem in `crates/server/src/storage/filesystem.rs` via fan-out + in-memory sort/slice; cross-account aggregates honor `filesystem_aggregate_threshold` per FR-029
- [X] A7 Implement the new trait methods on `MockStorageBackend` in `crates/server/src/testing/mocks.rs` with LIFO queue-based responses; add `with_*` helpers for each
- [X] A8 Rewire service-layer modules to call the new trait methods as thin pass-throughs: `dashboard_account_deltas.rs`, `dashboard_account_proposals.rs`, `dashboard_global_deltas.rs`, `dashboard_global_proposals.rs`, `dashboard_info.rs`. Drop the in-memory sort/filter helpers (`compare_entries`, `max_timestamp`) that the original service-layer fan-out required
- [X] A9 Prune service-layer unit tests that duplicated storage-layer logic; integration tests in `crates/server/tests/dashboard_*.rs` cover end-to-end behavior. Service-layer tests now focus on cursor-kind validation, EVM short-circuit, 404/503 mapping, and wire-shape correctness
- [X] A10 Update artifacts to reflect the reversal: `research.md` Decision 1 + Decision 7, `data-model.md` Persistence Changes + cursor sort table, `plan.md` Summary + Workstreams + Phasing + Validation, `spec.md` Data/Lifecycle Impact + Assumptions, `quickstart.md` Prerequisites, `contracts/dashboard.openapi.yaml` `latest_activity` + `status_timestamp` field descriptions, this `tasks.md` (Phase A block + risk callout)

---

## Dependencies

```
Phase 1 (Setup) ──┐
                  ├─→ Phase 2 (Foundational) ──┬─→ Phase 3 (US1) ──┐
                  │                            │                    │
                  │                            ├─→ Phase 4 (US2) ──┤
                  │                            │                    │
                  │                            ├─→ Phase 5 (US3) ──┤
                  │                            │                    │
                  │                            ├─→ Phase 6 (US4) ──┤
                  │                            │                    │
                  │                            ├─→ Phase 7 (US5) ──┤
                  │                            │                    │
                  │                            ├─→ Phase 8 (US6) ──┤
                  │                            │                    │
                  │                            └─→ Phase 9 (US7) ──┤
                  │                                                 │
                  └─────────────────────────────────────────────────┴─→ Phase 10 (Polish)
```

- **Phase 1 → Phase 2**: scaffolding must exist before foundational logic lands.
- **Phase 2 blocks all user stories**: cursor codec, pagination helpers, error taxonomy, and filesystem aggregate helper are required by every paginated endpoint and every typed error.
- **Phases 3–9 are independent of each other after Phase 2.** Each user story touches its own service module, its own integration test file (except US3+US4 which share `dashboard_account_history.rs`), and its own TS wrapper. Three of the seven user stories can land in parallel without touching the same files.
- **Phase 10 runs after Phases 3–9** but T058/T059 (docs) and T060 (validation matrix) can each be partial-run as user stories land if the team wants continuous green.

## Parallel Execution Opportunities

Within each phase, tasks marked `[P]` touch different files and have no dependencies on incomplete tasks in the same phase:

| Phase | Parallel batch |
|-------|----------------|
| 1 | T002 + T003 (independent module scaffolds in different crates/packages) |
| 2 | T008 + T009 + T010 + T011 (after T004–T007 land) |
| 3 (US1) | T016 + T018 (TS wrapper + TS test) parallel with T017 (Rust integration test) parallel with T019 (docs) |
| 4 (US2) | T023 (TS) parallel with T024 (Rust test) parallel with T025 (docs) |
| 5 (US3) | T030 + T032 + T033 (TS wrapper, TS test, docs) |
| 6 (US4) | T038 + T040 + T041 |
| 7 (US5) | T043 (TS) parallel with T042 (Rust) |
| 8 (US6) | T048 + T050 |
| 9 (US7) | T055 + T057 |
| 10 | T058 + T059 + T060 |

## Implementation Strategy

**MVP scope (smallest end-to-end slice that proves the architecture)**: Phases 1, 2, and 3 (US1).

Phase 3 is US1, the highest-priority story (paginated account list, P1) and exercises every foundational primitive — cursor codec, pagination helpers, error taxonomy, the breaking-change rollout to the TS consumer. Once US1 ships, every later story follows the same pattern with reduced risk.

**Suggested delivery order** (matches `plan.md` phasing, deviates from strict P1→P3 ordering for risk reasons):

1. **Phases 1+2** (foundational, blocking).
2. **Phases 5+6** (US3, US4 — per-account history). Smaller surface than the breaking change on US1; lets the team validate the per-account paginated read patterns end-to-end before committing the breaking change.
3. **Phases 3+4** (US1, US2 — account list breaking change + info endpoint). Land the breaking change with the dashboard UI consumer update in the same release.
4. **Phase 7** (US5 — explicit error outcomes). May land in parallel with phases 5/6 since it is structural; recorded as its own phase for tracking.
5. **Phases 8+9** (US6, US7 — global feeds, smallest priority). Defer if ship pressure forces it; the dashboard remains functional without these per FR-039.
6. **Phase 10** (polish + validation).

**Risk callouts**:

- **Breaking change in Phase 3 (US1)**: removes the unparameterized account list and `total_count` field. Internal dashboard UI consumer must be updated atomically per Constitution §I; coordinate the merge.
- **Phase A schema migration**: `research.md` Decision 1 was reversed during implementation; the migration `2026-05-10-000001_promote_delta_status` promotes typed `status_kind`/`status_timestamp` columns on both `deltas` and `delta_proposals` and is a precondition for the new code. Existing deployments must run `up.sql`; backfill is mandatory because both tables already have populated rows. Postgres dual-write is enforced via a `derive_status_columns(&DeltaStatus)` helper so the typed columns cannot drift from the Jsonb blob.
- **Filesystem-threshold degradation**: The 1,000-account default in T001 may need tuning per deployment; confirm with ops before merging T060.

## Format Validation

All 61 tasks follow the strict checklist format `- [ ] [TaskID] [P?] [Story?] Description with file path`:

- Setup phase (T001–T003): no story label.
- Foundational phase (T004–T011): no story label.
- User story phases (T012–T057): every task carries its `[USn]` label.
- Polish phase (T058–T061): no story label.
- `[P]` markers applied only where the task touches files independent of incomplete tasks in the same phase.
- Every task includes an exact file path or directory reference for the LLM/contributor to act on.
