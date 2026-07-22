# Implementation Plan: Human-readable error messages for wallet users

**Feature**: `009-human-readable-errors` · **Spec**: [spec.md](./spec.md) · **Branch**: `009-human-readable-errors-impl`

Implements the error contract reshaped in PR [#292](https://github.com/OpenZeppelin/guardian/pull/292) review by @zeljkoX: a single `{ code, message, meta }` object, identical on the HTTP body and the gRPC `Status.details`; the diagnostic `Display` string is logged server-side only; gRPC errors ride on `tonic::Status`.

## Wire contract (target)

```json
{ "code": "authorization_failed", "message": "You're not an authorized signer for this account.", "meta": { "retryable": false } }
```

- `code` — stable machine code (existing vocabulary, unchanged). Branch + i18n key.
- `message` — short, user-safe sentence; always present; safe to display. Wording not stable.
- `meta` — `retryable` (always present) + `retry_after_secs` / `missing_permissions` / `paused_at` / `paused_reason` when they apply.
- **HTTP**: response body is this object. No `success`, no `error`.
- **gRPC**: `Status.code` unchanged; `Status.message` = `message`; `Status.details` = this object (JSON), for every error.

## Layer-by-layer (bottom-up, Constitution I)

1. **Server core** (`crates/server/src/error.rs`): `GuardianError::user_message()` (sanitized per-variant, connectivity-vs-internal split per FR-004) + `retryable()`; `ErrorBody`/`ErrorMeta` structs; `IntoResponse` logs `Display` then emits the object; `From<GuardianError> for tonic::Status` attaches `{code,message,meta}` details for all variants with `Status.message = user_message`.
2. **Server edges**: `middleware/rate_limit.rs` reuses the canonical envelope; `api/http.rs` `configure` returns the user-safe message + code (in-band shape kept — *deferred follow-up*); dead `ErrorResponse` removed.
3. **gRPC handlers** (`api/grpc.rs`): all 9 service error arms return `Err(Status::from(e))` (was in-band `success=false`).
4. **OpenAPI**: `openapi.rs` `ApiErrorResponse` + `ApiErrorMeta`; regenerated `docs/openapi*.json`.
5. **Rust clients**: `crates/client` `ClientError::{guardian_code,user_message,guardian_meta,is_not_found}` parsed from `Status.details`; `crates/miden-multisig-client` `get_deltas` branches on `is_not_found()`.
6. **TS guardian-client** (wallet HTTP path): `GuardianHttpError` parses `{code,message,meta}` (snake→camel); replay-retry keyed on `authentication_failed` code.
7. **TS miden-multisig-client**: `toUserFacingError()` / `isLikelyNetworkError()` connectivity classifier (US3); re-exports the error types.
8. **TS guardian-operator-client**: `parseErrorBody`/`parseErrorResponse` read `message` + `meta.*`; types reshaped.
9. **Examples**: `operator-smoke-web` reads `data.message`.

## Validation

- Rust: `cargo check --workspace --all-targets` clean; `error::tests` (45), `error_envelope_http` (8), `dashboard` (73), `*_grpc` integration, `guardian-client` (41), `miden-multisig-client` (110).
- TS: `guardian-client` vitest (49), `guardian-operator-client` vitest (83), `miden-multisig-client` `connectivity` (6). Cross-package TS requires the local `guardian-client` linked/built (packages depend on published versions); CI uses the linked-PR mechanism. The full multisig vitest suite needs the miden-sdk WASM setup.

## Deferred follow-ups (out of this change, noted in commits)

- Strip the now-vestigial `success`/`error_code` from the gRPC **success** response messages (proto + handlers + Rust-client success parsing) and migrate the HTTP `configure` error path onto the `{code,message,meta}` envelope — completes a strict reading of Zeljko's "drop success / one object everywhere."
- Pre-service `Status::invalid_argument` auth-metadata guards (`api/grpc.rs`, `metadata/auth/credentials.rs`) still return developer-facing strings; route them through `GuardianError` for user-safe messages.

## Constitution check

- I (bottom-up): server → proto/OpenAPI → Rust clients → TS clients → examples all updated.
- II (parity): HTTP and gRPC carry the identical object; Rust + TS clients expose the same `code`/`message`/`meta`.
- IV (stable boundary errors): `code` vocabulary + HTTP/gRPC status mappings unchanged; diagnostic detail removed from the wire (disclosure fix by construction).
