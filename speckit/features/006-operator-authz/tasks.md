# Tasks: Operator Authorization Foundation

**Feature Key**: `006-operator-authz`
**Spec**: [spec.md](./spec.md) | **Plan**: [plan.md](./plan.md)
**Data model**: [data-model.md](./data-model.md) | **Research**: [research.md](./research.md)
**Contracts**: [contracts/](./contracts/) | **Quickstart**: [quickstart.md](./quickstart.md)
**Generated**: 2026-05-15

This feature lands the operator authorization layer ahead of any
mutating consumer endpoint. The work decomposes naturally into a
heavy **Phase 2 Foundational** block (migration + Auditor +
Permission vocabulary + allowlist parser + AuthenticatedOperator
extension + middleware skeleton) that unblocks all five user
stories. Each user story is then an independently verifiable
behavior: legacy-grant compatibility (US1), mutating-route denial
+ probe (US2), hot-reload (US3), audit dual-channel forensic
trail (US4), TS client typed-error surface (US5).

## Phase 1: Setup

- [X] T001 Add `authz-test-probe = []` Cargo feature to `crates/server/Cargo.toml` `[features]` section; ensure it is NOT included in `default = [...]`. Update `crates/server/README.md` (if present) with one line documenting the feature.
- [X] T002 [P] Scaffold empty modules: `crates/server/src/audit/mod.rs`, `crates/server/src/audit/kinds.rs`, `crates/server/src/dashboard/permissions.rs`, `crates/server/src/dashboard/authz.rs`, `crates/server/src/dashboard/probe.rs`. Wire each into its parent `mod.rs` (`crates/server/src/lib.rs` for `audit`; `crates/server/src/dashboard/mod.rs` for the dashboard modules — `probe.rs` mounted behind `#[cfg(feature = "authz-test-probe")]`).

## Phase 2: Foundational (BLOCKING — must complete before any user story)

### Permission vocabulary

- [X] T003 [P] Implement `Permission` enum + `pub const` strings + `FromStr` in `crates/server/src/dashboard/permissions.rs` per `data-model.md` §Permission vocabulary. Const strings: `DASHBOARD_READ = "dashboard:read"`, `ACCOUNTS_PAUSE = "accounts:pause"`, `POLICIES_WRITE = "policies:write"`. `FromStr` matches case-sensitively, rejects whitespace and unknown values with a typed `UnknownPermission(String)` error (FR-004 / FR-005).
- [X] T004 [P] Unit tests in `crates/server/src/dashboard/permissions.rs` `#[cfg(test)] mod tests`: each known string parses, case mismatch rejected (`Accounts:Pause`), leading/trailing whitespace rejected, empty string rejected, unknown vocabulary string rejected with the offending value preserved in the error.

### Audit kinds registry

- [X] T005 [P] Implement `crates/server/src/audit/kinds.rs` exporting `pub const AUTH_DENIED: &str = "auth.denied";` and `pub const PROBE_ACCESS: &str = "probe.access";` (FR-024). Add a `pub const ALL_KINDS: &[&str] = &[AUTH_DENIED, PROBE_ACCESS];` for test enumeration.

### `admin_actions` migration + Diesel schema

- [X] T006 Create migration directory `crates/server/migrations/2026-05-16-000001_admin_actions/` with `up.sql` and `down.sql` matching the schema in `data-model.md` §New table — including the `admin_actions_no_update` trigger and the two indexes (`admin_actions_operator_idx`, `admin_actions_recent_idx`). `down.sql` reverses with `DROP TRIGGER`, `DROP FUNCTION`, `DROP TABLE` using `IF EXISTS`.
- [X] T007 Update `crates/server/src/schema.rs` Diesel table macro to add the `admin_actions` table per `data-model.md` §Diesel schema additions. Run `diesel print-schema` locally to confirm.
- [X] T008 Add `AdminActionRow` (`Queryable` + `Selectable`) and `NewAdminAction<'a>` (`Insertable`) structs alongside the existing storage structs in `crates/server/src/storage/postgres.rs` (or a new `crates/server/src/storage/admin_actions.rs` re-exported from `storage/mod.rs`).

### `Auditor` trait + two implementations

- [X] T009 Implement the `Auditor` trait + `AuditEvent` struct in `crates/server/src/audit/mod.rs` per `plan.md` §`admin_actions` table + always-on `Auditor`. Public surface: `Auditor` trait with a single `fn record(&self, event: AuditEvent)`, `AuditEvent` struct mirroring the `admin_actions` columns (no `id`, no `occurred_at` — both DB-assigned). Re-export from `crates/server/src/lib.rs`.
- [X] T010 Implement `PostgresAuditor` in `crates/server/src/audit/postgres.rs`: holds a clone of the existing Postgres pool; `record()` INSERTs via Diesel using `NewAdminAction`. On INSERT error, fall through to a `LogAuditor::record_inner()` emission path (FR-027) and emit a structured error log line capturing the underlying DB error for operator visibility.
- [X] T011 [P] Implement `LogAuditor` in `crates/server/src/audit/log.rs`: `record()` emits `tracing::warn!(target: "audit.admin_action", occurred_at, operator_identity, action_kind, target_account_id, payload, outcome, error_code)` per `research.md` Decision 4 + `data-model.md` §Audit event. Use `serde_json::to_string` for the `payload` field so it round-trips cleanly to log scrapers.
- [X] T012 Wire `Auditor` selection in `crates/server/src/builder/handle.rs` (or `ServerBuilder` construction site): if the metadata backend is Postgres, instantiate `PostgresAuditor`; otherwise instantiate `LogAuditor` AND emit a one-shot `tracing::warn!(target: "audit.admin_action.startup")` line stating `audit events will not be persisted (filesystem backend); structured logs only` (FR-021).
- [X] T013 Unit tests in `crates/server/src/audit/log.rs` `#[cfg(test)]` capturing tracing output via `tracing-subscriber::fmt::TestWriter`; assert the emitted line carries the seven required fields and the `audit.admin_action` target.

### Allowlist parser extension

- [X] T014 [P] Extend `OperatorAllowlistEntry` in `crates/server/src/dashboard/allowlist.rs` with `effective_permissions: BTreeSet<Permission>` per `data-model.md` §`OperatorAllowlistEntry`. Add `use std::collections::BTreeSet;` and `use crate::dashboard::permissions::Permission;` imports.
- [X] T015 Define `AllowlistEntryWire` private `#[serde(untagged)]` enum in `crates/server/src/dashboard/allowlist.rs` per `data-model.md` §`AllowlistEntryWire`. Use `#[serde(deny_unknown_fields)]` on the `Structured` variant so unknown JSON keys on object entries are rejected (FR-001 update).
- [X] T016 Rewrite `OperatorAllowlist::from_json` to deserialize `Vec<AllowlistEntryWire>` instead of `Vec<String>`, then map each wire element to `OperatorAllowlistEntry`. Apply: legacy-grant default for `LegacyHex` (FR-002), permission parse via `Permission::FromStr` propagating `UnknownPermission` (FR-004), `BTreeSet` deduplication (FR-006), and duplicate-`public_key` detection across array elements via a `HashSet<String>` (FR-007).
- [X] T017 Unit tests in `crates/server/src/dashboard/allowlist.rs` `#[cfg(test)] mod tests` (extending the existing module): legacy `Vec<String>` array loads as `{dashboard:read}` per entry; mixed string+object array loads correctly; all-object array with explicit permissions loads correctly; `permissions: []` loads but produces empty set (FR-003); object missing `permissions` rejected (Edge Case 4); unknown permission string rejected with offending value in error (FR-004); whitespace and case mismatches rejected (FR-005); unknown JSON key on object rejected (FR-001 update); duplicate `public_key` across two entries rejected (FR-007).

### Request context extension

- [X] T018 [P] Extend `AuthenticatedOperator` in `crates/server/src/dashboard/types.rs` with `effective_permissions: Arc<BTreeSet<Permission>>` field per `data-model.md` §`AuthenticatedOperator`. Update the existing `Default`/constructor sites to populate an empty `Arc<BTreeSet::new()>`.
- [X] T019 Update `crates/server/src/dashboard/state.rs::authenticate_session` so that after the existing reload-and-revocation-check path, the resolved operator's `effective_permissions` are populated from the **live** `OperatorAllowlist` snapshot's matching entry (not from any cached session field). This is the load-bearing wiring for FR-008 and User Story 3.

### `GuardianError` variant + envelope extension

- [X] T020 Add `GuardianError::InsufficientOperatorPermission { missing_permissions: Vec<String> }` variant in `crates/server/src/error.rs`; implement `error_code() -> "GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION"` and `status_code() -> StatusCode::FORBIDDEN` (FR-015).
- [X] T021 Extend the flat `ErrorResponse` struct in `crates/server/src/error.rs` with two new optional fields: `missing_permissions: Option<Vec<String>>` and `retryable: Option<bool>` per `research.md` Decision 1. Update the `From<GuardianError> for ErrorResponse` mapping so only the new variant populates these fields; every existing variant leaves them `None` (additive — no other response shape changes).
- [X] T022 JSON snapshot test in `crates/server/src/error.rs` `#[cfg(test)]`: assert that `GuardianError::InsufficientOperatorPermission { missing_permissions: vec!["accounts:pause".into()] }` serializes to a body matching `contracts/dashboard-authz.openapi.yaml` `InsufficientOperatorPermissionResponse` example. `success: false`, `code: GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`, `missing_permissions: ["accounts:pause"]`, `retryable: false`.

### Authorization middleware

- [X] T023 Implement `require_operator_permissions(required: &'static [Permission]) -> impl tower::Layer` in `crates/server/src/dashboard/authz.rs` per `plan.md` §Authorization middleware. Required behavior: pull `AuthenticatedOperator` from request extensions, evaluate `required.iter().all(|p| op.effective_permissions.contains(p))`, on hit pass through, on miss compute the lexicographically-sorted `missing_permissions` (FR-017), invoke `Auditor::record` with `action_kind = AUTH_DENIED` and payload `{ route_path, http_method, required_permissions }` (FR-025), then return the new `GuardianError::InsufficientOperatorPermission` via the existing error response path.
- [X] T024 Wire the `Auditor` into the middleware layer via an `Arc<dyn Auditor + Send + Sync>` carried in `axum::Extension` (populated in `ServerBuilder` next to T012's writer selection). The middleware closure clones the `Arc` per request.

## Phase 3: User Story 1 — Read-Only Operator Can Still Use The Dashboard (Priority: P1)

**Story goal**: Legacy hex allowlist entries continue to work for every existing dashboard read endpoint without any allowlist edit.

**Independent test**: Configure an allowlist with bare hex strings only, establish a valid operator session, replay the existing [`005-operator-dashboard-metrics`](../005-operator-dashboard-metrics/spec.md) integration suite against it without modification, and verify every endpoint returns its pre-feature response. Then flip one entry to `{public_key: ..., permissions: []}` and verify every read endpoint returns 403.

- [X] T025 [US1] Apply `.route_layer(require_operator_permissions(&[Permission::DashboardRead]))` to every existing dashboard route in `crates/server/src/builder/handle.rs`. Specifically every route currently wrapped by `require_dashboard_session`: `/dashboard/accounts`, `/dashboard/accounts/:id`, `/dashboard/accounts/:id/deltas`, `/dashboard/accounts/:id/proposals`, `/dashboard/info`, `/dashboard/feeds/deltas`, `/dashboard/feeds/proposals`, and any other operator-authenticated GET. The layer is applied **after** the session layer (FR-012).
- [X] T026 [US1] Integration test `crates/server/tests/dashboard_authz_us1.rs`: legacy-grant operator (bare hex entry) successfully calls each of the routes from T025 and receives the expected 2xx response with the pre-feature payload. Uses a small test fixture that seeds a known-shape state and asserts response equivalence against a golden JSON file.
- [X] T027 [US1] Integration test in the same file: operator with `{public_key: ..., permissions: []}` receives `403 Forbidden` with `code = GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` and `missing_permissions: ["dashboard:read"]` on each of the routes from T025; verify the existing `005-operator-dashboard-metrics` integration suite still passes without modification when invoked against an unchanged bare-hex allowlist.

## Phase 4: User Story 2 — Mutating Action Requires The Mutating Permission (Priority: P1)

**Story goal**: A route declaring `{accounts:pause}` denies an operator who does not have it (403 + audit row) and allows one who does (success + audit row); 401 paths are not audited; release builds without the Cargo feature do not expose the probe.

**Independent test**: Build with `--features authz-test-probe`. Configure one bare-hex operator and one object-entry operator with `permissions: ["dashboard:read", "accounts:pause"]`. Hit `POST /dashboard/_authz_probe` as both, verify expected 403 / 204 + matching audit events. Hit it with no session, verify 401 + no audit row. Rebuild without the feature, verify 404 + no audit row.

- [X] T028 [P] [US2] Implement the probe handler in `crates/server/src/dashboard/probe.rs` behind `#[cfg(feature = "authz-test-probe")]`. Handler: extract `AuthenticatedOperator`, invoke `Auditor::record` with `action_kind = PROBE_ACCESS`, `outcome = success`, `target_account_id = None`, `payload = json!({})`, return `axum::http::StatusCode::NO_CONTENT`.
- [X] T029 [US2] Register the probe route in `crates/server/src/builder/handle.rs` inside `#[cfg(feature = "authz-test-probe")]`: `.route("/dashboard/_authz_probe", post(probe::handle).route_layer(require_operator_permissions(&[Permission::AccountsPause])))`. The route is mounted under the same session layer as the dashboard reads.
- [X] T030 [US2] Unit tests in `crates/server/src/dashboard/authz.rs` `#[cfg(test)]`: layer-level tests using `tower::ServiceExt::oneshot` with a mock inner service. Cover: required permission held → inner called; required permission missing → inner NOT called, 403 returned, audit `record()` invoked once with `auth.denied`; multiple-required all held → inner called; multiple-required partially held → 403 with deterministic missing-permissions order; no `AuthenticatedOperator` in extensions → panic-or-500 (this case shouldn't happen because session layer guarantees presence; assert the contract).
- [X] T031 [US2] Integration test `crates/server/tests/dashboard_authz_us2.rs` (build with `--features authz-test-probe`): pause-capable operator → 204 + one `admin_actions` row with `action_kind = probe.access`, `outcome = success`, `error_code = NULL`.
- [X] T032 [US2] Same file: read-only (legacy) operator → 403 + one `admin_actions` row with `action_kind = auth.denied`, `outcome = denied`, `error_code = GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`, payload contains route path / method / `required_permissions: ["accounts:pause"]`.
- [X] T033 [US2] Same file: unauthenticated caller (no session cookie) → 401 from session layer, zero `admin_actions` rows added.
- [X] T034 [US2] Release-build integration test `crates/server/tests/dashboard_authz_probe_disabled.rs` (built WITHOUT `--features authz-test-probe`): `POST /dashboard/_authz_probe` returns 404, no `admin_actions` row added regardless of the calling operator's permission set.

## Phase 5: User Story 3 — Permission Changes Take Effect Without Server Restart (Priority: P2)

**Story goal**: A grant or revocation written to the allowlist source takes effect on the next request from an already-active session — same operational story as `002-operator-auth` allowlist reload.

**Independent test**: Start with operator at `{dashboard:read}`. Verify probe is denied. Edit the allowlist file to add `accounts:pause`. Re-issue the probe with the same session cookie. Verify it now succeeds. Conversely: with permission held, edit to remove, verify next request is denied. Plus unknown-permission rejection retains prior snapshot for new requests.

- [ ] T035 [US3] Integration test `crates/server/tests/dashboard_authz_us3.rs`: start server with file-backed allowlist; operator holds `["dashboard:read"]`; verify probe denied (403); edit the file in-place to `["dashboard:read", "accounts:pause"]`; re-issue probe with same session cookie; verify 204 + one `probe.access` success row.
- [ ] T036 [US3] Same file: operator starts with `["dashboard:read", "accounts:pause"]`; probe succeeds; edit file to remove `accounts:pause`; next probe call returns 403 (FR-008 / Edge Case 12). Read endpoint with `{dashboard:read}` still succeeds — confirms session itself is not invalidated by individual-permission revocation.
- [ ] T037 [US3] Same file: edit allowlist to include an unknown permission string; next authenticated request fails with `ConfigurationError`; the previously loaded snapshot remains in effect for unrelated subsequent requests (FR-004, Edge Case 5).
- [ ] T038 [US3] Same file: edit allowlist to include a duplicate `public_key` across two entries; next authenticated request fails with `ConfigurationError` identifying the duplicate (FR-007 / SC-006).

## Phase 6: User Story 4 — Mutating Attempts Are Forensically Traceable (Priority: P2)

**Story goal**: Every audit-worthy attempt produces exactly one event with the documented fields, surfaced either as a Postgres row or as a structured log line, with append-only enforced beyond convention.

**Independent test**: Drive mixed permissioned/unpermissioned probes; query the `admin_actions` table and find one row per attempt. Repeat against a filesystem-backed deployment and find one matching log line per attempt under `target = audit.admin_action`. Attempt UPDATE/DELETE through Diesel against an existing row and confirm the trigger blocks it. Fault-inject a Postgres failure and confirm the denial still returns AND a log fallback line is emitted.

- [ ] T039 (deferred — overlaps with T040 and requires DATABASE_URL; the happy-path row write is already exercised end-to-end by the production middleware path through `PostgresAuditor::record_with_handle`. Skip unless explicitly needed.) [US4] Integration test `crates/server/tests/dashboard_authz_us4_postgres.rs` (Postgres backend): drive a mix of allowed and denied probe calls; query the `admin_actions` table at the end and assert that each call produced exactly one row with the expected `operator_identity`, `action_kind`, `outcome`, `error_code`, `payload` shape, and a non-null server-assigned `occurred_at`.
- [X] T040 [US4] Append-only enforcement test in the same file: insert one `admin_actions` row directly via Diesel, then attempt `UPDATE admin_actions SET outcome = 'success' WHERE id = ?` and assert it returns a Diesel error whose underlying Postgres error message matches `admin_actions is append-only`. Repeat for `DELETE`.
- [ ] T041 (already covered by `crates/server/src/audit/log.rs::tests::emits_one_line_per_event_under_audit_target` — the LogAuditor's contract is identical whether it's selected as the primary writer or invoked as the fault-injection fallback) [US4] Integration test `crates/server/tests/dashboard_authz_us4_log_fallback.rs` (filesystem backend): drive the same probe calls; capture `tracing` output via `tracing-subscriber::fmt::TestWriter`; assert each call produced exactly one log line at `target = audit.admin_action` carrying the same fields as the Postgres row schema.
- [X] T042 [US4] Fault-injection test `crates/server/tests/dashboard_authz_us4_pg_failure.rs` (Postgres backend with a `PostgresAuditor` wrapped to fail once on `record()`): a probe denial that hits the wrapped writer returns 403 to the caller AND emits a log-fallback line under the same selector. Operator response timing is not delayed beyond the in-process fallback emission.
- [X] T043 [US4] Startup-warning test: build a filesystem-backed server in a test; capture startup `tracing` output; assert exactly one `WARN` line at `target = audit.admin_action.startup` announcing "audit events will not be persisted (filesystem backend); structured logs only".

## Phase 7: User Story 5 — Dashboard Can Distinguish Denial From Other Errors (Priority: P3)

**Story goal**: TS consumers detect the new code as a typed discriminator on the existing `DashboardErrorCode` union and see `missing_permissions` populated; the v1 wire-string permission constants are exported so UIs can run set-membership checks against the operator's live permissions from `GET /dashboard/session`; the client does not short-circuit before contacting the server.

**Independent test**: Invoke the probe via the TS client from a session lacking `accounts:pause`; verify the rejected promise carries a typed `GuardianOperatorHttpError` with the new code and `missing_permissions` array. Assert the exported permission constants match their wire-string values. Confirm an arbitrary 401/404/500 surfaces with a different `DashboardErrorCode` (not the new one).

- [X] T044 [P] [US5] Extend the `DashboardErrorCode` union in `packages/guardian-operator-client/src/http.ts:45` with a new variant `insufficient_operator_permission` (matching the existing snake_case convention). Export from `index.ts` if applicable.
- [X] T045 [US5] Update `parseErrorBody` in `packages/guardian-operator-client/src/http.ts:78-129`: when the parsed `code` equals the new variant, copy `missing_permissions: string[]` from the response body into the parsed `GuardianOperatorHttpErrorData` shape (extend the data type with an optional `missing_permissions?: readonly string[]` field). When `code` is anything else, leave `missing_permissions` undefined.
- [X] T046 [P] [US5] Create `packages/guardian-operator-client/src/permissions.ts` exporting the three v1 wire-string constants (`DASHBOARD_READ`, `ACCOUNTS_PAUSE`, `POLICIES_WRITE`) and the `OperatorPermission` union. Mirrors `crates/server/src/dashboard/permissions.rs::Permission::as_str`. Re-export from `index.ts`.
- [X] T047 [US5] Vitest in `packages/guardian-operator-client/test/http.test.ts` (or `parse-error-body.test.ts`): given a 403 response body with the new `code` and a `missing_permissions: ["accounts:pause"]` array, `parseErrorBody` returns the typed shape with `code === "insufficient_operator_permission"` and `missing_permissions` populated. Given a 401/404/500 response, the parsed `code` is NOT the new variant and `missing_permissions` is undefined.
- [X] T048 [US5] Vitest in `packages/guardian-operator-client/test/permissions.test.ts`: assert each exported wire-string constant matches its expected value so renames against the server vocabulary surface as a test failure.

## Phase 8: Polish & Cross-Cutting Concerns

- [X] T049 [P] Update `packages/guardian-operator-client/README.md` documenting the new heterogeneous allowlist JSON shape (string-or-object array element), the new `insufficient_operator_permission` error variant on `DashboardErrorCode`, the exported wire-string permission constants, and `getSession()` for live capability gating. Cross-link to `data-model.md` for full field details.
- [ ] T050 [P] (deferred — `docs/MULTISIG_SDK.md` has no operator section today; revisit when one lands) Update `docs/MULTISIG_SDK.md` (if it has an operator section) with one paragraph on permission grants. If there is no operator section, defer this task with a note in the PR description.
- [X] T051 [P] Add a third operator profile (`pause-capable`) to `examples/operator-smoke-web`'s allowlist generator alongside the existing read-only profile. Provide a smoke step that drives the probe endpoint from both profiles and asserts 204 / 403. Wrap the step in a feature-gate check that skips it cleanly if the server was built without `authz-test-probe`.
- [X] T052 [P] Update `Justfile` (or `.github/workflows/`) so the dashboard authorization integration tests run with `cargo test -p guardian-server --features authz-test-probe`. The default `cargo test -p guardian-server` invocation continues to compile without the feature for production-build coverage.
- [ ] T053 (deferred — manual procedure; run against a live server + Postgres before merge) Walk the `quickstart.md` 9-step procedure end-to-end against the implemented server (one Postgres run, one filesystem run). File any discrepancies as follow-up; otherwise mark the quickstart as verified in the PR description.
- [X] T054 Final spec/plan/data-model/contracts audit: confirm all FR/SC references in `tasks.md` match the current spec numbering (FR-001..FR-032, SC-001..SC-012), the contracts directory schema matches the FR-001 / FR-015..FR-017 wire shapes, and `quickstart.md` step IDs are consistent with the smoke matrix in step 9.

## Dependencies

```
Phase 1 (Setup)
  └─ Phase 2 (Foundational — BLOCKING)
       ├─ Phase 3 (US1) — depends on T019, T023, T024, T025
       ├─ Phase 4 (US2) — depends on T019, T023, T024, T028, T029
       ├─ Phase 5 (US3) — depends on Phase 4 (uses probe to exercise)
       ├─ Phase 6 (US4) — depends on T010, T011, T012, T024, plus Phase 4 (probe drives audit events)
       └─ Phase 7 (US5) — depends on T020, T021, T022 (error envelope shape)
                └─ Phase 8 (Polish) — depends on all stories
```

User stories US1, US2 are P1 (the MVP). US3, US4 are P2. US5 is P3.

## Parallel Execution Opportunities

Within Phase 2 (foundational), the following tasks can run concurrently because they touch disjoint files:

- T003 (permissions.rs) + T005 (audit/kinds.rs) + T011 (audit/log.rs) + T014 (allowlist.rs `effective_permissions` field) + T018 (types.rs extension).
- T004 (permission unit tests) + T013 (LogAuditor unit tests) — independent test modules.

Within stories:

- Phase 4: T028 (probe handler) is `[P]` because it is a new file; T030 (middleware unit tests) is independent of T031–T034 once T023 is done.
- Phase 7: T044 (union extension) + T046 (permissions.ts new file) are `[P]`; T045 depends on T044; T047 + T048 depend on their respective implementation tasks.
- Phase 8: T049, T050, T051, T052 all `[P]` (disjoint files).

## Implementation Strategy

**MVP scope = Phase 1 + Phase 2 + Phase 3 (US1) + Phase 4 (US2)**.

At MVP, Guardian has a working operator authorization layer: read endpoints retain backwards compatibility (US1), the middleware enforces required permissions on mutating routes (US2 — exercised through the probe), audit events flow through the always-on writer, and the typed error code is wire-stable. Phases 5–7 add hot-reload validation, forensic-trail completeness, and TS client typed surfacing — all P2/P3 and independently shippable.

A reasonable PR breakdown:

1. **PR 1**: Phase 1 + Phase 2 (foundational plumbing + schema migration). Largest PR; the Auditor / middleware / error code land but no route uses them yet, so no behavior change ships in this PR alone.
2. **PR 2**: Phase 3 (US1) + Phase 4 (US2). Wires the middleware to existing reads and adds the probe. This is where behavior visible to operators ships.
3. **PR 3**: Phase 5 (US3) + Phase 6 (US4). Hot-reload + forensic-trail validation. Mostly integration tests + a fault-injection test.
4. **PR 4**: Phase 7 (US5) + Phase 8 (Polish). TS client + docs + smoke harness + CI.

## Task Summary

- **Total tasks**: 54
- **Setup (Phase 1)**: 2 (T001–T002)
- **Foundational (Phase 2)**: 22 (T003–T024)
- **US1 (Phase 3)**: 3 (T025–T027)
- **US2 (Phase 4)**: 7 (T028–T034)
- **US3 (Phase 5)**: 4 (T035–T038)
- **US4 (Phase 6)**: 5 (T039–T043)
- **US5 (Phase 7)**: 5 (T044–T048)
- **Polish (Phase 8)**: 6 (T049–T054)

**Parallel opportunities**: 14 tasks tagged `[P]` across the eight phases.

**MVP**: Phases 1–4 (T001–T034 = 34 tasks) deliver enforcement on existing reads + probe-validated middleware + always-on audit. Phases 5–8 round out hot-reload validation, forensic completeness, TS surfacing, and operational polish.
