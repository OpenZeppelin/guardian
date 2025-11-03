# API (HTTP and gRPC)

## Authentication

 - Per-account authentication: requests MUST include credentials authorised by the account’s policy.
 - Credentials are provided via HTTP headers `x-pubkey`, `x-signature` (and the same keys in gRPC metadata).
 - The supplied public key is hashed to a commitment and checked against the account’s allowlist, the signature is over an account digest to prevent cross-account replay.

## Data Shapes

- StateObject (HTTP JSON):
  - `account_id: string`, `state_json: object`, `commitment: string`, `created_at: string`, `updated_at: string`
- DeltaObject (HTTP JSON):
  - `account_id: string`, `nonce: u64`, `prev_commitment: string`, `new_commitment: string`, `delta_payload: object`, `ack_sig?: string`, `status: { status: "candidate"|"canonical"|"discarded", timestamp: string }`

## HTTP Endpoints

- POST /configure
  - Headers: `x-pubkey`, `x-signature`
  - Body: `{ account_id: string, auth: Auth, initial_state: object, storage_type: "Filesystem" }`
  - 200: `{ success: true, message: string, ack_pubkey: string }` (represents the server acknowledgement key; clients may treat this as the signer commitment)
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

- GET /pubkey
  - No authentication.
  - 200: `{ "pubkey": "0x..." }` exposing the acknowledgement signer commitment so clients can verify `ack_sig`.

Errors: `AccountNotFound`, `AuthenticationFailed`, `InvalidDelta`, `ConflictPendingDelta`, `CommitmentMismatch`, `DeltaNotFound`, `StateNotFound`.

## gRPC

The gRPC surface mirrors HTTP methods and data shapes. Credentials are provided via metadata.

## Idempotency and ordering

- `push_delta` MAY be retried by clients; server SHOULD treat identical deltas (same account_id, nonce, payload) as idempotent when possible.
- Server enforces `prev_commitment` match; nonce monotonicity is network-dependent.



## Examples

```bash
curl -X POST http://localhost:3000/configure \
  -H 'content-type: application/json' \
  -H 'x-pubkey: 0x...' \
  -H 'x-signature: 0x...' \
  -d '{
    "account_id": "0x...",
    "auth": { "MidenFalconRpo": { "cosigner_commitments": ["0x..."] } },
    "initial_state": { "...": "..." },
    "storage_type": "Filesystem"
  }'
```
