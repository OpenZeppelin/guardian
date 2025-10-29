# Rust E2E Example - Multi-Client PSM Workflow

End-to-end example demonstrating **multi-client collaboration** using PSM for state coordination with custom auth components.

## Overview

This example demonstrates a complete **multi-client workflow** where two clients collaborate through PSM:

1. **Create Account**: Multisig PSM account with 2 cosigners (Client 1 & Client 2)
2. **Client 1 - Push State**: Initial account configuration to PSM
3. **Client 2 - Pull State**: Retrieve and reconstruct account from PSM
4. **Client 2 - Create & Push Delta**: Modify account state via PSM
5. **Client 2 - Verify & Transact**: Use PSM ack signature for on-chain transactions

## Key Features

### 🔐 Custom PSM Auth Component

The example uses a **custom MASM auth component** where authentication logic is embedded in the account code:

```masm
const.PSM_SELECTOR_SLOT=4      # [1,0,0,0] = ON, [0,0,0,0] = OFF
const.PSM_PUBLIC_KEY_MAP_SLOT=5 # Map of PSM pubkey hashes

export.verify_psm_signature
    # Verify PSM signature if selector is ON
    # Uses rpo_falcon512 signature verification
end

export.auth__
    exec.verify_psm_signature
    dropw dropw
end
```

**Storage Layout:**
- **Slot 4**: PSM selector (toggle auth on/off)
- **Slot 5**: PSM public key map

### 👥 Multi-Client Coordination

- **Client 1**: Creates account and pushes to PSM
- **Client 2**: Pulls state, reconstructs account, creates deltas
- **PSM**: Validates deltas and provides ack signatures

### ✅ Full Account Reconstruction

Accounts are stored with complete `builder_params`:
- `init_seed`: Original 32-byte seed
- `auth_masm`: Complete MASM source code
- `psm_pubkey_hash`: For signature verification
- `cosigner_pubkeys`: All authorized signers

This enables **any client** to reconstruct a fully functional account.

## Running

```bash
# Start PSM server (in another terminal)
cd ../..
cargo run --package private-state-manager-server --bin server

# Run the multi-client example
cd examples/rust
cargo run --bin e2e

# Optional: Connect to Miden network
export MIDEN_NODE_URL=https://testnet-rpc.miden.io
cargo run --bin e2e
```

## Workflow Details

### Step 1: Create Multisig PSM Account

```rust
// Generate keys for both clients
let client1_pubkey = generate_key();
let client2_pubkey = generate_key();
let psm_server_pubkey = generate_key();

// Compile custom auth component
let auth_component = AccountComponent::compile(
    PSM_MULTISIG_AUTH,
    assembler,
    vec![slot_4, slot_5]  // PSM selector + pubkey map
);

// Create account with both cosigners
let account = AccountBuilder::new(seed)
    .with_component(auth_component)
    .with_component(BasicWallet)
    .build()?;
```

**Key Point**: Auth logic (PSM signature verification) is in the account code, not metadata.

### Step 2: Client 1 - Push State to PSM

```rust
// Client 1 authenticates with their key
let client1 = PsmClient::connect(endpoint)
    .await?
    .with_auth(Auth::FalconRpoSigner(client1_signer));

// Configure PSM with both cosigners
let auth_config = AuthConfig {
    auth_type: Some(AuthType::MidenFalconRpo(MidenFalconRpoAuth {
        cosigner_pubkeys: vec![client1_pubkey, client2_pubkey],
    })),
};

// Store complete account state
let state = json!({
    "data": account_base64,
    "builder_params": {
        "init_seed": seed_base64,
        "auth_masm": PSM_MULTISIG_AUTH,
        "psm_pubkey_hash": psm_hash_hex,
        "cosigner_pubkeys": [client1, client2],
    }
});

let response = client1.configure(&account_id, auth_config, state, "Filesystem").await?;
```

**Result**: Account stored in PSM, accessible by both clients.

### Step 3: Client 2 - Pull State from PSM

```rust
// Client 2 authenticates with their key
let client2 = PsmClient::connect(endpoint)
    .await?
    .with_auth(Auth::FalconRpoSigner(client2_signer));

// Retrieve account state
let response = client2.get_state(&account_id).await?;
let state_json = serde_json::from_str(&response.state.state_json)?;

// Deserialize account
let account_bytes = base64::decode(&state_json["data"])?;
let account = Account::read_from_bytes(&account_bytes)?;

// Extract seed and auth MASM
let seed = extract_seed(&state_json["builder_params"]["init_seed"])?;
let auth_masm = state_json["builder_params"]["auth_masm"].as_str()?;
```

**Result**: Client 2 has complete account information, including seed.

### Step 4: Client 2 - Reconstruct Account & Create Delta

```rust
// Reconstruct auth component with same storage slots
let auth_component = AccountComponent::compile(
    auth_masm,
    assembler,
    vec![slot_4, slot_5]
)?;

// Rebuild account with original seed
let (rebuilt_account, _) = AccountBuilder::new(seed)
    .with_component(auth_component)
    .with_component(BasicWallet)
    .build()?;

// Verify reconstruction
assert_eq!(rebuilt_account.id(), original_account.id());

// Create delta (e.g., toggle PSM auth OFF)
let mut storage_delta = AccountStorageDelta::default();
storage_delta.set_item(4, Word::from([0, 0, 0, 0])); // PSM OFF

let delta = AccountDelta::new(
    account_id,
    storage_delta,
    AccountVaultDelta::default(),
    new_nonce,
)?;
```

**Result**: Client 2 has fully functional account and created a delta.

### Step 5: Client 2 - Push Delta to PSM

```rust
let response = client2
    .push_delta(&account_id, nonce, prev_commitment, delta_json)
    .await?;

let ack_sig = response.delta.ack_sig;  // PSM's signature
let new_commitment = response.delta.new_commitment;
```

**Result**: Delta validated by PSM, ack signature provided.

### Step 6: Client 2 - Verify Ack & Prepare Transaction

```rust
// Verify PSM's signature
let valid = verify_commitment_signature(
    &new_commitment,
    &server_ack_pubkey,
    &ack_sig
)?;

if valid {
    // Apply delta locally
    account.apply_delta(&delta)?;

    // Ready for on-chain transaction:
    // 1. Create Miden transaction with updated account
    // 2. Include delta in transaction
    // 3. Prove transaction
    // 4. Submit with PSM's ack signature
}
```

**Result**: Delta authenticated, ready for on-chain submission.

## Account State Format

```json
{
  "data": "<base64-encoded Account bytes>",
  "account_id": "0x...",
  "builder_params": {
    "init_seed": "<base64 seed>",
    "account_type": "RegularAccountUpdatableCode",
    "storage_mode": "Private",
    "auth_masm": "<Complete MASM source>",
    "psm_pubkey": "0x...",
    "psm_pubkey_hash": "0x...",
    "cosigner_pubkeys": ["0x...", "0x..."]
  }
}
```

## PSM Benefits

✅ **Multi-Client Coordination**: Clients collaborate through PSM
✅ **Delta Validation**: PSM validates deltas before signing
✅ **Ack Signatures**: Cryptographic proof of PSM validation
✅ **Full Reconstruction**: Any cosigner can rebuild the account
✅ **Custom Auth**: Authentication logic embedded in account code
✅ **Flexible**: Toggle PSM auth on/off via storage slots

## Security Considerations

⚠️ **Seed Storage**: Seed stored in PSM means PSM can reconstruct accounts
- Encrypt seed at rest
- Use KMS for production
- Consider hardware security modules (HSM)

⚠️ **Cosigner Trust**: All cosigners can access and modify account state
- Ensure secure key management
- Audit delta history
- Use threshold signatures if needed

## Example Output

```
=== PSM Multi-Client E2E Flow ===

Setup: Generating keys...
  ✓ Client 1 pubkey: 0x1234...
  ✓ Client 2 pubkey: 0x5678...
  ✓ PSM server pubkey: 0xabcd...

Step 1: Creating multisig PSM account with 2 cosigners...
  ✓ Account ID: 0xef01...
  ✓ Commitment: 0x2345...
  ✓ Cosigners: client1, client2
  ✓ PSM auth enabled

Step 2: Client 1 - Push state to PSM...
  ✓ Account configured successfully
  ✓ Server ack pubkey: 0x6789...

Step 3: Client 2 - Pull state from PSM...
  ✓ Get state successful
    Commitment: 0x2345...
    ✓ Deserialized account
    ✓ Extracted seed
    ✓ Extracted auth MASM

Step 4: Client 2 - Reconstruct account and create delta...
  ✓ Account reconstructed
    Account ID match: true
    Current nonce: 0
  Creating delta to disable PSM auth...
  ✓ Created delta with nonce: 1

Step 5: Client 2 - Push delta to PSM...
  ✓ Delta pushed successfully
    New commitment: 0x3456...
    Server ack signature: 0xbcde...

Step 6: Client 2 - Verify PSM ack signature...
  ✓ PSM server signature is VALID
  ✓ Delta authenticated by PSM

  Account state after delta:
    New commitment: 0x3456...
    New nonce: 1

  Ready for on-chain transaction:
  ════════════════════════════════
  With the PSM-authenticated delta, you can now:
    1. Create a Miden transaction using the updated account
    2. Include the delta in the transaction
    3. Prove the transaction (expensive operation)
    4. Submit to Miden network with PSM's ack signature

  PSM Benefits:
    ✓ Delta authenticated by PSM before on-chain submission
    ✓ Multi-client coordination through PSM
    ✓ PSM signature proves delta was validated
    ✓ Reduces risk of invalid state transitions
```

## See Also

- [Web Example](../web/README.md) - TypeScript/WASM multi-tab demo
- [CLAUDE.md](../../CLAUDE.md) - Architecture overview
- [PSM Server](../../crates/server/) - Server implementation

