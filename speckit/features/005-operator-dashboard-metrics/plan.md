# Implementation Plan: Operator Dashboard Metrics — Pagination, Info, and Activity

**Feature Key**: `005-operator-dashboard-metrics` | **Date**: 2026-05-08 | **Spec**: [spec.md](./spec.md)

## Summary

Add cursor-paginated read APIs to the operator dashboard surface so a
deployed Guardian can back a real ops UI: paginated account list (breaking
change vs. `003-operator-account-apis`), aggregate inventory/health
endpoint, per-account delta history, per-account in-flight proposal
queue, and (lowest priority) two cross-account global feeds. All
endpoints sit behind the existing `002-operator-auth` session, are
read-only, and derive responses from existing `account_metadata`,
`states`, `deltas`, and `delta_proposals` storage.

**v1 is Miden-oriented.** A Guardian instance is gated to one network
family in practice (Miden default; EVM behind a server feature flag),
and the proposal queue endpoints surface only the Miden
`delta_proposals` flow. EVM accounts return empty results on those
endpoints and are not enumerated separately on the info response. A
follow-up feature owns EVM-specific dashboard surfaces.

**Phase A schema migration promotes typed `status_kind` and
`status_timestamp` columns** on both `deltas` and `delta_proposals`,
backfilled from the existing `status` `Jsonb` blob. The Postgres
backend pushes pagination, status filtering, and sort entirely into
SQL via composite indexes on `(status_kind, status_timestamp DESC,
account_id, id)`. Per-account history endpoints sort by the immutable
primary key (`delta.id DESC` / `delta_proposal.id DESC`) for fully
stable per-account cursors; the global delta and proposal feeds sort
by `(status_timestamp DESC, account_id ASC, id ASC)`. The filesystem
backend keeps the fan-out implementation, bounded by a configurable
inventory threshold (default 1,000 accounts) per FR-029. See
`research.md` Decision 1 for the rationale.

## Technical Context

- **Language / runtime**: Rust 2024 edition (server + clients), TypeScript
  (operator client + dashboard).
- **Server**: `crates/server` with axum HTTP + Diesel-backed Postgres, plus
  the filesystem backend in `src/storage/filesystem.rs`.
- **Auth**: existing operator session middleware in
  `crates/server/src/dashboard/middleware.rs` (cookie-backed, established by
  `002-operator-auth`).
- **TypeScript consumer**: `packages/guardian-operator-client` —
  `004-operator-http-client` already exports the typed wrappers for the
  current account list/detail; this feature extends it with paginated and
  new typed wrappers.
- **Storage**: Postgres tables `account_metadata`, `states`, `deltas`,
  `delta_proposals` per `schema.rs`. Phase A migration adds typed
  `status_kind text NOT NULL` and `status_timestamp timestamptz NOT
  NULL` columns to both `deltas` and `delta_proposals`, backfilled
  from the existing `status` `Jsonb` blob. Composite indexes on
  `(status_kind, status_timestamp DESC, account_id, id)` support the
  global feed sort. The `status` `Jsonb` blob is preserved (writers
  dual-write); the typed columns are derived state.
- **Dashboard surface scope**: HTTP only for v1 (FR-027). gRPC is an
  intentional documented divergence per Constitution §II — recorded in
  Decision 2 of `research.md`.
- **NEEDS CLARIFICATION**: none. All open questions resolved during the two
  spec review passes (see `checklists/requirements.md`).

## Constitution Check

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Bottom-up change propagation | OK | Server contract drives changes to TS `guardian-operator-client` typed wrappers (existing `list_operator_accounts`, plus six new). The internal dashboard UI consumer is in-scope for the breaking-change update on the list endpoint per FR-001. |
| II. Transport and cross-language parity | Documented divergence | Dashboard endpoints are HTTP-only for v1 per spec FR-027. No gRPC parity is implemented. Recorded in `research.md` Decision 2 as an explicit, deliberate divergence. The Rust base client (`guardian-client`) is not extended — the dashboard surface is intentionally TS-only consumer-side. |
| III. Append-only integrity and explicit lifecycles | OK | All endpoints are read-only (FR-026). Lifecycle statuses (`candidate`/`canonical`/`discarded`) are surfaced verbatim from storage; no implicit transitions or fallbacks introduced. The two state machines (delta vs. proposal) are kept on separate endpoints so their lifecycles do not blur. |
| IV. Explicit auth and stable boundary errors | OK | Operator session is the single auth path (FR-025). Error taxonomy pinned in FR-028: `401`/`404`/`400 InvalidCursor`/`400 InvalidLimit`/`400 InvalidStatusFilter`/`503 DataUnavailable`, each with a stable machine-readable code in the body. SC-006 + SC-012 enforce. |
| V. Evidence-driven delivery | OK | Seven independently testable user stories with acceptance scenarios; integration test matrix in Validation; `quickstart.md` walks the happy path; spec/api.md updated with the new contract. |

No unresolved violations. The HTTP-only scope is the only divergence and is
documented per Constitution §II.

## Workstreams

### Server — Schema migration (Phase A)

- **Migration**
  `crates/server/migrations/2026-05-10-000001_promote_delta_status/`
  adds typed columns and indexes:
  - `ALTER TABLE deltas ADD COLUMN status_kind text` /
    `status_timestamp timestamptz`, then backfill via `UPDATE deltas
    SET status_kind = status->>'status', status_timestamp =
    (status->>'timestamp')::timestamptz`, then `SET NOT NULL` on both
    columns.
  - Same migration steps for `delta_proposals`.
  - `CHECK (status_kind IN ('pending','candidate','canonical','discarded'))`
    on each table.
  - Composite indexes:
    `(status_kind, status_timestamp DESC, account_id, id)` on each
    table for the global feed sort; `(account_id, id DESC)` is left
    in place for per-account history.
  - `down.sql` reverses with `IF EXISTS` clauses; safe to rerun.
- **Diesel `schema.rs`** updated to reflect the two new columns on
  both tables; `Row` structs and `New*` insert structs gain the typed
  fields.

### Server — Storage helpers

- **Postgres dual-write**: every `INSERT`/`UPDATE` path that writes
  `status` (`submit_delta`, `submit_delta_proposal`,
  `update_delta_proposal`, `update_delta_status`) also writes
  `status_kind` and `status_timestamp` via a
  `derive_status_columns(&DeltaStatus) -> (&'static str,
  DateTime<Utc>)` helper, so the typed columns can never drift from
  the `status` Jsonb blob.
- **Postgres reads**: per-account history hits the
  `(account_id, id DESC)` index path (`SELECT ... WHERE account_id =
  $1 AND id < $cursor_id ORDER BY id DESC LIMIT $limit`). Global feeds
  hit the `(status_kind, status_timestamp DESC, account_id, id)`
  composite index with composite-cursor predicates of the form `WHERE
  status_kind = ANY($1) AND (status_timestamp, account_id, id) <
  ($cursor_ts, $cursor_account, $cursor_id) ORDER BY status_timestamp
  DESC, account_id ASC, id ASC LIMIT $limit`. Info per-status counts
  use `GROUP BY status_kind`; `latest_activity` is `GREATEST(MAX(d.status_timestamp),
  MAX(p.status_timestamp))` against the typed columns.
- **Filesystem backend** (`src/storage/filesystem.rs`): add helpers
  `list_deltas_for_account(account_id, limit, cursor)` and
  `list_proposals_for_account(account_id, limit, cursor)`. Cross-account
  aggregates use a single fan-out walk and short-circuit to a degraded
  marker (`AggregateUnavailableReason::FilesystemThresholdExceeded`) once
  the configured threshold (default 1,000 accounts; server config field
  `filesystem_aggregate_threshold`) is exceeded.
- **Cursor codec** (`src/dashboard/cursor.rs`, new module): opaque
  base64url-encoded HMAC-signed cursor with payload `{kind, last_id}`
  for the four immutable-sort kinds, and `{kind, last_updated_at,
  last_account_id}` for the account list. Signing secret comes from
  the existing dashboard config block; rotation is handled by
  accepting the previous secret for one release window (operational
  concern documented in `research.md` Decision 4).

### Server — Services

- New service modules under `crates/server/src/services/`:
  - `dashboard_accounts.rs` — extend with `list_dashboard_accounts_paged(state,
    limit, cursor) -> PagedResult<DashboardAccountSummary>`. The existing
    unparameterized `list_dashboard_accounts` is removed (breaking change
    per FR-001/FR-007).
  - `dashboard_info.rs` (new) — `get_dashboard_info(state) -> DashboardInfo`
    with environment, total account count, `latest_activity`
    (`GREATEST(MAX(d.status_timestamp), MAX(p.status_timestamp))` over
    the typed columns), and lifecycle counts via `GROUP BY
    status_kind`. Honors FR-029 degradation on filesystem. Does not
    surface per-network counts or a singular network field per FR-009.
  - `dashboard_account_deltas.rs` (new) — `list_account_deltas(state, id,
    limit, cursor) -> PagedResult<DashboardDeltaEntry>` with
    `candidate`/`canonical`/`discarded` only. Each entry carries
    `nonce`, `status`, `status_timestamp` (typed column),
    `prev_commitment`, `new_commitment` (nullable), and `retry_count`
    on candidate entries (always populated, default 0).
  - `dashboard_account_proposals.rs` (new) — `list_account_proposals(state,
    id, limit, cursor) -> PagedResult<DashboardProposalEntry>` with
    `commitment`, `nonce`, `proposer_id`, `originating_timestamp`,
    `signatures_collected`, `signatures_required`, `prev_commitment`,
    `new_commitment` (nullable). EVM accounts (`Auth::EvmEcdsa`)
    early-return empty per FR-017.
  - `dashboard_global_deltas.rs` (new) — `list_global_deltas(state, limit,
    cursor, status_filter)` with comma-separated status filter validation
    returning `400 InvalidStatusFilter` on unknown values.
  - `dashboard_global_proposals.rs` (new) — `list_global_proposals(state,
    limit, cursor)` with no status filter (all entries are in-flight by
    definition per FR-035).
- Common helper `dashboard_pagination.rs` (new) — `parse_limit(opt) ->
  Result<u32, ApiError>` enforcing `[1, 500]`, default 50; `parse_cursor`
  using the cursor codec; `PagedResult<T>` envelope.

### Server — HTTP Surface

- **Routes** registered in `src/api/mod.rs` under the operator-session
  middleware:
  - `GET /dashboard/accounts?limit=&cursor=` — replaces the
    `003-operator-account-apis` unparameterized variant.
  - `GET /dashboard/info`
  - `GET /dashboard/accounts/{account_id}/deltas?limit=&cursor=`
  - `GET /dashboard/accounts/{account_id}/proposals?limit=&cursor=`
  - `GET /dashboard/deltas?limit=&cursor=&status=` (smallest priority)
  - `GET /dashboard/proposals?limit=&cursor=` (smallest priority)
- Handlers in `src/api/dashboard.rs` plus a new
  `src/api/dashboard_feeds.rs` for the per-account and global history
  routes.
- Error mapping (`src/error.rs`): add typed variants `InvalidCursor`,
  `InvalidLimit`, `InvalidStatusFilter`, `DataUnavailable` and ensure
  each serializes a stable `code` field per FR-028.

### TypeScript — `guardian-operator-client`

- Extend `packages/guardian-operator-client/src/http.ts` with typed
  wrappers:
  - `listOperatorAccountsPaged({ limit?, cursor? })`
  - `getDashboardInfo()`
  - `listAccountDeltas(accountId, { limit?, cursor? })`
  - `listAccountProposals(accountId, { limit?, cursor? })`
  - `listGlobalDeltas({ limit?, cursor?, status? })`
  - `listGlobalProposals({ limit?, cursor? })`
- The existing `listOperatorAccounts` (unparameterized) is **removed** as
  part of the breaking-change update; the dashboard UI consumer is
  updated in the same release.
- Add response types under `src/server-types.ts`:
  `DashboardInfoResponse`, `DashboardDeltaEntry`,
  `DashboardProposalEntry`, `PagedResult<T>` envelope, plus the
  `DashboardErrorCode` union (`InvalidCursor` | `InvalidLimit` |
  `InvalidStatusFilter` | `DataUnavailable`).
- Update `http.test.ts` with a happy-path matrix per endpoint and an
  error-code matrix per FR-028.

### Tests

- `crates/server/tests/dashboard_paged_accounts.rs` (new) — integration
  tests covering US1 scenarios 1–8, including bare `?limit=`,
  out-of-range `limit`, and invalid cursor.
- `crates/server/tests/dashboard_info.rs` (new) — US2; happy-path
  inventory totals, empty inventory, partial-source-degraded marker.
- `crates/server/tests/dashboard_account_history.rs` (new) — US3 + US4;
  EVM account empty proposal queue; single-key Miden empty proposal
  queue; `retry_count` defaulting; 404/200/503 matrix per FR-022.
- `crates/server/tests/dashboard_global_feeds.rs` (new) — US6 + US7;
  multi-status filter, unknown status rejection, `account_id` tagging,
  filesystem-degradation behavior at threshold.
- `crates/server/tests/dashboard_errors.rs` (new) — US5; pinned status
  codes and machine-readable code body per FR-028 + SC-012.
- TS: `packages/guardian-operator-client/src/http.test.ts` extended with
  the same matrix at the wrapper level.

### Docs

- `spec/api.md`: update the dashboard section to describe the breaking
  change on `GET /dashboard/accounts`, document the five new routes,
  and pin the error taxonomy.
- `spec/components.md`: update the dashboard surface diagram to include
  the new modules.
- `packages/guardian-operator-client/README.md`: extend "Auth shape"
  with a "Pagination shape" section showing the cursor envelope.

## Phasing

1. **Phase A schema migration + cursor codec + pagination helpers +
   filesystem helpers** (blocking, server). New migration
   `2026-05-10-000001_promote_delta_status` (typed `status_kind`/`status_timestamp`
   columns + composite indexes + backfill); Diesel `schema.rs`
   updated; Postgres dual-write paths landed. New
   `src/dashboard/cursor.rs` and `src/services/dashboard_pagination.rs`;
   filesystem fan-out helpers + threshold short-circuit. Validation:
   migration up/down dry-run, codec roundtrip + tamper unit tests,
   filesystem helper unit tests.
2. **Per-account endpoints** (US3, US4 — P2). Deliver
   `/dashboard/accounts/{id}/deltas` and
   `/dashboard/accounts/{id}/proposals` with TS wrappers and tests.
3. **Account list breaking change + info endpoint** (US1, US2 — P1).
   Deliver paginated `/dashboard/accounts`, remove unparameterized
   mode and `total_count`; deliver `/dashboard/info`. Update internal
   dashboard UI consumer in the same change set per Constitution §I.
4. **Explicit error outcomes** (US5 — P3, cross-cutting). Land the
   typed error taxonomy with body codes and the SC-012 test matrix.
   (May be landed in parallel with phases 2/3 since it is structural.)
5. **Global feeds** (US6, US7 — P3, smallest). Deliver the two
   cross-account endpoints with status-filter validation and
   filesystem degradation handling. Defer if ship pressure forces it;
   the dashboard remains functional without them per FR-039.

## Validation

```bash
# Rust
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test -p guardian-server
cargo test -p guardian-server --features postgres
cargo test -p guardian-server --features integration -- dashboard_

# Run the Phase A migration before exercising the postgres-feature
# tests. Existing deployments must run this before the new code
# starts; backfill is mandatory.
# DATABASE_URL=$GUARDIAN_TEST_DB \
#   cargo run -p guardian-server --features postgres --bin migrate

# TypeScript
cd packages/guardian-operator-client && npm run lint && npm test && npm run build

# End-to-end smoke (operator dashboard)
# Run via the existing smoke-test-operator-dashboard skill once the new
# wrappers land in guardian-operator-client.
```

Required validation matrix (all green before merge):

| Layer | Coverage | File |
|-------|----------|------|
| Server unit | Cursor codec roundtrip, tamper detection | `crates/server/src/dashboard/cursor.rs` |
| Server integration | All 7 user stories | `crates/server/tests/dashboard_*.rs` |
| Server feature parity | Filesystem + Postgres | Same suite, both `--features` matrices |
| TS wrapper | Pagination + error matrix | `packages/guardian-operator-client/src/http.test.ts` |
| Docs | spec/api.md + spec/components.md | Manual review |

## Deferred

- Per-cosigner identity list on proposal entries (FR-020).
- `transaction_type` / category descriptor on delta entries (Out of Scope).
- Asset / balance / token-amount / TVL surfaces (FR-024); separate
  follow-up feature pending state-schema convention or account-inspector
  extension.
- gRPC parity for the dashboard endpoints (Constitution §II divergence
  documented in `research.md` Decision 2).
- Cursor TTL / explicit expiry (Out of Scope).
- A unified "not yet canonical" cross-endpoint view; v1 composes
  `/deltas?status=candidate` with the proposal queue on the dashboard
  side per Assumptions.
- Rate limiting and read-side audit logs on the new endpoints (FR-030 —
  intentional v1 no-op).
