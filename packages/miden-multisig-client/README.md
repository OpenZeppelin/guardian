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

// Create MultisigClient. Both endpoints are required; construction throws
// when either is omitted. midenRpcEndpoint must point at the same network as
// the injected MidenClient.
const client = new MultisigClient(midenClient, {
  guardianEndpoint: 'http://localhost:3000',
  midenRpcEndpoint: 'https://rpc.devnet.miden.io',
  prover: {
    url: 'https://prover.example',
    retry: { maxAttempts: 4 },
  },
  rpc: {
    retry: { maxAttempts: 3 },
  },
});
```

The nested `prover` configuration is optional. Without it, the injected Miden
client's prover is preserved. By default, cloneable remote provers get two total
attempts; endpoint-less injected provers, including local and callback provers,
run once. A custom URL must be absolute HTTP(S), overrides the injected prover,
and never falls back to a default endpoint. Retries apply only to transient
proof failures; transaction execution, submission, local state application, and
GUARDIAN calls are not retried.

An optional `rpc` configuration (`rpc: { retry: { maxAttempts: 3 } }`) tunes
retries for the idempotent Miden node reads this SDK issues — on-chain
account lookups, state-commitment verification, and the guardian-switch node
sync — against transient failures such as rate limiting. The default is two
total attempts (one retry); an explicit `maxAttempts` of 1 opts out. Syncs
performed directly on the injected Miden client are owned by the
application. Transaction submission is never retried under any
configuration: a submission whose outcome is unknown could execute twice if
re-sent. There is no timeout setting: the browser WASM RPC client cannot
cancel an in-flight request, so a JavaScript-side timeout would abandon a
call whose side effects may still land.

### Note transport endpoint

Private notes are relayed through a note transport service that is separate
from the node RPC. The injected Miden client owns note relay, so the endpoint
is set when constructing it rather than on `MultisigClientConfig`. The
`createDevnet` / `createTestnet` helpers wire the matching public transport
service automatically; a custom node endpoint has no derivable transport
service, so private-note relay stays disabled until `noteTransportUrl` is set
explicitly.

```typescript
const midenClient = await MidenClient.create({
  rpcUrl: 'https://my-node.internal:57291',
  noteTransportUrl: 'https://my-transport.internal',
});

const client = new MultisigClient(midenClient, {
  guardianEndpoint: 'http://localhost:3000',
  midenRpcEndpoint: 'https://my-node.internal:57291',
});
```

`noteTransportUrl` also accepts the `'devnet'` and `'testnet'` shorthands, so
a custom node on a public network can reuse that network's public transport
service.

The full cross-SDK reference, including the Rust equivalents, is
[`docs/MULTISIG_SDK.md`](https://github.com/OpenZeppelin/guardian/blob/main/docs/MULTISIG_SDK.md).

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

### Creating Proposals

Every `create*Proposal` method takes its required arguments followed by a
single optional options object (issue #387) — there are no positional
optional parameters:

```typescript
createP2idProposal(recipientId, faucetId, amount, { nonce, noteType, reclaimHeight, timelockHeight }?)
createConsumeNotesProposal(noteIds, { nonce }?)
createAddSignerProposal(commitment, { nonce, newThreshold }?)
createRemoveSignerProposal(commitment, { nonce, newThreshold }?)
createChangeThresholdProposal(threshold, { nonce }?)
createUpdateProcedureThresholdProposal(procedure, threshold, { nonce }?)
createSwitchGuardianProposal(endpoint, pubkey, { nonce }?)
createCustomProposal(requestBytes, label, { nonce }?)
```

All methods accept `nonce` (identifies the proposal; defaults to
`Date.now()`). `newThreshold` defaults to the current threshold on add and to
the min of the current threshold and the remaining signer count on remove.
The option shapes are exported as `CreateProposalOptions`,
`CreateSignerProposalOptions`, and `CreateP2idProposalOptions`.

> **Breaking change (issue #387):** these methods previously took `nonce` (and
> `newThreshold`) as positional parameters. Passing the old positional form
> now throws at runtime instead of silently applying defaults.

### Create a Proposal (Add Signer)

```typescript
// Create a proposal to add a new signer
const proposal = await multisig.createAddSignerProposal(
  newSignerCommitment, // Commitment of signer to add
  { newThreshold: 3 }, // Options: nonce, newThreshold
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

### Recover From a Dead Transaction (Abandon)

If an approved transaction died client-side after guardian approval, the
candidate keeps the account locked on GUARDIAN. Record an abandon intent
and poll for the resolution:

```typescript
const accepted = await multisig.abandonCandidate(nonce);
console.log(accepted.state); // 'pending'

// The guardian's worker confirms over a short quarantine (typically well
// under a minute) that the transaction did not land, then releases the
// account.
const status = await multisig.abandonStatus(nonce);
// 'waiting' | 'landed' | 'abandoned' | 'unexpected'
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

### Custom Proposal Types

GUARDIAN accepts any non-empty `proposalType`, so an integration can propose a
transaction the SDK does not model (an agglayer bridge note, an arbitrary dApp
transaction) under its own label. The SDK normalizes the label to lowercase
`snake_case` (trims and lowercases it, then requires `[a-z0-9_]+` — the same
shape as built-in labels like `add_signer`), so `'B2Agg'` becomes `b2agg` and
`'add signer'` / `'add-signer'` are rejected. Such a proposal buckets to
`proposalType: 'custom'` and keeps its normalized label in
`CustomProposalMetadata.rawProposalType`. It can be listed, signed, and
exported/imported through the normal flow, but the SDK cannot build its on-chain
transaction — the integration owns that recipe and submits it itself.

```typescript
import { buildP2idTransactionRequest } from '@openzeppelin/miden-multisig-client';

// Producer: build a transaction and propose it under a custom label.
// The options object accepts `noteType` (`NoteType.Public` (default) or
// `NoteType.Private`, from `@miden-sdk/miden-sdk`); a private note publishes
// only its hash on chain, so the recipient needs the note shared out-of-band.
// It also accepts `reclaimHeight`/`timelockHeight` (absolute block heights,
// issue #366); presence of either builds a P2IDE note instead of plain P2ID.
// The typed path is `createP2idProposal(recipient, faucet, amount,
// { nonce, noteType, reclaimHeight, timelockHeight })`, which persists the
// choices in signed metadata.
const { request, salt } = buildP2idTransactionRequest(senderId, recipientId, faucetId, amount);
const proposal = await multisig.createCustomProposal(request.serialize(), 'b2agg');

// Cosigners review and sign through the usual signProposal flow.

// Producer (once threshold is met): bind-check the request and fetch the
// validated advice (cosigner signatures + GUARDIAN ack). `prepareCustomExecution`
// verifies the request against the signed commitment *before* the ack request.
const advice = await multisig.prepareCustomExecution(proposal.id, request.serialize());

// The browser TransactionRequest is immutable, so rebuild from the same recipe
// (inputs + salt) with the advice, then submit.
const { request: finalRequest } = buildP2idTransactionRequest(
  senderId, recipientId, faucetId, amount, { salt, signatureAdviceMap: advice },
);
await multisig.submitTransaction(finalRequest);
```

The integration keeps only its own recipe (build inputs + salt) so it can
reproduce the exact transaction at execute time — the SDK does not store the
serialized request. The binding check guarantees the rebuilt transaction matches the
commitment the cosigners signed.

> **Rust ↔ TS parity:** both SDKs expose the same producer surface —
> `createCustomProposal` / `propose_custom_transaction`, `prepareCustomExecution` /
> `prepare_custom_execution`, and `submitTransaction` / `submit_transaction`. The
> only difference is advice injection: Rust mutates the request's advice map in
> place (`request.advice_map_mut().extend(advice)`), while the immutable browser
> `TransactionRequest` is rebuilt from the recipe with the advice.

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
Tracked by the `006-consume-notes-metadata` feature spec in the
repository.

## Testing

```bash
npm test           # Run tests once
npm run test:watch # Run tests in watch mode
```
