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
    .miden_endpoint(Endpoint::try_from("http://localhost:57291")?)
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

## Configuration

Beyond the endpoints and the account directory, the builder carries three
optional configuration surfaces. The cross-SDK reference, including the
TypeScript equivalents, is
[`docs/MULTISIG_SDK.md`](../../docs/MULTISIG_SDK.md).

### Miden node RPC (`rpc_config`)

`RpcConfig` sets the per-request gRPC deadline and the retry budget for
idempotent node reads. The policy wraps the node client itself, so every read
this SDK or the underlying `miden-client` issues (syncs, account, note, and
block lookups) is covered.

```rust
use miden_multisig_client::{RpcConfig, RpcRetryPolicy};

let rpc_config = RpcConfig::new()
    .with_timeout_ms(15_000)?
    .with_retry_policy(RpcRetryPolicy::new(3));

let mut client = MultisigClient::builder()
    .miden_endpoint(Endpoint::devnet())
    .guardian_endpoint("http://localhost:50051")
    .account_dir("/tmp/multisig")
    .rpc_config(rpc_config)
    .generate_key()
    .build()
    .await?;
```

The defaults are a 10-second deadline and two total attempts (one classified,
jittered retry); `RpcRetryPolicy::new(1)` opts out. Rate limiting and
transport-shaped connection failures retry. Permanent failures (invalid
argument, not found, authentication, TLS or certificate problems, invalid
endpoint) return immediately without consuming the budget, and once the
budget is exhausted the final upstream error is returned unchanged.

**Transaction submission is never retried** under any configuration: a
submission whose outcome is unknown could execute twice if re-sent. The same
policy covers the note transport, where fetches and stream establishment
retry but note sends are never retried in-call (the client's relay outbox
re-sends undelivered notes on later syncs).

### Note transport endpoint (`note_transport_endpoint`)

Private notes are relayed through a note transport service that is separate
from the node RPC. The testnet and devnet presets derive the transport
endpoint automatically; a custom node endpoint has no derivable transport
service, so private-note relay stays disabled until this is set explicitly.

```rust
let mut client = MultisigClient::builder()
    .miden_endpoint(Endpoint::try_from("https://my-node.internal:57291")?)
    .note_transport_endpoint("https://my-transport.internal")
    .guardian_endpoint("http://localhost:50051")
    .account_dir("/tmp/multisig")
    .generate_key()
    .build()
    .await?;
```

### Remote prover (`prover_config`)

`ProverConfig` overrides the remote prover endpoint and the proof retry
policy. Without it, the devnet and testnet presets use their public provers
and every other endpoint proves locally. A custom URL must be absolute
HTTP(S) and never falls back to a default endpoint. Remote proving gets two
total attempts by default and local proving is never retried; retries apply
only to transient proof failures, so transaction execution, submission, local
state application, and GUARDIAN calls each run once.

```rust
use miden_multisig_client::{ProverConfig, ProverRetryPolicy};

let prover_config = ProverConfig::new()
    .with_url("https://prover.example")?
    .with_retry_policy(ProverRetryPolicy::new(4));
```

## Core Workflow Examples

### Propose ➜ Sign ➜ Execute

```rust
use miden_multisig_client::TransactionType;
use miden_objects::account::AccountId;

let recipient = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b")?;
let faucet = AccountId::from_hex("0x7c7c7c7c7c7c7c017c7c7c7c7c7c7c")?;
let tx = TransactionType::transfer(recipient, faucet, 1_000);

// P2ID notes are public by default. For a private note (only its hash is
// published on chain; the recipient needs the note shared out-of-band) use
// `transfer_with_note_type`. `NoteType` is re-exported from `miden_protocol::note`.
use miden_protocol::note::NoteType;
let tx = TransactionType::transfer_with_note_type(recipient, faucet, 1_000, NoteType::Private);

// For a reclaimable and/or timelocked send use `transfer_p2ide` (issue #366):
// presence of either height creates a P2IDE note instead of a plain P2ID note.
// `P2ideHeights` uses `NonZeroU32` — the invalid zero height ("no constraint"
// on-chain) is unrepresentable.
use std::num::NonZeroU32;
use miden_multisig_client::P2ideHeights;

let tx = TransactionType::transfer_p2ide(
    recipient, faucet, 1_000, NoteType::Public,
    P2ideHeights {
        reclaim: NonZeroU32::new(500_000), // sender may reclaim from this block on
        timelock: None,                    // no consume-not-before constraint
    },
);

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

### Draining the Private-Note Transport Backlog

After recovery on a fresh device the local store has no note-transport
cursor — and in a store shared with other accounts, sync may have advanced
the cursor past private notes addressed to the newly recovered account.
`drain_private_note_backlog` rescans the full transport backlog for every
tracked note tag, regardless of the stored cursor. Run it after
`pull_account`: the drain only sees tags tracked in the store, and
`pull_account` inserting the account is what tracks its tag.

```rust
client.pull_account(account_id).await?;

let report = client.drain_private_note_backlog().await?;
// report.status: Completed | Unavailable | Failed
// report.imported: number of newly imported note records
```

Transport problems are reported in the result, never returned as errors:
`Unavailable` means no transport endpoint is configured or the transport
could not be reached before anything was imported; `Failed` with
`retryable: true` keeps any partial progress and rerunning the drain
continues it. The drain is idempotent and never regresses the stored
transport cursor.

Transport recovery is **not a backup**: it is bounded by the transport
service's retention. Senders may deliver private notes out-of-band without
using the transport, and relayed blobs are pruned after the retention
window.

### Recovering From a Dead Transaction (Abandon)

If `execute_proposal` dies after guardian approval (RPC submit failure,
prover timeout, crash), the approved candidate keeps the account locked on
GUARDIAN. Record an abandon intent and poll for the resolution:

```rust
use miden_multisig_client::{AbandonRequestState, AbandonStatus};

// The nonce the proposal was pushed with (committed account nonce + 1).
let state = client.abandon_candidate(nonce).await?;
assert_eq!(state, AbandonRequestState::Pending);

// The guardian's worker confirms over a short quarantine (typically well
// under a minute) that the transaction did not land, then releases the
// account.
loop {
    match client.abandon_status(nonce).await? {
        AbandonStatus::Waiting => tokio::time::sleep(Duration::from_secs(5)).await,
        AbandonStatus::Abandoned => break,           // account released
        AbandonStatus::Landed => break,              // tx landed after all
        AbandonStatus::Unexpected => anyhow::bail!("unexpected candidate state"),
    }
}
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

let faucet = AccountId::from_hex("0x7c7c7c7c7c7c7c017c7c7c7c7c7c7c")?;
let filter = NoteFilter::by_faucet_min_amount(faucet, 5_000);
let spendable = client.list_consumable_notes_filtered(filter).await?;
```

### Recovering Notes From Pending Proposals

After key-based recovery the local Miden store starts empty. v2
`consume_notes` proposals embed the serialized notes they consume, and
`import_notes_from_proposals` turns those embedded bytes back into store
records: it fetches each note's on-chain inclusion proof and imports the
note individually, so it works for private notes too.

```rust
let proposals = client.list_proposals().await?;
let outcomes = client.import_notes_from_proposals(&proposals).await;
for outcome in &outcomes {
    println!("{}: {} {:?}", outcome.identifier, outcome.status, outcome.reason);
}
client.sync().await?; // verifies the imported notes
```

Each unique embedded note gets its own `NoteImportOutcome` (`Imported`,
`AlreadyPresent`, `AlreadyConsumed`, `NotCommitted`, `Invalid`, or `Failed`);
duplicates across proposals fold into one outcome, and a malformed or
failing note never blocks the others, which is why the method returns a
plain `Vec` instead of `Result`. Notes not yet on chain are recorded in
`Expected` state (their tag is tracked so a later sync picks them up) and
reported retryable.

Proposals are opportunistic recovery material, not a backup: v1 proposals
carry no note bytes, proposals disappear once canonicalized, and embedded
note bytes are visible to the GUARDIAN operator (existing v2 behavior, not a
new exposure).

### Custom Proposal Types

GUARDIAN accepts any non-empty `proposal_type`, so an integration can propose a
transaction the SDK does not model (an agglayer bridge note, an arbitrary dApp
transaction) under its own label. The SDK normalizes the label to lowercase
`snake_case` (trims and lowercases it, then requires `[a-z0-9_]+` — the same
shape as built-in labels like `add_signer`), so `"B2Agg"` becomes `b2agg` and
`"add signer"` / `"add-signer"` are rejected. Such a proposal buckets to
`TransactionType::Custom` and keeps its normalized label in
`ProposalMetadata.proposal_type`. It can be listed, signed, and
exported/imported through the normal flow, but the generic SDK cannot build its
on-chain transaction — the integration owns that recipe and drives execution.

```rust
use miden_client::Serializable;
use miden_multisig_client::{build_p2id_transaction_request, generate_salt};
use miden_protocol::note::NoteType;

// Producer: build a transaction and propose it under a custom label.
let salt = generate_salt();
let mut request = build_p2id_transaction_request(
    account.inner(), recipient, vec![asset], NoteType::Public, salt, std::iter::empty(),
)?;
let proposal = client.propose_custom_transaction(&request.to_bytes(), "b2agg").await?;

// Cosigners review and sign through the usual list/sign flow.

// Producer (once threshold is met): bind-check the request, fetch the validated
// advice, inject it into the request, and submit. `prepare_custom_execution`
// verifies the request against the signed commitment *before* the GUARDIAN ack,
// re-executing at the proposal's anchored reference block; `submit_transaction`
// takes the proposal id to execute at that same anchor, since the collected
// signatures only authorize the summary produced there.
let advice = client.prepare_custom_execution(&proposal.id, &request.to_bytes()).await?;
request.advice_map_mut().extend(advice);
client.submit_transaction(&proposal.id, request).await?;
```

The integration keeps only its own recipe (build inputs + salt) so it can
reproduce the exact transaction at execute time — the SDK does not store the
serialized request. The binding check guarantees the rebuilt transaction matches
the commitment the cosigners signed.

## Delta History

Render the account's confirmed history after recovery. One
`HistoryPage` per call, newest-first by nonce, with typed note summaries:

```rust
let mut cursor: Option<String> = None;
loop {
    let page = client.delta_history(Some(50), cursor.take()).await?;
    for entry in &page.entries {
        // entry.status is HistoryEntryStatus::Canonical; notes carry a
        // typed tag, visibility, assets, and counterparties.
        println!("{} at {}", entry.nonce, entry.timestamp);
    }
    match page.next_cursor {
        Some(next) => cursor = Some(next),
        None => break,
    }
}
```

Only canonical (confirmed) deltas appear — pending proposals live on
`list_proposals()` — and only transactions pushed through Guardian are
visible to it.

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
