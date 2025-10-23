# Private State Manager Specification

Private state manager is a system that allows a device, or a group of devices, to backup and sync their state securely without trust assumptions about other participants or the server operator.

It consists of 2 main elements:

- State: canonical representation of the state of an entity.
- Delta: valid changes applied to the state.


## Definitions

### State

A state is a data structure that represents the current state of a local account, contract, or any other entity that lives in the local device and has to be kept private and in sync with other devices and valid against some network that asserts its validity.

Example:
```json
{
    "account_id": "1234567890",
    "commitment": "0x1234567890",
    "nonce": 10,
    "assets": [
      {
        "balance": 12000,
        "asset_id": "USDC",
      },
      {
        "balance": 2,
        "asset_id": "ETH",
      }
    ],
}
```

### Delta

A delta is whatever changes you apply to that state in append-only operations. The change on the state is also validated against some network state and acknowledged (signed) by the private state manager.

Example:
```json
{
    "account_id": "1234567890",
    "prev_commitment": "0x1234567890",
    "nonce": 10,
    "ops": [
      { 
        "type": "transfer",
        "asset_id": "USDC",
        "amount": 100,
      }
    ],
}
```

### Account ID

Is the unique identifier of an account holding a state, the private state manager can host multiple accounts and route authenticated requests to each.

### Commitment

Is the commitment of the state, it's a hash, nonce, or any other identifier that serves as the unique identifier of the current state of the account. It's used to cerifify that the state is not forked or corrupted. Each new delta includes a prev_commitment field that references the commitment of the base state in which the delta is applied.

### Nonce

In most networks, the nonce is an incremental counter that serves as a protection mechanism against replay attacks, in this system, we also use the nonce to identify and index deltas.

## Basic principles

- Both State and Deltas are represented as generic JSON objects and completely agnostic of the underlying data model.
- The state should never be forked or corrupted, each delta is validated against the previous state and (optionally) the network state.
- The state must be protected against **external users**, only the account shareholders should be able to access the state.
- The state must be protected against **internal users**, the state should be modified only applying valid deltas, that (optionally) can be verified against the network.
- The state must be protected against the **Private State Manager server operator**, it will support running the server in a secure enclave, doing TLS termination inside the enclave + encrypted storage.
- The implementation is very extensible in different dimensions:
 - The Network against which the state is validated (Miden, Ethereum, Bitcoin, etc.)
 - The underlying storage (filesystem, database, etc.)
 - The requests authentication (public/private keys, JWT etc.)
 - The acknowledgement (ack) signature scheme (falcon, ed25519, etc.)

## Components

### API

The API exposes a simple interface for operating states and deltas with HTTP and gRPC protocols supported. The behaviour of the system will be the same regardless of the protocol used, this ensures consistency across different clients.

```rust
trait API {
  // Configure a new account passing an initial state and authentication credentials.
  fn configure(&self, params: ConfigureAccountParams) -> Result<ConfigureAccountResult>;

  // Push a new delta to the account, the server responds with the acknowledgement.
  fn push_delta(&self, params: PushDeltaParams) -> Result<PushDeltaResult>;

  // Get a specific delta by nonce.
  fn get_delta(&self, params: GetDeltaParams) -> Result<GetDeltaResult>;

  // Get  merged delta since a given nonce
  fn get_delta_since(&self, params: GetDeltaSinceParams) -> Result<GetDeltaSinceResult>;

  // Get the current state of the account
  fn get_state(&self, params: GetStateParams) -> Result<GetStateResult>;
}
```

### Metadata

Metadata is a component that stores the configuration and metadata of the accounts hosted by the private state manager. It's also responsible for validating the authentication credentials.

Account metadata is stored in a key-value store, the key is the account ID and the value is the AccountMetadata object.

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

### Auth

Auth is the authentication configuration of an account, it's used to verify the authenticity of the requests made to the server, supporting extensions to multiple types of Credentials, like public/private keys, JWT, etc. At the moment, the only supported authentication scheme is the Miden Falcon RPO signature scheme.

`cosigner_pubkeys` is a list of public keys that are authorized to sign requests on behalf of the account, and it should match the cosigners of the account in Miden network.

```rust
pub enum Auth {
    // Miden Falcon RPO signature scheme
    MidenFalconRpo { cosigner_pubkeys: Vec<String> },
}
```

### Acknowledger

Acknowledger acts as the component that generates proofs of stored deltas, as a security measure, clients integrating Private State Manager will require this ack proof in order to perform some network operation, like submitting a transaction.

Acknowledger can be extended to support multiple schemes, but the most practical implementation is to use asymetric cryptography, in the future we might include support to other primitives, like ZK proofs.

```rust
pub enum Acknowledger {
    FilesystemMidenFalconRpo(MidenFalconRpoSigner),
}

pub trait Acknowledger {
    // Initial implementations will use asymetric
    // cryptography, extensible to multiple schemes.
    pub fn pubkey(&self) -> String;

    // Receives a delta with no acknowledgement and
    // returns it with an acknowledgement in it.
    pub fn ack_delta(&self, delta: &DeltaObject) -> Result<DeltaObject>;
}
```

### Network 

Network is the component that handles the communication with the blockchain or system that holds the canonical state, it's responsible for verifying the state and deltas validity, and for implementing the custom logic for applying the deltas to the state.

Each networks might have different ways to apply deltas to state or to verify validity, so we abstract the implementation details and provide a common interface for all networks.

```rust
trait NetworkClient {

}
```