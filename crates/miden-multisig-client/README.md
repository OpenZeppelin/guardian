# Miden Multisig Client

High-level Rust SDK built on top of `miden-client` for private multisignature workflows on Miden. The crate wraps the on-chain multisig contracts plus Guardian coordination so you can:

- create multisig accounts, register them with a GUARDIAN, and keep state off-chain,
- propose, sign, and execute transactions with threshold enforcement,
- fall back to offline `SwitchGuardian` workflows when connectivity is limited,
- export/import proposals as files for sharing using side channels,

## How Private Multisigs & GUARDIAN Work

Miden multisig accounts store their authentication logic on-chain, but **their state (signers, metadata, proposals)** is kept private. GUARDIAN acts as a coordination server:

1. A proposer pushes a delta (transaction plan) to Guardian. GUARDIAN tracks who signed and emits an ack signature once the threshold is met.
2. Cosigners fetch pending deltas, verify details locally, sign the transaction summary, and push signatures back to GUARDIAN.
3. Once ready, any cosigner builds the final transaction using all cosigner signatures + the GUARDIAN ack, executes it on-chain.

## Installation

Add the crate to your workspace (already available in this repo). From another project:

```toml
[dependencies]
miden-multisig-client = { git = "https://github.com/OpenZeppelin/guardian", package = "miden-multisig-client" }
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
    .guardian_endpoint("http://localhost:50051")
    // Directory where the underlying miden-client SQLite store will live
    .account_dir("/tmp/multisig")
    // Generate a new Falcon keypair for GUARDIAN authentication (builder can also accept your own key)
    .generate_key()
    .build()
    .await?;

let account = client.create_account(2, vec![signer1, signer2]).await?;
println!("Account registered on GUARDIAN endpoint: {}", client.guardian_endpoint());
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

// Proposer creates the delta on GUARDIAN
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

### Fallback to Offline (if GUARDIAN unavailable)

If the GUARDIAN endpoint can’t be reached, the SDK can produce an offline proposal only for `SwitchGuardian` transactions:

```rust
use miden_multisig_client::{ProposalResult, TransactionType};

let tx = TransactionType::switch_guardian("https://new-guardian.example.com", new_guardian_commitment);
match client.propose_with_fallback(tx).await? {
    ProposalResult::Online(p) => {
        println!("Proposal {} is live on GUARDIAN", p.id);
    }
    ProposalResult::Offline(exported) => {
        let path = "proposal_offline.json";
        std::fs::write(path, exported.to_json()?)?;
        println!("GUARDIAN unavailable. Share {} with cosigners, collect signatures, then run `execute_imported_proposal` once ready.", path);
    }
}
```

#### Fully Offline Signing and Execution

```rust
use miden_multisig_client::TransactionType;

let tx = TransactionType::switch_guardian("https://guardian.example.com", new_guardian_commitment);
let mut exported = client.create_proposal_offline(tx).await?;

// Cosigner signs locally
client.sign_imported_proposal(&mut exported).await?;
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


## Consume-notes metadata versions

`consume_notes` proposals come in two metadata shapes. The discriminator
is the `consume_notes_metadata_version` field on the wire.

- **v1 (legacy)** — `consume_notes_metadata_version` absent on the wire.
  The proposal carries only `note_ids`; the verifier rebuilds the
  transaction request by fetching each note from its **own local Miden
  store**. If the verifier does not have the note locally (cursor
  advanced past the block, store was wiped, private-note transport
  pruned the blob), verification fails with
  `MultisigError::LegacyConsumeNotesNoteMissing` and the cosigner
  cannot sign. This is the failure tracked by
  [issue #229](https://github.com/OpenZeppelin/guardian/issues/229).
- **v2 (self-contained)** — `consume_notes_metadata_version: 2` plus a
  `consume_notes_notes` array carrying base64-serialized `Note` bytes
  aligned by index with `note_ids`. Verification rebuilds the request
  from the embedded notes alone — no local-store read, no network
  call. This restores the same "rebuild from signed metadata" invariant
  every other proposal type already satisfied (and that audit finding
  **M-08** remediated for `p2id`).

Proposal creation always emits v2 starting with this release; the
proposer is the one party guaranteed to hold the notes locally. The
per-proposal v2 payload is capped at `MAX_CONSUME_NOTES_METADATA_BYTES`
(256 KiB) and the size is enforced at creation time so the failure
surfaces to the proposer before any signature collection begins.

### Error taxonomy

All four errors below carry a stable, cross-SDK string code via
`MultisigError::code()`. The TS SDK exposes the same identifiers as
`Error.code`.

| Variant | `.code()` | When |
|---|---|---|
| `NoteBindingMismatch` | `consume_notes_note_binding_mismatch` | v2: `notes.len() != note_ids.len()`, or `note.id() != note_ids[i]` |
| `UnsupportedMetadataVersion { found }` | `consume_notes_unsupported_metadata_version` | Unrecognized version (including v1 on a cut-over build) |
| `ConsumeNotesMetadataOversize { limit, actual }` | `consume_notes_metadata_oversize` | v2 metadata serialization exceeds 256 KiB at creation |
| `LegacyConsumeNotesNoteMissing { note_id }` | `consume_notes_legacy_note_missing` | v1 path: local store does not contain the referenced note |

### Cut-over policy

The `legacy-consume-notes` Cargo feature (default-on in this transitional
release) gates whether the crate accepts v1 metadata for verification.
A future cut-over release will ship with `default = []`, at which point
v1 proposals are refused with `UnsupportedMetadataVersion { found: None }`
on every code path. Deployments should drain or re-propose any v1
`consume_notes` proposals in flight before upgrading past the cut-over
client version. Tracked by spec
[`006-consume-notes-metadata`](../../speckit/features/006-consume-notes-metadata/spec.md).

## Demo CLI

 Run the Terminal UI demo in [`examples/demo`](../../examples/demo/), which exercises the same APIs for account management, note listing, proposal signing, and offline export/import.

Contributions and bug reports are welcome!
