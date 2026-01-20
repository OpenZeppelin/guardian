# @openzeppelin/psm-client

TypeScript HTTP client for Private State Manager (PSM) server.

## Installation

```bash
npm install @openzeppelin/psm-client
```

## Setup

```typescript
import { PsmHttpClient } from '@openzeppelin/psm-client';

const client = new PsmHttpClient('http://localhost:3000');
```

## Usage

### Get Server Public Key (Unauthenticated)

```typescript
const pubkey = await client.getPubkey();
console.log('PSM pubkey:', pubkey);
```

### Set Signer for Authenticated Requests

All endpoints except `getPubkey()` require authentication. You must provide a signer that implements the `Signer` interface:

```typescript
import type { Signer } from '@openzeppelin/psm-client';

const signer: Signer = {
  commitment: '0x...', // 64 hex chars
  publicKey: '0x...',  // Full public key hex
  signAccountId: (accountId: string) => '0x...', // Returns signature hex
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

### Work with Delta Proposals

```typescript
// Get all proposals for an account
const proposals = await client.getDeltaProposals(accountId);

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

The client throws `PsmHttpError` for non-2xx responses:

```typescript
import { PsmHttpError } from '@openzeppelin/psm-client';

try {
  await client.getState(accountId);
} catch (error) {
  if (error instanceof PsmHttpError) {
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
