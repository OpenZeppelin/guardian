# Tasks: Human-readable error messages for wallet users

Feature `009-human-readable-errors`. Status as of the implementation branch
`009-human-readable-errors-impl`. See [plan.md](./plan.md).

## Done

- [x] **T001** Server: `GuardianError::user_message()` (sanitized, connectivity-vs-internal split) + `retryable()`. *(FR-001..FR-004)*
- [x] **T002** Server: reshape error wire object to `{ code, message, meta }`; `Display` logged not returned; drop `success`/`error`. *(FR-005, FR-007)*
- [x] **T003** Server: generalize `From<GuardianError> for tonic::Status` — `Status.message` = user message, `Status.details` = the object for every variant. *(FR-006, FR-008, FR-009)*
- [x] **T004** Server edges: rate-limit middleware + `configure` use the user-safe message; dead `ErrorResponse` removed.
- [x] **T005** gRPC handlers return `Err(Status::from(e))` (was in-band `success=false`). *(FR-006)*
- [x] **T006** OpenAPI `ApiErrorResponse`/`ApiErrorMeta` + regenerate `docs/openapi*.json`. *(FR-010)*
- [x] **T007** Rust `crates/client`: `guardian_code()`/`user_message()`/`guardian_meta()`/`is_not_found()`. *(FR-011)*
- [x] **T008** `crates/miden-multisig-client`: `get_deltas` branches on `is_not_found()` (not message text). *(FR-012, FR-014)*
- [x] **T009** TS `guardian-client`: `GuardianHttpError` parses `{ code, message, meta }`; replay-retry on `authentication_failed`. *(FR-010, FR-013)*
- [x] **T010** TS `miden-multisig-client`: `toUserFacingError()`/`isLikelyNetworkError()` connectivity classifier. *(FR-011, FR-013, US3)*
- [x] **T011** TS `guardian-operator-client`: parsers + types reshaped. *(FR-010)*
- [x] **T012** Examples: `operator-smoke-web` reads `data.message`. *(FR-014)*
- [x] **T013** Tests: SC-001 (every variant non-empty), SC-002 (sanitization + no `success`/`error`), SC-005 (connectivity vs internal split), parity + client/connectivity tests. *(SC-001..SC-007)*

## Deferred follow-ups (separate change)

- [ ] **T014** Strip vestigial `success`/`error_code` from gRPC success response messages (proto + handlers + Rust-client success parsing); migrate HTTP `configure` error path onto the envelope. *(strict "drop success" everywhere)*
- [ ] **T015** Route pre-service `Status::invalid_argument` auth-metadata guards through `GuardianError` for user-safe messages.
- [ ] **T016** CI: ensure cross-package TS builds link the in-repo `guardian-client` (the packages otherwise resolve published versions).
- [ ] **T017** Typed error codes in the TS clients — issue [#318](https://github.com/OpenZeppelin/guardian/issues/318): `GuardianErrorCode` union in `guardian-client` (no `| string` widening), fix the existing `DashboardErrorCode | string` widening in `guardian-operator-client`, add a drift-guard test against `GuardianError::code()`, and stop embedding the raw body in `GuardianHttpError.message` (mirror of the operator-client fix from the #304 review).
