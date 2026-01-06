# Miden Multisig SDK

An SDK for creating and managing multisignature accounts on the Miden network. Available for both **TypeScript** (web/browser) and **Rust** (native/server) environments.

## Table of Contents

- [Quick Start](#quick-start)
- [Core Concepts](#core-concepts)
- [TypeScript SDK Guide](#typescript-sdk-guide)
- [Rust SDK Guide](#rust-sdk-guide)
- [Use Cases](#use-cases)
- [Offline Workflow](#offline-workflow)
- [Error Handling](#error-handling)
- [Security Best Practices](#security-best-practices)

---

## Quick Start

### Installation

The multisig sdk has as peer dependency on the miden-sdk, you will need to install both.

**TypeScript (npm)**
```bash
npm install @openzeppelin/miden-multisig-client @demox-labs/miden-sdk
```

**Rust (Cargo.toml)**
```toml
[dependencies]
miden-multisig-client = "0.12.5"
miden-client = "0.12.5"
```

### 5-Minute Example

Create a 1-of-3 multisig account, propose a transfer, collect signatures, and execute.

#### TypeScript

```typescript
import { WebClient, SecretKey } from '@demox-labs/miden-sdk';
import { MultisigClient, FalconSigner } from '@openzeppelin/miden-multisig-client';

// 1. Setup clients
const midenClient = await WebClient.createClient('https://rpc.testnet.miden.io:443');
const secretKey = SecretKey.rpoFalconWithRNG(seed);
const signer = new FalconSigner(secretKey);
const client = new MultisigClient(midenClient, { psmEndpoint: 'http://localhost:3000' });

// 2. Get PSM server public key
const psmCommitment = await client.psmClient.getPubkey();

// 3. Create 1-of-3 multisig account
const config = {
  threshold: 1,
  signerCommitments: [signer.commitment, cosigner1Commitment, cosigner2Commitment],
  psmCommitment,
  psmEnabled: true,
};
const multisig = await client.create(config, signer);
await multisig.registerOnPsm();

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

#### Rust

```rust
use miden_multisig_client::{MultisigClient, TransactionType, Endpoint};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Setup client
    let mut client = MultisigClient::builder()
        // the Miden node RPC endpoint
        .miden_endpoint(Endpoint::new("http://localhost:57291"))
        // the PSM server endpoint
        .psm_endpoint("http://localhost:50051")
        // the directory where the miden-client will store the account data
        .account_dir("/tmp/multisig-client") 
        // generate a new Falcon keypair for PSM authentication
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

### Private State Manager (PSM)

PSM is a coordination server that:
- Stores the account state off-chain
- Coordinates proposal signing between cosigners
- Provides acknowledgment signatures for on-chain execution (ensures the new state is available for the rest of the cosigners)
- Keeps multisig metadata private

> **Note**: PSM server setup is covered in separate documentation. This SDK assumes a running PSM instance.

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
import { WebClient, SecretKey } from '@demox-labs/miden-sdk';
import {
  MultisigClient,
  Multisig,
  FalconSigner,
  AccountInspector,
  type MultisigConfig,
} from '@openzeppelin/miden-multisig-client';

// Initialize web client (connects to Miden node)
const webClient = await WebClient.createClient('https://rpc.testnet.miden.io:443');

// Create signer from secret key
const secretKey = SecretKey.rpoFalconWithRNG(seed);
const signer = new FalconSigner(secretKey);

// Initialize multisig client
const client = new MultisigClient(webClient, {
  psmEndpoint: 'http://localhost:3000'
});
```

### Creating Accounts

```typescript
// Get PSM server's public key commitment
const psmCommitment = await client.psmClient.getPubkey();

// Define multisig configuration
const config: MultisigConfig = {
  threshold: 2,                              // 2 signatures required
  signerCommitments: [                       // 3 authorized signers
    signer.commitment,                       // Your commitment
    '0x1234...abcd',                        // Cosigner 1
    '0x5678...efgh',                        // Cosigner 2
  ],
  psmCommitment,                            // PSM server commitment
  psmEnabled: true,
};

// Create the account
const multisig = await client.create(config, signer);

// Register with PSM (stores initial state)
await multisig.registerOnPsm();

console.log('Account ID:', multisig.accountId);
console.log('Threshold:', multisig.threshold);
console.log('Signers:', multisig.signerCommitments);
```

### Loading Existing Accounts

```typescript
// Load as a cosigner joining an existing multisig
const multisig = await client.load(accountId, signer);

// Fetch latest state from PSM
const state = await multisig.fetchState();

// Inspect account configuration
const detected = AccountInspector.fromBase64(state.stateDataBase64);
console.log('Threshold:', detected.threshold);
console.log('Signers:', detected.signerCommitments);
console.log('Vault balances:', detected.vaultBalances);
```

### Proposal Operations

#### P2ID Transfer (Send Funds)

```typescript
const proposal = await multisig.createP2idProposal(
  recipientAccountId,    // Recipient's account ID
  faucetAccountId,       // Faucet (token) ID
  1000n                  // Amount to send
);
```

#### Consume Notes (Claim Received Funds)

```typescript
// Get consumable notes
const notes = await multisig.getConsumableNotes();

// Create proposal to consume them
const noteIds = notes.map(n => n.id);
const proposal = await multisig.createConsumeNotesProposal(noteIds);
```

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

#### Switch PSM Provider

```typescript
const proposal = await multisig.createSwitchPsmProposal(
  newPsmEndpoint,        // New PSM server URL
  newPsmCommitment       // New PSM server's public key
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
| `load(accountId, signer)` | Load existing account from PSM |
| `psmClient` | Access to underlying PSM HTTP client |

#### Multisig

| Method | Description |
|--------|-------------|
| `accountId` | Get account ID (hex string) |
| `threshold` | Get current threshold |
| `signerCommitments` | Get list of signer commitments |
| `fetchState()` | Fetch latest state from PSM |
| `registerOnPsm()` | Register new account with PSM |
| `syncProposals()` | Sync proposals from PSM |
| `listProposals()` | Get cached proposals |
| `createP2idProposal(recipient, faucet, amount, nonce?)` | Create transfer proposal |
| `createConsumeNotesProposal(noteIds, nonce?)` | Create note consumption proposal |
| `createAddSignerProposal(commitment, nonce?, threshold?)` | Create add signer proposal |
| `createRemoveSignerProposal(commitment, nonce?, threshold?)` | Create remove signer proposal |
| `createChangeThresholdProposal(threshold, nonce?)` | Create threshold change proposal |
| `createSwitchPsmProposal(endpoint, pubkey, nonce?)` | Create PSM switch proposal |
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
| `signAccountId(id)` | Sign account ID for auth |
| `signCommitment(hex)` | Sign commitment/word |

#### AccountInspector

| Method | Description |
|--------|-------------|
| `fromBase64(data)` | Inspect base64-encoded account |
| `fromAccount(account)` | Inspect Account object |

Returns `DetectedMultisigConfig`:
- `threshold`: number
- `numSigners`: number
- `signerCommitments`: string[]
- `psmEnabled`: boolean
- `psmCommitment`: string
- `vaultBalances`: { faucetId, amount }[]

---

## Rust SDK Guide

### Installation & Setup

```rust
use miden_multisig_client::{
    MultisigClient, MultisigClientBuilder,
    MultisigAccount, TransactionType,
    Proposal, ProposalStatus,
    KeyManager, PsmKeyStore,
    Endpoint, Word, AccountId, SecretKey,
};

// Build client with fluent API
let mut client = MultisigClient::builder()
    .miden_endpoint(Endpoint::new("http://localhost:57291"))
    .psm_endpoint("http://localhost:50051")
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

// Register with PSM
client.push_account().await?;

println!("Created account: {}", account.id());
println!("Threshold: {}", account.threshold()?);
println!("Signers: {:?}", account.cosigner_commitments_hex());
```

### Loading Existing Accounts

```rust
// Pull account from PSM (as a cosigner)
let account = client.pull_account(account_id).await?;

// Sync with Miden network
client.sync().await?;

// Inspect account
println!("Threshold: {}", account.threshold()?);
println!("Nonce: {}", account.nonce());
println!("PSM enabled: {}", account.psm_enabled()?);
```

### Transaction Types

```rust
// P2ID Transfer
let tx = TransactionType::transfer(recipient_id, faucet_id, 1000);

// Consume Notes
let tx = TransactionType::consume_notes(vec![note_id1, note_id2]);

// Add Cosigner
let tx = TransactionType::add_cosigner(new_commitment);

// Remove Cosigner
let tx = TransactionType::remove_cosigner(commitment_to_remove);

// Update Signers (change threshold and/or signer set)
let tx = TransactionType::update_signers(new_threshold, new_signer_list);

// Switch PSM Provider
let tx = TransactionType::switch_psm(new_endpoint, new_commitment);
```

### Proposal Operations

```rust
// Create and submit proposal
let proposal = client.propose_transaction(tx).await?;
println!("Proposal ID: {}", proposal.id);

// Or with offline fallback
match client.propose_with_fallback(tx).await? {
    ProposalResult::Online(proposal) => {
        println!("Submitted to PSM: {}", proposal.id);
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
        ProposalStatus::Pending { signatures_collected, signatures_required, signers } => {
            println!("{}: {}/{} signatures",
                proposal.id, signatures_collected, signatures_required);
            println!("  Signed by: {:?}", signers);
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
// Create offline proposal (when PSM unavailable)
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

### API Reference

#### MultisigClient

| Method | Description |
|--------|-------------|
| `builder()` | Create builder for configuration |
| `create_account(threshold, commitments)` | Create new multisig |
| `pull_account(id)` | Join existing multisig |
| `push_account()` | Register account with PSM |
| `sync()` | Sync with Miden network |
| `account()` | Get loaded account (Option) |
| `account_id()` | Get account ID (Option) |
| `user_commitment()` | Get user's key commitment |
| `user_commitment_hex()` | Get commitment as hex |
| `propose_transaction(tx)` | Create and submit proposal |
| `propose_with_fallback(tx)` | Online or offline proposal |
| `list_proposals()` | List pending proposals |
| `sign_proposal(id)` | Sign a proposal |
| `execute_proposal(id)` | Execute ready proposal |
| `create_proposal_offline(tx)` | Create offline proposal |
| `sign_imported_proposal(exported)` | Sign offline proposal |
| `execute_imported_proposal(exported)` | Execute offline proposal |
| `export_proposal(id, path)` | Export to file |
| `import_proposal(path)` | Import from file |
| `list_consumable_notes()` | List available notes |
| `list_consumable_notes_filtered(filter)` | Filter notes |

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
| `psm_enabled()` | PSM integration enabled |
| `psm_commitment()` | PSM server commitment |

#### TransactionType

| Variant | Description |
|---------|-------------|
| `P2ID { recipient, faucet_id, amount }` | Transfer funds |
| `ConsumeNotes { note_ids }` | Consume notes |
| `AddCosigner { new_commitment }` | Add signer |
| `RemoveCosigner { commitment }` | Remove signer |
| `UpdateSigners { new_threshold, signer_commitments }` | Update config |
| `SwitchPsm { new_endpoint, new_commitment }` | Switch PSM |

#### ProposalStatus

| Variant | Description |
|---------|-------------|
| `Pending { signatures_collected, signatures_required, signers }` | Collecting sigs |
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
  psmCommitment,
  psmEnabled: true,
};

const treasury = await client.create(config, ceoSigner);
await treasury.registerOnPsm();

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

### Use Case 4: Note Consumption

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
  "transaction_type": "P2ID",
  "tx_summary": { "...base64 encoded..." },
  "signatures": [
    {
      "signer_commitment": "0x1234...",
      "signature": "0x5678..."
    }
  ],
  "signatures_required": 2,
  "metadata": {
    "recipient_id": "0x...",
    "faucet_id": "0x...",
    "amount": "1000"
  }
}
```

## Version Compatibility

| SDK Version | miden-client | miden-sdk (npm) | Notes |
|-------------|--------------|-----------------|-------|
| 0.1.x | 0.12.5 | ^0.12.5 | Current |

### Breaking Changes

Check the [CHANGELOG](../CHANGELOG.md) for breaking changes between versions.

---

## Additional Resources

- [Miden Documentation](https://docs.miden.io/)
- [PSM Documentation](../crates/server/README.md)
  - [PSM Specification](../spec/index.md)
- [Example Applications](../examples/)
  - [Web Example](../examples/web/)
  - [CLI Demo](../examples/demo/)
  - [Rust Example](../examples/rust/)
