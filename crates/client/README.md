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

### Abandoning a Stuck Candidate

If an approved transaction dies client-side after guardian approval (RPC
submit failure, prover timeout, crash), its candidate delta keeps the
account locked — new proposals are answered `409 conflict_pending_delta` —
until the server's grace period and retry budget run out. Only the client
knows the transaction will never land:

```rust
// Records an abandon intent; the guardian's worker confirms over a short
// quarantine that the tx did not land, then releases the account.
let response = client.abandon_candidate(&account_id, nonce).await?;
assert_eq!(response.state, "pending");

// Poll the delta for the resolution: still `candidate` -> waiting,
// `canonical` -> the tx landed after all, `discarded` with reason
// `client_abandoned` -> the account is released.
let delta = client.get_delta(&account_id, nonce).await?;
```

### Rate Limits and Retries

The server rate-limits both its gRPC and HTTP surfaces. The sustained
per-minute limit is keyed per IP alone, so gRPC and HTTP calls from one
client draw on the same allowance; the burst limit is keyed per IP and
method. An over-budget call fails with `ResourceExhausted`, code
`rate_limit_exceeded`, and a backoff hint. `ClientError` classifies it:
`is_retryable()` reads the error envelope (falling back to the status-code
class), and `retry_after()` returns the server's hint, preferring the
`retry-after` status metadata over the envelope value.

Rate-limit rejections happen before the server touches any state, so
retrying them is always safe. The client does not retry automatically
(automatic backoff is tracked in
[#360](https://github.com/OpenZeppelin/guardian/issues/360)); a bounded
loop over the exposed hint is a few lines:

```rust
let max_attempts = 3;
let mut attempts = 0;
let state = loop {
    match client.get_state(&account_id).await {
        Ok(state) => break state,
        Err(err) if err.is_retryable() && attempts < max_attempts => {
            attempts += 1;
            let delay = err.retry_after().unwrap_or(Duration::from_secs(1));
            tokio::time::sleep(delay).await;
        }
        Err(err) => return Err(err.into()),
    }
};
```

Retries are idempotent (the original request timestamp is preserved). The
server refuses with `GUARDIAN_CANDIDATE_LANDED` when the transaction
demonstrably landed.

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
 
