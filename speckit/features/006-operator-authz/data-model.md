# Data Model: Operator Authorization Foundation

**Feature Key**: `006-operator-authz` | **Date**: 2026-05-15

This document specifies the persistence and in-memory data shapes
introduced or extended by this feature. Read alongside `spec.md`
(authority on semantics, including §Design decisions).

## Persistence Changes

### New table: `admin_actions` (Postgres only)

Migration: `crates/server/migrations/2026-05-16-000001_admin_actions/`

```sql
CREATE TABLE admin_actions (
    id                  BIGSERIAL PRIMARY KEY,
    occurred_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    operator_identity   TEXT NOT NULL,
    action_kind         TEXT NOT NULL,
    target_account_id   TEXT NULL,
    payload             JSONB NOT NULL DEFAULT '{}'::jsonb,
    outcome             TEXT NOT NULL CHECK (outcome IN ('success','denied')),
    error_code          TEXT NULL,
    client_ip           TEXT NULL
);
CREATE INDEX admin_actions_operator_idx
    ON admin_actions (operator_identity, occurred_at DESC);
CREATE INDEX admin_actions_recent_idx
    ON admin_actions (occurred_at DESC);

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

**Validation rules**:

- `occurred_at`: assigned by Postgres `DEFAULT now()`; the application
  layer does not pass a value (FR-022).
- `operator_identity`: TEXT hex string matching the `commitment` field
  on the existing operator records (FR-023).
- `action_kind`: TEXT from the controlled vocabulary in
  `crates/server/src/audit/kinds.rs`. v1: `auth.denied`,
  `probe.access`. Not enforced at the DB layer; the writer
  surface restricts the values.
- `target_account_id`: nullable TEXT. Populated when the action
  targets a specific account (future #181 `accounts.pause` rows).
  Null for `auth.denied` and `probe.access` since they are
  non-account-specific.
- `payload`: JSONB. v1 schema per `action_kind`:
  - `auth.denied`: `{ "route_path": "...", "http_method": "POST",
    "required_permissions": ["..."] }` (FR-025).
  - `probe.access`: same shape as `auth.denied` so success and
    denied rows for the same route carry symmetric forensic
    context — downstream queries don't have to branch on
    `outcome` to find route + required permissions.
  - Other kinds: defined by consumer features. Convention is that
    success and denial rows for the same `action_kind` carry the
    same payload shape; consumers SHOULD follow this for any new
    mutating endpoint.

  `route_path` is the full pre-nest request path
  (`/dashboard/_authz_probe`, not `/_authz_probe`) — the audit
  emission pulls `axum::extract::OriginalUri` so the recorded
  value matches what an incident responder would `curl`.
- `outcome`: TEXT, `success` or `denied`. CHECK constraint pins
  domain.
- `error_code`: nullable TEXT. Populated when `outcome = denied`
  (`GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` in v1).
- `client_ip`: nullable TEXT. Originating client IP at the audit
  boundary, populated via the shared
  `crate::middleware::client_ip::extract_client_ip` helper
  (precedence: `X-Forwarded-For` first parseable → `X-Real-IP` →
  axum `ConnectInfo`). NULL when no request context is available
  (synthetic callers in fault-injection tests, future
  service-to-service paths).

**Append-only enforcement**: the `admin_actions_no_update` trigger
fires on UPDATE or DELETE and raises an exception. Retention work
in a future feature must drop the trigger as part of its own
migration (Decision 2).

**Indexes**:

- `admin_actions_operator_idx (operator_identity, occurred_at DESC)`
  — primary lookup pattern for forensic queries ("show me what
  operator X did").
- `admin_actions_recent_idx (occurred_at DESC)` — secondary pattern
  for "show me recent admin actions across the deployment".

### Diesel schema additions

In `crates/server/src/schema.rs`:

```rust
diesel::table! {
    admin_actions (id) {
        id -> Int8,
        occurred_at -> Timestamptz,
        operator_identity -> Text,
        action_kind -> Text,
        target_account_id -> Nullable<Text>,
        payload -> Jsonb,
        outcome -> Text,
        error_code -> Nullable<Text>,
        client_ip -> Nullable<Text>,
    }
}
```

Two struct families:

```rust
#[derive(Queryable, Selectable)]
#[diesel(table_name = admin_actions)]
pub struct AdminActionRow {
    pub id: i64,
    pub occurred_at: DateTime<Utc>,
    pub operator_identity: String,
    pub action_kind: String,
    pub target_account_id: Option<String>,
    pub payload: serde_json::Value,
    pub outcome: String,
    pub error_code: Option<String>,
    pub client_ip: Option<String>,
}

#[derive(Insertable)]
#[diesel(table_name = admin_actions)]
pub struct NewAdminAction<'a> {
    pub operator_identity: &'a str,
    pub action_kind: &'a str,
    pub target_account_id: Option<&'a str>,
    pub payload: &'a serde_json::Value,
    pub outcome: &'a str,
    pub error_code: Option<&'a str>,
    pub client_ip: Option<&'a str>,
}
```

`id` and `occurred_at` are DB-assigned; `NewAdminAction` does not
carry them.

## In-Memory Models

### Permission vocabulary (new)

Module: `crates/server/src/dashboard/permissions.rs`

```rust
#[derive(Copy, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    DashboardRead,
    AccountsPause,
    PoliciesWrite,
}

pub const DASHBOARD_READ: &str = "dashboard:read";
pub const ACCOUNTS_PAUSE: &str = "accounts:pause";
pub const POLICIES_WRITE: &str = "policies:write";

impl Permission {
    pub fn as_str(&self) -> &'static str { /* matches the const */ }
}

impl std::str::FromStr for Permission {
    type Err = UnknownPermission;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            DASHBOARD_READ => Ok(Self::DashboardRead),
            ACCOUNTS_PAUSE => Ok(Self::AccountsPause),
            POLICIES_WRITE => Ok(Self::PoliciesWrite),
            other => Err(UnknownPermission(other.to_owned())),
        }
    }
}
```

`FromStr` matches case-sensitively (`Accounts:Pause` rejected,
FR-005) and rejects leading/trailing whitespace (matched literally,
no `.trim()` upstream).

### `AllowlistEntryWire` (new, transient)

Module: `crates/server/src/dashboard/allowlist.rs` (existing)

```rust
#[derive(Deserialize)]
#[serde(untagged)]
enum AllowlistEntryWire {
    LegacyHex(String),
    Structured {
        public_key: String,
        permissions: Vec<String>,
    },
}
```

This is a wire-only type; the parser maps each variant into
`OperatorAllowlistEntry` (below) and drops the wire type. Mixed
arrays are explicitly permitted (FR-001).

### `OperatorAllowlistEntry` (extended, in-memory)

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OperatorAllowlistEntry {
    pub operator_id: String,
    pub commitment: String,
    pub effective_permissions: BTreeSet<Permission>,
}
```

Mapping rules:

- `AllowlistEntryWire::LegacyHex(hex)` →
  `OperatorAllowlistEntry { operator_id: hex.clone(), commitment:
  hex, effective_permissions: { DashboardRead } }` (FR-002).
- `AllowlistEntryWire::Structured { public_key, permissions }`:
  - parse each `permissions` string via `Permission::FromStr`;
    propagate any `UnknownPermission` as a load-time error (FR-004).
  - deduplicate via `BTreeSet::from_iter`.
  - if the resulting set is empty, the entry is still loaded but
    every authorization check will deny it (FR-003 explicit-empty).
- Across the array: a `HashSet<String>` of `commitment` values
  detects duplicates; the second occurrence aborts the load
  (FR-007).

### `AuthenticatedOperator` (extended)

Module: `crates/server/src/dashboard/types.rs:6-10`

```rust
#[derive(Clone, Debug)]
pub struct AuthenticatedOperator {
    pub operator_id: String,
    pub commitment: String,
    pub effective_permissions: Arc<BTreeSet<Permission>>,
}
```

- `operator_id` and `commitment`: unchanged from today.
- `effective_permissions`: populated by
  `state.rs::authenticate_session` from the **live** allowlist
  snapshot at each request, not from the session record (FR-008).
- `Arc<BTreeSet<...>>`: cheap clone into request extensions and
  audit-event payloads.

## Wire Shapes

### Allowlist JSON (extended, applies to all three sources)

The JSON document accepted by `OperatorAllowlist::from_json` (and
therefore by `GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON`,
`GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE`, and
`GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ID`):

```jsonc
[
  // Legacy element: read-only
  "0x094f145ec43583db3ca443f43a67545c...",

  // Structured element: explicit permissions
  {
    "public_key": "0x0944089753530e9104cc9fc4...",
    "permissions": ["dashboard:read", "accounts:pause"]
  },

  // Explicit-deny element: loads but denies everywhere
  {
    "public_key": "0x0a11...",
    "permissions": []
  }
]
```

Validation:

- Mixed shapes allowed (FR-001).
- Bare hex string → `{dashboard:read}` (FR-002).
- Object missing `permissions` → load error (Edge Case 4).
- Empty `permissions` array → no permissions (FR-003).
- Unknown permission string → load error (FR-004).
- Case-sensitive matching, no whitespace trimming (FR-005).
- Duplicate `public_key` across entries → load error (FR-007).

A JSON Schema for this shape lives in `contracts/allowlist.schema.json`.

### Error envelope (extended additively)

Existing `ErrorResponse` in `crates/server/src/error.rs` (today):

```jsonc
{
  "success": false,
  "code": "string",
  "error": "string",
  "retry_after_secs": 0  // optional
}
```

After this feature (additive — only the new code populates the new
fields):

```jsonc
{
  "success": false,
  "code": "string",
  "error": "string",
  "retry_after_secs": 0,                  // optional, existing
  "missing_permissions": ["dashboard:read"], // optional, new
  "retryable": false                          // optional, new
}
```

For `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` (FR-016):

```json
{
  "success": false,
  "code": "GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION",
  "error": "Operator lacks required permissions: accounts:pause",
  "missing_permissions": ["accounts:pause"],
  "retryable": false
}
```

`missing_permissions` is sorted lexicographic ASCII (FR-017).

### Audit event (Postgres row + log line, same shape)

The Postgres row shape is the table schema above. The log line
emits the same fields under `target = "audit.admin_action"`:

```text
WARN audit.admin_action
  occurred_at=2026-05-15T16:00:00Z
  operator_identity=0x094f...
  action_kind=auth.denied
  target_account_id=null
  payload={"route_path":"/dashboard/_authz_probe","http_method":"POST","required_permissions":["accounts:pause"]}
  outcome=denied
  error_code=GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION
  client_ip=203.0.113.5
```

The log line is the **same event** as the row, not a duplicate; on
Postgres-backed deployments only the row exists, on filesystem-only
deployments only the log line exists, and on Postgres-write-failure
the log line stands in for the missing row (FR-027).

## Lifecycle and Read-Path Rules

- **`admin_actions` is append-only.** No code path produces UPDATE
  or DELETE; the trigger enforces. Retention work (deferred) must
  drop the trigger in its own migration.
- **Allowlist snapshot is read-mostly.** The existing
  `RwLock<OperatorAllowlist>` (`dashboard/state.rs`) provides
  atomic reads of the entire permission set within one
  authentication call (Edge Case 9).
- **Session principals are reissued per request.** Every
  authenticated request re-derives `effective_permissions` from the
  live allowlist snapshot (FR-008). The session record itself
  carries identity only; permissions are not cached in the session.
- **`Auditor::record` is fire-and-forget from the caller's
  perspective.** A denial response is returned regardless of the
  audit write outcome (FR-027). The Postgres writer falls through
  to the log path on transient failure so the event is never
  silently lost.

## Constants and Glossary

- **Permission**: one of `dashboard:read`, `accounts:pause`,
  `policies:write` (v1).
- **`operator_id`**: TEXT identity tied to an allowlist entry;
  today populated as the commitment hex.
- **`commitment`**: TEXT hex of the Falcon public key commitment.
- **`action_kind`**: TEXT from
  `crates/server/src/audit/kinds.rs`. v1: `auth.denied`,
  `probe.access`. Future PRs add their own.
- **`outcome`**: TEXT, `success` or `denied`. CHECK-constrained.
- **`error_code`**: stable Guardian error string, e.g.
  `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`. NULL on success.
