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

### List Accounts

```typescript
const response = await client.listAccounts();
console.log(response.totalCount);
console.log(response.accounts[0]?.accountId);
```

### Fetch One Account

```typescript
const response = await client.getAccount('0x...');
console.log(response.account.authorizedSignerIds);
```

### Logout

```typescript
await client.logout();
```

## Cookie Transport

The Guardian operator session is cookie-based. This package does not manage a
cookie jar. Configure `credentials` or a custom `fetch` implementation
appropriate for your runtime.

## Error Handling

```typescript
import {
  GuardianOperatorContractError,
  GuardianOperatorHttpError,
} from '@openzeppelin/guardian-operator-client';

try {
  await client.listAccounts();
} catch (error) {
  if (error instanceof GuardianOperatorHttpError) {
    console.error(error.status, error.data?.error);
  }

  if (error instanceof GuardianOperatorContractError) {
    console.error(error.message);
  }
}
```
