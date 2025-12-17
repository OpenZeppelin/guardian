# @openzeppelin/miden-multisig-client

TypeScript SDK for Miden multisig accounts with Private State Manager (PSM) integration.

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

```typescript
const multisig = await client.load(accountId, config, signer);
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
const proposal = await multisig.createAddSignerProposal(
  webClient,
  newSignerCommitment, // Commitment of signer to add
  Date.now(),          // Optional nonce
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

```typescript
const proposals = await multisig.syncProposals();
for (const p of proposals) {
  console.log(`${p.id}: ${p.status.type}`);
}
```

### Check Proposal Status

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

## API Reference

### MultisigClient

| Method | Description |
|--------|-------------|
| `psmClient` | Access the underlying PSM HTTP client |
| `create(config, signer)` | Create a new multisig account |
| `load(accountId, config, signer)` | Load an existing multisig |

### Multisig

| Property | Description |
|----------|-------------|
| `accountId` | The account ID (hex string) |
| `threshold` | Number of signatures required |
| `signerCommitments` | All signer public key commitments |
| `psmCommitment` | PSM server's public key commitment |
| `signerCommitment` | Current signer's commitment |

| Method | Description |
|--------|-------------|
| `fetchState()` | Get account state from PSM |
| `registerOnPsm(initialState?)` | Register account on PSM |
| `syncProposals()` | Sync proposals from PSM |
| `listProposals()` | List cached proposals |
| `createProposal(nonce, txSummary, metadata?)` | Create a proposal |
| `createAddSignerProposal(webClient, commitment, nonce?, threshold?)` | Create add-signer proposal |
| `signProposal(proposalId)` | Sign a proposal |
| `executeProposal(proposalId, webClient)` | Execute a ready proposal |
| `exportProposal(proposalId)` | Export for offline signing |

### FalconSigner

```typescript
const signer = new FalconSigner(secretKey);
console.log(signer.commitment); // Public key commitment
console.log(signer.publicKey);  // Full public key hex
```

## Types

Key types exported:

- `MultisigConfig` - Configuration for creating multisig
- `Proposal` - Proposal data with status and signatures
- `ProposalStatus` - Status union (pending/ready/finalized)
- `ProposalMetadata` - Metadata for execution (target config, salt)
- `ExportedProposal` - Proposal data for offline signing
- `Signer` - Re-exported from psm-client

## License

MIT
