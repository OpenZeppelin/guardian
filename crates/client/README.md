# Guardian Client

A minimal Rust client library for interacting with the Guardian gRPC service.

## API Reference

### Client Creation

```rust
use std::sync::Arc;

use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use guardian_client::{FalconKeyStore, GuardianClient};

// Without authentication (only for configure endpoint)
let client = GuardianClient::connect("http://localhost:50051").await?;

// With request signing (required for all other endpoints)
let secret_key = SecretKey::new();
let signer = Arc::new(FalconKeyStore::new(secret_key));
let client = GuardianClient::connect("http://localhost:50051")
    .await?
    .with_signer(signer);
```

## Authentication

The client uses Falcon Poseidon2 signatures for authenticated requests. Here is how to set it up:

### 1. Create a Signer

```rust
use std::sync::Arc;

use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use guardian_client::FalconKeyStore;

// Generate a new secret key
let secret_key = SecretKey::new();
let signer = Arc::new(FalconKeyStore::new(secret_key));

// Get the public key for authorization
let pubkey_hex = signer.public_key_hex();
```

### 2. Configure Client with Signer

```rust
let client = GuardianClient::connect("http://localhost:50051")
    .await?
    .with_signer(signer.clone());
```

### 3. Set Up Account Authorization

```rust
use guardian_client::auth;

// Add the public key to the account's authorized cosigners
let auth_config = auth::miden_falcon_rpo_auth(vec![pubkey_hex]);
```

## Server Signature Verification

After pushing a delta, the server returns an Acknowledgment signature that signs the new commitment. You should verify this signature to ensure the server is signing with the expected public key.

```rust
use guardian_client::verify_commitment_signature;

let push_response = client.push_delta(&account_id, 1, prev_commitment, delta).await?;

if let Some(delta) = &push_response.delta {
    if !delta.ack_sig.is_empty() {
        // Get server public key (provided during account setup or configuration)
        let server_pubkey = "0x..."; // Server's public key hex

        let is_valid = verify_commitment_signature(
            &delta.new_commitment,
            server_pubkey,
            &delta.ack_sig
        )?;

        if is_valid {
            println!("Server signature verified!");
        } else {
            println!("Server signature verification failed!");
        }
    }
}
```

The server signs the `new_commitment` (the resulting commitment after applying the delta) to provide cryptographic proof that it processed the delta correctly.

### Example

```bash
cargo run --package guardian-client --example e2e
```
 
