# Implementation Plan: Operator Authorization Foundation

**Feature Key**: `006-operator-authz` | **Date**: 2026-05-15 | **Spec**: [spec.md](./spec.md)

## Summary

Land the operator authorization layer in isolation, ahead of any
mutating consumer endpoint. The feature extends the existing operator
allowlist JSON (consumed by `GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON`,
`_FILE`, and `_SECRET_ID` — all three flow through one `from_json`
parser at `crates/server/src/dashboard/allowlist.rs:125`) so each
array element is either a legacy hex string
(`{dashboard:read}` only) or an object
`{ "public_key": "0xhex", "permissions": [...] }`. The authenticated
principal grows an effective-permission set; a new authorization
middleware runs after `require_dashboard_session` and denies requests
missing any required permission with a new stable error code
`GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`. A new append-only
`admin_actions` table backs a single `Auditor::record` writer that is
**always invoked**: it persists rows on Postgres deployments and
emits one structured log line per event on filesystem-only
deployments (with a loud startup warning). A Cargo-feature-gated
probe endpoint exercises the middleware end-to-end before #181
lands. `@openzeppelin/guardian-operator-client` extends its existing
`DashboardErrorCode` union with the new variant and ships a
`getSession()` wrapper so dashboards read the authenticated operator's
live permission set from the server.

The change is plumbing only. No DB-backed operator table, no
dashboard CRUD for operators/permissions, no new gRPC, no policy
DSL. Mutating consumer endpoints ([#181](https://github.com/OpenZeppelin/guardian/issues/181),
[#182](https://github.com/OpenZeppelin/guardian/issues/182)) reuse
this layer in follow-up features.

## Technical Context

- **Language / runtime**: Rust 2024 edition (server), TypeScript 5
  (`guardian-operator-client`).
- **Server**: `crates/server` with axum HTTP + Diesel-backed Postgres;
  filesystem metadata backend in `src/storage/filesystem.rs`. Operator
  surface is HTTP-only — no gRPC twins
  (`crates/server/proto/guardian.proto:6-42` carries only
  account/state RPCs).
- **Auth**: existing operator session middleware in
  `crates/server/src/dashboard/middleware.rs::require_dashboard_session`
  (cookie-backed, identity-only,
  [`002-operator-auth`](../002-operator-auth/spec.md)). Session
  storage is in-process
  `Arc<Mutex<HashMap<...>>>` (`dashboard/state.rs:27-28`).
- **Allowlist**: parsed by
  `crates/server/src/dashboard/allowlist.rs::OperatorAllowlist::from_json`
  (legacy `Vec<String>`) or `from_entries` (structured). Three
  sources feed one parser: deploy-time
  `GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON` (Terraform → AWS secret per
  `scripts/aws-deploy.sh:292-293`), `_FILE`, `_SECRET_ID`. Reload
  fires on every authentication request via
  `dashboard/state.rs::authenticate_session` →
  `refresh_allowlist:369-390`.
- **Storage**: Postgres metadata backend
  (`crates/server/src/storage/postgres.rs` + `migrations/`); filesystem
  backend (`storage/filesystem.rs`). This feature adds **one new
  table** (`admin_actions`) on the Postgres side only; the filesystem
  backend uses the log-fallback writer.
- **TypeScript consumer**: `packages/guardian-operator-client` —
  already exports a `DashboardErrorCode` union and a
  `GuardianOperatorHttpError` parsing path
  (`packages/guardian-operator-client/src/http.ts:45-167`). This
  feature extends both.
- **NEEDS CLARIFICATION**: none. Open questions are bounded to two
  small plan-phase decisions captured in `research.md` (envelope
  shape, append-only enforcement layer).

## Constitution Check

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Bottom-up change propagation | OK | Server-side allowlist/middleware/audit/error-code drives TS `guardian-operator-client` extensions (FR-030..FR-032). No new Rust base-client consumer is required since the operator surface is dashboard-only. |
| II. Transport and cross-language parity | Documented divergence | Operator surface is HTTP-only (no gRPC twin exists or is added). Same documented divergence already accepted by [`005-operator-dashboard-metrics`](../005-operator-dashboard-metrics/plan.md); reiterated in Decision 6 below. |
| III. Append-only integrity and explicit lifecycles | OK | `admin_actions` is append-only by design (FR-026) with an enforcement layer pinned in Decision 2. No state-machine transitions are added or modified; per-account state, delta, and proposal lifecycles are untouched. |
| IV. Explicit auth and stable boundary errors | OK | Introduces one new stable error code with pinned HTTP status (FR-015) and a typed TS discriminator (FR-030). The middleware retroactively applies `{dashboard:read}` to existing reads (FR-014), explicitly tightening the boundary rather than adding implicit fallback. |
| V. Evidence-driven delivery | OK | Five testable user stories, twelve success criteria. Validation matrix below covers Rust unit + integration, TS unit, and the `examples/operator-smoke-web` smoke surface. |

No unresolved violations. The HTTP-only divergence and the file-vs-secret-vs-env allowlist surface are inherited from prior features.

## Workstreams

### Server — Allowlist permissions parsing (Phase A)

- **JSON schema**: extend `OperatorAllowlist::from_json` at
  `crates/server/src/dashboard/allowlist.rs:125` to accept a
  heterogeneous array. Each element deserializes via a serde
  `untagged` enum (see `research.md` Decision 3):
  ```rust
  #[derive(Deserialize)]
  #[serde(untagged)]
  enum AllowlistEntryWire {
      LegacyHex(String),
      Structured { public_key: String, permissions: Vec<String> },
  }
  ```
  `LegacyHex` → `{dashboard:read}`. `Structured` → exact permission
  set (deduplicated; unknown strings reject the load).
- **Permission vocabulary**: add a new module
  `crates/server/src/dashboard/permissions.rs` exporting a stable
  `Permission` enum + `pub const` string constants for the three v1
  values. The enum's `FromStr` validates case-sensitivity and
  rejects whitespace (FR-005). Vocabulary lives in one Rust module
  so consumers (#181, #182) import a constant rather than a magic
  string.
- **Allowlist value type**: extend the in-memory `OperatorAllowlist`
  /`OperatorAllowlistEntryInput` to carry an `effective_permissions:
  BTreeSet<Permission>` field per entry. `BTreeSet` for deterministic
  ordering in the audit payload and missing-permissions field.
- **Duplicate detection**: reject duplicate `public_key` across array
  elements at load time (FR-007). Implemented as a `HashSet` check
  in the same load loop.
- **Tests**: unit tests on `from_json` covering legacy array,
  mixed array, all-object array, unknown permission, whitespace,
  case, `permissions: []`, missing `permissions` on object,
  duplicate key. Drop into existing
  `crates/server/src/dashboard/allowlist.rs` `#[cfg(test)]` module.

### Server — Request context extension (Phase A)

- **`AuthenticatedOperator`**: extend
  `crates/server/src/dashboard/types.rs:6-10` with
  `effective_permissions: Arc<BTreeSet<Permission>>` (Arc so it can
  be cheaply cloned into request extensions). Existing
  `operator_id` + `commitment` fields unchanged. (Decision 7.)
- **Session middleware**: update
  `crates/server/src/dashboard/state.rs::authenticate_session` so
  the post-reload allowlist lookup populates
  `effective_permissions` from the live snapshot rather than from
  the session record. This is what makes hot-reload of permissions
  take effect on live sessions (FR-008 / SC-004).
- **Handler ergonomics**: handlers consume `Extension<AuthenticatedOperator>`
  unchanged; new permissions check is enforced in middleware, not
  in handlers, so existing handler signatures are unaffected.

### Server — Authorization middleware (Phase B)

- **New module**:
  `crates/server/src/dashboard/authz.rs` exposes
  `require_operator_permissions(required: &'static [Permission])
  -> impl tower::Layer<...>` which:
  - Pulls `AuthenticatedOperator` from request extensions (always
    present because the layer is mounted **after**
    `require_dashboard_session` — FR-012).
  - Checks `required.iter().all(|p| ext.effective_permissions.contains(p))`.
  - On hit, calls the inner service.
  - On miss, computes `missing_permissions = required \
    effective_permissions` (lexicographic sort, FR-017), builds the
    `GuardianError::InsufficientOperatorPermission { missing }`,
    invokes the `Auditor::record` writer with `action_kind =
    auth.denied` and a `payload` containing route path, HTTP method,
    and the required set (FR-025), and returns a `403` response.
- **Route wiring**: in
  `crates/server/src/builder/handle.rs`, every dashboard route under
  the session layer gets a `.route_layer(require_operator_permissions(&[Permission::DashboardRead]))`
  applied after the session layer. The probe and (future) mutating
  endpoints declare `&[Permission::AccountsPause]` etc.

### Server — Error code wire-through (Phase B)

- **New variant**:
  `crates/server/src/error.rs::GuardianError::InsufficientOperatorPermission
  { missing_permissions: Vec<String> }` with `error_code() ->
  "GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION"` and `status_code() ->
  StatusCode::FORBIDDEN`.
- **Envelope**: extend the existing flat `ErrorResponse` additively
  with two new optional fields — `missing_permissions:
  Option<Vec<String>>` and `retryable: Option<bool>` (Decision 1).
  Existing TS `parseErrorBody` keeps working byte-for-byte for
  every other code; the new code populates both fields.
- **Tests**: round-trip test in
  `crates/server/src/error.rs` `#[cfg(test)]` snapshot-asserting
  the JSON shape, plus an integration test in
  `crates/server/src/dashboard/middleware.rs::tests` (or new
  `authz.rs::tests`) exercising 401 vs 403 path ordering.

### Server — `admin_actions` table + always-on `Auditor` (Phase A — runnable independently)

- **Migration**:
  `crates/server/migrations/2026-05-16-000001_admin_actions/up.sql`
  creates:
  ```sql
  CREATE TABLE admin_actions (
      id              BIGSERIAL PRIMARY KEY,
      occurred_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
      operator_identity TEXT NOT NULL,
      action_kind     TEXT NOT NULL,
      target_account_id TEXT NULL,
      payload         JSONB NOT NULL DEFAULT '{}'::jsonb,
      outcome         TEXT NOT NULL CHECK (outcome IN ('success','denied')),
      error_code      TEXT NULL
  );
  CREATE INDEX admin_actions_operator_idx
      ON admin_actions (operator_identity, occurred_at DESC);
  CREATE INDEX admin_actions_recent_idx
      ON admin_actions (occurred_at DESC);
  -- Decision 2: enforce append-only at DB layer
  CREATE OR REPLACE FUNCTION admin_actions_append_only()
      RETURNS trigger AS $$
      BEGIN
          RAISE EXCEPTION 'admin_actions is append-only';
      END;
  $$ LANGUAGE plpgsql;
  CREATE TRIGGER admin_actions_no_update
      BEFORE UPDATE OR DELETE ON admin_actions
      FOR EACH ROW EXECUTE FUNCTION admin_actions_append_only();
  ```
  `down.sql` reverses with `DROP TRIGGER`, `DROP FUNCTION`, `DROP TABLE`.
- **Diesel `schema.rs`**: add `admin_actions` table + `AdminAction`
  + `NewAdminAction` structs.
- **`Auditor` trait**:
  ```rust
  // crates/server/src/audit/mod.rs (new module)
  pub trait Auditor: Send + Sync {
      fn record(&self, event: AuditEvent);
  }
  pub struct AuditEvent { /* mirrors admin_actions columns */ }
  ```
  Two implementations:
  - `PostgresAuditor` (binds the `MetadataStore` Postgres pool) —
    INSERTs the row. If the INSERT fails, falls through to the
    `LogAuditor` emission path (FR-027).
  - `LogAuditor` — emits `tracing::warn!(target: "audit.admin_action",
    ...)` with structured fields matching the row schema.
- **Selection**: `ServerBuilder` selects the implementation based on
  whether a Postgres `MetadataStore` is configured. Filesystem-only
  deployments get `LogAuditor` directly + a one-shot startup
  `tracing::warn!` (FR-021).
- **`action_kind` registry**: new module
  `crates/server/src/audit/kinds.rs` exporting `pub const AUTH_DENIED:
  &str = "auth.denied"` etc. (FR-024). Consumer features (#181) PR
  in their own consts to this module.

### Server — Probe endpoint (Phase B)

- **Cargo feature**: add `authz-test-probe = []` to
  `crates/server/Cargo.toml` (default-off in release builds —
  Decision 5).
- **Module**: `crates/server/src/dashboard/probe.rs` registered
  inside `#[cfg(feature = "authz-test-probe")]` blocks in
  `crates/server/src/builder/handle.rs`. One route, e.g.
  `POST /dashboard/_authz_probe`, declares
  `require_operator_permissions(&[Permission::AccountsPause])`. On
  success, invokes `Auditor::record` with `action_kind =
  probe.access`, `outcome = success`. Returns `204 No Content`.
- **Test surface**: server integration tests build with
  `--features authz-test-probe`; the existing `cargo test -p guardian-server`
  command is updated in `Justfile`/CI to add this feature for the
  authz integration suite.

### TypeScript — `guardian-operator-client` (Phase C)

- **Error code variant**: add a new entry to the
  `DashboardErrorCode` union at
  `packages/guardian-operator-client/src/http.ts:45` (working name
  `insufficient_operator_permission` to match the existing
  `snake_case` convention).
- **`parseErrorBody`**: in
  `packages/guardian-operator-client/src/http.ts:78-129`, when the
  parsed `code` equals the new variant, copy the
  `missing_permissions` array out of the response body into the
  parsed `GuardianOperatorHttpErrorData`.
- **Permission constants + session wrapper**:
  `packages/guardian-operator-client/src/permissions.ts` exports the
  three v1 wire-string constants (`DASHBOARD_READ`,
  `ACCOUNTS_PAUSE`, `POLICIES_WRITE`) and the `OperatorPermission`
  union. `GuardianOperatorHttpClient` adds `getSession(): Promise<{
  operatorId: string; permissions: string[] }>` so dashboards read
  the operator's live permission set from `GET /dashboard/session`
  and compare entries against the exported constants. Consumers MAY
  use these for UI gating; the client MUST NOT short-circuit a
  request based on them (FR-032).
- **Vitest coverage**: extend
  `packages/guardian-operator-client/test/http.test.ts` (or
  equivalent) with a `parseErrorBody` round-trip for the new code,
  plus a `getSession()` test covering the populated and explicit-empty
  responses, and a `permissions.test.ts` assertion that the wire-string
  constants match the server's vocabulary.

### Tests

- **Rust unit**: allowlist loader (mixed shapes, unknown,
  whitespace, duplicate); permission enum parse; middleware layer
  short-circuit; `LogAuditor` selector format; `PostgresAuditor` row
  shape (via in-memory mock or Diesel test harness); error-code
  JSON snapshot.
- **Rust integration** (`crates/server/tests/dashboard_authz.rs`,
  new file): legacy-grant operator passes dashboard reads;
  object-empty-`permissions` operator gets 403 on every dashboard
  read; permissioned operator passes probe and produces one
  `admin_actions` row; unpermissioned operator fails probe and
  produces one denied row; unauthenticated caller gets 401 with no
  audit row; reload-mid-session adds permission and unblocks next
  request.
- **Fault injection**: a focused integration test that wraps the
  `PostgresAuditor` to fail-once and asserts that the denial still
  returns AND a `LogAuditor`-shaped tracing line is emitted (SC-012).
- **TypeScript unit**: `parseErrorBody` decodes the new variant
  with `missing_permissions` populated; `permissions.ts` snapshot
  is stable.
- **Smoke**: `examples/operator-smoke-web` gets a third allowlist
  profile (legacy + read-only + pause-capable) and a smoke step
  exercising the probe with the pause-capable identity. Skipped if
  the server is built without `authz-test-probe`.

### Docs

- Update `docs/MULTISIG_SDK.md` (operator section, if present) and
  `packages/guardian-operator-client/README.md` to describe the new
  JSON allowlist shape and the typed error variant. No new top-level
  doc.

## Phasing

Three phases. A and B parallelizable; C depends on B's error-code wire-through.

- **Phase A — Foundations** (parallel-safe)
  - A1: `admin_actions` migration + `Auditor` module + log fallback
    selection in `ServerBuilder`.
  - A2: allowlist heterogeneous JSON parsing + permission vocabulary
    module + duplicate-key detection.
  - A3: `AuthenticatedOperator` extension + session-middleware
    permission lookup against live allowlist.
- **Phase B — Middleware + error + probe** (depends on A2/A3)
  - B1: new `GuardianError` variant + envelope extension + JSON
    snapshot tests.
  - B2: `require_operator_permissions` Tower layer +
    `auth.denied` audit emission.
  - B3: route-wiring in `builder/handle.rs` retrofitting
    `{dashboard:read}` on every existing dashboard route.
  - B4: probe endpoint behind `authz-test-probe` Cargo feature +
    integration test.
- **Phase C — TS client + docs** (depends on B1)
  - C1: `DashboardErrorCode` union extension + `parseErrorBody`
    update + Vitest.
  - C2: permission constants + `getSession()` wrapper + Vitest.
  - C3: README updates.

## Validation

Per [guardian-validation-matrix](.claude/skills/guardian-validation-matrix/SKILL.md):

| Layer | Command | Coverage |
|-------|---------|----------|
| Server unit | `cargo test -p guardian-server --features authz-test-probe` | Allowlist loader, permission enum, middleware, Auditor, error code |
| Server integration | `cargo test -p guardian-server --features authz-test-probe --test dashboard_authz` | All five user stories end-to-end via test server |
| Server lints | `cargo clippy -p guardian-server --features authz-test-probe -- -D warnings` | No new lints |
| TS unit | `cd packages/guardian-operator-client && npm test` | `parseErrorBody`, permission metadata snapshot |
| TS typecheck | `cd packages/guardian-operator-client && npm run typecheck` | Union extension compiles cleanly |
| Smoke (manual) | [smoke-test-operator-dashboard](.claude/skills/smoke-test-operator-dashboard/SKILL.md) | Three allowlist profiles + probe call |

Postgres-side append-only is asserted by a dedicated test that
attempts `UPDATE admin_actions ...` through Diesel and expects the
trigger to fire (SC-009).

Log-fallback assertion is captured by capturing `tracing` output
during a filesystem-backend integration run and grepping for the
`audit.admin_action` target (SC-011).

## Deferred

- DB-backed operator storage and dashboard CRUD endpoints — separate
  follow-up ticket (see spec §Dependencies). v1 manages operators by
  editing the existing JSON source.
- Retention policy for `admin_actions`. Volume is tens/day; revisit
  when warranted.
- Audit row read endpoint. `psql` for now.
- Per-account permission scoping. Server-wide perms only in v1.
- gRPC operator parity. Not adding to operator surface.
- Pinning the human-readable `message` of the new error code.
- External SIEM/log-aggregator ingestion of `audit.admin_action`
  events. Surface is structured; pipelines are operator-side.
- Disjunctive ("any of") required-permission sets in middleware
  (FR-011).
- Hot-reload of `GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE` watcher. The
  existing reload-on-authenticate path is sufficient.
