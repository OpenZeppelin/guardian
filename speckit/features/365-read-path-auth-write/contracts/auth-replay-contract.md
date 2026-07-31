# Contract: Authenticated-Read Replay Protection (Frozen Surface + Internal Delta)

**Feature**: `365-read-path-auth-write` | **Date**: 2026-07-31

This feature deliberately introduces **no wire-contract change** (FR-004). This document pins the external surface that must be bit-identical before and after, and records the internal contract that does change. It is the parity artifact for Constitution Principles I, II, and IV.

## External surface — FROZEN (verification target, not a change)

### Affected endpoints (behavioral no-op required)

All account-scoped endpoints that traverse `resolve_account`, over **both** HTTP and gRPC:

| Reads | Writes |
|---|---|
| `get_state` | `push_delta` |
| `get_delta_since` | `push_delta_proposal` |
| `get_delta_proposals` | `sign_delta_proposal` |
| `get_delta_proposal` | `abandon_candidate` |
| `get_delta` | |

For every endpoint above, the following are unchanged:

1. **Request/response shapes** — no proto change, no HTTP JSON change. (`guardian.proto` untouched; the contract-change workflow of AGENTS.md §4 is explicitly *not* triggered.)
2. **Acceptance semantics** — a request is accepted iff: account exists ∧ timestamp within ±300,000 ms of server time ∧ signature valid ∧ timestamp strictly greater than the account's last accepted timestamp. Identical predicate, identical ordering of checks.
3. **Replay rejection error** — `GuardianError::AuthenticationFailed("Replay attack detected: timestamp must be greater than previous request")` from `services/mod.rs:183-185`, with its existing mapping to HTTP status / gRPC code / typed error-code vocabulary. Byte-identical message.
4. **Skew rejection error** — the `"Request timestamp outside allowed window: …"` variant, untouched.
5. **Storage-failure error** — CAS infrastructure failure still maps to `GuardianError::StorageError("Failed to update last auth timestamp: …")`.

### Explicitly unaffected paths (must not start paying the write)

`/state/lookup` (timestamp-window-only by documented design), `/pubkey`, `/status`, `/`, dashboard session-cookie reads, EVM session-auth endpoints.

### Client compatibility assertion

`last_auth_timestamp` appears in no API response, no dashboard payload, and no SDK type. Existing Rust (`guardian-client`, `miden-multisig-client`) and TypeScript (`@openzeppelin/guardian-client`, `@openzeppelin/miden-multisig-client`) packages require **zero changes**; SC-005 verifies this with smoke flows rather than assumption.

## Internal contract — CHANGED (server-private)

### `MetadataStore` trait (`crates/server/src/metadata/mod.rs`)

```rust
async fn update_last_auth_timestamp_cas(
    &self,
    account_id: &str,
    new_timestamp: i64,
) -> Result<bool, String>;
```

- `now: &str` parameter removed (no more `updated_at` side effect — FR-008).
- Return-mapping contract (each row is a mandatory store-level unit test, both backends — data-model.md validation rule 1):

  | Prior state | New timestamp | Result | Stored value after |
  |---|---|---|---|
  | no row | any valid | `Ok(true)` (row created) | new |
  | `T` | `> T` | `Ok(true)` | new |
  | `T` | `== T` | `Ok(false)` (0 rows affected) | `T` |
  | `T` | `< T` | `Ok(false)` | `T` |
  | any | any, storage failing | `Err` | unchanged |

  A replay MUST surface as `Ok(false)`, never as `Err` — the call site maps the two to different error surfaces.
- Atomicity scope: implementations MUST be atomic under concurrent callers within the backend's supported deployment model — **across processes** for Postgres (the multi-replica backend), **within one process** for the filesystem backend, which is single-process by design across all of its operations (pre-existing documented limitation, unchanged by this feature).
- **New negative obligation**: no `MetadataStore` method other than this one may read or write replay state. In particular `set()` MUST NOT carry it (kills the stale-clobber race at `configure_account.rs:150` and `evm/service.rs:133` permanently).

### Storage schemas

- Postgres: new `account_auth_state` table; `account_metadata.last_auth_timestamp` dropped (see data-model.md for DDL, upsert-CAS, and migration).
- Filesystem: new `auth_state.json` beside the metadata cache; metadata cache no longer rewritten on authentication.

### Struct

`AccountMetadata` loses `last_auth_timestamp`. Server-internal only; serialization of this struct never crosses the API boundary.

## Verification hooks

| Frozen item | Verified by |
|---|---|
| Replay/skew error bytes & codes | Existing auth integration tests (must pass unchanged) + FR-009 tests |
| CAS return mapping | Store-level unit tests per the table above, both backends |
| Acceptance predicate across replicas | Two-replica replay checks on Postgres (quickstart.md, SC-003) |
| No client impact | Rust + TS SDK smoke flows against the changed server (SC-005) |
| HTTP/gRPC parity | Both transports exercised in the existing integration suites; shared `resolve_account` makes divergence impossible by construction |
