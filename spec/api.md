# API (HTTP and gRPC)

## Authentication

 - Per-account auth, requests MUST include valid credentials matching account metadata.
 - For Miden, the signature is over the `account_id` (RPO256 digest of the account ID).
 - HTTP headers: `x-pubkey`, `x-signature`. gRPC metadata keys: `x-pubkey`, `x-signature`.

## Data Shapes

- StateObject (HTTP JSON):
  - `account_id: string`, `state_json: object`, `commitment: string`, `created_at: string`, `updated_at: string`
- DeltaObject (HTTP JSON):
  - `account_id: string`, `nonce: u64`, `prev_commitment: string`, `new_commitment: string`, `delta_payload: object`, `ack_sig?: string`, `status: { status: "candidate"|"canonical"|"discarded", timestamp: string }`

## HTTP Endpoints

- POST /configure
  - Headers: `x-pubkey`, `x-signature`
  - Body: `{ account_id: string, auth: Auth, initial_state: object, storage_type: "Filesystem" }`
  - 200: `{ success: true, message: string, ack_pubkey: string }`
  - 400: `{ success: false, message: string, ack_pubkey: null }`
- POST /delta
  - Headers: `x-pubkey`, `x-signature`
  - Body: `DeltaObject` (client sets `account_id`, `nonce`, `prev_commitment`, `delta_payload`; server fills `new_commitment`, `ack_sig`, `status`)
  - 200: `DeltaObject`
  - 400: error response (invalid auth/delta/commitment mismatch) with message
- GET /delta?account_id=...&nonce=...
  - Headers: `x-pubkey`, `x-signature`
  - 200: `DeltaObject`
  - 404: not found
- GET /delta/since?account_id=...&from_nonce=...
  - Headers: `x-pubkey`, `x-signature`
  - 200: `DeltaObject` representing merged snapshot
  - 404: not found
- GET /state?account_id=...
  - Headers: `x-pubkey`, `x-signature`
  - 200: `StateObject`
  - 404: not found

Errors: `AccountNotFound`, `AuthenticationFailed`, `InvalidDelta`, `ConflictPendingDelta`, `CommitmentMismatch`, `DeltaNotFound`, `StateNotFound`.

## gRPC

- Service: `StateManager` (see generated descriptors and `proto/state_manager.proto`). Methods mirror HTTP:
  - `Configure(ConfigureRequest) -> ConfigureResponse` (includes `ack_pubkey`)
  - `PushDelta(PushDeltaRequest) -> PushDeltaResponse` (returns `delta` and `ack_sig`)
  - `GetDelta(GetDeltaRequest) -> GetDeltaResponse`
  - `GetDeltaSince(GetDeltaSinceRequest) -> GetDeltaSinceResponse`
  - `GetState(GetStateRequest) -> GetStateResponse`

## Idempotency and Ordering

- `push_delta` MAY be retried by clients; server SHOULD treat identical deltas (same account_id, nonce, payload) as idempotent when possible.
- Server enforces `prev_commitment` match; nonce monotonicity is network-dependent.

## Component trait (reference)

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

## Examples

```bash
curl -X POST http://localhost:8080/configure \
  -H 'content-type: application/json' \
  -H 'x-pubkey: 0x...' \
  -H 'x-signature: 0x...' \
  -d '{
    "account_id": "0x...",
    "auth": { "MidenFalconRpo": { "cosigner_pubkeys": ["0x..."] } },
    "initial_state": { "...": "..." },
    "storage_type": "Filesystem"
  }'
```
