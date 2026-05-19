# Data Model: Operator-Initiated Per-Account Pause

## Persistence Changes

### Migration `2026-05-19-000001_account_pause_fields`

Adds two nullable columns to the existing `account_metadata` table
introduced by `2026-03-12-000002_account_metadata`.

```sql
-- up.sql
ALTER TABLE account_metadata ADD COLUMN paused_at TIMESTAMPTZ NULL;
ALTER TABLE account_metadata ADD COLUMN paused_reason TEXT NULL;

-- Partial index supports "list all currently-paused accounts" cheaply
-- even on a wide table. Index size is proportional to the count of
-- currently-paused accounts, not total accounts.
CREATE INDEX IF NOT EXISTS idx_account_metadata_paused
    ON account_metadata(paused_at)
    WHERE paused_at IS NOT NULL;
```

```sql
-- down.sql
DROP INDEX IF EXISTS idx_account_metadata_paused;
ALTER TABLE account_metadata DROP COLUMN IF EXISTS paused_reason;
ALTER TABLE account_metadata DROP COLUMN IF EXISTS paused_at;
```

**Backfill**: none required. Existing rows are active (both columns
NULL). New writes set the columns directly; idempotency is encoded
at the UPDATE level via `COALESCE` (see Decision 2 in
`research.md`).

**Diesel `schema.rs`** updated to include the two new nullable
columns. `AccountMetadataRow` and `NewAccountMetadataRow` gain the
two optional fields.

### Logical invariant: `(paused_at IS NULL) ↔ (paused_reason IS NULL)`

`paused_reason` only carries meaning when `paused_at` is set. The
two fields are flipped together: pause sets both, unpause clears
both. The chokepoint helper (`ensure_account_active`) reads
`paused_at`; if non-null, it also includes `paused_reason` in the
response error. There is no application-level state where one
column is set and the other is not. (A `CHECK (paused_at IS NULL =
paused_reason IS NULL)` constraint is **not** added to the schema —
the invariant is enforced at the trait level via the typed
`set_pause` / `clear_pause` helpers, which flip both columns
together.)

## Rust types

### `AccountMetadata` (extension)

```rust
// crates/server/src/metadata/mod.rs
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AccountMetadata {
    pub account_id: String,
    pub auth: Auth,
    pub network_config: NetworkConfig,
    pub created_at: String,
    pub updated_at: String,
    pub has_pending_candidate: bool,
    #[serde(default)]
    pub last_auth_timestamp: Option<i64>,
    // ── New, feature 001-account-pausing ──
    /// UTC timestamp of the first pause request that took effect.
    /// `None` when the account is active. First-writer-wins: re-pausing
    /// a paused account does NOT change this value. See `research.md`
    /// Decision 2.
    #[serde(default)]
    pub paused_at: Option<DateTime<Utc>>,
    /// Operator-supplied reason from the original (first) pause
    /// request. `None` when the account is active. Capped at 512
    /// UTF-8 characters by the handler. Required on pause.
    #[serde(default)]
    pub paused_reason: Option<String>,
}
```

`paused_at` is `Option<DateTime<Utc>>` rather than the broader
`String` to keep clock-zone semantics explicit. RFC 3339 / ISO 8601
serialization is the default for `chrono::DateTime<Utc>` under
serde and matches the wire format other Guardian APIs emit.

### `AccountStatus` (new)

```rust
// crates/server/src/services/account_status.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountStatus {
    Active,
    Paused,
}
```

Used in:
- `PauseTransition` (audit + response envelope).
- Audit payloads (`before_state`, `after_state`).
- Account-detail derived field (optional convenience — clients can
  also derive from `paused_at == null`).

### `PauseTransition` (new)

```rust
// Returned from MetadataStore::set_pause / clear_pause.
#[derive(Debug, Clone)]
pub struct PauseTransition {
    pub before_state: AccountStatus,
    pub after_state: AccountStatus,
    pub paused_at: Option<DateTime<Utc>>,
    pub paused_reason: Option<String>,
}
```

This is the value the handler audits and serializes back to the
client. Carrying both before and after states explicitly makes
idempotent retries auditable (FR-019): an `accounts.pause` row with
`before_state == after_state == Paused` records the attempt without
implying a state change.

### `GuardianError::AccountPaused` (new variant)

```rust
// crates/server/src/error.rs
pub enum GuardianError {
    // ... existing variants ...

    /// Account is paused; mutating action rejected with stable code
    /// `GUARDIAN_ACCOUNT_PAUSED`. HTTP 409 Conflict, gRPC
    /// FAILED_PRECONDITION. `details` carry the persisted
    /// `paused_at` and `paused_reason` so clients can show context
    /// without a follow-up GET. Feature 001-account-pausing
    /// FR-010 / FR-011.
    AccountPaused {
        paused_at: DateTime<Utc>,
        paused_reason: Option<String>,
    },
}
```

Mapping table additions:

| Surface | Mapping |
|---------|---------|
| `code()` (`error.rs` `code()` fn) | `AccountPaused { .. } => "GUARDIAN_ACCOUNT_PAUSED"` |
| `IntoResponse` | `AccountPaused { .. } => StatusCode::CONFLICT` (409) |
| `tonic::Code` | `AccountPaused { .. } => tonic::Code::FailedPrecondition` |
| Envelope (`ErrorBody`) | extended with `paused_at: Option<String>` (RFC 3339) and `paused_reason: Option<String>`, set only on this variant |
| `retryable` | `Some(false)` — pause is operator-controlled, retry without operator action loops forever |

## Audit event

### New `action_kind` consts

```rust
// crates/server/src/audit/kinds.rs
pub const ACCOUNTS_PAUSE: &str = "accounts.pause";
pub const ACCOUNTS_UNPAUSE: &str = "accounts.unpause";

pub const ALL_KINDS: &[&str] = &[
    AUTH_DENIED,
    PROBE_ACCESS,
    ACCOUNTS_PAUSE,
    ACCOUNTS_UNPAUSE,
];
```

### Payload schema

```jsonc
// admin_actions.payload for accounts.pause / accounts.unpause
{
  "before_state": "active" | "paused",
  "after_state":  "active" | "paused",
  "reason":       "<string or null>"
}
```

- `target_account_id`: the paused/unpaused account ID. **Always set**
  for these kinds.
- `outcome`: `"success"` for completed transitions (including
  idempotent retries — FR-019).
- `error_code`: `null` on the success path; `"GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION"`
  for authz rejections (emitted by the existing middleware, not by
  this feature). This feature does not introduce `outcome = "denied"`
  rows of its own — pause/unpause that reach the handler always
  succeed at the persistence layer (idempotency).
- `operator_identity`: from the existing `AuthenticatedOperator`
  context — same field 006-operator-authz populates.
- `client_ip`: from the existing request-context extraction
  (`extract_client_ip`).

### Idempotent retry semantics

| Pre-state | Action | Persistence change | Audit `before_state` | Audit `after_state` | Response `paused_at` |
|-----------|--------|-------------------|---------------------|--------------------|---------------------|
| Active    | Pause   | `paused_at` ← now, `paused_reason` ← supplied | `active`  | `paused`  | now |
| Paused    | Pause (retry) | none (COALESCE preserves original) | `paused`  | `paused`  | **original** timestamp |
| Paused    | Unpause | both cleared to NULL | `paused`  | `active`  | (cleared; absent) |
| Active    | Unpause (no-op) | none | `active`  | `active`  | (none — never set) |

All four rows are written to `admin_actions`. The "Pause retry" and
"Unpause no-op" rows record that the attempt happened without
implying a state change.

## Response shapes

### Pause response (HTTP 200)

```jsonc
// POST /dashboard/accounts/{account_id}/pause
{
  "account_id":    "acct_X",
  "before_state":  "active",
  "after_state":   "paused",
  "paused_at":     "2026-05-19T14:23:00Z",
  "paused_reason": "suspected cosigner compromise"
}
```

On the idempotent-retry path:

```jsonc
// (account was already paused before this request)
{
  "account_id":    "acct_X",
  "before_state":  "paused",
  "after_state":   "paused",
  "paused_at":     "2026-05-19T14:23:00Z",   // ORIGINAL timestamp
  "paused_reason": "suspected cosigner compromise"  // ORIGINAL reason
}
```

### Unpause response (HTTP 200)

```jsonc
// POST /dashboard/accounts/{account_id}/unpause
{
  "account_id":   "acct_X",
  "before_state": "paused",
  "after_state":  "active",
  "reason":       "investigation closed, no compromise"
}
```

### `GUARDIAN_ACCOUNT_PAUSED` error body (HTTP 409)

```jsonc
{
  "code":          "GUARDIAN_ACCOUNT_PAUSED",
  "message":       "Account is paused: suspected cosigner compromise",
  "paused_at":     "2026-05-19T14:23:00Z",
  "paused_reason": "suspected cosigner compromise",
  "retryable":     false
}
```

### Extended account-detail (HTTP 200)

```jsonc
// GET /dashboard/accounts/{account_id} — fields ADDED to existing response
{
  // ... existing fields ...
  "paused_at":     "2026-05-19T14:23:00Z" | null,
  "paused_reason": "suspected cosigner compromise" | null
}
```

Both fields are emitted whether the account is paused or active —
clients receive `null` on active accounts rather than missing
fields, so deserialization is uniform.

## TypeScript types

```ts
// packages/guardian-operator-client/src/server-types.ts

export interface OperatorAccountDetail {
  // ... existing fields ...
  pausedAt: string | null;       // RFC 3339 UTC; null when active
  pausedReason: string | null;   // present iff pausedAt is non-null
}

export interface PauseAccountResponse {
  accountId: string;
  beforeState: "active" | "paused";
  afterState:  "active" | "paused";
  pausedAt: string;
  pausedReason: string;
}

export interface UnpauseAccountResponse {
  accountId: string;
  beforeState: "active" | "paused";
  afterState:  "active" | "paused";
  reason: string | null;
}

// Extends the existing operator-error code union
export type OperatorErrorCode =
  | "GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION"
  | "GUARDIAN_ACCOUNT_PAUSED"
  // ... existing codes ...
  ;

export interface AccountPausedErrorDetails {
  pausedAt: string;
  pausedReason: string | null;
}
```

Field naming follows the operator client's existing camelCase
convention; the Rust HTTP envelope uses snake_case (`paused_at`,
`paused_reason`). The deserializer maps between the two — same
pattern the existing `OperatorAccountDetail` fields use.

## State machine

```
                ┌────────────────────┐
                │       Active       │ ◄────────────┐
                │ paused_at == NULL  │              │
                └────────────────────┘              │
                          │                         │
        POST /pause       │                         │ POST /unpause
        { reason: "..." } │                         │ { reason?: "..." }
                          ▼                         │
                ┌────────────────────┐              │
                │       Paused       │ ─────────────┘
                │ paused_at = <ts>   │
                │ paused_reason = "" │
                └────────────────────┘
                          │
                          │  POST /pause (idempotent retry)
                          │  → no state change, audit row,
                          │     original paused_at preserved
                          ▼
                       (self)
```

Two states, two operator-driven transitions, two idempotent
self-loops. Both transitions are audited; both self-loops are
audited (`before_state == after_state`).

## Cross-references

- Spec FR-001 / FR-002: pause/unpause endpoints (response shapes above).
- Spec FR-003 / FR-004: authz + session preconditions (unchanged; existing middleware).
- Spec FR-005: account-detail extension (response shape above).
- Spec FR-007: `reason` validation (≤ 512 UTF-8; required on pause; optional on unpause).
- Spec FR-008–FR-012: chokepoint + error code (variant + mappings above). FR-008 covers the multisig pipeline (`services::push_delta`, `push_delta_proposal`, `sign_delta_proposal`) and the EVM pipeline (`evm::service::create_proposal`, `approve_proposal`, `cancel_proposal`) when the EVM Cargo feature is enabled. Admin/setup paths (`configure_account`, `register_account`) are out of scope per Non-Goals.
- Spec FR-013 / FR-014: idempotency table above.
- Spec FR-015: durability via DB columns (migration above).
- Spec FR-017: read-after-write — the `set_pause` UPDATE commits before the response returns.
- Spec FR-018 / FR-019: audit-row coverage (payload + idempotency tables above).
- Spec FR-021: audit-writer fallback to log path (existing `Auditor` trait behavior — see `crates/server/src/audit/log.rs`).
- Spec FR-022–FR-024: TypeScript client types above.
- Spec FR-025 / FR-026: chokepoint single-call-site invariant + forward-swap shape — encoded in research Decision 1 and Decision 5.
