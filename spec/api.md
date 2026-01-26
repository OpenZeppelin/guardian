# API (HTTP and gRPC)

## Authentication

 - Per-account authentication: requests MUST include credentials authorised by the account's policy.
 - Credentials are provided via HTTP headers `x-pubkey`, `x-signature`, `x-timestamp` (and the same keys in gRPC metadata).
 - The supplied public key is hashed to a commitment and checked against the account's allowlist.
 - The signature is over a digest of `(account_id, timestamp)` to prevent both cross-account and replay attacks.

### Replay Protection

 - The signed payload includes a Unix timestamp (milliseconds since epoch) alongside the account ID.
 - Server enforces a maximum clock skew window of **300,000 milliseconds** (5 minutes) from the current server time.
 - Server tracks `last_auth_timestamp` per account; requests with a timestamp ≤ the last seen timestamp are rejected.
 - The `last_auth_timestamp` is updated atomically using compare-and-swap when authentication succeeds.
 - Signed message format: `RPO256_hash([account_id_prefix, account_id_suffix, timestamp_ms, 0])`

## Data Shapes

- StateObject (HTTP JSON):
  - `account_id: string`, `state_json: object`, `commitment: string`, `created_at: string`, `updated_at: string`
- DeltaObject (HTTP JSON):
  - `account_id: string`, `nonce: u64`, `prev_commitment: string`, `new_commitment?: string`, `delta_payload: object`, `ack_sig?: string`, `status: { status: "pending"|"candidate"|"canonical"|"discarded", ... }`
- DeltaProposalPayload (HTTP JSON):
  - `tx_summary: object` (base64-encoded `TransactionSummary` as produced by the network client)
  - `signatures?: DeltaSignature[]` (optional signatures collected by the proposer)
- DeltaSignature (HTTP JSON):
  - `signer_id: string` (hex commitment of the signer’s Falcon public key)
  - `signature: { scheme: "falcon", signature: string }`
- DeltaProposal (HTTP JSON):
  - Same base fields as `DeltaObject`, but proposals are always returned with `new_commitment = null`, `ack_sig = null`, and `status = { "status": "pending", "timestamp": string, "proposer_id": string, "cosigner_sigs": CosignerSignature[] }`
- CosignerSignature (HTTP JSON):
  - `signer_id: string`, `timestamp: string`, `signature: { scheme: "falcon", signature: string }`
- DeltaProposalEnvelope (HTTP JSON):
  - `{ delta: DeltaProposal, commitment: string }` where `commitment` is the stable proposal identifier derived from `(account_id, nonce, tx_summary)`

## HTTP Endpoints

- Rate limiting:
  - HTTP endpoints are rate limited by client IP.
  - Burst limits are applied per IP and per endpoint path.
  - Sustained limits are applied per IP and per IP+account/signer when available.
  - Exceeded limits return `429 Too Many Requests` and include a `Retry-After` header.

- POST /configure
  - Headers: `x-pubkey`, `x-signature`, `x-timestamp`
  - Body: `{ account_id: string, auth: Auth, initial_state: object }`
  - 200: `{ success: true, message: string, ack_pubkey: string }` (represents the server acknowledgement key; clients may treat this as the signer commitment)
  - 400: `{ success: false, message: string, ack_pubkey: null }`
- POST /delta
  - Headers: `x-pubkey`, `x-signature`, `x-timestamp`
  - Body: `DeltaObject` (client sets `account_id`, `nonce`, `prev_commitment`, `delta_payload`; server fills `new_commitment`, `ack_sig`, `status`)
  - 200: `DeltaObject`
  - 400: error response (invalid auth/delta/commitment mismatch) with message
- GET /delta?account_id=...&nonce=...
  - Headers: `x-pubkey`, `x-signature`, `x-timestamp`
  - 200: `DeltaObject`
  - 404: not found
- GET /delta/since?account_id=...&from_nonce=...
  - Headers: `x-pubkey`, `x-signature`, `x-timestamp`
  - 200: `DeltaObject` representing merged snapshot
  - 404: not found
- GET /state?account_id=...
  - Headers: `x-pubkey`, `x-signature`, `x-timestamp`
  - 200: `StateObject`
  - 404: not found

- POST /delta/proposal
  - Headers: `x-pubkey`, `x-signature`, `x-timestamp`
  - Body: `{ account_id: string, nonce: u64, delta_payload: DeltaProposalPayload }`
  - Behaviour: validates proposer credentials, re-validates the provided `tx_summary` against the latest persisted state, derives a proposal commitment via the network client, and persists the proposal with `status.pending`
  - 200: `DeltaProposalEnvelope`
  - 400: `InvalidDelta`, `AccountNotFound`, `AuthenticationFailed`
- GET /delta/proposal?account_id=...
  - Headers: `x-pubkey`, `x-signature`, `x-timestamp`
  - Returns only proposals whose `status` is still `pending`, ordered by nonce
  - 200: `{ proposals: DeltaProposal[] }` (empty list on missing accounts or storage errors to avoid leaking existence)
- PUT /delta/proposal
  - Headers: `x-pubkey`, `x-signature`, `x-timestamp`
  - Body: `{ account_id: string, commitment: string, signature: { scheme: "falcon", signature: string } }`
  - Behaviour: loads the pending proposal identified by `commitment`, derives the signer commitment from the caller's pubkey, rejects duplicate signatures, and appends `{ signer_id, timestamp, signature }` to `status.pending.cosigner_sigs`
  - 200: `DeltaProposal`
  - 400: `ProposalNotFound`, `ProposalAlreadySigned`, `InvalidProposalSignature`, plus the standard auth/account errors

- GET /pubkey
  - No authentication.
  - 200: `{ "pubkey": "0x..." }` exposing the acknowledgement signer commitment so clients can verify `ack_sig`.

Errors: `AccountNotFound`, `AuthenticationFailed`, `InvalidDelta`, `ConflictPendingDelta`, `CommitmentMismatch`, `DeltaNotFound`, `StateNotFound`, `TimestampExpired`, `TimestampReplay`.

## gRPC

The gRPC surface mirrors HTTP methods and data shapes. Credentials are provided via metadata headers. New RPCs:

- `PushDeltaProposal(PushDeltaProposalRequest) -> PushDeltaProposalResponse` (returns `delta` + `commitment`)
- `GetDeltaProposals(GetDeltaProposalsRequest) -> GetDeltaProposalsResponse`
- `SignDeltaProposal(SignDeltaProposalRequest) -> SignDeltaProposalResponse`

## Idempotency and ordering

- `push_delta` MAY be retried by clients; server SHOULD treat identical deltas (same account_id, nonce, payload) as idempotent when possible.
- Server enforces `prev_commitment` match; nonce monotonicity is network-dependent.



## Examples

```bash
# Note: x-signature must be over RPO256_hash([account_id_prefix, account_id_suffix, timestamp_ms, 0])
# where timestamp_ms is the same Unix epoch milliseconds value sent in x-timestamp

curl -X POST http://localhost:3000/configure \
  -H 'content-type: application/json' \
  -H 'x-pubkey: 0x...' \
  -H 'x-signature: 0x...' \
  -H 'x-timestamp: 1700000000000' \
  -d '{
    "account_id": "0x...",
    "auth": { "MidenFalconRpo": { "cosigner_commitments": ["0x..."] } },
    "initial_state": { "...": "..." }
  }'

curl -X POST http://localhost:3000/delta/proposal \
  -H 'content-type: application/json' \
  -H 'x-pubkey: 0x...' \
  -H 'x-signature: 0x...' \
  -H 'x-timestamp: 1700000000000' \
  -d '{
    "account_id": "0x...",
    "nonce": 42,
    "delta_payload": {
      "tx_summary": { "data": "..." },
      "signatures": [
        {
          "signer_id": "0xpubkeycommitment",
          "signature": {
            "scheme": "falcon",
            "signature": "0x..."
          }
        }
      ]
    }
  }'

curl -X PUT http://localhost:3000/delta/proposal \
  -H 'content-type: application/json' \
  -H 'x-pubkey: 0xcosigner2pubkey' \
  -H 'x-signature: 0xcosigner2sig' \
  -H 'x-timestamp: 1700000000000' \
  -d '{
    "account_id": "0x...",
    "commitment": "0xproposalid",
    "signature": {
      "scheme": "falcon",
      "signature": "0x..."
    }
  }'
```
