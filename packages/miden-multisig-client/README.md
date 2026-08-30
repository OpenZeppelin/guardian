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
npm install @openzeppelin/miden-multisig-client @miden-sdk/miden-sdk@0.16.0-rc.5
```

> **Why the peer version is exact**: no stable `0.16.0` is published, so a
> `0.16.x`/`^0.16.0` range resolves to nothing, and the transaction-summary
> layout and procedure roots are only byte-compatible within one pre-release
> pair. Package releases wait for upstream 0.16 to stabilize.

## Miden compatibility

This package's version and Miden's are **not** aligned. Pick the release that
matches your Miden node:

| This package | Miden protocol |
|---|---|
| 0.17.x | 0.16.x (pre-release) |
| 0.16.x | 0.15.x |
| 0.15.x | 0.15.x |

Adopting a new Miden line has twice required an irreversible reset of
Guardian-stored account data, so an upgrade is not a drop-in. Full matrix, the
breaking changes per line, and what each upgrade does to stored data:
[MIDEN_COMPATIBILITY.md](https://github.com/OpenZeppelin/guardian/blob/main/docs/MIDEN_COMPATIBILITY.md).

Renamed after `0.17.0-rc.1`, which shipped the old name: `summarySalt` is now
`summaryAuthArg`. On Miden 0.16 the word it returns is the transaction's auth
arg, which is only the salt when the request did not commit to fee conversion
info, so the old name was misleading rather than merely different. There is no
alias. Callers on `0.17.0-rc.1` or on the 0.16.x line rename at the call site.

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

### Read Signer Public-Key Commitments

Since accounts use the upstream `AuthGuardedMultisig` component, the Miden
SDK's `Account.getPublicKeyCommitments()` returns the approver commitments
natively. The `AccountInspector` accessors are the strict, layout-insulated
alternative (issue #306): they validate the complete set against the
configured signer count and throw instead of silently omitting unreadable
entries, and they shield consumers from storage-layout changes across
contract versions.

Holding only a fetched Miden SDK `Account` (e.g. a wallet or dApp):

```typescript
import { AccountInspector } from '@openzeppelin/miden-multisig-client';

const commitments = AccountInspector.getSignerPublicKeyCommitments(account);
const guardianKey = AccountInspector.getGuardianPublicKeyCommitment(account);
```

Or from a loaded `Multisig` instance (reads current store-backed state):

```typescript
const signerKeys = await multisig.getSignerPublicKeyCommitments();
```

Commitments are ordered by signer index as currently stored; indices re-pack
when signers are removed, so index 0 is the key listed first at creation (by
convention the creating client's own key) only until the first membership
change. Hot/cold roles are a consumer-side convention, not part of on-chain
state. `getSignerPublicKeyCommitments` throws rather than silently returning
a truncated list when any signer entry is absent; `getGuardianPublicKeyCommitment`
throws when the guardian entry is missing (the guarded-multisig always
includes a guardian). Both are gated on this SDK's pinned contract version
and reject accounts built from a different miden-standards release.

The `Account` passed to `AccountInspector` must come from the same copy of
`@miden-sdk/miden-sdk` that this package links. An application bundling its
own SDK copy (common in wallets) will get a descriptive error from the SDK's
instance checks; construct the account with this package's SDK instance
instead.

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

Per-procedure threshold overrides are absolute signature counts and are never
re-scaled on-chain, so growing the signer set silently lowers every override's
effective signing ratio (a 2-of-2 override becomes 2-of-n).
`createAddSignerProposal` logs a `console.warn` per affected override, and
`multisig.overridesDilutedBySignerGrowth(newNumSigners)` returns them for UIs
that want to prompt before proposing. Raise the affected overrides via an
update-procedure-threshold proposal alongside the growth.

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

### Delta History

Render the account's confirmed history after recovery. One page
per call, newest-first by nonce, with server-decoded note summaries:

```typescript
let cursor: string | undefined;
do {
  const page = await multisig.deltaHistory({ limit: 50, cursor });
  for (const entry of page.entries) {
    console.log(entry.nonce, entry.timestamp, entry.outputNotes);
  }
  cursor = page.nextCursor;
} while (cursor !== undefined);
```

Only canonical (confirmed) deltas appear — pending proposals live on
`syncProposals()` — and only transactions pushed through Guardian are
visible to it.

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
// A custom proposal owns its own recipe, so unlike `createP2idProposal` it has to
// commit the fee conversion info itself. Without it the auth arg carries none and
// `fee::pay_fee` aborts at creation on any chain that charges a fee.
const feeFaucetId = await multisig.getFeeFaucetId();
const { request, salt } = buildP2idTransactionRequest(
  senderId, recipientId, faucetId, amount,
  { feeFaucetId },
);
const proposal = await multisig.createCustomProposal(request.serialize(), 'b2agg');

// Cosigners review and sign through the usual signProposal flow.

// Producer (once threshold is met): bind-check the request and fetch the
// validated advice (cosigner signatures + GUARDIAN ack). `prepareCustomExecution`
// verifies the request against the signed commitment *before* the ack request.
const advice = await multisig.prepareCustomExecution(proposal.id, request.serialize());

// The browser TransactionRequest is immutable, so rebuild from the same recipe
// (inputs + salt) with the advice, then submit. `submitTransaction` takes the
// proposal id to execute at the proposal's anchored reference block, since the
// collected signatures only authorize the summary produced there.
// `feeFaucetId` is the value retained from creation, not a fresh read: the auth
// arg commits it, so a re-read would stop reproducing the signed summary if the
// chain's fee faucet had moved.
const { request: finalRequest } = buildP2idTransactionRequest(
  senderId, recipientId, faucetId, amount,
  { salt, signatureAdviceMap: advice, feeFaucetId },
);
await multisig.submitTransaction(proposal.id, finalRequest);
```

The integration keeps only its own recipe (build inputs and salt) so it can
reproduce the exact transaction at execute time — the SDK does not store the
serialized request. The salt feeds the auth arg, so a rebuild that used a
different one would not match. The binding check guarantees the rebuilt
transaction matches the commitment the cosigners signed.

`SignatureOptions.feeFaucetId` is available on every builder and makes the auth
arg commit to fee conversion info. **Set it.** The guarded-multisig auth
component calls `fee::pay_fee` before building the summary, so on a chain whose
`verification_base_fee` is non-zero an unset faucet produces a request that
aborts at proving with `ERR_FEE_CONVERSION_INFO_MISSING`. Pass
`getFeeFaucetId()`, which reads the faucet from the block a request built now
will be anchored to. See
[`docs/MIDEN_COMPATIBILITY.md`](https://github.com/OpenZeppelin/guardian/blob/main/docs/MIDEN_COMPATIBILITY.md).

The faucet becomes part of the recipe: keep it alongside the salt and reuse the
same value at execute time. Fee parameters are a per-block header field, so
`getFeeFaucetId()` is not cached and a second call is not a substitute for
retaining the first — by then the client may be synced to a different block.

An integration that builds its own `TransactionRequestBuilder`, rather than
going through the builders above, can reach the same wiring directly:

| Export | Use |
|--------|-----|
| `applyAuthArg(builder, salt, feeFaucetId?)` | Sets the auth arg and, when a faucet is given, the advice entry — in one call, freeing every wasm handle involved. This is what the builders above use and what an integration should normally call. **It consumes `salt`**: pass a copy if you need it afterwards. |
| `resolveAuthArg(salt, feeFaucetId?)` | Returns `{ authArg, adviceMap? }` without touching a builder, for a caller assembling the request some other way. `adviceMap` is absent on the bare path, where `authArg` *is* the salt handle rather than a copy. The caller owns what comes back. |
| `nativeConversionInfo(feeFaucetId)` | The conversion-info `Word` for a faucet, at the native 1:1 rate. |
| `feeAuthArg(conversionInfo, salt)` | The commitment `hash(CONVERSION_INFO \|\| SALT)`. |

Omitting `feeFaucetId` from either of the first two reproduces the pre-0.16 bare
auth arg, with the same consequence as omitting `SignatureOptions.feeFaucetId`.
Prefer that option where a supplied builder covers the case; these are for the
transaction shapes it does not.

**Retain the salt.** It cannot be recovered from the proposal once the request
commits fee conversion info, because the auth arg is then the commitment
`hash(CONVERSION_INFO || SALT)` rather than the salt itself, and that is not
invertible. `summaryAuthArg` reads the auth-arg word back out of a summary, but
that word is the commitment, not the salt:

```typescript
import { summaryAuthArg } from '@openzeppelin/miden-multisig-client';
import { TransactionSummary } from '@miden-sdk/miden-sdk';

const authArg = summaryAuthArg(TransactionSummary.deserialize(bytes));
```

On the Miden 0.16 pre-release line a summary no longer carries a dedicated salt
word: it binds seven user-defined elements, and the guarded-multisig auth
component zeroes the leading three and passes the auth args as the trailing
four. `summaryAuthArg` reads that convention, so prefer it over indexing
`userParams()` by hand.

> **Rust ↔ TS parity:** both SDKs expose the same producer surface —
> `createCustomProposal` / `propose_custom_transaction`, `prepareCustomExecution` /
> `prepare_custom_execution`, and `submitTransaction` / `submit_transaction`. The
> only difference is advice injection: Rust mutates the request's advice map in
> place (`request.advice_map_mut().extend(advice)`), while the immutable browser
> `TransactionRequest` is rebuilt from the recipe with the advice.
>
> Both SDKs commit fee conversion info from every typed `create*Proposal` path,
> and derive it the same way — rate 1/1 under the fee faucet named by the block
> the proposal is anchored to. That is what keeps their proposals interchangeable
> without transmitting the faucet: a rebuilder reads it from the anchor, which
> travels with the proposal and is checked against the signed summary.
> `createP2idProposal` overrides a `feeFaucetId` arriving through a structurally
> compatible options object rather than honouring it, because a caller-chosen
> faucet would produce a proposal no rebuild could reproduce.
>
> Either SDK's exported builders can still *decline* to commit — omit
> `SignatureOptions.feeFaucetId` in TypeScript, or pass `None` for the
> `Option<FeeConversionInfo>` parameter in Rust. That yields the pre-0.16 bare
> auth arg, which only executes on a zero-fee chain. The SDKs differ on the
> rebuild side instead: TypeScript's `detectAuthArgConvention` reproduces either
> convention, while Rust assumes committed. Doing so normally produces a custom
> proposal — and neither
> SDK reconstructs those, so both can list, sign and drive one to submission.
> Note that neither SDK's `executeProposal` accepts a custom proposal; submission
> goes through the custom-execution path. `prepare_custom_execution` takes the
> serialized request from the caller and only checks that it reproduces the
> signed commitment, so a Rust integration handed the request TypeScript built
> will pass that check; it just has no builder of its own that can rebuild it.
> Retain the request bytes rather than expecting to regenerate them.
>
> Registering a bare-auth-arg request under a typed metadata shape instead — via
> `createProposal` — is worse, and not merely uninteroperable: the Rust SDK's
> `list_proposals` rebuilds every typed proposal from `salt_hex`, always as
> committed, and fails for the whole account rather than skipping the one it
> cannot verify.

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

### Recovering Notes After Device Loss

Normal forward sync cannot see notes that landed behind the store's
cursors. After `load()` on a recovered account, `multisig.recoverNotes()`
runs the three recovery strategies as one flow — the private-note transport
drain, the proposal-embedded note import, and the historical public-note
backfill — and finishes with a normal sync (transport fetch, chain sync,
and GUARDIAN state sync) so imported notes are verified and ready to
consume. The flow is the one public entry point; the strategies are
internal.

```typescript
const multisig = await client.load(accountId, signer);

const report = await multisig.recoverNotes();
console.log(`recovered ${report.imported} notes`);
for (const problem of report.problems) {
  console.log(`step ${problem.step} did not run: ${problem.reason}`);
}
```

Strategies are individually selectable and the backfill's block range can be
bounded:

```typescript
// Rescan only the transport backlog and recent public history, skipping the
// proposal import and the final verifying sync.
const report = await multisig.recoverNotes({
  proposalImport: false,
  fromBlock: 1_690_000,
  syncAfter: false,
});
```

The combined `NoteRecoveryReport` carries each strategy's own report
(`transport`, `proposalImport`, `backfill`) untouched; a strategy that
cannot run at all (GUARDIAN unreachable, chain tip unresolvable, broken
local store) becomes a `RecoveryStepProblem` entry instead of aborting the
flow, and `retryable: true` means rerunning the flow — which is idempotent —
can plausibly recover more. In brief:

- **Transport drain** — rescans the full private-note transport backlog for
  every tracked tag, regardless of the stored cursor (and without ever
  regressing it). Bounded by the transport service's retention: a
  best-effort rescan, **not** a backup.
- **Proposal import** — rebuilds importable notes from the bytes v2
  consume-notes proposals embed (validated against the proposals' declared
  note ids) plus node-fetched inclusion proofs, so it works for private
  notes too. Per-note `NoteImportOutcome`s; corrupt proposals and notes are
  isolated as `invalid` outcomes instead of blocking the rest.
- **Public backfill** — tag-scoped historical scan (genesis to tip by
  default) importing discovered public notes with their proofs, never
  touching the global sync height; cost scales with matches, not range
  length. This SDK screens discoveries statically against the well-known
  P2ID/P2IDE scripts (custom-script notes are counted as
  `skippedUnscreenable`), and unscannable sub-ranges are reported in
  `uncovered` instead of failing the flow.

For the full report semantics see
[`docs/MULTISIG_SDK.md`](https://github.com/OpenZeppelin/guardian/blob/main/docs/MULTISIG_SDK.md).

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

Each error class exposes a stable `.code` string. Where the Rust SDK has
an equivalent failure the code is identical to its
`MultisigError::code()` value, so cross-SDK tests and operator dashboards
can branch on one taxonomy; the two exceptions are called out below the
table.

| Error class | `.code` | When |
|---|---|---|
| `NoteBindingMismatchError` | `consume_notes_note_binding_mismatch` | v2: `notes.length !== noteIds.length`, or `note.id().toString() !== noteIds[i]` |
| `UnsupportedMetadataVersionError` | `consume_notes_unsupported_metadata_version` | Unrecognized version (including v1 on a cut-over build) |
| `ConsumeNotesMetadataOversizeError` | `consume_notes_metadata_oversize` | v2 metadata serialization exceeds 256 KiB at creation |
| `LegacyConsumeNotesNoteMissingError` | `consume_notes_legacy_note_missing` | v1 path: local store does not contain the referenced note |
| `ProposalAuthArgUnresolvableError` | `proposal_auth_arg_unresolvable` | A proposal's signed auth arg is neither its recorded salt nor a fee-conversion commitment to it, so no reconstruction from the recorded salt succeeds. `switch_guardian` instead rebuilds from the summary's own auth arg, reproducing the signed word but not its fee preimage, so it executes only on a zero-fee chain |
| `ProposalSaltMalformedError` | `proposal_salt_malformed` | A proposal's recorded salt is not a readable 32-byte word |
| `FeeFaucetAnchorMismatchError` | `fee_faucet_anchor_mismatch` | The chain's fee faucet changed mid-build, so the faucet the auth arg committed is not the one the proposal's anchored block reports |

`fee_faucet_anchor_mismatch` is raised at creation, before the proposal is
offered for signing, and is the one error here that is transient: build the
proposal again against the now-current faucet. It carries
`committedFeeFaucetIdHex` and `anchoredFeeFaucetIdHex`, so a caller that
retries automatically can do so on the type rather than by matching the
message. It has no Rust counterpart because the Rust create path cannot hit
it: `ProposalBuilder::build_*` holds `&mut MidenSdkClient` across both the
faucet read and the anchor capture, so the borrow checker forbids the
interleaving sync that opens the window in JavaScript.

The two proposal-recovery errors above it carry no Rust code. `proposal_auth_arg_unresolvable` has no
Rust counterpart at all, because the Rust SDK has no convention detector
to fail: it derives the conversion info from the proposal's anchor and
rebuilds as committed unconditionally, so a bare typed proposal trips the
generic binding check instead. `proposal_salt_malformed` does have a Rust
behavioural equivalent — `Proposal::salt()` fails there on the same
input — but it surfaces as an uncoded `InvalidConfig`. Both are
recoverable for a
`switch_guardian`, whose salt the GUARDIAN being switched away from
serves and nothing binds — that one type falls back to the summary's own
auth arg rather than letting an outgoing GUARDIAN strand a fully signed
switch. For every other type both are fatal, and the proposal has to be
recreated.

Two limits on that fallback are worth knowing. It only helps a proposal
the client already holds: `syncProposals()` verifies everything GUARDIAN
serves and rejects the whole call on the first proposal it cannot
verify, so an outgoing GUARDIAN can still keep a switch out of a *fresh*
client's cache by serving one unverifiable proposal alongside it. That
predates fee conversion info and the Rust `list_proposals` has the same
shape, so closing it belongs to a coordinated change in both SDKs.
Second, keeping the fee advice on a committed switch is a courtesy to a
cooperative GUARDIAN, not a guarantee: serving the auth arg itself as
the salt classifies as `bare` and drops the faucet without ever reaching
the fallback. Since the guarded auth procedure began paying its own fee,
that is no longer free — a dropped faucet aborts at proving with
`ERR_FEE_CONVERSION_INFO_MISSING` on any chain that charges one. It still
fails closed rather than producing a wrong transaction, but an outgoing
GUARDIAN can use it to strand a switch on a fee-charging chain.

`ProposalSaltMalformedError.saltHex` is typed `unknown`, because GUARDIAN's
JSON response is cast rather than validated and the field arrives as
whatever was served. It is non-enumerable, so it stays out of
`util.inspect` and `JSON.stringify` output; use `.quotedSalt` for a
bounded, printable rendering.

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
