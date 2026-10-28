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
// quarantine that the tx did not land, then releases the account. The
// transaction is named by its summary's commitment: a Guardian execution
// can take the nonce, and the abandon still answers for this transaction.
let proposal_id = guardian_client::tx_summary_commitment_hex(&tx_summary);
let response = client
    .abandon_candidate(&account_id, nonce, &proposal_id)
    .await?;
assert_eq!(response.state, "pending");

// Poll the delta for the resolution. Compare its commitment with the one
// the abandon returned first: a Guardian execution can take the nonce
// with a delta of its own, which says nothing about the abandoned tx.
match client.get_delta(&account_id, nonce).await?.delta {
    Some(delta) if delta.new_commitment.eq_ignore_ascii_case(&response.commitment) => {
        // `candidate` -> waiting, `canonical` -> the tx landed after all,
        // `discarded` with reason `client_abandoned` -> the account is
        // released.
    }
    Some(_) => {
        // Another delta holds the nonce: `canonical` -> the abandoned tx
        // can no longer land, `candidate` -> waiting on that delta.
    }
    None => {}
}
```

Naming the transaction needs a Guardian server from the same release: an
older server does not know `proposal_id` and refuses the signed request as
unauthenticated, before recording anything.

### Canonical Nonce

Nonce and commitment of the latest canonical state, without the state blob
(issue #191); the server answers from the nonce and commitment it stores with
the state. The multisig SDK's `sync` runs this check before `get_state`. The
full state fetch can be skipped when Guardian's nonce is below the local
account's nonce, or equal to it with the same commitment. An equal nonce at a
different commitment means the local account has diverged from Guardian, so
fetch the state in that case too.

```rust
let head = client.get_canonical_nonce(&account_id).await?;
let in_sync = head.nonce < local_nonce
    || (head.nonce == local_nonce && head.commitment == local_commitment);
if !in_sync {
    let state = client.get_state(&account_id).await?;
    // reconcile the local store with `state`
}
```

### Delta History

Paginated canonical delta history (issue #413), newest-first by nonce, with
server-decoded note summaries. Pass the previous page's `next_cursor` to
resume; `None` on the response means the feed is exhausted. Only canonical
deltas appear, and only transactions pushed through Guardian are visible.

```rust
let mut cursor: Option<String> = None;
loop {
    let page = client
        .get_delta_history(&account_id, Some(50), cursor.take())
        .await?;
    for entry in &page.entries {
        // entry.status == "canonical"; notes carry tag, note_type,
        // assets, sender/recipient where the note script exposes them.
        println!("{} at {}", entry.nonce, entry.timestamp);
    }
    match page.next_cursor {
        Some(next) => cursor = Some(next),
        None => break,
    }
}
```

### Guardian Execution

A Guardian-executable proposal (one whose payload stores a `transaction_request`) can be
proved, submitted and committed by Guardian once it has enough signatures:

```rust
let allow_private_note = false;
let mut execution = client
    .execute_delta_proposal(&account_id, &proposal_id, allow_private_note)
    .await?;
while !execution.state.is_terminal() {
    tokio::time::sleep(Duration::from_secs(2)).await;
    execution = client.get_delta_proposal_execution(&account_id, &proposal_id).await?;
}

// The account's in-flight execution, if any
let current = client.get_current_execution(&account_id).await?;
```

Guardian refuses a transaction that creates a private output note with
`GUARDIAN_PROPOSAL_EXECUTES_LOCALLY` (`meta.reason` `private_note`) unless
`allow_private_note` is `true`; the flag is part of the signed request.

A refusal is a `ClientError` whose `guardian_code()` is the wire code (for example
`GUARDIAN_EXECUTION_CONFLICT`), with `is_retryable()`, `retry_after()` and, on a conflict,
`guardian_meta()["blocking_proposal_id"]`. The multisig SDK adds
`wait_for_guardian_execution` with backoff, retries and a deadline; see
[`docs/MULTISIG_SDK.md`](../../docs/MULTISIG_SDK.md#guardian-execution).

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
retrying them is always safe. The client does not retry rate limits
automatically (automatic backoff is tracked in
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

### Replay-protection retries

Signed requests carry a strictly increasing per-instance timestamp
(`max(now_ms, previous + 1)`). When a correctly signed request still loses
the server's per-signer replay check (stable code `authentication_replay`,
typically two in-flight requests landing out of order), the client retries
automatically, up to 2 times with a 50ms backoff, minting a fresh timestamp
and signature over the identical payload each attempt.
`ClientError::is_replay_rejection()` exposes the classification. Terminal
authentication failures (`authentication_failed`: clock outside the skew
window, invalid or unauthorized signature) are never retried.

The client retries only `authentication_replay`; it never retries
`authentication_failed`. During a mixed server/client rollout, a replay CAS
reported under the older authentication code—or received by a client without
replay-specific retry handling—can therefore surface as a terminal error until
both sides use the same error contract.

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

The `AuthConfig` passed to `configure` lists the account's cosigners. When every
cosigner and the GUARDIAN use one scheme, use that scheme's variant:

```rust
use guardian_client::{AuthConfig, MidenFalconRpoAuth, auth_config::AuthType};

let auth_config = AuthConfig {
    auth_type: Some(AuthType::MidenFalconRpo(MidenFalconRpoAuth {
        cosigner_commitments: vec![commitment_hex],
    })),
};
```

An account whose cosigners mix Falcon and ECDSA keys declares each cosigner's
scheme and the GUARDIAN's scheme. Servers before this variant reject it, so keep
the single-scheme variants for every other account:

```rust
use guardian_client::{AuthConfig, MidenAuth, MidenCosigner, auth_config::AuthType};

let auth_config = AuthConfig {
    auth_type: Some(AuthType::Miden(MidenAuth {
        cosigners: vec![
            MidenCosigner { commitment: wallet_commitment_hex, scheme: "ecdsa".into() },
            MidenCosigner { commitment: falcon_commitment_hex, scheme: "falcon".into() },
        ],
        guardian_scheme: "ecdsa".into(),
    })),
};
```

### 4. Sign Requests With a Session (optional)

When the Guardian accepts sessions (it advertises `sessions` on HTTP
`GET /status`; an older Guardian answers `CreateSession` as unimplemented),
the wallet can sign one session grant and let a delegated P-256 key sign the
session-eligible requests (state and delta reads, proposal
list/get/create/sign, delta pushes):

```rust
use std::time::Duration;

use guardian_client::StartSessionOptions;

let info = client
    .start_session(StartSessionOptions {
        network: "devnet".to_string(), // the Guardian's /status environment: local, devnet, testnet, cedar or mainnet
        ttl: Duration::from_secs(3600), // more than 5 minutes, at most the Guardian's maximum (8 hours by default)
    })
    .await?;

// ...get_state, get_delta_since, push_delta_proposal, push_delta: signed by the session key.

client.revoke_session().await?; // this session only, signed by the session key
client.revoke_all_sessions().await?; // every session of the wallet, wallet-signed
```

Every other route, including `configure`, `abandon` and account lookup, keeps
using the wallet signer. A session-signed `push_delta` needs a Guardian that
accepts it; an older one answers `wallet_signature_required`. The session is used only while the
wallet that granted it is the client's signer. When Guardian answers a
session-signed request with `session_expired`, `session_revoked` or
`authentication_failed`, the client drops the session and the next request is
wallet-signed; start a new session then (`session()` returns `None`).
`authorization_failed` (the wallet does not cosign that account) leaves the
session in place. The grant is signed by the configured signer, which this
client holds in process, so there is no confirmation step.

`revoke_all_sessions` ends the sessions already registered with Guardian. A
grant that was signed but not yet registered can still be registered for up
to about 10 minutes, so when a key may be compromised, call it again 10
minutes later.

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
 
