# Miden Multisig Client

High-level Rust SDK built on top of `miden-client` for private multisignature workflows on Miden. The crate wraps the on-chain multisig contracts plus Private State Manager (PSM) coordination so you can:

- create multisig accounts, register them with a PSM, and keep state off-chain,
- propose, sign, and execute transactions with threshold enforcement,
- fall back to fully offline flows when connectivity is limited,
- export/import proposals as files for sharing using side channels,

## How Private Multisigs & PSM Work

Miden multisig accounts store their authentication logic on-chain, but **their state (signers, metadata, proposals)** is kept private. PSM acts as a coordination server:

1. A proposer pushes a delta (transaction plan) to Private State Manager (PSM). PSM tracks who signed and emits an ack signature once the threshold is met.
2. Cosigners fetch pending deltas, verify details locally, sign the transaction summary, and push signatures back to PSM.
3. Once ready, any cosigner builds the final transaction using all cosigner signatures + the PSM ack, executes it on-chain.

## Installation

Add the crate to your workspace (already available in this repo). From another project:

```toml
[dependencies]
miden-multisig-client = { git = "https://github.com/OpenZeppelin/private-state-manager", package = "miden-multisig-client" }
```

## Quick Start

```rust
use miden_client::rpc::Endpoint;
use miden_multisig_client::{MultisigClient, TransactionType};
use miden_objects::{Word, account::AccountId};

# async fn example() -> anyhow::Result<()> {
let signer1: Word = /* your RPO Falcon commitment */ Word::default();
let signer2: Word = Word::default();

let mut client = MultisigClient::builder()
    .miden_endpoint(Endpoint::new("http://localhost:57291"))
    .psm_endpoint("http://localhost:50051")
    // Directory where the underlying miden-client SQLite store will live
    .account_dir("/tmp/multisig")
    // Generate a new Falcon keypair for PSM authentication (builder can also accept your own key)
    .generate_key()
    .build()
    .await?;

let account = client.create_account(2, vec![signer1, signer2]).await?;
println!("Account registered on PSM endpoint: {}", client.psm_endpoint());
# Ok(())
# }
```

## Core Workflow Examples

### Propose ➜ Sign ➜ Execute

```rust
use miden_multisig_client::TransactionType;
use miden_objects::account::AccountId;

let recipient = AccountId::from_hex("0x7bfb0f38b0fafa103f86a805594170")?;
let faucet = AccountId::from_hex("0x7bfb0f38b0fafa103f86a805594171")?;
let tx = TransactionType::transfer(recipient, faucet, 1_000);

// Proposer creates the delta on PSM
let proposal = client.propose_transaction(tx).await?;

// Second cosigner lists available proposals and signs the matching one
let proposals = client.list_proposals().await?;
let to_sign = proposals
    .iter()
    .find(|p| p.id == proposal.id)
    .expect("proposal not found");
client.sign_proposal(&to_sign.id).await?;

// Once threshold is met, any cosigner can execute
client.execute_proposal(&proposal.id).await?;
```

### Fallback to Offline (if PSM unavailable)

If the PSM endpoint can’t be reached, the SDK automatically produces an offline proposal so you can continue via side-channel sharing:

```rust
use miden_multisig_client::{ProposalResult, TransactionType};

let tx = TransactionType::consume_notes(vec![note_id]);
match client.propose_with_fallback(tx).await? {
    ProposalResult::Online(p) => {
        println!("Proposal {} is live on PSM", p.id);
    }
    ProposalResult::Offline(exported) => {
        let path = "proposal_offline.json";
        std::fs::write(path, exported.to_json()?)?;
        println!("PSM unavailable. Share {} with cosigners, collect signatures, then run `execute_imported_proposal` once ready.", path);
    }
}
```

#### Fully Offline Signing and Execution

```rust
use miden_multisig_client::TransactionType;

let tx = TransactionType::switch_psm("https://psm.example.com", new_psm_commitment);
let mut exported = client.create_proposal_offline(tx).await?;

// Cosigner signs locally
client.sign_imported_proposal(&mut exported)?;
std::fs::write("proposal_signed.json", exported.to_json()?)?;

// Once enough signatures are collected offline:
client.execute_imported_proposal(&exported).await?;
```

### Listing Notes

List all notes that are currently consumable by the loaded account:

```rust
let notes = client.list_consumable_notes().await?;
for note in notes {
    println!("Note {} has {} assets", note.id.to_hex(), note.assets.len());
}
```

List notes from a specific faucet with a minimum amount filter:

```rust
use miden_multisig_client::NoteFilter;

let faucet = AccountId::from_hex("0x7bfb0f38b0fafa103f86a805594170")?;
let filter = NoteFilter::by_faucet_min_amount(faucet, 5_000);
let spendable = client.list_consumable_notes_filtered(filter).await?;
```

## Demo CLI

 Run the Terminal UI demo in [`examples/demo`](../../examples/demo/), which exercises the same APIs for account management, note listing, proposal signing, and offline export/import.

Contributions and bug reports are welcome!

