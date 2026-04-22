# miden-rpc-client

Minimal Miden RPC client with full API access using tonic-generated code from miden-node proto definitions.

## Available RPC Methods

### Via `client_mut()`

```rust
let mut client = MidenRpcClient::connect("https://rpc.devnet.miden.io").await?;

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

### API usage

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
