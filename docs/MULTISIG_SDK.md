# Miden Multisig SDK

An SDK for creating and managing multisignature accounts on the Miden network. Available for both **TypeScript** (web/browser) and **Rust** (native/server) environments.

> New to Guardian? Read [`docs/CONCEPTS.md`](./CONCEPTS.md) for the trust
> model and state/delta lifecycle, and
> [`docs/architecture/services.md`](./architecture/services.md) for the
> server-side surface this SDK targets.

## Table of Contents

- [Quick Start](#quick-start)
- [Core Concepts](#core-concepts)
- [TypeScript SDK Guide](#typescript-sdk-guide)
- [Rust SDK Guide](#rust-sdk-guide)
- [Use Cases](#use-cases)
- [Offline Workflow](#offline-workflow)
- [Releasing](#releasing)

---

## Quick Start

### Installation

The multisig sdk has as peer dependency on the miden-sdk, you will need to install both.

**TypeScript (npm)**
```bash
npm install @openzeppelin/miden-multisig-client @miden-sdk/miden-sdk
```

**Rust (Cargo.toml)**
```toml
[dependencies]
miden-multisig-client = "0.15.1"
miden-client = "0.15.0"
```

### 5-Minute Example

Create a 1-of-3 multisig account, propose a transfer, collect signatures, and execute.

#### TypeScript

```typescript
import { MidenClient, AuthSecretKey } from '@miden-sdk/miden-sdk';
import { MultisigClient, FalconSigner } from '@openzeppelin/miden-multisig-client';

// 1. Setup clients
const midenClient = await MidenClient.createDevnet();
const secretKey = AuthSecretKey.rpoFalconWithRNG(undefined);
const signer = new FalconSigner(secretKey);
const client = new MultisigClient(midenClient, {
  guardianEndpoint: 'http://localhost:3000',
  midenRpcEndpoint: 'https://rpc.devnet.miden.io',
});
// Both endpoints are required; construction throws when either is omitted.
// midenRpcEndpoint must point at the same network as the injected MidenClient.

// 2. Get GUARDIAN server public key
const guardianCommitment = await client.guardianClient.getPubkey();

// 3. Create 1-of-3 multisig account
const config = {
  threshold: 1,
  signerCommitments: [signer.commitment, cosigner1Commitment, cosigner2Commitment],
  guardianCommitment,
  guardianEnabled: true,
};
const multisig = await client.create(config, signer);
await multisig.registerOnGuardian();

console.log('Account created:', multisig.accountId);

// 4. Create a transfer proposal
const proposal = await multisig.createP2idProposal(
  recipientAccountId,
  faucetAccountId,
  1000n  // amount
);

console.log('Proposal created:', proposal.id);

// 5. Cosigners sign (only one cosigner is needed)
await multisig.signProposal(proposal.id);

// 6. Execute when threshold is met
await multisig.executeProposal(proposal.id);

console.log('Transfer executed!');
```

### Prover endpoint and retry policy

Both multisig SDKs retry only remote transaction proving. The default is two
total proof attempts. In TypeScript, endpoint-less injected provers, including
local and callback provers, run once. A custom remote URL overrides the Miden
client's injected prover.

```typescript
const client = new MultisigClient(midenClient, {
  guardianEndpoint: 'http://localhost:3000',
  midenRpcEndpoint: 'https://rpc.devnet.miden.io',
  prover: {
    url: 'https://prover.example',
    retry: { maxAttempts: 4 },
  },
});
```

```rust
use miden_multisig_client::{ProverConfig, ProverRetryPolicy};

let prover = ProverConfig::new()
    .with_url("https://prover.example")?
    .with_retry_policy(ProverRetryPolicy::new(4));

let client = MultisigClient::builder()
    .miden_endpoint(Endpoint::devnet())
    .guardian_endpoint("http://localhost:50051")
    .account_dir("/tmp/multisig-client")
    .prover_config(prover)
    .generate_key()
    .build()
    .await?;
```

URLs are validated during construction and must be absolute HTTP(S) URLs. A
custom prover never falls back to a default endpoint. Retries cover transient
proving conditions such as cancellation, deadlines, temporary unavailability,
capacity exhaustion, HTTP 408/429/502/503/504, I/O timeout, connection reset,
and broken pipe. Permanent or unrecognized failures return immediately.

Only proving is retried by this policy: transaction execution, GUARDIAN
coordination, Miden submission, and local application each run once. A larger
attempt budget can recover from brief failures but does not add prover
capacity. Idempotent Miden node reads have their own opt-in retry policy — see
the next section.

### Miden RPC retry policy

Both multisig SDKs retry idempotent Miden node reads under an `rpc` policy
that mirrors the prover configuration. The default is two total attempts —
one classified, jittered retry — matching the prover policy's default; an
explicit `maxAttempts` of 1 opts out. Transaction submission is **never
retried**, under any configuration: a submission whose outcome is unknown
could execute twice if re-sent.

Coverage differs by language. In Rust, the policy wraps the node client
itself, so every node read the SDK or the underlying Miden client issues —
syncs, account, note, and block lookups — retries under it. The same policy
also covers the private-note transport: note fetches and stream
establishment retry, while **note sends are never retried in-call** (the
client's relay outbox re-sends undelivered notes on later syncs, so an
in-call resend could deliver twice). Configuring the transport endpoint
itself is covered in [Note transport endpoint](#note-transport-endpoint).
Connection failures retry only when the cause is transport-shaped; TLS,
certificate, and invalid-endpoint problems fail immediately. In TypeScript,
the policy wraps the reads the SDK issues directly: on-chain account lookups,
state-commitment verification, and the guardian-switch node sync; syncs you
perform on the injected Miden client are owned by your application. As part
of this policy the Rust SDK also disables the Miden client library's internal
transport-retry loop, which silently retransmitted rate-limited requests —
**including submissions** — up to four extra times. This explicit policy is
the only retry layer: reads get one retry by default, submissions never.

```typescript
const client = new MultisigClient(midenClient, {
  guardianEndpoint: 'http://localhost:3000',
  midenRpcEndpoint: 'https://rpc.devnet.miden.io',
  prover: { retry: { maxAttempts: 4 } },
  rpc: { retry: { maxAttempts: 3 } },
});
```

```rust
use miden_multisig_client::{RpcConfig, RpcRetryPolicy};

let rpc = RpcConfig::new()
    .with_timeout_ms(15_000)?
    .with_retry_policy(RpcRetryPolicy::new(3));

let client = MultisigClient::builder()
    .miden_endpoint(Endpoint::devnet())
    .guardian_endpoint("http://localhost:50051")
    .account_dir("/tmp/multisig-client")
    .rpc_config(rpc)
    .generate_key()
    .build()
    .await?;
```

The classifier is shared with the prover policy (same transient/permanent
partition and backoff), extended with the node's transport renderings: a
rate-limit rejection (`Too Many Requests!`), and connection failures the node
reports under an `Unknown` status (`i/o timeout`, `connection error`,
`transport error`). Permanent failures — invalid arguments, not-found,
authentication — return immediately without consuming the budget, and once
the budget is exhausted the final upstream error is returned unchanged. A
larger budget adds patience against a rate-limited or briefly unreachable
node; it does not add node capacity.

In Rust, the per-request gRPC deadline defaults to 10 seconds on every SDK
node connection (presets, custom endpoints, and the direct commitment
reads); `with_timeout_ms` replaces it uniformly.
The TypeScript configuration intentionally has **no timeout**: the browser
WASM RPC client cannot cancel an in-flight request, and a JavaScript-side
timeout would abandon the call — whose side effects may still land — while
reporting failure. Retry behavior itself is fixture-verified to be identical
across both SDKs.

The Guardian server exposes the same policy for its own node reads via
`GUARDIAN_MIDEN_RPC_ENDPOINT`, `GUARDIAN_MIDEN_RPC_TIMEOUT_MS`, and
`GUARDIAN_MIDEN_RPC_MAX_ATTEMPTS` — see
[docs/CONFIGURATION.md](./CONFIGURATION.md).

### Note transport endpoint

Private notes are relayed through a note transport service that is separate
from the node RPC. On the testnet and devnet presets both SDKs derive the
transport endpoint automatically; a custom node endpoint has no derivable
transport service, so it must be set explicitly — otherwise private-note
relay is disabled.

In Rust the endpoint lives on the builder, next to the node endpoint:

```rust
let client = MultisigClient::builder()
    .miden_endpoint(Endpoint::try_from("https://my-node.internal:57291")?)
    .note_transport_endpoint("https://my-transport.internal")
    .guardian_endpoint("http://localhost:50051")
    .account_dir("/tmp/multisig-client")
    .generate_key()
    .build()
    .await?;
```

In TypeScript the injected Miden client owns note relay, so the endpoint is
set when constructing it, not on the `MultisigClient` configuration:

```typescript
import { MidenClient } from '@miden-sdk/miden-sdk';
import { MultisigClient } from '@openzeppelin/miden-multisig-client';

const midenClient = await MidenClient.create({
  rpcUrl: 'https://my-node.internal:57291',
  noteTransportUrl: 'https://my-transport.internal',
});

const client = new MultisigClient(midenClient, {
  guardianEndpoint: 'http://localhost:3000',
  midenRpcEndpoint: 'https://my-node.internal:57291',
});
```

`noteTransportUrl` also accepts the `'testnet'` and `'devnet'` shorthands, so
a custom node on a public network can reuse that network's public transport
service. In both SDKs the transport shares the node RPC resilience policy
described above.

#### Rust

```rust
use miden_multisig_client::{MultisigClient, TransactionType, Endpoint};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Setup client
    let mut client = MultisigClient::builder()
        // the Miden node RPC endpoint
        .miden_endpoint(Endpoint::new("http://localhost:57291"))
        // the GUARDIAN server endpoint
        .guardian_endpoint("http://localhost:50051")
        // the directory where the miden-client will store the account data
        .account_dir("/tmp/multisig-client") 
        // generate a new Falcon keypair for GUARDIAN authentication
        .generate_key()
        .build()
        .await?;

    // 2. Create 1-of-3 multisig account
    let signer_commitments = vec![
        client.user_commitment(),
        cosigner1_commitment,
        cosigner2_commitment,
    ];
    let account = client.create_account(1, signer_commitments).await?;
    client.push_account().await?;

    println!("Account created: {}", account.id());

    // 3. Create a transfer proposal
    let tx = TransactionType::transfer(recipient_id, faucet_id, 1000);
    let proposal = client.propose_transaction(tx).await?;

    println!("Proposal created: {}", proposal.id);

    // 4. Cosigners sign (only one cosigner is needed)
    client.sign_proposal(&proposal.id).await?;

    // 5. Execute when threshold is met
    client.execute_proposal(&proposal.id).await?;

    println!("Transfer executed!");
    Ok(())
}
```

---

## Core Concepts

### Multisig Accounts

A multisig account requires **M-of-N** signatures to authorize transactions:
- **Threshold (M)**: Minimum signatures required
- **Signers (N)**: Total number of authorized cosigners
- **Commitment**: Each signer's Falcon public key commitment (32 bytes, 64 hex chars)

### Guardian

GUARDIAN is a coordination server that:
- Stores the account state off-chain
- Coordinates proposal signing between cosigners
- Provides acknowledgment signatures for on-chain execution (ensures the new state is available for the rest of the cosigners)
- Keeps multisig metadata private

> **Note**: GUARDIAN server setup is covered in separate documentation. This SDK assumes a running GUARDIAN instance.

### Proposal Lifecycle

```
┌──────────┐     ┌──────────┐     ┌───────────┐
│ PENDING  │ ──► │  READY   │ ──► │ FINALIZED │
└──────────┘     └──────────┘     └───────────┘
     │                │                 │
 Collecting      Threshold           Executed
 signatures        met              on-chain
```

**States:**
- **Pending**: Proposal created, collecting signatures (shows X/Y signed)
- **Ready**: Threshold met, can be executed
- **Finalized**: Executed on-chain or discarded

### Custom Proposal Types

Guardian accepts any non-empty `proposal_type`, not just the first-party
operations (issue #266). A proposal whose type the SDK does not model is
exposed as the **custom** bucket — `TransactionType::Custom` in Rust,
`proposalType: 'custom'` in TypeScript — while the label is preserved
(Rust `ProposalMetadata.proposal_type`, TypeScript `CustomProposalMetadata.rawProposalType`)
so it can be displayed. The SDK normalizes the label to lowercase `snake_case`
(trim + lowercase, then require `[a-z0-9_]+` — the same shape as built-in
labels), so `b2agg` is accepted, `B2Agg` is lowercased to `b2agg`, and
`add signer` / `add-signer` are rejected. (Normalization is SDK-side; the server
itself still accepts any non-empty string.)

Custom proposals can be listed, displayed, signed, and exported/imported.

**Producer API (issue #266).** The integration that owns a custom type builds
its own transaction and drives the create + execute ends; the SDK never
executes a transaction it does not understand. The model is **symmetric across
Rust and TypeScript**:

- **Create** — `propose_custom_transaction(transaction_request_bytes, proposal_type)` (Rust) /
  `createCustomProposal(transactionRequestBytes, proposalType)` (TS). The bytes are a
  serialized transaction request; the SDK derives the summary and pushes the
  proposal with the custom label. They are **not** stored on the server.
  Cosigners then review and sign through the normal flow.
- **Execute** — `prepare_custom_execution(proposal_id, transaction_request_bytes)` (Rust) /
  `prepareCustomExecution(proposalId, transactionRequestBytes)` (TS). The SDK verifies the
  proposal is ready, binding-checks the request against the signed commitment
  (before any acknowledgment request), fetches the GUARDIAN ack, and returns the
  **advice** (cosigner signatures + ack). The integration injects that advice
  into its own transaction and submits via its own client:

  ```ts
  // TypeScript: rebuild via the integration's builder (the wasm request is immutable)
  const advice = await multisig.prepareCustomExecution(proposalId, transactionRequestBytes);
  const finalReq = myBuilder.extendAdviceMap(advice).build();
  await multisig.submitTransaction(finalReq);
  ```
  ```rust
  // Rust: inject into the request's advice map, submit via the SDK helper
  let advice = client.prepare_custom_execution(&proposal_id, &transaction_request_bytes).await?;
  let mut req = deserialize_transaction_request(&transaction_request_bytes)?;
  req.advice_map_mut().extend(advice);
  client.submit_transaction(req).await?;
  ```

The SDK owns the security-critical pieces (binding check, signature + ack
assembly, ack-after-binding ordering); the integration owns only the
transaction recipe + submit. `execute_proposal` on a custom type returns a
clear error pointing to `prepare_custom_execution`. Because the integration must
rebuild its transaction to execute, **custom execution is performed by a party
that holds the recipe** (typically the producer), not by an arbitrary cosigner.

The returned advice is keyed by the signer and GUARDIAN commitments
(domain-separated digests over the signed `tx_summary`), the same keys the
SDK's own built-in execution uses. Extending a transaction's advice map with it
therefore does not collide with the transaction's ordinary inputs; the
integration extends rather than replaces its advice map.

> **Security:** for first-party types the SDK reconstructs the transaction from
> metadata and checks it against the signed `tx_summary` commitment. For custom
> types there is no such reconstruction, so the SDK cannot verify that display
> metadata (e.g. `description`) matches what the transaction actually does.
> Cosigners must verify the raw `tx_summary` they are signing — not trust the
> label or description.

### Offline Workflow

For air-gapped or offline signing scenarios:

```
┌─────────────┐         ┌─────────────┐         ┌─────────────┐
│  Proposer   │         │  Cosigner   │         │  Executor   │
│  (Online)   │         │ (Air-gapped)│         │  (Online)   │
└──────┬──────┘         └──────┬──────┘         └──────┬──────┘
       │                       │                       │
       │  Export proposal.json │                       │
       │──────────────────────►│                       │
       │                       │                       │
       │                       │ Sign offline          │
       │                       │                       │
       │                       │ Export signed.json    │
       │                       │──────────────────────►│
       │                       │                       │
       │                       │              Import & Execute
       ▼                       ▼                       ▼
```

---

## TypeScript SDK Guide

### Installation & Setup

```typescript
import { MidenClient, AuthSecretKey } from '@miden-sdk/miden-sdk';
import {
  MultisigClient,
  Multisig,
  FalconSigner,
  AccountInspector,
  type MultisigConfig,
} from '@openzeppelin/miden-multisig-client';

// Initialize Miden client (connects to Miden node)
const midenClient = await MidenClient.createDevnet();

// Create signer from secret key
const secretKey = AuthSecretKey.rpoFalconWithRNG(undefined);
const signer = new FalconSigner(secretKey);

// Initialize multisig client
const client = new MultisigClient(midenClient, {
  guardianEndpoint: 'http://localhost:3000',
  midenRpcEndpoint: 'https://rpc.devnet.miden.io',
});
```

### Creating Accounts

```typescript
// Get GUARDIAN server's public key commitment
const guardianCommitment = await client.guardianClient.getPubkey();

// Define multisig configuration
const config: MultisigConfig = {
  threshold: 2,                              // 2 signatures required
  signerCommitments: [                       // 3 authorized signers
    signer.commitment,                       // Your commitment
    '0x1234...abcd',                        // Cosigner 1
    '0x5678...efgh',                        // Cosigner 2
  ],
  guardianCommitment,                            // GUARDIAN server commitment
  guardianEnabled: true,
};

// Create the account
const multisig = await client.create(config, signer);

// Register with GUARDIAN (stores initial state)
await multisig.registerOnGuardian();

console.log('Account ID:', multisig.accountId);
console.log('Threshold:', multisig.threshold);
console.log('Signers:', multisig.signerCommitments);
```

### Loading Existing Accounts

```typescript
// Load as a cosigner joining an existing multisig
const multisig = await client.load(accountId, signer);

// Fetch latest state from GUARDIAN
const state = await multisig.fetchState();

// Inspect account configuration
const detected = AccountInspector.fromBase64(state.stateDataBase64);
console.log('Threshold:', detected.threshold);
console.log('Signers:', detected.signerCommitments);
console.log('Vault balances:', detected.vaultBalances);
```

### Recovering Accounts By Key

Use `recoverByKey` when a wallet has a signing key from an account's
authorization set but does not know the account ID yet. The helper queries
Guardian's `/state/lookup` endpoint with proof-of-possession of the key,
fetches state for each matching account, and returns `(accountId, state)`
pairs.

```typescript
const recovered = await client.recoverByKey(signer);

if (recovered.length === 0) {
  console.log('No account on this Guardian authorizes this key');
}

for (const { accountId, state } of recovered) {
  console.log('Recovered account:', accountId);
  console.log('State commitment:', state.commitment);

  const multisig = await client.load(accountId, signer);
  // Continue with normal proposal or sync flows.
}
```

The signer passed to `recoverByKey` must implement `signLookupMessage`. The
bundled `FalconSigner`, `EcdsaSigner`, Miden Wallet signer, and Para signer
support it. Multiple matches are valid: the same key commitment may authorize
more than one account, and the method returns all matches instead of choosing
one implicitly.

### Proposal Operations

#### P2ID Transfer (Send Funds)

```typescript
const proposal = await multisig.createP2idProposal(
  recipientAccountId,    // Recipient's account ID
  faucetAccountId,       // Faucet (token) ID
  1000n                  // Amount to send
);

// Private note: only the note's hash is published on chain. Pass `noteType`
// in the options object (`NoteType` comes from `@miden-sdk/miden-sdk`).
import { NoteType } from '@miden-sdk/miden-sdk';

const privateProposal = await multisig.createP2idProposal(
  recipientAccountId,
  faucetAccountId,
  1000n,
  undefined,                       // nonce (defaults to Date.now())
  { noteType: NoteType.Private },  // note visibility; defaults to NoteType.Public
);
```

> **Warning:** a `private` P2ID note publishes only its hash on chain. The
> recipient cannot discover the note by syncing; the full note details must
> be shared with them out-of-band before they can consume it. Use
> `exportNoteToBytes` / `importNoteFromBytes` (or the browser file variants
> `exportNoteToFile` / `importNoteFromFile`) for that transfer (issue #356):

```typescript
// Sender: resolve the note ID BEFORE executing (it derives from the
// pre-execution vault state), then export after execution.
const noteId = await multisig.getP2idNoteId(privateProposal);
// ...sign + execute the proposal...
const noteFileBytes = await multisig.exportNoteToBytes(noteId);
// Deliver `noteFileBytes` to the recipient out-of-band (file, message, ...).
// Or, in a browser, trigger a download of the note file directly:
await multisig.exportNoteToFile(noteId);

// Recipient: import the bytes (or a File from an <input type="file">), then
// sync so the note's on-chain commitment is tracked; it then appears in
// getConsumableNotes() and can be consumed with createConsumeNotesProposal
// as usual.
const importedNoteId = await multisig.importNoteFromBytes(noteFileBytes);
```

> **Note:** every cosigner device that verifies or signs the consume-notes
> proposal needs the note in its local store with the on-chain inclusion
> proof — deliver the note file to each of them (import + sync), not just to
> the proposer. A cosigner whose store lacks the authenticated note rebuilds
> the transaction differently (the input-notes commitment distinguishes
> authenticated from unauthenticated consumption) and rejects the proposal
> with `metadata does not match tx_summary`. The sender's own device heals
> itself: it already knows the full note, so a post-commit sync is enough.

#### Consume Notes (Claim Received Funds)

```typescript
// Get consumable notes
const notes = await multisig.getConsumableNotes();

// Create proposal to consume them
const noteIds = notes.map(n => n.id);
const proposal = await multisig.createConsumeNotesProposal(noteIds);
```

A private note received out-of-band must first be loaded with
`importNoteFromBytes(noteFileBytes)` or `importNoteFromFile(file)` (see the
P2ID section above); after a sync it shows up in `getConsumableNotes()` like
any public note.

#### Add Signer

```typescript
const proposal = await multisig.createAddSignerProposal(
  newSignerCommitment,   // New signer's public key commitment
  undefined,             // Optional nonce
  newThreshold           // Optional new threshold
);
```

#### Remove Signer

```typescript
const proposal = await multisig.createRemoveSignerProposal(
  signerToRemove,        // Signer's commitment to remove
  undefined,             // Optional nonce
  newThreshold           // Optional new threshold
);
```

#### Change Threshold

```typescript
const proposal = await multisig.createChangeThresholdProposal(
  newThreshold           // New threshold value
);
```

#### Switch GUARDIAN Provider

```typescript
const proposal = await multisig.createSwitchGuardianProposal(
  newGuardianEndpoint,        // New GUARDIAN server URL
  newGuardianCommitment       // New GUARDIAN server's public key
);
```

### Signing & Executing Proposals

```typescript
// List all pending proposals
const proposals = await multisig.syncProposals();

for (const proposal of proposals) {
  console.log(`${proposal.id}: ${proposal.status.type}`);

  if (proposal.status.type === 'pending') {
    console.log(`  Signatures: ${proposal.status.signaturesCollected}/${proposal.status.signaturesRequired}`);
  }
}

// Sign a proposal
const signed = await multisig.signProposal(proposalId);

// Execute when ready
if (signed.status.type === 'ready') {
  await multisig.executeProposal(proposalId);
}
```

### Offline Export/Import

```typescript
// Export proposal for offline signing
const json = multisig.exportProposalToJson(proposalId);
// Share via file, QR code, etc.

// On air-gapped machine: import and sign
const imported = multisig.importProposal(json);
const signedJson = multisig.signProposalOffline(proposalId);

// Back on online machine: import signed proposal
const signedProposal = multisig.importProposal(signedJson);
await multisig.executeProposal(signedProposal.id);
```

### API Reference

#### MultisigClient

| Method | Description |
|--------|-------------|
| `create(config, signer)` | Create new multisig account |
| `load(accountId, signer)` | Load existing account from GUARDIAN |
| `recoverByKey(signer)` | Discover accounts that authorize the signer's key and fetch each current state |
| `guardianClient` | Access to underlying GUARDIAN HTTP client |

#### Multisig

| Method | Description |
|--------|-------------|
| `accountId` | Get account ID (hex string) |
| `threshold` | Get current threshold |
| `signerCommitments` | Get list of signer commitments |
| `fetchState()` | Fetch latest state from GUARDIAN |
| `registerOnGuardian()` | Register new account with GUARDIAN |
| `syncProposals()` | Sync proposals from GUARDIAN |
| `abandonCandidate(nonce)` | Record an abandon intent for a stuck candidate (worker resolves after a short quarantine) |
| `abandonStatus(nonce)` | Poll the abandon resolution: `waiting` / `landed` / `abandoned` / `unexpected` |
| `listProposals()` | Get cached proposals |
| `createP2idProposal(recipient, faucet, amount, nonce?, { noteType }?)` | Create transfer proposal (`noteType`: `NoteType.Public` (default) or `NoteType.Private`) |
| `createConsumeNotesProposal(noteIds, nonce?)` | Create note consumption proposal |
| `getP2idNoteId(proposal)` | Compute the note ID a P2ID proposal creates (call before executing) |
| `exportNoteToBytes(noteId)` | Export a created note as note-file bytes for out-of-band delivery |
| `exportNoteToFile(noteId, filename?)` | Browser-only: download the note file |
| `importNoteFromBytes(noteBytes)` | Import a note file received out-of-band |
| `importNoteFromFile(file)` | Import a note file from a browser `File`/`Blob` |
| `createAddSignerProposal(commitment, nonce?, threshold?)` | Create add signer proposal |
| `createRemoveSignerProposal(commitment, nonce?, threshold?)` | Create remove signer proposal |
| `createChangeThresholdProposal(threshold, nonce?)` | Create threshold change proposal |
| `createSwitchGuardianProposal(endpoint, pubkey, nonce?)` | Create GUARDIAN switch proposal |
| `signProposal(id)` | Sign a proposal |
| `executeProposal(id)` | Execute ready proposal |
| `exportProposalToJson(id)` | Export for offline signing |
| `importProposal(json)` | Import offline proposal |
| `signProposalOffline(id)` | Sign imported proposal offline |
| `getConsumableNotes()` | Get notes that can be consumed |

#### FalconSigner

| Property/Method | Description |
|-----------------|-------------|
| `commitment` | Public key commitment (hex) |
| `publicKey` | Serialized public key (hex) |
| `signRequest(id, timestamp, requestPayload)` | Sign account ID + timestamp + request payload digest for auth |
| `signCommitment(hex)` | Sign commitment/word |
| `signLookupMessage(timestamp, keyCommitment)` | Sign account-less lookup digest for `recoverByKey` |

#### AccountInspector

| Method | Description |
|--------|-------------|
| `fromBase64(data)` | Inspect base64-encoded account |
| `fromAccount(account)` | Inspect Account object |

Returns `DetectedMultisigConfig`:
- `threshold`: number
- `numSigners`: number
- `signerCommitments`: string[]
- `guardianEnabled`: boolean
- `guardianCommitment`: string
- `vaultBalances`: { faucetId, amount }[]

---

## Rust SDK Guide

### Installation & Setup

```rust
use miden_multisig_client::{
    MultisigClient, MultisigClientBuilder,
    MultisigAccount, TransactionType,
    Proposal, ProposalStatus,
    KeyManager, GuardianKeyStore,
    Endpoint, Word, AccountId, SecretKey,
};

// Build client with fluent API
let mut client = MultisigClient::builder()
    .miden_endpoint(Endpoint::new("http://localhost:57291"))
    .guardian_endpoint("http://localhost:50051")
    .account_dir("/tmp/multisig-data")
    .generate_key()  // Or: .with_secret_key(key)
    .build()
    .await?;
```

### Creating Accounts

```rust
// Collect signer commitments (your key + cosigners)
let signer_commitments = vec![
    client.user_commitment(),          // Your commitment
    commitment_from_hex("0x1234...")?, // Cosigner 1
    commitment_from_hex("0x5678...")?, // Cosigner 2
];

// Create 2-of-3 multisig
let account = client.create_account(2, signer_commitments).await?;

// Register with GUARDIAN
client.push_account().await?;

println!("Created account: {}", account.id());
println!("Threshold: {}", account.threshold()?);
println!("Signers: {:?}", account.cosigner_commitments_hex());
```

### Loading Existing Accounts

```rust
// Pull account from GUARDIAN (as a cosigner)
let account = client.pull_account(account_id).await?;

// Sync with Miden network
client.sync().await?;

// Inspect account
println!("Threshold: {}", account.threshold()?);
println!("Nonce: {}", account.nonce());
println!("GUARDIAN enabled: {}", account.guardian_enabled()?);
```

### Recovering Accounts By Key

Use `recover_by_key` when the configured signer is known but the account ID is
not. The client signs a lookup-bound authentication message, asks Guardian for
accounts that authorize the signer's commitment, fetches state for each match,
and returns `RecoveredAccount` values.

```rust
let recovered = client.recover_by_key().await?;

if recovered.is_empty() {
    println!("No account on this Guardian authorizes this key");
}

for entry in recovered {
    println!("Recovered account: {}", entry.account_id);
    println!("State commitment: {}", entry.state.commitment);

    let account_id = AccountId::from_hex(&entry.account_id)?;
    client.pull_account(account_id).await?;
    // Continue with normal proposal or sync flows.
}
```

An empty list means the key is valid but this Guardian has no account metadata
that authorizes its commitment. Authentication failures, malformed lookup
responses, and per-account `get_state` failures are returned as errors.

### Transaction Types

```rust
// P2ID Transfer (public note)
let tx = TransactionType::transfer(recipient_id, faucet_id, 1000);

// P2ID Transfer with a private note (only the note hash is published on
// chain; the recipient needs the note shared out-of-band — see
// "Out-of-Band Note Transfer" below). `NoteType` is re-exported from
// `miden_protocol::note`.
let tx = TransactionType::transfer_with_note_type(
    recipient_id, faucet_id, 1000, NoteType::Private,
);

// Consume Notes
let tx = TransactionType::consume_notes(vec![note_id1, note_id2]);

// Add Cosigner
let tx = TransactionType::add_cosigner(new_commitment);

// Remove Cosigner
let tx = TransactionType::remove_cosigner(commitment_to_remove);

// Update Signers (change threshold and/or signer set)
let tx = TransactionType::update_signers(new_threshold, new_signer_list);

// Switch GUARDIAN Provider
let tx = TransactionType::switch_guardian(new_endpoint, new_commitment);
```

### Proposal Operations

```rust
// Create and submit proposal
let proposal = client.propose_transaction(tx).await?;
println!("Proposal ID: {}", proposal.id);

// Or with offline fallback
match client.propose_with_fallback(tx).await? {
    ProposalResult::Online(proposal) => {
        println!("Submitted to GUARDIAN: {}", proposal.id);
    }
    ProposalResult::Offline(exported) => {
        // Save for file-based sharing
        std::fs::write("proposal.json", exported.to_json()?)?;
    }
}
```

### Listing & Signing Proposals

```rust
// List pending proposals
let proposals = client.list_proposals().await?;

for proposal in &proposals {
    match &proposal.status {
        ProposalStatus::Pending => {
            let (signatures_collected, signatures_required) = proposal.signature_counts();
            println!("{}: {}/{} signatures",
                proposal.id, signatures_collected, signatures_required);
            println!("  Signed by: {:?}", proposal.metadata.signers);
        }
        ProposalStatus::Ready => {
            println!("{}: Ready to execute", proposal.id);
        }
        ProposalStatus::Finalized => {
            println!("{}: Already executed", proposal.id);
        }
    }
}

// Sign a proposal
client.sign_proposal(&proposal_id).await?;

// Execute when ready
client.execute_proposal(&proposal_id).await?;
```

### Offline Export/Import

```rust
// Create offline proposal (when GUARDIAN unavailable)
let exported = client.create_proposal_offline(tx).await?;
std::fs::write("proposal.json", exported.to_json()?)?;

// On air-gapped machine: load and sign
let json = std::fs::read_to_string("proposal.json")?;
let mut exported: ExportedProposal = serde_json::from_str(&json)?;
client.sign_imported_proposal(&mut exported)?;
std::fs::write("signed.json", exported.to_json()?)?;

// Back online: execute
let json = std::fs::read_to_string("signed.json")?;
let exported: ExportedProposal = serde_json::from_str(&json)?;
client.execute_imported_proposal(&exported).await?;
```

### Note Filtering

```rust
use miden_multisig_client::NoteFilter;

// List all consumable notes
let notes = client.list_consumable_notes().await?;

// Filter by faucet
let filter = NoteFilter::by_faucet(faucet_id);
let notes = client.list_consumable_notes_filtered(filter).await?;

// Filter by faucet with minimum amount
let filter = NoteFilter::by_faucet_min_amount(faucet_id, 5000);
let notes = client.list_consumable_notes_filtered(filter).await?;

for note in notes {
    println!("Note {}: {} tokens", note.id, note.amount_for_faucet(faucet_id));
}
```

### Out-of-Band Note Transfer (Private Notes)

A private P2ID note publishes only its commitment on chain, so the recipient's
client can never learn the note contents via sync. The sender must export the
note and deliver the file out-of-band (issue #356):

```rust
// Sender: resolve the note ID BEFORE executing (it derives from the
// pre-execution vault state), then export after execution.
let note_id = client.p2id_note_id(&proposal)?;
client.execute_proposal(&proposal.id).await?;
client.export_note_to_file(&note_id.to_hex(), Path::new("note.mno")).await?;
// Deliver note.mno to the recipient out-of-band (file, message, ...).

// Recipient: import the file, then sync so the note's on-chain commitment
// is tracked; it then appears in list_consumable_notes() and can be
// consumed with a regular consume-notes proposal.
let imported_note_id = client.import_note_from_file(Path::new("note.mno")).await?;
client.sync().await?;
```

`export_note_to_bytes` / `import_note_from_bytes` are the in-memory variants
for programmatic delivery.

Every cosigner device that verifies or signs the consume-notes proposal needs
the note in its local store with the on-chain inclusion proof — deliver the
note file to each of them (import + sync), not just to the proposer. A
cosigner whose store lacks the authenticated note rebuilds the transaction
differently (the input-notes commitment distinguishes authenticated from
unauthenticated consumption) and rejects the proposal with `metadata does not
match tx_summary`. The sender's own device heals itself: it already knows the
full note, so a post-commit sync is enough.

### API Reference

#### MultisigClient

| Method | Description |
|--------|-------------|
| `builder()` | Create builder for configuration |
| `create_account(threshold, commitments)` | Create new multisig |
| `pull_account(id)` | Join existing multisig |
| `push_account()` | Register account with GUARDIAN |
| `sync()` | Sync with Miden network |
| `account()` | Get loaded account (Option) |
| `account_id()` | Get account ID (Option) |
| `user_commitment()` | Get user's key commitment |
| `user_commitment_hex()` | Get commitment as hex |
| `recover_by_key()` | Discover accounts that authorize the configured signer and fetch each current state |
| `propose_transaction(tx)` | Create and submit proposal |
| `propose_with_fallback(tx)` | Online or offline proposal |
| `list_proposals()` | List pending proposals |
| `sign_proposal(id)` | Sign a proposal |
| `execute_proposal(id)` | Execute ready proposal |
| `abandon_candidate(nonce)` | Record an abandon intent for a stuck candidate (worker resolves after a short quarantine) |
| `abandon_status(nonce)` | Poll the abandon resolution: `Waiting` / `Landed` / `Abandoned` / `Unexpected` |
| `create_proposal_offline(tx)` | Create offline proposal |
| `sign_imported_proposal(exported)` | Sign offline proposal |
| `execute_imported_proposal(exported)` | Execute offline proposal |
| `export_proposal(id, path)` | Export to file |
| `import_proposal(path)` | Import from file |
| `list_consumable_notes()` | List available notes |
| `list_consumable_notes_filtered(filter)` | Filter notes |
| `p2id_note_id(proposal)` | Compute the note ID a P2ID proposal creates (call before executing) |
| `export_note_to_file(note_id, path)` | Export a created note to a file for out-of-band delivery |
| `export_note_to_bytes(note_id)` | Export a created note as note-file bytes |
| `import_note_from_file(path)` | Import a note file received out-of-band |
| `import_note_from_bytes(bytes)` | Import a note from note-file bytes |

#### MultisigAccount

| Method | Description |
|--------|-------------|
| `id()` | Account ID |
| `nonce()` | Current nonce |
| `commitment()` | Account state commitment |
| `threshold()` | Signing threshold |
| `num_signers()` | Number of signers |
| `cosigner_commitments()` | List of commitments (Word) |
| `cosigner_commitments_hex()` | List as hex strings |
| `is_cosigner(commitment)` | Check if commitment is signer |
| `guardian_enabled()` | GUARDIAN integration enabled |
| `guardian_commitment()` | GUARDIAN server commitment |

#### TransactionType

| Variant | Description |
|---------|-------------|
| `P2ID { recipient, faucet_id, amount, note_type }` | Transfer funds (`note_type` selects note visibility; defaults to `NoteType::Public` via `transfer()`) |
| `ConsumeNotes { note_ids }` | Consume notes |
| `AddCosigner { new_commitment }` | Add signer |
| `RemoveCosigner { commitment }` | Remove signer |
| `UpdateSigners { new_threshold, signer_commitments }` | Update config |
| `SwitchGuardian { new_endpoint, new_commitment }` | Switch GUARDIAN |

#### ProposalStatus

| Variant | Description |
|---------|-------------|
| `Pending` | Collecting sigs (`proposal.signature_counts()`, `proposal.metadata.signers`) |
| `Ready` | Threshold met |
| `Finalized` | Executed |

---

## Use Cases

### Use Case 1: Treasury Management (2-of-3)

A company treasury requiring 2 of 3 executives to approve transfers.

```typescript
// Setup: CEO, CFO, and COO each have their own signer
const config = {
  threshold: 2,
  signerCommitments: [ceoCommitment, cfoCommitment, cooCommitment],
  guardianCommitment,
  guardianEnabled: true,
};

const treasury = await client.create(config, ceoSigner);
await treasury.registerOnGuardian();

// CEO proposes payment to vendor
const payment = await treasury.createP2idProposal(
  vendorAccountId,
  usdcFaucetId,
  50000n
);

// CFO reviews and signs
const cfoMultisig = await cfoClient.load(treasury.accountId, cfoSigner);
await cfoMultisig.syncProposals();
await cfoMultisig.signProposal(payment.id);

// Payment executes (threshold met: CEO + CFO = 2)
await treasury.executeProposal(payment.id);
```

### Use Case 2: Secure Operations (3-of-5)

High-security operations requiring 3 of 5 board members.

```rust
// Create 3-of-5 multisig
let board_commitments = vec![member1, member2, member3, member4, member5];
let account = client.create_account(3, board_commitments).await?;

// Propose removing a compromised member
let tx = TransactionType::remove_cosigner(compromised_member);
let proposal = client.propose_transaction(tx).await?;

// Three members must sign
// member1.sign_proposal(...)
// member2.sign_proposal(...)
// member3.sign_proposal(...)

// Execute with 3 signatures
client.execute_proposal(&proposal.id).await?;
```

### Use Case 3: Note Consumption

Claiming tokens sent to the multisig.

```typescript
// Check for incoming notes
const notes = await multisig.getConsumableNotes();

console.log('Pending notes:');
for (const note of notes) {
  for (const asset of note.assets) {
    if (asset.isFungible()) {
      console.log(`  ${note.id}: ${asset.amount()} from faucet ${asset.faucetId()}`);
    }
  }
}

// Create proposal to consume all notes
const noteIds = notes.map(n => n.id);
const proposal = await multisig.createConsumeNotesProposal(noteIds);

// After threshold signatures...
await multisig.executeProposal(proposal.id);

console.log('Notes consumed, funds now in vault');
```

---

## Offline Workflow

### Complete Flow Diagram

```
┌─────────────────────────────────────────────────────────────────────┐
│                         OFFLINE SIGNING FLOW                         │
└─────────────────────────────────────────────────────────────────────┘

  PROPOSER (Online)           COSIGNER (Air-gapped)        EXECUTOR (Online)
  ─────────────────           ────────────────────         ────────────────
        │                            │                            │
        │ create_proposal_offline()  │                            │
        │ or propose_with_fallback() │                            │
        ▼                            │                            │
   ┌──────────┐                      │                            │
   │ Export   │                      │                            │
   │ proposal │                      │                            │
   │  .json   │─────── USB ─────────►│                            │
   └──────────┘                      │                            │
        │                            ▼                            │
        │                     ┌──────────────┐                    │
        │                     │ Import JSON  │                    │
        │                     │ Sign offline │                    │
        │                     │ Export JSON  │                    │
        │                     └──────────────┘                    │
        │                            │                            │
        │                            │─────── USB ───────────────►│
        │                            │                            ▼
        │                            │                     ┌─────────────┐
        │                            │                     │ Import JSON │
        │                            │                     │ Verify sigs │
        │                            │                     │ Execute tx  │
        │                            │                     └─────────────┘
        ▼                            ▼                            ▼
```

### Export Format (JSON)

```json
{
  "version": 1,
  "account_id": "0x7925bdcc9c4df01068e79d4c94beeb",
  "id": "0xabcd1234...",
  "nonce": 5,
  "tx_summary": {
    "...": "transaction summary JSON"
  },
  "signatures": [
    {
      "signer_commitment": "0x1234...",
      "signature": "0x5678..."
    }
  ],
  "signatures_required": 2,
  "metadata": {
    "proposal_type": "add_signer",
    "salt_hex": "0x...",
    "new_threshold": 2,
    "signer_commitments_hex": ["0x...", "0x..."]
  }
}
```

## Version Compatibility

| SDK Version | miden-client | miden-sdk (npm) | Notes |
|-------------|--------------|-----------------|-------|
| 0.15.x | 0.15.0 | ^0.15.0 | Miden 0.15 protocol; v1 account IDs, bech32m addresses |
| 0.14.x | 0.14.x | ^0.14.0 | Devnet default, MidenClient public API |
| 0.13.x | 0.13.0 | ^0.13.0 | ECDSA support, wallet signers |
| 0.12.x | 0.12.5 | ^0.12.5 | Initial release |

### Breaking Changes

Check the [GitHub release notes](https://github.com/OpenZeppelin/guardian/releases) for breaking changes between versions.

---

## Releasing

Steps for publishing a new version of the SDK (Rust crates + TypeScript packages).

### Pre-Release Checklist

1. All tests pass:

```bash
# Rust
cargo test --locked \
  -p guardian-shared \
  -p guardian-client \
  -p miden-confidential-contracts \
  -p miden-multisig-client

# TypeScript
cd packages/guardian-client && npm test
cd packages/guardian-evm-client && npm test
cd packages/miden-multisig-client && npm test
cd packages/guardian-operator-client && npm test
```

2. TypeScript packages build cleanly:

```bash
cd packages/guardian-client && npm run build
cd packages/guardian-evm-client && npm run build
cd packages/miden-multisig-client && npm run build
cd packages/guardian-operator-client && npm run build
```

3. Version numbers are updated in all files (see below).

4. The coordinated Rust publication archive passes:

```bash
cargo publish --dry-run --locked \
  -p guardian-shared \
  -p guardian-client \
  -p miden-confidential-contracts \
  -p miden-multisig-client
```

### Version Bump

Update the version in these files:

| File | Field | Inherits |
|------|-------|----------|
| `Cargo.toml` (workspace root) | `[workspace.package] version` | `shared`, `client`, `contracts`, `miden-multisig-client` |
| `crates/contracts/Cargo.toml` | `guardian-shared` dep version | - |
| `crates/client/Cargo.toml` | `guardian-shared` dep version | - |
| `crates/miden-multisig-client/Cargo.toml` | `guardian-client`, `guardian-shared`, `miden-confidential-contracts` dep versions | - |
| `packages/guardian-client/package.json` | `version` | - |
| `packages/guardian-evm-client/package.json` | `version` | - |
| `packages/miden-multisig-client/package.json` | `version` + `@openzeppelin/guardian-client` dep version | - |
| `packages/guardian-operator-client/package.json` | `version` | - |

The `server`, `miden-rpc-client`, `miden-keystore`, and example crates have their own independent versions and are not published.

### Publishing Rust Crates

Rust crates are published by the
[`Publish Rust Crates`](../.github/workflows/publish-crates.yml) workflow.
Publishing a GitHub Release automatically selects all four crates, validates
that the tag is `v` followed by their coordinated version, requests one
`release` environment approval, and uses crates.io trusted publishing. Cargo
receives all selected packages in one command and handles dependency-safe
publication.

Configure a crates.io trusted publisher for each crate before the first
automated publication:

```text
GitHub owner: OpenZeppelin
Repository: guardian
Workflow: publish-crates.yml
Environment: release
```

Use a manual dry run to validate one or more selected crates without requesting
approval or credentials:

```bash
gh workflow run publish-crates.yml \
  --ref main \
  -f dry-run=true \
  -f guardian-shared=true \
  -f guardian-client=true \
  -f miden-confidential-contracts=true \
  -f miden-multisig-client=true
```

A manual run with `dry-run=false` publishes only the selected crates after one
approval. If an unselected internal prerequisite is not already visible at the
exact coordinated version, validation fails before publication.

Publication is rerunnable. Exact versions already on crates.io are reported as
already published and omitted from the Cargo command. If a publication is
interrupted, rerun it; the preflight skips versions that reached crates.io and
Cargo processes the remainder. Trusted-publishing failure fails closed; the
workflow has no registry-token fallback.

### Publishing TypeScript Packages

Publish in dependency order:

```bash
# 1. Build TypeScript packages
cd packages/guardian-client && npm run build
cd packages/guardian-evm-client && npm run build
cd packages/miden-multisig-client && npm run build
cd packages/guardian-operator-client && npm run build

# 2. Publish base clients first (no internal deps)
cd packages/guardian-client && npm publish --access public
cd packages/guardian-evm-client && npm publish --access public

# 3. Publish miden-multisig-client (depends on guardian-client)
cd packages/miden-multisig-client && npm publish --access public

# 4. Publish operator-client (no internal dependencies)
cd packages/guardian-operator-client && npm publish --access public
```

### Post-Release

1. Create a draft GitHub Release for review:

```bash
gh release create v<version> --generate-notes --draft
```

2. Publish the draft when the coordinated Rust, TypeScript, and server-image
   release workflows should start:

```bash
gh release edit v<version> --draft=false
```

3. Approve the `release` environment jobs that should publish. The Rust
   workflow summary records the source SHA, coordinated version,
   authentication mode, and ordered per-crate outcomes without credentials.

---

## Additional Resources

- [Miden Documentation](https://docs.miden.io/)
- [GUARDIAN Documentation](../crates/server/README.md)
  - [GUARDIAN Specification](../spec/index.md)
- [Example Applications](../examples/)
  - [Web Example](../examples/web/)
  - [CLI Demo](../examples/demo/)
  - [Rust Example](../examples/rust/)
