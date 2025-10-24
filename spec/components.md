# Components

## API

The API exposes a simple interface for operating states and deltas with HTTP and gRPC protocols supported. The behaviour of the system will be the same regardless of the protocol used, this ensures consistency across different clients. See api.md for details and the component trait reference.

## Metadata

```rust
trait AccountMetadataStore {
  // Get the metadata of an account
  fn get(&self, account_id: &str) -> Result<AccountMetadata>;

  // Store the metadata of an account
  fn set(&self, account_id: &str, metadata: AccountMetadata) -> Result<()>;

  // List all account IDs
  fn list(&self) -> Result<Vec<String>>;

  // Update the authentication configuration of an account
  fn update_auth(&self, account_id: &str, auth: Auth) -> Result<()>;
}

pub struct AccountMetadata {
    // The account ID
    pub account_id: String,

    // The authentication configuration
    pub auth: Auth,

    // The storage type
    pub storage_type: StorageType,

    pub created_at: String,
    pub updated_at: String,
}
```

## Auth

```rust
pub enum Auth {
    // Miden Falcon RPO signature scheme
    MidenFalconRpo { cosigner_pubkeys: Vec<String> },
}
```

Credentials are provided per request:

- HTTP headers: `x-pubkey`, `x-signature`
- gRPC metadata: `x-pubkey`, `x-signature`

Verification rules:

- The `x-pubkey` MUST be present in `cosigner_pubkeys` for the account.
- The signature is over the RPO256 digest of the `account_id` (not the full payload).

## Acknowledger

```rust
pub enum Acknowledger {
    FilesystemMidenFalconRpo(MidenFalconRpoSigner),
}

pub trait Acknowledger {
    pub fn pubkey(&self) -> String;
    pub fn ack_delta(&self, delta: &DeltaObject) -> Result<DeltaObject>;
}
```

For Miden Falcon RPO, the server signs the RPO256 digest of `new_commitment` and returns the signature as `ack_sig` (hex). The server's acknowledgment public key is returned during `configure` as `ack_pubkey`.

## Network

```rust
trait NetworkClient {
  fn get_state_commitment(
    &self,
    account_id: &str,
    state_json: &serde_json::Value,
  ) -> Result<String, String>;

  async fn verify_state(
    &mut self,
    account_id: &str,
    state_json: &serde_json::Value,
  ) -> Result<(), String>;

  fn verify_delta(
    &self,
    prev_proof: &str,
    prev_state_json: &serde_json::Value,
    delta_payload: &serde_json::Value,
  ) -> Result<(), String>;

  fn apply_delta(
    &self,
    prev_state_json: &serde_json::Value,
    delta_payload: &serde_json::Value,
  ) -> Result<(serde_json::Value, String), String>;

  fn merge_deltas(
    &self,
    delta_payloads: Vec<serde_json::Value>,
  ) -> Result<serde_json::Value, String>;

  fn validate_account_id(&self, account_id: &str) -> Result<(), String>;

  fn validate_credential(
    &self,
    state_json: &serde_json::Value,
    credential: &Credentials,
  ) -> Result<(), String>;

  async fn should_update_auth(
    &mut self,
    state_json: &serde_json::Value,
  ) -> Result<Option<Auth>, String>;
}
```

## Storage

```rust
trait StorageBackend {
  async fn submit_state(&self, state: &AccountState) -> Result<(), String>;
  async fn submit_delta(&self, delta: &DeltaObject) -> Result<(), String>;
  async fn pull_state(&self, account_id: &str) -> Result<AccountState, String>;
  async fn pull_delta(&self, account_id: &str, nonce: u64) -> Result<DeltaObject, String>;
  async fn pull_deltas_after(
    &self,
    account_id: &str,
    from_nonce: u64,
  ) -> Result<Vec<DeltaObject>, String>;
}
```
