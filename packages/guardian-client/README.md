# @openzeppelin/guardian-client

TypeScript HTTP client for Guardian server.

## Installation

```bash
npm install @openzeppelin/guardian-client
```

## Setup

```typescript
import { GuardianHttpClient } from '@openzeppelin/guardian-client';

const client = new GuardianHttpClient('http://localhost:3000');
```

## Usage

### Get Server Public Key (Unauthenticated)

```typescript
const pubkey = await client.getPubkey();
console.log('GUARDIAN pubkey:', pubkey);
```

### Set Signer for Authenticated Requests

All endpoints except `getPubkey()` require authentication. You must provide a signer that implements the `Signer` interface:

```typescript
import type { Signer, RequestAuthPayload } from '@openzeppelin/guardian-client';

const signer: Signer = {
  commitment: '0x...', // 64 hex chars
  publicKey: '0x...',  // Full public key hex
  // Sign account ID + timestamp + request payload digest
  signRequest: (accountId: string, timestamp: number, requestPayload: RequestAuthPayload) => {
    // requestPayload is canonicalized by the client before this call
    // implement your signing logic here
    return '0x...';
  },
  signCommitment: (commitmentHex: string) => '0x...', // Returns signature hex
};

client.setSigner(signer);
```

### Configure an Account

```typescript
await client.configure({
  account_id: '0x...',
  auth: {
    MidenFalconRpo: {
      cosigner_commitments: ['0x...', '0x...'],
    },
  },
  initial_state: { data: '<base64-encoded-account>', account_id: '0x...' },
});
```

### Get Account State

```typescript
const state = await client.getState(accountId);
console.log('Commitment:', state.commitment);
console.log('State data:', state.state_json.data);
```

### Look Up An Account By Key Commitment

When a wallet only holds a signing key, it cannot derive the account ID
directly. The Guardian server exposes `GET /state/lookup` so the wallet
can ask "which account(s) authorize this commitment?" and proceed with
the existing recovery flow.

The signer used here MUST implement `signLookupMessage`, which signs the
domain-separated `LookupAuthMessage::to_word(timestampMs, keyCommitment)`
digest. The canonical implementation lives in
`@openzeppelin/miden-multisig-client` (which has access to the Miden SDK's
RPO256); this package keeps the digest computation out of its zero-dependency
surface.

```typescript
const result = await client.lookupAccountByKeyCommitment(keyCommitmentHex);

if (result.accounts.length === 0) {
  console.log('No account authorizes this commitment with this operator.');
} else {
  for (const { accountId } of result.accounts) {
    console.log('Recovered account:', accountId);
    // Continue with the existing /state flow:
    const state = await client.getState(accountId);
    // ... register a new key via the existing delta/proposal flow.
  }
}
```

For a higher-level helper that composes lookup + state fetch, see
`recoverByKey` in `@openzeppelin/miden-multisig-client`.

### Work with Delta Proposals

```typescript
// Get all proposals for an account
const proposals = await client.getDeltaProposals(accountId);

// Get one proposal by commitment
const proposal = await client.getDeltaProposal(accountId, '0x...');

// Push a new proposal
const response = await client.pushDeltaProposal({
  account_id: accountId,
  nonce: 1,
  delta_payload: {
    tx_summary: { data: '<base64-tx-summary>' },
    signatures: [],
  },
});

// Sign a proposal
const delta = await client.signDeltaProposal({
  account_id: accountId,
  commitment: response.commitment,
  signature: { scheme: 'falcon', signature: '0x...' },
});

// Execute a proposal
const result = await client.pushDelta({
  account_id: accountId,
  nonce: 1,
  prev_commitment: '0x...',
  delta_payload: { data: '<base64-tx-summary>' },
  status: { status: 'pending', timestamp: '...', proposer_id: '0x...', cosigner_sigs: [] },
});
```

### Get Deltas

```typescript
// Get specific delta by nonce
const delta = await client.getDelta(accountId, 5);

// Get merged delta since a nonce
const merged = await client.getDeltaSince(accountId, 3);
```

## Error Handling

The client throws `GuardianHttpError` for non-2xx responses:

```typescript
import { GuardianHttpError } from '@openzeppelin/guardian-client';

try {
  await client.getState(accountId);
} catch (error) {
  if (error instanceof GuardianHttpError) {
    console.error(`HTTP ${error.status}: ${error.statusText}`);
    console.error('Body:', error.body);
  }
}
```

## Testing

```bash
npm test           # Run tests once
npm run test:watch # Run tests in watch mode
```
