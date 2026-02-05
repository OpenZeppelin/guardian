# @openzeppelin/miden-multisig-client

TypeScript SDK for private multisignature workflows on Miden. This package wraps the on-chain multisig contracts plus Private State Manager (PSM) coordination so you can:

- Create multisig accounts, register them with a PSM, and keep state off-chain
- Propose, sign, and execute transactions with threshold enforcement
- Export/import proposals as files for sharing using side channels
- Integrate external wallets via the external signing API

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

// Create MultisigClient and fetch PSM info
const client = new MultisigClient(webClient, {
  psmEndpoint: 'http://localhost:3000',
});
const { psmCommitment } = await client.initialize();
```

## Usage

### Create a Multisig Account

```typescript
const config = {
  threshold: 2,
  signerCommitments: [signer.commitment, otherSignerCommitment],
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

### Sync Everything

Fetch proposals, state, consumable notes, and config in one call:

```typescript
const { proposals, state, notes, config } = await multisig.syncAll();
for (const p of proposals) {
  console.log(`${p.id}: ${p.status.type}`);
}
```

### List Cached Proposals

Returns cached proposals without making a network request:

```typescript
const proposals = multisig.listTransactionProposals();
for (const p of proposals) {
  if (p.status.type === 'pending') {
    console.log(`Pending: ${p.status.signaturesCollected}/${p.status.signaturesRequired}`);
  } else if (p.status.type === 'ready') {
    console.log('Ready to execute!');
  }
}
```

### Create Proposals

All create methods return `{ proposal, proposals }` — the new proposal plus an auto-synced full list:

```typescript
// Add a signer
const { proposal, proposals } = await multisig.createAddSignerProposal(
  newSignerCommitment,
  { newThreshold: 3 },
);

// Remove a signer
await multisig.createRemoveSignerProposal(signerToRemove);

// Change threshold
await multisig.createChangeThresholdProposal(3);

// Consume notes
await multisig.createConsumeNotesProposal(noteIds);

// Send payment (P2ID)
await multisig.createSendProposal(recipientId, faucetId, amount);

// Switch PSM provider
await multisig.createSwitchPsmProposal(newEndpoint, newPubkey);
```

### Sign a Proposal

```typescript
const proposals = await multisig.signTransactionProposal(proposal.commitment);
```

### Execute a Proposal

When a proposal has enough signatures:

```typescript
if (proposal.status.type === 'ready') {
  await multisig.executeTransactionProposal(proposal.commitment);
}
```

### External Signing

For wallet integrations where the signing key is external (e.g., a browser wallet):

```typescript
// Fetch proposals
const proposals = multisig.listTransactionProposals();

// Sign the commitment externally
const signature = await wallet.sign(proposals[0].commitment);

// Submit the external signature
await multisig.signTransactionProposalExternal({
  commitment: proposals[0].commitment,
  signature,
  publicKey: wallet.publicKey,
  scheme: 'ecdsa',
});
```

### Export/Import Proposals

Share proposals via side channels for offline signing:

```typescript
// Export
const json = multisig.exportTransactionProposalToJson(proposal.commitment);

// Sign offline and get updated JSON
const signedJson = multisig.signTransactionProposalOffline(proposal.commitment);

// Import
const { proposal, proposals } = multisig.importTransactionProposal(json);
```

## Testing

```bash
npm test           # Run tests once
npm run test:watch # Run tests in watch mode
```
