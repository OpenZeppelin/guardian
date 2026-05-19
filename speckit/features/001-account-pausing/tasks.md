---
description: "Tasks for 001-account-pausing — operator-initiated per-account pause"
---

# Tasks: Operator-Initiated Per-Account Pause

**Input**: Design documents from `speckit/features/001-account-pausing/`
**Prerequisites**: `plan.md` (required), `spec.md` (required), `research.md`, `data-model.md`, `contracts/pause.openapi.yaml`, `quickstart.md`

**Tests**: Integration tests are REQUIRED for this feature (spec SCs and plan §Validation pin a coverage matrix). They are interleaved with implementation per user story rather than written upfront — chokepoint + handler + audit are tightly coupled, so tests are written against the real chokepoint module once it exists.

**Organization**: Tasks are grouped by user story so each story is independently testable. Phase 2 (Foundational) is a hard prerequisite for every user-story phase.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel — different files, no dependencies on incomplete tasks.
- **[Story]**: Which user story this task serves (`[US1]`, `[US2]`, `[US3]`). Setup, Foundational, and Polish phases have no story label.

## Path conventions

Server: `crates/server/`. TypeScript client: `packages/guardian-operator-client/`. Feature spec docs: `speckit/features/001-account-pausing/`.

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Sanity-check baseline; nothing new is introduced at the project-structure level (no new crate, no new package, no new dependencies).

- [X] T001 Confirm baseline builds clean before any feature change: `cargo build -p guardian-server --features postgres` and `cd packages/guardian-operator-client && npm run build` both pass on the active branch.

**Checkpoint**: Baseline green. No setup steps follow.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Schema migration, persistence trait extension, error variant, audit-kind registration, and the chokepoint helper module + wiring. Both P1 user stories (US1 pause, US2 unpause) depend on every task in this phase. US3 depends on the persistence changes only.

**⚠️ CRITICAL**: No user-story task starts until Phase 2 is complete.

### Schema & types

- [X] T002 Create migration directory `crates/server/migrations/2026-05-19-000001_account_pause_fields/` with `up.sql` adding `paused_at TIMESTAMPTZ NULL` and `paused_reason TEXT NULL` to `account_metadata` plus the partial index `idx_account_metadata_paused ON account_metadata(paused_at) WHERE paused_at IS NOT NULL` (exact SQL in `data-model.md` §Migration).
- [X] T003 Author `down.sql` in the same migration directory dropping the partial index and both columns with `IF EXISTS` clauses (matches `data-model.md` §Migration).
- [X] T004 Update Diesel `schema.rs` in `crates/server/src/schema.rs` (or wherever the workspace's generated schema lives) to include the two new nullable columns on `account_metadata`.
- [X] T005 [P] Extend `AccountMetadata` struct in `crates/server/src/metadata/mod.rs` with `paused_at: Option<DateTime<Utc>>` and `paused_reason: Option<String>` (both `#[serde(default)]`). Update any `AccountMetadata { .. }` literal call sites the compiler flags.
- [X] T006 [P] Add `AccountStatus` enum (`Active` / `Paused`) and `PauseTransition` struct in a new module `crates/server/src/services/account_status.rs`. Match the exact shapes in `data-model.md` §Rust types.

### Persistence trait

- [X] T007 Extend the `MetadataStore` trait in `crates/server/src/metadata/mod.rs` with two new async methods `set_pause(account_id, now, reason) -> Result<PauseTransition, String>` and `clear_pause(account_id) -> Result<PauseTransition, String>`. Document idempotency semantics inline (first-writer-wins on `set_pause`; no-op on `clear_pause` for an already-active account) per spec FR-013 / FR-014.
- [X] T008 [P] Implement `MetadataStore::set_pause` and `clear_pause` for the Postgres backend in `crates/server/src/metadata/postgres.rs`. `set_pause` uses `UPDATE account_metadata SET paused_at = COALESCE(paused_at, $1), paused_reason = COALESCE(paused_reason, $2) WHERE account_id = $3 RETURNING ...`; `clear_pause` is `UPDATE … SET paused_at = NULL, paused_reason = NULL …`. Return `PauseTransition` with `before_state` derived from the row read on the request, and `after_state` from the returned column values.
- [X] T009 [P] Implement `MetadataStore::set_pause` and `clear_pause` for the filesystem backend in `crates/server/src/metadata/filesystem.rs`. Read-modify-write under the existing per-file lock. Mirror Postgres idempotency at the in-memory level.
- [X] T010 Unit tests for both backends (filesystem; postgres deferred to integration suite) asserting first-writer-wins on `set_pause` (re-pause preserves original `paused_at`, original `paused_reason`) and idempotent no-op on `clear_pause` for an already-active account. Live alongside the implementations in `postgres.rs` / `filesystem.rs`.

### Error model

- [X] T011 Add `AccountPaused { paused_at: DateTime<Utc>, paused_reason: Option<String> }` variant to `GuardianError` in `crates/server/src/error.rs`. Add the four mapping branches per `data-model.md` §`GuardianError::AccountPaused`: code string `"GUARDIAN_ACCOUNT_PAUSED"`, HTTP `StatusCode::CONFLICT`, gRPC `tonic::Code::FailedPrecondition`, `retryable = Some(false)`.
- [X] T012 Extend the existing `ErrorBody` envelope in `crates/server/src/error.rs` with optional `paused_at: Option<String>` and `paused_reason: Option<String>` fields, populated only on the `AccountPaused` variant. Follow the additive-envelope pattern that `InsufficientOperatorPermission` uses for `missing_permissions`.
- [X] T013 Unit tests in `crates/server/src/error.rs` test module covering: `code()` returns `"GUARDIAN_ACCOUNT_PAUSED"`; HTTP `IntoResponse` returns 409 with the expected JSON envelope (including `paused_at`, `paused_reason`, `retryable: false`); gRPC `Status` carries `FAILED_PRECONDITION` plus `details` with both fields.

### Audit-kind registry

- [X] T014 Add two new const strings to `crates/server/src/audit/kinds.rs`: `pub const ACCOUNTS_PAUSE: &str = "accounts.pause"` and `pub const ACCOUNTS_UNPAUSE: &str = "accounts.unpause"`. Extend `ALL_KINDS` slice to include them. Update the existing module comment to drop the "(e.g. #181 will register …)" forward-reference now that the consts exist. Existing kind-naming tests will assert lowercase + domain.verb form for the new entries.

### Chokepoint helper

- [X] T015 Add `pub async fn ensure_account_active(state: &AppState, account_id: &str) -> Result<(), GuardianError>` to `crates/server/src/services/account_status.rs`. Reads `state.metadata.get(account_id)`; returns `GuardianError::AccountNotFound` on missing (matching existing handler shape); returns `GuardianError::AccountPaused { paused_at, paused_reason }` when `paused_at` is non-null; `Ok(())` otherwise. Document the FR-025 single-call-site invariant in module-level comment.
- [X] T016 Wire `ensure_account_active` into `crates/server/src/services/push_delta.rs` as the first non-validation step. Confirm any existing not-found path still produces `AccountNotFound` (the helper must not change the not-found error model).
- [X] T017 [P] Wire `ensure_account_active` into `crates/server/src/services/push_delta_proposal.rs` as the first non-validation step.
- [X] T018 [P] Wire `ensure_account_active` into `crates/server/src/services/sign_delta_proposal.rs` as the first non-validation step.
- [X] T019 [P] Wire `ensure_account_active` into `crates/server/src/evm/service.rs::create_proposal` under `#[cfg(feature = "evm")]` as the first non-validation step.
- [X] T020 [P] Wire `ensure_account_active` into `crates/server/src/evm/service.rs::approve_proposal` under `#[cfg(feature = "evm")]`.
- [X] T021 [P] Wire `ensure_account_active` into `crates/server/src/evm/service.rs::cancel_proposal` under `#[cfg(feature = "evm")]`.
- [X] T022 Confirm admin/setup paths (`services::configure_account`, `evm::service::register_account`) do NOT call `ensure_account_active` — this is the explicit Non-Goal. Add a one-line module-level comment in each pointing to the spec Non-Goals so a future contributor doesn't add the call.

**Checkpoint**: Migration applies cleanly up + down; both backends implement `set_pause`/`clear_pause` with idempotency tests green; `GuardianError::AccountPaused` round-trips on HTTP + gRPC; audit kinds registered; chokepoint wired into all six mutating entry points; admin paths confirmed unguarded.

---

## Phase 3: User Story 1 — Pause an account during incident response (Priority: P1) 🎯 MVP

**Goal**: An operator with `accounts:pause` POSTs `/dashboard/accounts/{account_id}/pause { reason }` and the account transitions to paused; subsequent mutating calls on any of the six entry points are rejected with `GUARDIAN_ACCOUNT_PAUSED`; the pause transition is recorded in `admin_actions`.

**Independent Test**: After Phase 2, with a clean test account and an operator session that holds `accounts:pause`, the integration test in `account_pause_endpoint.rs::pause_account_blocks_mutating_calls` covers FR-001, FR-003, FR-004, FR-005 (paused fields visible on detail), FR-007 (reason required + capped), FR-008–FR-012 (chokepoint enforcement on the multisig pipeline), FR-018, and US1 acceptance scenarios 1–3. SC-002 covers all six mutating entry points via `account_pause_chokepoint.rs`.

### Implementation

- [X] T023 [US1] Add `services::pause_account::pause(state, operator, account_id, reason) -> Result<PauseResponse, GuardianError>` in new file `crates/server/src/services/pause_account.rs`. Performs reason validation per FR-007 (non-empty, ≤ 512 UTF-8 chars; emits `GuardianError::InvalidInput` if violated), calls `state.metadata.set_pause(account_id, Utc::now(), reason)`, surfaces `AccountNotFound` unchanged, builds the `PauseResponse` body shape in `data-model.md`, and emits exactly one `admin_actions` row via `state.auditor.record(...)` with `action_kind = ACCOUNTS_PAUSE`, `target_account_id = Some(account_id)`, payload `{ before_state, after_state, reason }`, `outcome = Success`. The audit emit happens **after** the persistence transition succeeds but **before** the response returns.
- [X] T024 [US1] Add `pause_account` HTTP handler in `crates/server/src/api/dashboard.rs`. Path-extracts `account_id`, JSON-deserializes the `{ reason }` body, calls `services::pause_account::pause`, returns 200 with the `PauseResponse` body. Reject malformed JSON / missing `reason` field with 400 / `InvalidInput`.
- [X] T025 [US1] Register `POST /dashboard/accounts/{account_id}/pause` route in `crates/server/src/dashboard/builder/handle.rs` under the existing dashboard router. Wrap with the per-route authz layer declaring `&[Permission::AccountsPause]`. Confirm route_layer composition order (session middleware outer, authz inner) matches the established `006-operator-authz` pattern.
- [ ] T026 [US1] Integration test `crates/server/tests/account_pause_endpoint.rs` covering all four US1 acceptance scenarios from the spec: (1) success path with reason persists `paused_at`/`paused_reason`, response shape per OpenAPI; (2) post-pause `push_delta` / `push_delta_proposal` / `sign_delta_proposal` over HTTP are rejected with 409 / `GUARDIAN_ACCOUNT_PAUSED` carrying `paused_at` + `paused_reason`; (3) `admin_actions` has exactly one `accounts.pause` row with the operator's identity and correct payload; (4) operator session without `accounts:pause` is rejected with 403 / `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` and **no** `accounts.pause` row is written (FR-020).
- [ ] T027 [P] [US1] Integration test `crates/server/tests/account_pause_chokepoint.rs` covering the chokepoint across all multisig entry points on **both** gRPC and HTTP transports. Pause an account, then drive `push_delta`, `push_delta_proposal`, `sign_delta_proposal` from each transport — assert every call rejects with the same code string + identical `paused_at` + `paused_reason` payload across transports (gRPC `Status::FailedPrecondition` carries the same fields). This is the SC-002 multisig coverage.
- [ ] T028 [P] [US1] Integration test `account_pause_chokepoint_evm` (under `--features integration evm`) in the same file as T027, gated by `#[cfg(feature = "evm")]`. Pauses an EVM account and drives `create_proposal`, `approve_proposal`, `cancel_proposal` through the HTTP layer — assert every call rejects with `GUARDIAN_ACCOUNT_PAUSED` and identical payload. Completes SC-002.
- [ ] T029 [US1] Integration test `crates/server/tests/account_pause_audit.rs` asserting the FR-018 / FR-019 idempotency-audit matrix: pause → row 1 (`before=active, after=paused`); re-pause → row 2 (`before=paused, after=paused`, original `paused_at` preserved in account_metadata); `accounts:pause`-less operator pause attempt → no `accounts.pause` row, only `auth.denied` row from the middleware (FR-020).
- [ ] T030 [US1] Integration test in `account_pause_endpoint.rs` covering the reason-validation matrix (FR-007): missing body → 400; empty `reason` → 400; `reason` > 512 UTF-8 chars → 400; valid `reason` happy path → 200. All 400 responses carry `code = "GUARDIAN_INVALID_INPUT"` (or whatever the existing `InvalidInput` code maps to — confirm at implementation time).

**Checkpoint**: US1 fully functional. An operator can pause an account and observe enforcement on all six mutating entry points (multisig + feature-gated EVM) across gRPC + HTTP. `admin_actions` has the expected row(s). Permission gate works. Reason validation works.

---

## Phase 4: User Story 2 — Unpause once the incident is resolved (Priority: P1)

**Goal**: An operator with `accounts:pause` POSTs `/dashboard/accounts/{account_id}/unpause` and the account transitions back to active; previously rejected mutating calls now succeed; the transition is recorded in `admin_actions`. Independent of US1 only structurally — same module, same authz, same audit machinery; ships in the same PR.

**Independent Test**: After Phase 2 + US1 implementation, the integration test in `account_pause_endpoint.rs::unpause_account_restores_mutation` covers US2 acceptance scenarios 1–3 + FR-002 + FR-014 (idempotent no-op on already-active).

### Implementation

- [X] T031 [US2] Add `services::unpause_account::unpause(state, operator, account_id, reason) -> Result<UnpauseResponse, GuardianError>` in new file `crates/server/src/services/unpause_account.rs`. Reason is optional; if present, validate ≤ 512 UTF-8 chars (FR-007); calls `state.metadata.clear_pause(account_id)`; emits an `admin_actions` row with `action_kind = ACCOUNTS_UNPAUSE`, payload `{ before_state, after_state, reason }`. Idempotent no-op on already-active account still emits the audit row with `before_state == after_state == active` per FR-019.
- [X] T032 [US2] Add `unpause_account` HTTP handler in `crates/server/src/api/dashboard.rs`. Optional `{ reason? }` body — treat missing body as `{}`. Returns 200 with `UnpauseResponse`.
- [X] T033 [US2] Register `POST /dashboard/accounts/{account_id}/unpause` route in `crates/server/src/dashboard/builder/handle.rs` with the same `&[Permission::AccountsPause]` authz layer.
- [ ] T034 [US2] Integration tests in `crates/server/tests/account_pause_endpoint.rs` covering US2 acceptance scenarios 1–3: (1) unpause from paused → 200, `paused_at`/`paused_reason` cleared to NULL, response shape per OpenAPI; (2) previously rejected `push_delta` succeeds on the normal path; (3) `admin_actions` has the `accounts.unpause` row with operator identity, account ID, reason, before/after states.
- [ ] T035 [US2] Integration test in `account_pause_audit.rs` covering FR-014 / FR-019: unpause-while-active returns 200 with `before_state == after_state == active`; the `admin_actions` row exists with that same shape. No state changes on the account_metadata row.
- [ ] T036 [US2] Integration test in `account_pause_endpoint.rs` covering FR-016 (in-flight mutations not rolled back): start a `push_delta` that has already passed the chokepoint and is in the middle of persistence; concurrently issue pause; assert the in-flight delta either completes successfully (won the race) or fails at a deterministic later step — never partially applied. (This is a serializability assertion, not a timing assertion; assert via the database transaction model.)

**Checkpoint**: US2 fully functional. Operator can complete the pause → unpause cycle. In-flight mutations not rolled back. Audit log captures the full lifecycle.

---

## Phase 5: User Story 3 — Operator and dashboard can see pause state (Priority: P2)

**Goal**: The existing operator account-detail endpoint surfaces `paused_at` + `paused_reason`; the TypeScript operator client exposes typed access to those fields, the new `GUARDIAN_ACCOUNT_PAUSED` error code, and the two new methods `pauseAccount` / `unpauseAccount`.

**Independent Test**: After Phase 2 (read fields on the server side) + US1 (the only way to populate the fields), `account_pause_endpoint.rs::account_detail_surfaces_pause_state` covers US3 scenarios 1–2; `packages/guardian-operator-client/src/http.test.ts` covers US3 scenario 3.

### Server implementation

- [X] T037 [US3] Extend the operator account-detail projection in `crates/server/src/services/dashboard_account_snapshot.rs` (or wherever the existing `OperatorAccountDetail` builder lives — confirm at implementation time) to include `paused_at` and `paused_reason` sourced from the metadata row. Emit them whether the account is paused or active (null on active) so deserialization is uniform.
- [ ] T038 [US3] Integration test `account_pause_endpoint.rs::account_detail_surfaces_pause_state` covering US3 scenarios 1–2: GET detail on a paused account returns `paused_at` (non-null RFC 3339) and `paused_reason` (non-null non-empty string matching the supplied reason); GET on an active account returns both as `null`.
- [ ] T039 [US3] Add the SC-005 restart test in `account_pause_endpoint.rs`: pause an account; recycle the server process (or the test-harness equivalent); on restart, GET detail still reports `paused_at` and `paused_reason`; a mutating attempt still rejects with `GUARDIAN_ACCOUNT_PAUSED` carrying the **original** timestamp.

### TypeScript client

- [X] T040 [P] [US3] Extend `OperatorAccountDetail` in `packages/guardian-operator-client/src/server-types.ts` with `pausedAt: string | null` and `pausedReason: string | null`. Add `PauseAccountResponse`, `UnpauseAccountResponse`, and `AccountPausedErrorDetails` interfaces per `data-model.md` §TypeScript types.
- [X] T041 [P] [US3] Add `GUARDIAN_ACCOUNT_PAUSED` to the operator-error code union in `server-types.ts`, with the typed `details: { pausedAt: string; pausedReason: string | null }` payload.
- [X] T042 [US3] Add `pauseAccount(accountId: string, reason: string): Promise<PauseAccountResponse>` and `unpauseAccount(accountId: string, reason?: string): Promise<UnpauseAccountResponse>` methods to the operator client in `packages/guardian-operator-client/src/http.ts`. Wire them through the existing JSON-POST + error-mapping helpers so the new error code is surfaced via the existing typed-error branch.
- [ ] T043 [US3] Update `packages/guardian-operator-client/src/http.test.ts` with: happy-path `pauseAccount` (mocked 200, asserts response shape); happy-path `unpauseAccount`; `pauseAccount` reason-validation matrix (missing → throws, oversized → throws via 400 response handling); `GUARDIAN_ACCOUNT_PAUSED` typed-branch test (mocked 409 response → caller receives the typed error with `details.pausedAt`/`details.pausedReason`).

**Checkpoint**: Dashboards and SDK consumers see pause state without log parsing. The TS client is the production surface for the operator dashboard UI to consume.

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Documentation, the read-side structural invariant test (SC-006), and the end-to-end smoke. Most of this is non-blocking and can ship in parallel with US3.

- [ ] T044 [P] Add a "Pausing an account" subsection to `packages/guardian-operator-client/README.md` mirroring the existing "Auth shape" / "Pagination shape" pattern. Include the TS snippet from `quickstart.md` §"TypeScript client".
- [ ] T045 [P] Update `spec/api.md` to document the two new endpoints, the extended `GET /dashboard/accounts/{account_id}` fields, and the new `GUARDIAN_ACCOUNT_PAUSED` error code with its HTTP 409 / gRPC `FAILED_PRECONDITION` mapping.
- [ ] T046 [P] SC-006 structural test: add a unit test in `crates/server/src/services/account_status.rs` (or a co-located test file) that uses `grep`-equivalent introspection (e.g. compile-time `include_str!` on the relevant service files) to assert `ensure_account_active` is referenced only from the seven allowed sites: the six mutating service entry points plus the helper itself. The test fails if a new call site is added without explicit acknowledgement. (If implementing this as a string-grep test feels brittle, an acceptable equivalent is documenting the invariant in the module comment and relying on T027/T028 integration coverage — note the chosen approach in the test file.)
- [ ] T047 Run the operator-dashboard smoke via the `smoke-test-operator-dashboard` skill once T042 lands in the operator client. Walk steps 1–8 of `quickstart.md` end-to-end. Confirm the audit rows on the live `admin_actions` table match the expected per-step row pattern.
- [ ] T048 Final validation pass: run the full `plan.md` §Validation block (`cargo fmt`, `cargo clippy -D warnings`, `cargo test -p guardian-server`, both `--features postgres` and `--features "integration evm"` permutations; TypeScript lint + test + build). Confirm `cargo test -- account_pause_` is green across all matrices.

---

## Dependencies

- **Phase 1 → Phase 2 → all User Stories → Phase 6 (Polish)**. Phase 6 starts as soon as the corresponding user story it depends on is green (e.g. T044 needs T042, T045 needs T037).
- **Phase 2 is fully blocking**: every Phase 3 / 4 / 5 task imports symbols introduced in Phase 2 (the `AccountPaused` error variant, the `ensure_account_active` helper, the `set_pause` / `clear_pause` trait methods, the two new audit-kind consts).
- **US1 → US2**: structurally independent, but ship in the same PR — they share the new dashboard handler module + audit kinds + authz layer + operator-client wrappers. Splitting them across PRs would land a partial pause kill switch in production, which Non-Goals explicitly rejects.
- **US3 depends on US1** for end-to-end testing only (T038 needs a way to populate the pause fields; only US1 provides it). T037/T040/T041/T042/T043 are themselves implementable against an empty `paused_at` column once Phase 2 lands.
- **Polish phase tasks**:
  - T044 needs T042 (client methods exist).
  - T045 needs T024 + T032 (handlers exist) and T037 (extended detail).
  - T046 is independent and can land alongside Phase 2 once the chokepoint module exists.
  - T047 needs T042 (smoke test drives the published TS surface).
  - T048 needs everything.

## Parallel execution opportunities

Within Phase 2, after the migration + struct-extension cascade (T002 → T003 → T004 → T005):

- T005, T006 are `[P]` — different files.
- T008 (Postgres) and T009 (filesystem) are `[P]`.
- T017, T018, T019, T020, T021 are `[P]` — five different files; each just adds one `ensure_account_active(...)?` call.
- T011, T012 are sequential (same file).

Within Phase 3 (US1) after T026 lands:

- T027, T028 are `[P]` — distinct test fns gated by different cargo features.

Within Phase 5 (US3):

- T040, T041 are `[P]` — both touch `server-types.ts` but on disjoint type definitions; merge order doesn't matter.

Within Phase 6:

- T044, T045, T046 are all `[P]`.

## Implementation strategy

### Suggested MVP scope

**MVP = Phase 2 + Phase 3 (US1) + the unpause symmetry of Phase 4 (US2) + the read-side server change (T037) + the TS client wrappers (T042)**. That ships the pause kill switch with a way out, audit coverage, and the dashboard's read surface — the complete operational loop for incident response.

Specifically: T001–T031, plus T032, T033, T034, T037, T042, T044. T035, T036, T038, T039, T040, T041, T043, T045, T046, T047, T048 can land in a fast-follow PR if shipping pressure exists, but **T046 (SC-006 structural check) and T039 (SC-005 restart test) should not slip — both encode security-meaningful invariants**.

### Incremental delivery order

1. **Land Phase 2 in a first PR.** Migration + persistence + chokepoint + error variant. No user-facing change yet; the chokepoint always permits (no account is paused). All Phase 2 tests green.
2. **Land Phases 3 + 4 in a second PR.** Pause + unpause endpoints, audit emission, full US1 + US2 integration coverage. The kill switch is real.
3. **Land Phase 5 in a third PR.** Account-detail extension + TS client.
4. **Land Phase 6 in a fourth PR.** Docs, structural test, smoke. Non-blocking polish.

A single combined PR is also acceptable — the feature is small enough — but the three-PR sequence above isolates the security-significant change (kill switch live) from the read-side polish and the docs.

### Format validation

Every task above starts with `- [ ]`, carries a `T###` ID, includes a file path in the description, and (for user-story phases) the `[USx]` label. Setup, Foundational, and Polish tasks intentionally have no story label per the speckit-tasks rules.
