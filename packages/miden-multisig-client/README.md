# @openzeppelin/miden-multisig-client

TypeScript SDK for private multisignature workflows on Miden. This package wraps the on-chain multisig contracts plus Guardian coordination so you can:

- Create multisig accounts, register them with a GUARDIAN, and keep state off-chain
- Propose, sign, and execute transactions with threshold enforcement
- Export/import proposals as files for sharing using side channels

## How Private Multisigs & GUARDIAN Work

Miden multisig accounts store their authentication logic on-chain, but **their state (signers, metadata, proposals)** is kept private. GUARDIAN acts as a coordination server:

1. A proposer pushes a delta (transaction plan) to Guardian. GUARDIAN tracks who signed and emits an ack signature once the threshold is met.
2. Cosigners fetch pending deltas, verify details locally, sign the transaction summary, and push signatures back to GUARDIAN.
3. Once ready, any cosigner builds the final transaction using all cosigner signatures + the GUARDIAN ack, executes it on-chain.

## Installation

```bash
npm install @openzeppelin/miden-multisig-client @miden-sdk/miden-sdk
```

## Setup

```typescript
import { MultisigClient, FalconSigner } from '@openzeppelin/miden-multisig-client';
import { AuthSecretKey, MidenClient } from '@miden-sdk/miden-sdk';

const midenClient = await MidenClient.createDevnet();

// Create a signer from your secret key
const secretKey = AuthSecretKey.rpoFalconWithRNG(undefined);
const signer = new FalconSigner(secretKey);

// Create MultisigClient
const client = new MultisigClient(midenClient, {
  guardianEndpoint: 'http://localhost:3000',
  midenRpcEndpoint: 'https://rpc.devnet.miden.io',
});
```

## Usage

### Get GUARDIAN Public Key

Before creating a multisig, get the GUARDIAN server's public key commitment:

```typescript
const guardianCommitment = await client.guardianClient.getPubkey();
```

### Create a Multisig Account

```typescript
const config = {
  threshold: 2, // Require 2 signatures
  signerCommitments: [
    signer.commitment,      // Your commitment
    otherSigner.commitment, // Cosigner's commitment
  ],
  guardianCommitment,
};

const multisig = await client.create(config, signer);
console.log('Account ID:', multisig.accountId);
```

### Register on GUARDIAN

After creating the account, register it on the GUARDIAN server:

```typescript
await multisig.registerOnGuardian();
```

### Load an Existing Multisig

The configuration is automatically detected from the account's on-chain storage:

```typescript
const multisig = await client.load(accountId, signer);
```

### Recover An Account By Key

When the wallet only holds a signing key from the account's authorization
set, it does not yet know the account ID. `recoverByKey` queries Guardian's
`/state/lookup` endpoint with proof-of-possession of the key, fetches state
for each matching account, and returns `(accountId, state)` pairs.

```typescript
const recovered = await client.recoverByKey(signer);

if (recovered.length === 0) {
  // No account on this Guardian operator authorizes the key.
} else {
  for (const { accountId, state } of recovered) {
    const multisig = await client.load(accountId, signer);
    // ...
  }
}
```

The `Signer` passed to `recoverByKey` MUST implement `signLookupMessage`
(the bundled `FalconSigner` and `EcdsaSigner` both do). The lookup endpoint
authenticates by proof-of-possession of the queried commitment — same key
that already authenticates per-account requests, so revealing the account ID
does not grant any new capability. See the design doc for the security
analysis.

Multiple matches are returned uniformly: a key may legitimately authorize
several accounts, and the helper surfaces all of them rather than silently
picking one. The returned list is empty (not an error) when no account
authorizes the queried commitment.

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
  newSignerCommitment, // Commitment of signer to add
  undefined,
  3,
);
console.log('Proposal ID:', proposal.id);
```

### Sign a Proposal

```typescript
const signedProposal = await multisig.signProposal(proposal.id);
console.log('Signatures:', signedProposal.signatures.length);
```

### Sync Proposals

Fetches proposals from the GUARDIAN server and updates local state:

```typescript
const proposals = await multisig.syncProposals();
for (const p of proposals) {
  console.log(`${p.id}: ${p.status}`);
}
```

### Check Proposal Status

Returns cached proposals without making a network request:

```typescript
const proposals = multisig.listProposals();
for (const p of proposals) {
  if (p.status === 'pending') {
    console.log(`Pending signatures: ${p.signatures.length}`);
  } else if (p.status === 'ready') {
    console.log('Ready to execute!');
  }
}
```

### Execute a Proposal

When a proposal has enough signatures:

```typescript
if (proposal.status === 'ready') {
  await multisig.executeProposal(proposal.id);
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

### Import and Sign a Proposal Offline

Imported proposals are now validated against their transaction summary before they are cached or
signed:

```typescript
const imported = await multisig.importProposal(jsonFromCosigner);
const signedJson = await multisig.signProposalOffline(imported.id);
console.log(signedJson);
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
// => Uint8Array with the Falcon Poseidon2 auth prefix
```

## Consume-notes metadata versions

`consume_notes` proposals come in two metadata shapes. The
`metadataVersion` field on `ConsumeNotesProposalMetadata` is the
discriminator.

- **v1 (legacy)** — `metadataVersion` absent. The proposal carries
  only `noteIds`; verification rebuilds the transaction request by
  fetching each note from the cosigner's **own local Miden store**
  (IndexedDB in the browser). If the verifier does not have the note
  locally (cursor advanced past the block, store was cleared via
  `clearMidenDatabase()`, private-note transport pruned the blob),
  verification throws `LegacyConsumeNotesNoteMissingError` and the
  cosigner cannot sign. This is the failure tracked by
  [issue #229](https://github.com/OpenZeppelin/guardian/issues/229).
- **v2 (self-contained)** — `metadataVersion: 2` plus a `notes` array
  of base64-encoded `Note.serialize()` bytes, aligned by index with
  `noteIds`. Verification rebuilds the request from the embedded notes
  alone — no `getInputNote`, no network call. Restores the same
  "rebuild from signed metadata" invariant every other proposal type
  already satisfied (and that audit finding **M-08** remediated for
  `p2id`).

`createConsumeNotesProposal` always emits v2 starting with this
release; the proposer is the one party guaranteed to hold the notes
locally. The per-proposal v2 payload is capped at
`MAX_CONSUME_NOTES_METADATA_BYTES` (256 KiB) and the size is enforced
at creation time so the failure surfaces to the proposer before any
signature collection begins.

```typescript
import {
  MAX_CONSUME_NOTES_METADATA_BYTES,
  CONSUME_NOTES_METADATA_VERSION_V2,
  ConsumeNotesMetadataOversizeError,
  LegacyConsumeNotesNoteMissingError,
  NoteBindingMismatchError,
  UnsupportedMetadataVersionError,
  isConsumeNotesV2,
} from '@openzeppelin/miden-multisig-client';
```

### Error taxonomy

Each error class exposes a stable `.code` string identical to the Rust
SDK's `MultisigError::code()` value, so cross-SDK tests and operator
dashboards can branch on one taxonomy.

| Error class | `.code` | When |
|---|---|---|
| `NoteBindingMismatchError` | `consume_notes_note_binding_mismatch` | v2: `notes.length !== noteIds.length`, or `note.id().toString() !== noteIds[i]` |
| `UnsupportedMetadataVersionError` | `consume_notes_unsupported_metadata_version` | Unrecognized version (including v1 on a cut-over build) |
| `ConsumeNotesMetadataOversizeError` | `consume_notes_metadata_oversize` | v2 metadata serialization exceeds 256 KiB at creation |
| `LegacyConsumeNotesNoteMissingError` | `consume_notes_legacy_note_missing` | v1 path: local store does not contain the referenced note |

### Cut-over policy

The `LEGACY_CONSUME_NOTES_ENABLED` build-time constant (default `true`
in this transitional release) gates whether this build accepts v1
metadata for verification. A future cut-over release will flip the
default to `false`, at which point v1 proposals are refused with
`UnsupportedMetadataVersionError(undefined)` on every code path.
Deployments should drain or re-propose any v1 `consume_notes`
proposals in flight before upgrading past the cut-over client version.
Tracked by spec
[`006-consume-notes-metadata`](../../speckit/features/006-consume-notes-metadata/spec.md).

## Testing

```bash
npm test           # Run tests once
npm run test:watch # Run tests in watch mode
```
