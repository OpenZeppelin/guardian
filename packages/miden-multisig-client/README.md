# @openzeppelin/miden-multisig-client

TypeScript SDK for private multisignature workflows on Miden. This package wraps the on-chain multisig contracts plus Private State Manager (PSM) coordination so you can:

- Create multisig accounts, register them with a PSM, and keep state off-chain
- Propose, sign, and execute transactions with threshold enforcement
- Export/import proposals as files for sharing using side channels

## How Private Multisigs & PSM Work

Miden multisig accounts store their authentication logic on-chain, but **their state (signers, metadata, proposals)** is kept private. PSM acts as a coordination server:

1. A proposer pushes a delta (transaction plan) to Private State Manager (PSM). PSM tracks who signed and emits an ack signature once the threshold is met.
2. Cosigners fetch pending deltas, verify details locally, sign the transaction summary, and push signatures back to PSM.
3. Once ready, any cosigner builds the final transaction using all cosigner signatures + the PSM ack, executes it on-chain.

## Installation

```bash
npm install @openzeppelin/miden-multisig-client @demox-labs/miden-sdk
```

## Setup

```typescript
import { MultisigClient, FalconSigner } from '@openzeppelin/miden-multisig-client';
import { WebClient, SecretKey } from '@demox-labs/miden-sdk';

// Initialize Miden WebClient
const webClient = await WebClient.createClient('https://rpc.testnet.miden.io:443');

// Create a signer from your secret key
const secretKey = SecretKey.rpoFalconWithRNG(seed);
const signer = new FalconSigner(secretKey);

// Create MultisigClient
const client = new MultisigClient(webClient, {
  psmEndpoint: 'http://localhost:3000',
});
```

## Usage

### Get PSM Public Key

Before creating a multisig, get the PSM server's public key commitment:

```typescript
const psmCommitment = await client.psmClient.getPubkey();
```

### Create a Multisig Account

```typescript
const config = {
  threshold: 2, // Require 2 signatures
  signerCommitments: [
    signer.commitment,      // Your commitment
    otherSigner.commitment, // Cosigner's commitment
  ],
  psmCommitment,
};

const multisig = await client.create(config, signer);
console.log('Account ID:', multisig.accountId);
```

### Register on PSM

After creating the account, register it on the PSM server:

```typescript
await multisig.registerOnPsm();
```

### Load an Existing Multisig

The configuration is automatically detected from the account's on-chain storage:

```typescript
const multisig = await client.load(accountId, signer);
```

### Fetch Account State

```typescript
const state = await multisig.fetchState();
console.log('Commitment:', state.commitment);
console.log('Created:', state.createdAt);
```

### Create a Proposal (Add Signer)

```typescript
// Create a proposal to add a new signer
const nonce = Math.floor(Math.random() * 1_000_000_000);
const proposal = await multisig.createAddSignerProposal(
  webClient,
  newSignerCommitment, // Commitment of signer to add
  nonce,               // Optional nonce (random value)
  3,                   // Optional new threshold
);
console.log('Proposal ID:', proposal.id);
```

### Sign a Proposal

```typescript
const signedProposal = await multisig.signProposal(proposal.id);
console.log('Signatures:', signedProposal.signatures.length);
```

### Sync Proposals

Fetches proposals from the PSM server and updates local state:

```typescript
const proposals = await multisig.syncProposals();
for (const p of proposals) {
  console.log(`${p.id}: ${p.status.type}`);
}
```

### Check Proposal Status

Returns cached proposals without making a network request:

```typescript
const proposals = multisig.listProposals();
for (const p of proposals) {
  if (p.status.type === 'pending') {
    console.log(`Pending: ${p.status.signaturesCollected}/${p.status.signaturesRequired}`);
  } else if (p.status.type === 'ready') {
    console.log('Ready to execute!');
  }
}
```

### Execute a Proposal

When a proposal has enough signatures:

```typescript
if (proposal.status.type === 'ready') {
  await multisig.executeProposal(proposal.id, webClient);
  console.log('Transaction executed on-chain!');
}
```

### Export Proposal for Offline Signing

```typescript
const exported = await multisig.exportProposal(proposal.id);
// Send `exported` to offline signer
console.log('TX Summary:', exported.txSummaryBase64);
console.log('Commitment to sign:', exported.commitment);
```

## Transaction Utilities

The package also exports utility functions for building transactions:

```typescript
import {
  normalizeHexWord,
  hexToUint8Array,
  signatureHexToBytes,
  buildSignatureAdviceEntry,
} from '@openzeppelin/miden-multisig-client';

// Normalize hex for Word.fromHex (pads to 64 chars)
const normalized = normalizeHexWord('abc123');
// => '0x0000...abc123'

// Convert hex to bytes
const bytes = hexToUint8Array('deadbeef');
// => Uint8Array([0xde, 0xad, 0xbe, 0xef])

// Add auth scheme prefix to signature
const sigBytes = signatureHexToBytes(signatureHex);
// => Uint8Array with 0x00 prefix (RpoFalcon512)
```

## Testing

```bash
npm test           # Run tests once
npm run test:watch # Run tests in watch mode
```
