# miden-rpc-client

Minimal Miden RPC client with full API access using tonic-generated code from miden-node proto definitions.

## Available RPC Methods

### Via `client_mut()`

```rust
let mut client = MidenRpcClient::connect("https://rpc.testnet.miden.io").await?;

let status = client.client_mut().status(tonic::Request::new(())).await?;
let block = client.client_mut().get_block_by_number(request).await?;
```

**Full method list:**
1. `Status` - Node status information
2. `CheckNullifiers` - Nullifier proofs
3. `GetAccountDetails` - Account state by ID
4. `GetAccountProof` - Account state proof
5. `GetBlockByNumber` - Raw block data
6. `GetBlockHeaderByNumber` - Block headers with optional MMR proof
7. `GetNotesById` - Notes matching IDs
8. `GetNoteScriptByRoot` - Note script by root hash
9. `SubmitProvenTransaction` - Submit single transaction
10. `SubmitProvenBatch` - Submit transaction batch
11. `SyncNullifiers` - Nullifiers by prefix
12. `SyncAccountVault` - Account vault updates
13. `SyncNotes` - Note synchronization
14. `SyncState` - Full state sync
15. `SyncStorageMaps` - Storage map updates
16. `SyncTransactions` - Transaction records

### Convenience Methods

High-level wrappers for common operations:

```rust
// Get node status
let status = client.get_status().await?;

// Get block header (optionally with MMR proof)
let header = client.get_block_header(Some(12345), true).await?;

// Submit transaction
let response = client.submit_transaction(proven_tx_bytes).await?;

// Sync state for accounts and notes
let sync_response = client.sync_state(
    block_num,
    account_ids,
    note_tags,
).await?;

// Check nullifiers
let proofs = client.check_nullifiers(nullifiers).await?;

// Get notes by ID
let notes = client.get_notes_by_id(note_ids).await?;

// Get account commitment (convenience wrapper)
let commitment = client.get_account_commitment(&account_id).await?;
```

## Usage Example

```rust
use miden_rpc_client::MidenRpcClient;
use miden_objects::account::AccountId;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to Miden testnet
    let mut client = MidenRpcClient::connect("https://rpc.testnet.miden.io").await?;

    // Example 1: Get node status
    let status = client.get_status().await?;
    println!("Node version: {}", status.version);

    // Example 2: Get account commitment
    let account_id = AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f")?;
    let commitment = client.get_account_commitment(&account_id).await?;
    println!("Commitment: {}", commitment);

    // Example 3: Get latest block header
    let header = client.get_block_header(None, false).await?;
    println!("Latest block: {:?}", header.block_header);

    // Example 4: Use full API for advanced operations
    let notes_request = tonic::Request::new(
        crate::note::NoteIdList { ids: vec![/* note IDs */] }
    );
    let notes = client.client_mut().get_notes_by_id(notes_request).await?;

    Ok(())
}
```

## Proto Definitions

Proto files are sourced directly from [miden-node](https://github.com/0xPolygonMiden/miden-node/tree/next/proto/proto):
- `proto/rpc.proto` - Main API service
- `proto/types/*.proto` - Common types (account, note, transaction, etc.)
- `proto/store/*.proto` - Store-specific types

