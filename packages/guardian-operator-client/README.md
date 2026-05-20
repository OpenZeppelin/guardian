# @openzeppelin/guardian-operator-client

TypeScript HTTP client for Guardian operator dashboard endpoints.

## Installation

```bash
npm install @openzeppelin/guardian-operator-client
```

## Setup

```typescript
import { GuardianOperatorHttpClient } from '@openzeppelin/guardian-operator-client';

const client = new GuardianOperatorHttpClient({
  baseUrl: 'http://localhost:3000',
  credentials: 'include',
});
```

## Usage

### Request A Challenge

```typescript
const challenge = await client.challenge('0x...');
console.log(challenge.challenge.signingDigest);
```

### Verify A Signed Challenge

The package does not talk to wallets or sign challenges. Callers provide the
commitment and signature.

```typescript
const verified = await client.verify({
  commitment: '0x...',
  signature: '0x...',
});

console.log(verified.operatorId);
```

### List Accounts (Paginated)

```typescript
// Default page size is 50; max is 500.
const page = await client.listAccounts({ limit: 50 });
console.log(page.items[0]?.accountId);

// Resume the next page with the cursor from the previous response.
if (page.nextCursor !== null) {
  const next = await client.listAccounts({
    limit: 50,
    cursor: page.nextCursor,
  });
  console.log(next.items.length);
}

// Filter by pause state. Omit `paused` for all accounts.
const pausedOnly = await client.listAccounts({ paused: true });
const activeOnly = await client.listAccounts({ paused: false });
```

### Fetch One Account

`getAccount()` returns the bare `DashboardAccountDetail` — there is no
`{ success, account }` wrapper. Read fields directly on the response.

```typescript
const detail = await client.getAccount('0x...');
console.log(detail.authorizedSignerIds);
console.log(detail.currentCommitment, detail.stateUpdatedAt);
```

### Inventory And Health Snapshot

```typescript
const info = await client.getDashboardInfo();
console.log(info.totalAccountCount, info.environment);
console.log(info.deltaStatusCounts.candidate);
console.log(info.inFlightProposalCount);
if (info.serviceStatus === 'degraded') {
  console.warn('degraded aggregates:', info.degradedAggregates);
}
```

### Per-Account Delta Feed

```typescript
const page = await client.listAccountDeltas('0x...', { limit: 50 });
for (const entry of page.items) {
  console.log(entry.nonce, entry.status, entry.statusTimestamp);
}
```

### Per-Account In-Flight Proposals

Single-key Miden accounts and EVM accounts always return an empty
proposal queue.

```typescript
const page = await client.listAccountProposals('0x...', { limit: 50 });
for (const entry of page.items) {
  console.log(
    entry.commitment,
    entry.signaturesCollected,
    '/',
    entry.signaturesRequired,
  );
}
```

### Pause / Unpause Account

Both endpoints require the `accounts:pause` permission and are
idempotent. Re-pausing an already-paused account returns success with
the original `pausedAt` and `pausedReason` preserved (first-writer
wins). `reason` is required on pause (non-empty, ≤ 512 chars) and
optional on unpause.

```typescript
const paused = await client.pauseAccount('0x...', 'compliance review');
console.log(paused.pausedAt, paused.pausedReason);

await client.unpauseAccount('0x...', 'cleared by legal');
```

While an account is paused, mutating endpoints (delta submission,
proposal creation, auth updates) reject with HTTP 409 and code
`account_paused` (normalized by the client from the server wire code
`GUARDIAN_ACCOUNT_PAUSED`). Read endpoints — including
`listAccounts({ paused: true })` and `getAccount()` — continue to work
and surface `pausedAt` / `pausedReason` on the detail response.

### Logout

```typescript
await client.logout();
```

## Pagination Shape

Every paginated dashboard endpoint returns a `PagedResult<T>` envelope:

```jsonc
{
  "items": [ /* T entries */ ],
  "next_cursor": "string | null"  // null at end of list
}
```

- `limit` defaults to `50` and is capped at `500`. A bare `?limit=`
  (present but empty) is treated as omitted; out-of-range values
  return `400 invalid_limit`.
- `cursor` is opaque, server-signed, and tied to a specific endpoint
  kind (e.g. an account-list cursor cannot be replayed on the
  deltas endpoint). Tampered or stale cursors return
  `400 invalid_cursor`. There is no client-visible TTL.
- A `cursor` may be supplied without a `limit`; the default of 50
  applies.
- The account list endpoint sorts by `(updated_at DESC, account_id
  ASC)`. Concurrent updates to `updated_at` MAY cause an account to
  be skipped or repeated across a traversal — documented expected
  behavior of cursor pagination over a mutable sort key. All other
  paginated endpoints sort by an immutable per-account `nonce` (or
  `(nonce, commitment)` for proposals) and are fully stable.

## Cookie Transport

The Guardian operator session is cookie-based. This package does not manage a
cookie jar. Configure `credentials` or a custom `fetch` implementation
appropriate for your runtime.

## Error Handling

```typescript
import {
  GuardianOperatorContractError,
  GuardianOperatorHttpError,
  isDashboardErrorCode,
} from '@openzeppelin/guardian-operator-client';

try {
  await client.listAccounts({ limit: 9999 });
} catch (error) {
  if (error instanceof GuardianOperatorHttpError) {
    // Stable machine-readable code lives on `error.data.code`.
    // Branch on it rather than on `error.status` or
    // `error.data.error` (the human message).
    const code = error.data?.code;
    if (code && isDashboardErrorCode(code)) {
      switch (code) {
        case 'invalid_limit':
        case 'invalid_cursor':
        case 'invalid_status_filter':
          // Caller bug — fix the request.
          break;
        case 'account_not_found':
          // Path-addressed account does not exist.
          break;
        case 'data_unavailable':
          // Metadata exists but storage cannot be read; retry later.
          break;
        case 'authentication_failed':
          // Operator session is missing, tampered, or expired —
          // re-issue the challenge / verify flow.
          break;
      }
    }
  }

  if (error instanceof GuardianOperatorContractError) {
    console.error(error.message);
  }
}
```

The dashboard error taxonomy (feature `005-operator-dashboard-metrics`
FR-028) is:

| HTTP | Body `code` | When |
|------|-------------|------|
| 401 | `authentication_failed` | missing / tampered / expired operator session |
| 403 | `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` | operator session is valid but lacks one or more route-required permissions (feature `006-operator-authz`) |
| 404 | `account_not_found` | path-addressed account does not exist |
| 400 | `invalid_cursor` | tampered, malformed, or stale cursor |
| 400 | `invalid_limit` | `limit` outside `[1, 500]` |
| 400 | `invalid_status_filter` | global delta feed `status` filter is unknown or malformed |
| 409 | `account_paused` | mutating call against a paused account; error details carry `pausedAt` and `pausedReason`. Wire code `GUARDIAN_ACCOUNT_PAUSED` is normalized by the client. |
| 503 | `data_unavailable` | metadata exists but underlying records cannot be read |

### Operator Authorization (feature `006-operator-authz`)

The dashboard now enforces per-operator permissions on every route. The
allowlist JSON consumed by `GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON` /
`_FILE` / `_SECRET_ID` accepts a **heterogeneous** array of two element
shapes:

```jsonc
[
  // Legacy element: bare hex string → `{dashboard:read}` only.
  "0x094f145ec43583db3ca443f43a67545c...",

  // Structured element: explicit permission set.
  {
    "public_key": "0x0944089753530e9104cc9fc4...",
    "permissions": ["dashboard:read", "accounts:pause"]
  },

  // Explicit-deny element: loads, but denies every authorization
  // check (different from omitting the operator entirely).
  {
    "public_key": "0x0a11...",
    "permissions": []
  }
]
```

Recognized permissions in v1: `dashboard:read`, `accounts:pause`,
`policies:write`.

When the server denies a request for insufficient permissions, the
response body extends the existing flat envelope additively:

```json
{
  "success": false,
  "code": "GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION",
  "error": "Operator lacks required permissions: accounts:pause",
  "missing_permissions": ["accounts:pause"],
  "retryable": false
}
```

`parseErrorBody` and the `GuardianOperatorHttpErrorData` shape both
expose `missingPermissions` and `retryable` for this code; the fields
are `undefined` for every other code.

For UI gating, call `getSession()` to read the authenticated
operator's live permission set and compare entries against the
exported wire-string constants:

```typescript
import {
  ACCOUNTS_PAUSE,
  DASHBOARD_READ,
  GuardianOperatorHttpClient,
} from '@openzeppelin/guardian-operator-client';

const client = new GuardianOperatorHttpClient('https://guardian.example');
const { permissions } = await client.getSession();

const canPause = permissions.includes(ACCOUNTS_PAUSE);
const canRead = permissions.includes(DASHBOARD_READ);
```

The session endpoint re-reads permissions from the allowlist on every
call, so allowlist edits take effect without re-login. The server is
still the source of truth on every mutating endpoint — the client
MUST NOT short-circuit a request based on its own capability check.
