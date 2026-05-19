# Implementation Plan: Operator-Initiated Per-Account Pause

**Feature Key**: `001-account-pausing` | **Date**: 2026-05-19 | **Spec**: [spec.md](./spec.md)

## Summary

Ship an operator-initiated per-account pause kill switch on top of the
`006-operator-authz` foundation that already shipped. Two new HTTP
endpoints (`POST /dashboard/accounts/{account_id}/pause` and
`/unpause`) gated by the existing `Permission::AccountsPause`
permission flip a pair of new columns on `account_metadata`
(`paused_at TIMESTAMPTZ NULL`, `paused_reason TEXT NULL`). A new
single-purpose chokepoint helper `ensure_account_active(account_id)`
is invoked from every per-account mutating proposal/delta/signature
pipeline entry point before any state mutation — the three multisig
services (`services::push_delta`, `services::push_delta_proposal`,
`services::sign_delta_proposal`) and, when the EVM Cargo feature is
enabled, the three EVM services (`evm::service::create_proposal`,
`evm::service::approve_proposal`, `evm::service::cancel_proposal`).
Admin/setup paths (`configure_account`, `register_account`) are
intentionally not gated — see spec Non-Goals. When the
helper rejects, the call surfaces a new `GuardianError::AccountPaused`
variant whose stable code is `GUARDIAN_ACCOUNT_PAUSED`, mapping to
HTTP **409 Conflict** and gRPC **`FAILED_PRECONDITION`**, with the
`paused_at` and `paused_reason` carried in the response body and gRPC
status `details`. Every transition (pause, unpause, idempotent retry)
writes a row to the existing `admin_actions` audit table via the
`Auditor` trait. The operator client (`@openzeppelin/guardian-operator-client`)
gains typed `pauseAccount`/`unpauseAccount` methods and surfaces the
new error code so dashboard code can branch without string-matching.

The implementation is deliberately a **self-contained flag with a
single chokepoint**, not a `Policy` impl. The chokepoint module is
the only call site that interrogates pause state — mutating handlers
must not read `paused_at` directly. When `#182` (`PolicyEngine`)
lands, `ensure_account_active` is replaced wholesale by
`policy_engine.evaluate_all(...)` with no API, audit, or storage
change. That guarantee is encoded in FR-012/FR-025/FR-026 of the spec
and validated by SC-007.

## Technical Context

- **Language / runtime**: Rust 2024 edition (server); TypeScript
  (`@openzeppelin/guardian-operator-client`).
- **Server**: `crates/server` — axum HTTP + tonic gRPC, Diesel-backed
  Postgres, plus the filesystem backend in
  `src/storage/filesystem.rs` and `src/metadata/filesystem.rs`.
- **Auth**: existing two-layer dashboard middleware:
  `require_dashboard_session` (outer) then `authz::enforce` (inner
  via `route_layer`). Established by `002-operator-auth` and
  `006-operator-authz`. The pause routes register under the same
  `authz::enforce` pattern, declaring `&[Permission::AccountsPause]`.
- **Permission gate**: `Permission::AccountsPause` (`"accounts:pause"`)
  in `crates/server/src/dashboard/permissions.rs:25` — already wired
  into the allowlist, the TypeScript `KNOWN_OPERATOR_PERMISSIONS`
  validation, and the operator client constants.
- **Storage**: extend `account_metadata` table (initial schema
  `crates/server/migrations/2026-03-12-000002_account_metadata`) via a
  new migration `2026-05-19-000001_account_pause_fields` that adds
  `paused_at TIMESTAMPTZ NULL` and `paused_reason TEXT NULL`. No new
  table.
- **Persistence trait**: `MetadataStore` in
  `crates/server/src/metadata/mod.rs:40` already abstracts the
  Postgres/filesystem split — both backends gain the two new fields
  on the `AccountMetadata` struct plus two new helpers
  (`set_pause` / `clear_pause`) to flip the columns in one round
  trip rather than fetching + setting the whole row.
- **Error model**: extend `GuardianError` enum
  (`crates/server/src/error.rs:9-95`) with a new
  `AccountPaused { paused_at, paused_reason }` variant. HTTP status
  `409 Conflict`, gRPC status `FAILED_PRECONDITION`, response body
  carries `code = "GUARDIAN_ACCOUNT_PAUSED"` plus a `details` block
  with the two pause fields. The existing additive-envelope shape
  (`code`, `message`, optional fields like `missing_permissions`,
  `retryable`) is extended with a new optional `paused_at` /
  `paused_reason` pair, matching the precedent set by
  `InsufficientOperatorPermission`.
- **Audit**: existing `Auditor` trait in
  `crates/server/src/audit/mod.rs:80`. Two new `action_kind` consts
  registered in `audit/kinds.rs`: `ACCOUNTS_PAUSE = "accounts.pause"`
  and `ACCOUNTS_UNPAUSE = "accounts.unpause"`. `target_account_id` is
  set; `payload` carries `{ before_state, after_state, reason }`.
  Idempotent retries audit with `before_state == after_state`.
- **TypeScript consumer**:
  `packages/guardian-operator-client/src/http.ts` adds two methods +
  the new error code constant; `src/server-types.ts` adds the two
  optional nullable fields on `OperatorAccountDetail` and a
  `GUARDIAN_ACCOUNT_PAUSED` member to the typed error union.
- **Chokepoint placement**: `crates/server/src/services/account_status.rs`
  (new module) exposing one async fn `ensure_account_active(state:
  &AppState, account_id: &str) -> Result<(), GuardianError>`. Called
  as the first non-validation step in the multisig mutating
  pipeline (`services::push_delta`,
  `services::push_delta_proposal`,
  `services::sign_delta_proposal`) and — under the EVM Cargo
  feature — in the EVM mutating pipeline
  (`evm::service::create_proposal`,
  `evm::service::approve_proposal`,
  `evm::service::cancel_proposal`). The helper is the **only** call
  site that reads `paused_at` / `paused_reason` outside of read
  endpoints (FR-025).
- **NEEDS CLARIFICATION**: none. All spec-phase clarifications
  resolved (see spec.md "Resolved clarifications").

## Constitution Check

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Bottom-up change propagation | OK | Server contract (new error, new fields on account detail, two new endpoints) drives changes in `@openzeppelin/guardian-operator-client` typed wrappers + error union in the same release. The Rust `guardian-client` is **not** extended — the pause/unpause control surface is operator-only by design (Non-Goals). |
| II. Transport and cross-language parity | Documented divergence | The pause/unpause **control endpoints** are HTTP-only (operator dashboard surface is HTTP per `005-operator-dashboard-metrics` Decision 2). The pause **enforcement** at the chokepoint applies on both gRPC and HTTP mutating callers identically — same `GuardianError::AccountPaused` variant, same code string, same `details` shape. gRPC surfaces 409 as `FAILED_PRECONDITION` via the existing `GuardianError → tonic::Code` mapping. Divergence is **only** in where pause is *issued* (HTTP), not in where it is *enforced*. Recorded in `research.md` Decision 6. |
| III. Append-only integrity and explicit lifecycles | OK | Pause is a per-account operational gate, not a state/delta/proposal lifecycle change. Already-canonical deltas are not rewritten; in-flight proofs at the sequencer are not rolled back (FR-016). Audit rows in `admin_actions` are append-only by Postgres trigger (already established by 006). Pause state itself has an explicit two-value lifecycle (`active` ↔ `paused`) with explicit operator transitions — no implicit fallback. |
| IV. Explicit authentication and stable boundary errors | OK | Both endpoints require operator session (existing middleware) + `Permission::AccountsPause` (existing authz middleware, returns `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` on miss). New `GUARDIAN_ACCOUNT_PAUSED` is a stable code added to the existing `GuardianError` enum with pinned HTTP/gRPC mapping and a fixed `details` shape. Idempotent re-pause does **not** overwrite `paused_at` — see Decision 2; this protects the forensic timestamp. The pause `reason` field is required on pause (FR-007). |
| V. Evidence-driven delivery | OK | Three independently testable user stories (US1 pause, US2 unpause, US3 read), 7 success criteria, integration tests across gRPC + HTTP + filesystem + Postgres, validation matrix in §Validation. Spec.md §Edge Cases enumerates the race, restart, audit-failure, and authz-bypass paths. SC-007 (PolicyEngine swap is a localized change) is verified at #182 merge time. |

No unresolved violations. The single divergence (control endpoints
HTTP-only) follows the precedent set by `005-operator-dashboard-metrics`
and is recorded in `research.md` Decision 6.

## Project Structure

### Documentation (this feature)

```text
speckit/features/001-account-pausing/
├── plan.md                 # This file
├── research.md             # Phase 0 — six decisions
├── data-model.md           # Phase 1 — schema, types, error shape
├── quickstart.md           # Phase 1 — happy path walkthrough
├── contracts/
│   └── pause.openapi.yaml  # Phase 1 — pause/unpause + modified account-detail
├── checklists/
│   └── requirements.md     # Spec-phase quality checklist (already complete)
└── tasks.md                # Phase 2 output (/speckit-tasks — not created here)
```

### Source code (repository root)

```text
crates/server/
├── migrations/
│   └── 2026-05-19-000001_account_pause_fields/   # NEW
│       ├── up.sql                                # ADD COLUMN paused_at, paused_reason
│       └── down.sql                              # DROP both columns
├── src/
│   ├── audit/
│   │   └── kinds.rs                              # +ACCOUNTS_PAUSE, +ACCOUNTS_UNPAUSE consts; extend ALL_KINDS
│   ├── api/
│   │   └── dashboard.rs                          # +pause_account, +unpause_account handlers; extend account-detail response
│   ├── dashboard/
│   │   └── builder/handle.rs                     # +two route registrations w/ Permission::AccountsPause guard
│   ├── error.rs                                  # +AccountPaused variant; HTTP 409; gRPC FAILED_PRECONDITION; envelope fields
│   ├── metadata/
│   │   ├── mod.rs                                # +pause/unpause fields on AccountMetadata; +trait methods
│   │   ├── filesystem.rs                         # implement set_pause / clear_pause
│   │   └── postgres.rs                           # implement set_pause / clear_pause + read paused_at into struct
│   ├── services/
│   │   ├── account_status.rs                    # NEW — ensure_account_active chokepoint
│   │   ├── push_delta.rs                         # call ensure_account_active first
│   │   ├── push_delta_proposal.rs                # call ensure_account_active first
│   │   ├── sign_delta_proposal.rs                # call ensure_account_active first
│   │   ├── pause_account.rs                      # NEW — service for POST /dashboard/accounts/{account_id}/pause
│   │   ├── unpause_account.rs                    # NEW — service for POST /dashboard/accounts/{account_id}/unpause
│   │   └── dashboard_account_snapshot.rs         # add paused_at + paused_reason to detail projection
│   └── evm/
│       └── service.rs                            # (cfg evm) call ensure_account_active in create/approve/cancel_proposal
└── tests/
    ├── account_pause_endpoint.rs                 # NEW — US1/US2 integration matrix
    ├── account_pause_chokepoint.rs               # NEW — gRPC + HTTP mutating-path coverage
    ├── account_pause_audit.rs                    # NEW — admin_actions row coverage incl. idempotent retries
    └── account_pause_authz.rs                    # NEW — accounts:pause rejection path

packages/guardian-operator-client/
├── src/
│   ├── http.ts                                   # +pauseAccount, +unpauseAccount; surface GUARDIAN_ACCOUNT_PAUSED
│   ├── server-types.ts                           # +pausedAt, +pausedReason on OperatorAccountDetail; +error code union member
│   └── permissions.ts                            # (no change — accounts:pause already present)
└── src/http.test.ts                              # +pause/unpause matrix; +typed error assertions
```

**Structure Decision**: Single-project layout — server changes
co-located under `crates/server`, TypeScript client under
`packages/guardian-operator-client`. No new crate or package
introduced. No documentation crate split — the public-facing
contract surfaces in `spec/api.md` and the operator-client
`README.md` (Validation §Docs).

## Workstreams

### Server — Schema migration

- **Migration**
  `crates/server/migrations/2026-05-19-000001_account_pause_fields/`
  adds the two pause columns to `account_metadata`:
  - `up.sql`:
    ```sql
    ALTER TABLE account_metadata ADD COLUMN paused_at TIMESTAMPTZ NULL;
    ALTER TABLE account_metadata ADD COLUMN paused_reason TEXT NULL;
    CREATE INDEX IF NOT EXISTS idx_account_metadata_paused
        ON account_metadata(paused_at)
        WHERE paused_at IS NOT NULL;
    ```
  - `down.sql` drops the index and both columns with `IF EXISTS`.
  - Backfill is trivial: existing rows get `NULL` (active) for both
    columns. No data migration step needed.
- **Diesel `schema.rs`** updated to include the two new nullable
  columns on `account_metadata`. `AccountMetadataRow` /
  `NewAccountMetadataRow` insert structs gain the optional fields.

### Server — Persistence trait

- Extend `crates/server/src/metadata/mod.rs::AccountMetadata` with
  `paused_at: Option<DateTime<Utc>>` and
  `paused_reason: Option<String>`. Both serde-default to `None`.
- Add two methods to the `MetadataStore` trait with a default
  implementation that falls back to `get`/`set` so both backends
  remain interchangeable:
  ```rust
  /// Atomically set pause state. `now` is the caller-supplied
  /// timestamp; storing it server-side keeps clock authority on the
  /// server. Idempotent: if already paused, returns the existing
  /// `(paused_at, paused_reason)` unchanged (FR-013).
  async fn set_pause(
      &self,
      account_id: &str,
      now: DateTime<Utc>,
      reason: &str,
  ) -> Result<PauseTransition, String>;

  /// Clear pause state. Idempotent: no-op if already active (FR-014).
  async fn clear_pause(
      &self,
      account_id: &str,
  ) -> Result<PauseTransition, String>;
  ```
- `PauseTransition` carries `before_state: AccountStatus`,
  `after_state: AccountStatus`, `paused_at`, `paused_reason` so the
  handler emits the right audit row without a second read.
- **Postgres backend**
  (`crates/server/src/metadata/postgres.rs`): `set_pause` is a single
  `UPDATE account_metadata SET paused_at = COALESCE(paused_at, $1),
  paused_reason = COALESCE(paused_reason, $2) WHERE account_id = $3
  RETURNING paused_at, paused_reason, ...`. The `COALESCE`
  encodes idempotency (FR-013). `clear_pause` is `UPDATE … SET
  paused_at = NULL, paused_reason = NULL …`.
- **Filesystem backend**
  (`crates/server/src/metadata/filesystem.rs`): read-modify-write of
  the JSON record under the existing file lock. The lock is the
  serialization point.

### Server — Chokepoint helper

- New module `crates/server/src/services/account_status.rs` with one
  pub async fn:
  ```rust
  pub async fn ensure_account_active(
      state: &AppState,
      account_id: &str,
  ) -> Result<(), GuardianError>;
  ```
- Reads `state.metadata.get(account_id)`. If the record is missing,
  returns the same `AccountNotFound` the mutating service would
  produce, so the chokepoint does not change the error model on the
  not-found path.
- If `paused_at` is non-null, returns `GuardianError::AccountPaused
  { paused_at, paused_reason }`.
- **Single call site invariant (FR-025)**: this is the only place
  outside read endpoints and the pause handlers that reads
  `paused_at`. Enforced via a `#[deny(...)]`-style lint? No — by
  convention, with a code-review checklist item and the
  `account_pause_chokepoint.rs` integration test that drives all
  three mutating entry points and asserts behavior. Plan-level
  decision: keep this as a convention rather than a static check
  (Decision 5).
- Wired into:
  - `services::push_delta` — first non-validation step.
  - `services::push_delta_proposal` — first non-validation step.
  - `services::sign_delta_proposal` — first non-validation step.
  - `evm::service::create_proposal` (under `#[cfg(feature = "evm")]`) — first non-validation step.
  - `evm::service::approve_proposal` (under `#[cfg(feature = "evm")]`) — first non-validation step.
  - `evm::service::cancel_proposal` (under `#[cfg(feature = "evm")]`) — first non-validation step.

  Admin/setup paths (`services::configure_account`,
  `evm::service::register_account`) intentionally do NOT call
  the helper — see spec Non-Goals.

### Server — Error model

- Extend `GuardianError` (`crates/server/src/error.rs:9`) with:
  ```rust
  /// Account is paused; mutating action rejected with stable code
  /// GUARDIAN_ACCOUNT_PAUSED. HTTP 409 Conflict, gRPC
  /// FAILED_PRECONDITION. Details carry the persisted `paused_at`
  /// and `paused_reason` so clients can show context without a
  /// follow-up GET. Feature 001-account-pausing FR-010 / FR-011.
  AccountPaused {
      paused_at: DateTime<Utc>,
      paused_reason: Option<String>,
  },
  ```
- HTTP status mapping (`impl IntoResponse for GuardianError`):
  add `AccountPaused { .. } => StatusCode::CONFLICT`.
- gRPC status mapping: add `AccountPaused { .. } => tonic::Code::FailedPrecondition`.
- Code string: `AccountPaused { .. } => "GUARDIAN_ACCOUNT_PAUSED"`.
- Response envelope: extend the existing additive envelope with
  optional `paused_at: Option<String>` (RFC 3339) and
  `paused_reason: Option<String>` fields, populated only on the
  `AccountPaused` variant. Follows the same pattern as
  `missing_permissions` for `InsufficientOperatorPermission`.
- gRPC `details` carry `paused_at` (timestamp) and `paused_reason`
  via the existing tonic `Status::with_details` pattern; field names
  match the HTTP body so clients can deserialize once.

### Server — Audit

- Register two new `action_kind` consts in
  `crates/server/src/audit/kinds.rs` (already documents that #181
  will add these):
  ```rust
  pub const ACCOUNTS_PAUSE: &str = "accounts.pause";
  pub const ACCOUNTS_UNPAUSE: &str = "accounts.unpause";
  ```
  Extend `ALL_KINDS` and update the test.
- Audit `payload` schema for both kinds:
  ```jsonc
  {
    "before_state": "active" | "paused",
    "after_state":  "active" | "paused",
    "reason": "<string or null>"
  }
  ```
  `target_account_id` is set. `outcome` is `Success` for completed
  flips (including idempotent retries — FR-019). The handler emits
  the row **after** the persistence transition succeeds but **before**
  returning to the client, so the rule "200 implies audit row exists"
  holds. Audit-writer failures fall back to the structured log path
  established by 006-operator-authz; pause is NOT rolled back on
  audit failure (FR-021).

### Server — HTTP handlers

- Routes registered in
  `crates/server/src/dashboard/builder/handle.rs` under the existing
  dashboard router, each wrapped in the per-route authz layer
  declaring `&[Permission::AccountsPause]`:
  - `POST /dashboard/accounts/{account_id}/pause`
  - `POST /dashboard/accounts/{account_id}/unpause`
- Handlers in `crates/server/src/api/dashboard.rs`:
  - `pause_account(Path(account_id), Json(body))` validates body
    (`reason` required, non-empty, ≤ 512 chars — FR-007), calls
    `services::pause_account::pause(state, operator, account_id,
    reason)`, returns 200 with `{ paused_at, paused_reason,
    before_state, after_state }`.
  - `unpause_account(Path(account_id), Json(body))` validates body
    (`reason` optional, ≤ 512 chars if present), calls
    `services::unpause_account::unpause(state, operator, account_id,
    reason)`, returns 200 with `{ before_state, after_state, reason }`.
- Existing account-detail handler (account snapshot read) extends
  the response shape with `paused_at` and `paused_reason` as
  nullable fields, sourced from the metadata row (FR-005).
- HTTP path uses `/dashboard/accounts/...` rather than the
  arch-doc-suggested `/v1/operator/accounts/...` to **match the
  existing operator dashboard surface** established by
  `003-operator-account-apis` and `005-operator-dashboard-metrics`.
  See `research.md` Decision 3 for the rationale.

### Server — Services

- `services::pause_account::pause` — orchestrates validation, the
  `metadata.set_pause` call, the audit emission, and the response.
  Calls the metadata helper exactly once per request.
- `services::unpause_account::unpause` — symmetric; calls
  `metadata.clear_pause`.
- Both services treat "account not found" as the existing
  `AccountNotFound` 404, with **no audit row** (the request never
  reached an authenticated authorized actor against a real account).

### TypeScript — `guardian-operator-client`

- Extend `packages/guardian-operator-client/src/http.ts`:
  - `pauseAccount(accountId: string, reason: string): Promise<PauseResponse>`
  - `unpauseAccount(accountId: string, reason?: string): Promise<UnpauseResponse>`
- Extend `packages/guardian-operator-client/src/server-types.ts`:
  - Add `pausedAt: string | null` and `pausedReason: string | null`
    to `OperatorAccountDetail`.
  - Add `GUARDIAN_ACCOUNT_PAUSED` to the operator-error code union,
    with typed `details: { pausedAt: string; pausedReason: string | null }`.
- `http.test.ts` gains:
  - Happy-path pause/unpause matrix.
  - Reason-validation matrix (missing reason on pause → 400 path).
  - `GUARDIAN_ACCOUNT_PAUSED` deserialization on a mocked 409
    response — assert the typed branch is reachable without
    string-matching.

### Docs

- `spec/api.md`: add the two new endpoints, the new error code,
  and the extended account-detail fields.
- `packages/guardian-operator-client/README.md`: add a "Pausing an
  account" subsection mirroring the existing "Auth shape" / "Pagination
  shape" pattern.
- The `crates/server/src/audit/kinds.rs` comment block already
  predicts these kinds — update the comment to remove the
  "(e.g. #181 will register …)" phrasing once the consts are
  registered.

## Phasing

1. **Phase A — Migration + persistence trait** (blocking).
   `2026-05-19-000001_account_pause_fields` migration; `schema.rs`
   update; `AccountMetadata` struct fields; `set_pause` /
   `clear_pause` implementations on both backends; unit tests
   asserting idempotency at the persistence layer
   (`COALESCE` keeps original `paused_at`).
2. **Phase B — Error variant + chokepoint** (blocking, US1/US2
   prerequisite). New `GuardianError::AccountPaused`; HTTP + gRPC
   mappings; envelope extension; `ensure_account_active` module;
   wiring into the three mutating services. Integration test
   `account_pause_chokepoint.rs` drives a paused account from both
   gRPC and HTTP and asserts identical rejection shape.
3. **Phase C — Pause/unpause endpoints + audit** (US1, US2 — both
   P1). Route registration; handlers; reason validation (FR-007);
   `accounts.pause` / `accounts.unpause` audit kinds; idempotent-
   retry audit (FR-019). Integration test `account_pause_endpoint.rs`
   covers all US1 + US2 acceptance scenarios.
4. **Phase D — Account-detail read field + TS client** (US3 — P2).
   Extend the snapshot projection; bump the operator client to expose
   the two fields + the new error code + the two new methods. TS
   tests pinned to the wire shape.
5. **Phase E — Docs + final validation** (cross-cutting). `spec/api.md`,
   client README; run the operator-dashboard smoke test against a
   manually-paused account; confirm SC-005 (restart preserves pause)
   by stopping/starting the server in the integration harness.

Phases A–C are strictly sequential; D and E can run in parallel
once C lands. SC-007 (PolicyEngine swap localizability) is verified
at #182 merge time, not at this feature's merge.

## Validation

```bash
# Rust
cargo fmt --all -- --check
cargo clippy --workspace -- -D warnings
cargo test -p guardian-server
cargo test -p guardian-server --features postgres
cargo test -p guardian-server --features integration -- account_pause_
cargo test -p guardian-server --features "integration evm" -- account_pause_chokepoint_evm

# Run the new migration before exercising postgres-feature tests.
# Existing deployments must run this before the new code starts.
# DATABASE_URL=$GUARDIAN_TEST_DB \
#   cargo run -p guardian-server --features postgres --bin migrate

# TypeScript
cd packages/guardian-operator-client && npm run lint && npm test && npm run build

# End-to-end smoke (operator dashboard)
# Run via the existing smoke-test-operator-dashboard skill once the new
# wrappers land in guardian-operator-client. Manually exercise:
#   1) login
#   2) GET /dashboard/accounts/{id}      → paused_at == null
#   3) POST /dashboard/accounts/{id}/pause { reason: "..." }
#   4) Attempt push_delta on that account → 409 GUARDIAN_ACCOUNT_PAUSED
#   5) GET /dashboard/accounts/{id}      → paused_at populated
#   6) POST /dashboard/accounts/{id}/unpause { reason: "..." }
#   7) push_delta succeeds; admin_actions has three rows
```

Required validation matrix (all green before merge):

| Layer | Coverage | File |
|-------|----------|------|
| Server unit | `AccountPaused` HTTP/gRPC mapping + envelope serialization | `crates/server/src/error.rs` (existing test module) |
| Server unit | Persistence idempotency: re-pause preserves original `paused_at` | `crates/server/src/metadata/postgres.rs` + `filesystem.rs` tests |
| Server integration | All US1/US2 acceptance scenarios | `crates/server/tests/account_pause_endpoint.rs` |
| Server integration | Chokepoint on gRPC + HTTP for all multisig mutating paths; EVM mutating paths under `--features evm` | `crates/server/tests/account_pause_chokepoint.rs` |
| Server integration | `admin_actions` row coverage incl. idempotent retries | `crates/server/tests/account_pause_audit.rs` |
| Server integration | `accounts:pause` enforcement path | `crates/server/tests/account_pause_authz.rs` |
| Server integration | Restart preserves pause (SC-005) | `account_pause_endpoint.rs` — restart subtest |
| Server feature parity | Filesystem + Postgres | All `account_pause_*` tests run in both `--features` matrices |
| TS wrapper | pause/unpause happy + error matrix | `packages/guardian-operator-client/src/http.test.ts` |
| Docs | `spec/api.md` + client README | Manual review |

## Deferred

- `PolicyEngine` and any `Policy` trait, `AllowedRecipients`,
  runtime policy CRUD — out of scope per Non-Goals; tracked under
  `#182`.
- System-wide pause / global kill switch — Non-Goals.
- Bulk pause (multi-account) — Non-Goals.
- TTL-based auto-unpause — Non-Goals.
- Dashboard UI rendering — separate UI task. Operator client
  exposes the surface; UI work consumes it.
- gRPC parity for the pause/unpause **control** endpoints — they
  live under the operator dashboard surface which is HTTP-only
  per `005-operator-dashboard-metrics` Decision 2; documented
  divergence in `research.md` Decision 6. Pause **enforcement**
  is fully parity-preserved at the chokepoint.
- `#179` (Guardian error model unification) — soft prerequisite;
  this feature adds one new code string into the existing model
  and will fold into the central catalog when #179 lands without
  semantic change.
- Static enforcement of the "single chokepoint" invariant via a
  lint or macro — kept as a code-review convention + integration
  test (Decision 5).
