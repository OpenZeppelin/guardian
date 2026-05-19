# Quickstart: Operator-Initiated Per-Account Pause

This walks the happy path for the pause feature once the
implementation lands. All requests carry the
`guardian_operator_session` cookie established by the
`002-operator-auth` login flow.

## 0. Prerequisites

- Running `guardian-server` (Postgres or filesystem backend).
- Run the new migration before starting the server:
  `2026-05-19-000001_account_pause_fields`. Adds `paused_at` and
  `paused_reason` nullable columns to `account_metadata` plus a
  partial index. Backfill is trivial (NULL = active) — no data
  migration step.
- A signed-in operator session — see `002-operator-auth`
  quickstart.
- The operator entry in the allowlist JSON must hold the
  `accounts:pause` permission. Example structured entry:
  ```json
  {
    "public_key": "<hex>",
    "permissions": ["dashboard:read", "accounts:pause"]
  }
  ```
  Legacy bare-hex entries grant `dashboard:read` only (per
  `006-operator-authz`) and cannot pause; promote the entry to a
  structured object first.
- For the gRPC enforcement sub-walks, a multisig SDK or `grpcurl`
  pointed at the same `guardian-server` instance.

## 1. Confirm the account is active

```text
GET /dashboard/accounts/acct_X
Cookie: guardian_operator_session=...
```

Expected (relevant fields):

```jsonc
{
  // ... existing fields ...
  "paused_at":     null,
  "paused_reason": null
}
```

## 2. Pause the account

```text
POST /dashboard/accounts/acct_X/pause
Content-Type: application/json
Cookie: guardian_operator_session=...

{ "reason": "suspected cosigner compromise" }
```

Expected — HTTP 200:

```jsonc
{
  "account_id":    "acct_X",
  "before_state":  "active",
  "after_state":   "paused",
  "paused_at":     "2026-05-19T14:23:00Z",
  "paused_reason": "suspected cosigner compromise"
}
```

A row appears in `admin_actions`:

```sql
SELECT action_kind, target_account_id, payload, outcome
  FROM admin_actions
 WHERE target_account_id = 'acct_X'
 ORDER BY occurred_at DESC
 LIMIT 1;
```

```text
 action_kind       | target_account_id | payload                                                                                          | outcome
-------------------+-------------------+--------------------------------------------------------------------------------------------------+---------
 accounts.pause    | acct_X            | {"before_state":"active","after_state":"paused","reason":"suspected cosigner compromise"}        | success
```

## 3. Attempt a mutating action — rejected

### Over HTTP

```text
POST /accounts/acct_X/deltas              (or whatever mutating route)
Content-Type: application/json
... signed request body ...
```

Expected — HTTP **409 Conflict**:

```jsonc
{
  "code":          "GUARDIAN_ACCOUNT_PAUSED",
  "message":       "Account is paused: suspected cosigner compromise",
  "paused_at":     "2026-05-19T14:23:00Z",
  "paused_reason": "suspected cosigner compromise",
  "retryable":     false
}
```

### Over gRPC

A gRPC mutating call (`push_delta`, `push_delta_proposal`,
`sign_delta_proposal`) against `acct_X` returns:

```text
Status: FAILED_PRECONDITION
Message: "Account is paused: suspected cosigner compromise"
Details:
  - paused_at:     "2026-05-19T14:23:00Z"
  - paused_reason: "suspected cosigner compromise"
```

No state change is persisted on either transport.

## 4. Idempotent re-pause preserves the original timestamp

```text
POST /dashboard/accounts/acct_X/pause
{ "reason": "second-opinion confirms compromise" }
```

Expected — HTTP 200, but `paused_at` is the **first** pause's
timestamp:

```jsonc
{
  "account_id":    "acct_X",
  "before_state":  "paused",
  "after_state":   "paused",
  "paused_at":     "2026-05-19T14:23:00Z",   // unchanged
  "paused_reason": "suspected cosigner compromise"  // unchanged
}
```

A second row appears in `admin_actions`:

```text
 action_kind     | target_account_id | payload                                                                                    | outcome
-----------------+-------------------+--------------------------------------------------------------------------------------------+---------
 accounts.pause  | acct_X            | {"before_state":"paused","after_state":"paused","reason":"second-opinion confirms compromise"} | success
```

Forensic timestamp is preserved; the retry is still attributable
via the audit log.

## 5. Read endpoints still work

```text
GET /dashboard/accounts/acct_X
GET /dashboard/accounts/acct_X/deltas
GET /dashboard/accounts/acct_X/proposals
GET /dashboard/info
```

All return their normal responses with no pause-specific gating
on the read side. The account-detail response surfaces the
current `paused_at` and `paused_reason`:

```jsonc
{
  // ... existing fields ...
  "paused_at":     "2026-05-19T14:23:00Z",
  "paused_reason": "suspected cosigner compromise"
}
```

## 6. Unpause once the incident is resolved

```text
POST /dashboard/accounts/acct_X/unpause
Content-Type: application/json

{ "reason": "investigation closed, no compromise" }
```

Expected — HTTP 200:

```jsonc
{
  "account_id":   "acct_X",
  "before_state": "paused",
  "after_state":  "active",
  "reason":       "investigation closed, no compromise"
}
```

A third row appears in `admin_actions`:

```text
 action_kind       | target_account_id | payload                                                                                                | outcome
-------------------+-------------------+--------------------------------------------------------------------------------------------------------+---------
 accounts.unpause  | acct_X            | {"before_state":"paused","after_state":"active","reason":"investigation closed, no compromise"}        | success
```

The previously blocked mutating call now succeeds on retry.

## 7. Permission gate works

If an operator session without `accounts:pause` attempts step 2:

```text
POST /dashboard/accounts/acct_X/pause
{ "reason": "..." }
```

Returns — HTTP **403 Forbidden**:

```jsonc
{
  "code":                "GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION",
  "message":             "Operator missing required permissions: accounts:pause",
  "missing_permissions": ["accounts:pause"],
  "retryable":           false
}
```

The pause handler never runs; no `accounts.pause` audit row is
written (the authz middleware emits its own `auth.denied` row
instead, per `006-operator-authz` FR-025).

## 8. Restart preserves pause (SC-005)

```bash
# Stop the server. State on disk: paused_at = 2026-05-19T14:23:00Z.
# Start the server.
```

```text
GET /dashboard/accounts/acct_X
```

Still reports `paused_at` and `paused_reason` from before the
restart. A mutating attempt still returns
`GUARDIAN_ACCOUNT_PAUSED` with the original timestamp. No
operator action was required.

## TypeScript client (operator dashboard)

```ts
import { GuardianOperatorClient } from "@openzeppelin/guardian-operator-client";

const client = new GuardianOperatorClient({ baseUrl, fetch });

// Pause
const pause = await client.pauseAccount("acct_X", "suspected cosigner compromise");
console.log(pause.afterState);  // "paused"
console.log(pause.pausedAt);    // "2026-05-19T14:23:00Z"

// Mutating call against a paused account
try {
  await client.somethingMutating("acct_X", ...);
} catch (err) {
  if (err.code === "GUARDIAN_ACCOUNT_PAUSED") {
    // Typed access to details — no string parsing needed
    showBanner(`Paused at ${err.details.pausedAt}: ${err.details.pausedReason}`);
  }
}

// Unpause
const unpause = await client.unpauseAccount("acct_X", "investigation closed");
console.log(unpause.afterState);  // "active"
```

## End-to-end smoke

Once the operator-client wrappers land, the existing
`smoke-test-operator-dashboard` skill can drive steps 1–7
through the published TypeScript surface. Step 8 (restart)
requires a controlled `guardian-server` restart and is covered
by the `account_pause_endpoint.rs` integration test under the
`integration` cargo feature.
