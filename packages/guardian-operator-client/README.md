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
| 404 | `account_not_found` | path-addressed account does not exist |
| 400 | `invalid_cursor` | tampered, malformed, or stale cursor |
| 400 | `invalid_limit` | `limit` outside `[1, 500]` |
| 400 | `invalid_status_filter` | global delta feed `status` filter is unknown or malformed |
| 503 | `data_unavailable` | metadata exists but underlying records cannot be read |
