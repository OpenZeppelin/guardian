# API (HTTP and gRPC)

## Authentication

- Per-account Miden requests MUST include credentials authorised by the account's policy.
- Miden credentials are provided via HTTP headers `x-pubkey`, `x-signature`, `x-timestamp` and the same keys in gRPC metadata.
- Miden `x-pubkey` is interpreted by the account auth policy:
  - Miden Falcon/ECDSA accounts use the serialized public key or its commitment.
- EVM HTTP requests under `/evm/*` use a `guardian_evm_session` cookie. The session EOA is recovered from a wallet signature and is checked against the configured account signer set or proposal signer snapshot.
- Replay protection applies to every Miden signed request. EVM challenge nonces are single-use and time-limited, and EVM sessions expire.

### Replay Protection

- The signed payload includes a Unix timestamp in milliseconds.
- The server enforces a maximum clock skew window of **300,000 milliseconds** (5 minutes).
- The server tracks `last_auth_timestamp` per account; requests with a timestamp less than or equal to the last accepted timestamp are rejected.
- `last_auth_timestamp` is updated atomically when authentication succeeds.

### Miden Request Signing

- HTTP request payload digest: RPO256 over canonical JSON bytes of the request payload (`body` for `POST`/`PUT`, query object for `GET`).
- gRPC request payload digest: RPO256 over protobuf-encoded request bytes.
- Signed message format: `RPO256_hash([account_id_prefix, account_id_suffix, timestamp_ms, payload_hash_0, payload_hash_1, payload_hash_2, payload_hash_3])`.

### Lookup Request Signing

The `GET /state/lookup` endpoint and the matching `GetAccountByKeyCommitment` gRPC method use a dedicated, account-less signed-message format because the account ID is the very value the caller is trying to discover. The format is **domain-separated by construction** from `Miden Request Signing` above so a signature crafted for one shape cannot validate against the other in either direction.

- Domain tag: `DOMAIN_TAG = RPO256(felts(b"guardian.lookup.v1"))` — a fixed 4-felt word, computed once and embedded in the binary. Future incompatible changes MUST bump the version segment.
- Signed message format: `RPO256_hash([DOMAIN_TAG_w0..w3, timestamp_ms, key_commitment_w0..w3])`.
- Authentication: proof-of-possession of the queried commitment. Identity is derived from the signature itself — Falcon signatures embed the public key, ECDSA signatures recover it via the recovery byte. The server then requires `commitment_of(derived_pk) == key_commitment` after cryptographic signature verification. `x-pubkey` is sent on the wire for parity with per-account requests but is not consulted on this path; signers that only expose the 32-byte commitment (e.g., browser Miden wallet) work because the signature is what proves possession.
- Replay protection: `MAX_TIMESTAMP_SKEW_MS` skew window only. No per-commitment last-seen tracking; a replayed valid request returns the same `account_id` to a key holder who already obtained it.

### EVM Session Authentication

- EVM support is behavior-gated by the server `evm` feature. Default builds do not register `/evm/*` routes or initialize EVM session state, Alloy readers, or proposal handlers.
- EVM clients authenticate through `/evm/auth/challenge` and `/evm/auth/verify`, and clear the session through `/evm/auth/logout`.
- Challenge signatures use `eth_signTypedData_v4` over EIP-712 typed data:
  - Domain: `{ name: "Guardian EVM Session", version: "1" }`.
  - Message: `{ wallet, nonce, issued_at, expires_at }`.
- Guardian derives the authenticated EOA with `ecrecover`, consumes the challenge nonce once, and stores the recovered address in a secure cookie-backed session.
- Cross-origin browser clients use credentialed CORS when configured with an explicit origin allowlist. The EVM session cookie remains host-only and `HttpOnly`.
- EVM sessions expire; challenge nonces are time-limited and single-use.

## Data Shapes

### Account Identifiers

- Miden account IDs use the existing Miden account identifier format.
- EVM account IDs are canonical strings: `evm:<chain_id>:<normalized_smart_account_address>`.
- EVM accounts are registered through `/evm/accounts` and use `/evm/proposals*` for proposal coordination.

### AuthConfig

HTTP JSON uses externally tagged variants:

```json
{ "MidenFalconRpo": { "cosigner_commitments": ["0x..."] } }
```

```json
{ "MidenEcdsa": { "cosigner_commitments": ["0x..."] } }
```

```json
{ "EvmEcdsa": { "signers": ["0x..."] } }
```

The contract may expose EVM-shaped auth metadata, but `/configure` and the Miden delta routes only accept Miden auth variants. EVM account registration derives signer metadata from the validator module through `/evm/accounts`.

gRPC uses `AuthConfig::{miden_falcon_rpo, miden_ecdsa, evm_ecdsa}` for schema compatibility, while EVM behavior remains HTTP-only under `/evm/*`.

### NetworkConfig

HTTP JSON uses a `kind` discriminator:

```json
{ "kind": "miden", "network_type": "local" }
```

```json
{
  "kind": "evm",
  "chain_id": 31337,
  "account_address": "0x...",
  "multisig_validator_address": "0x..."
}
```

- `network_config` is optional for legacy Miden `/configure` requests and defaults to `{ "kind": "miden", "network_type": "local" }`.
- Miden state/delta routes only accept `kind: "miden"` accounts. EVM account metadata is created through `/evm/accounts`.
- EVM `account_address` is the smart account address and must match `account_id`.
- EVM `multisig_validator_address` is the ERC-7579 multisig validator module address.
- Guardian does not trust client-provided RPC endpoints. RPC URLs are resolved server-side from `GUARDIAN_EVM_RPC_URLS`; the EntryPoint address is resolved server-side from `GUARDIAN_EVM_ENTRYPOINT_ADDRESS` and defaults to the EntryPoint v0.9 address.

gRPC uses `NetworkConfig::{miden, evm}`.

### StateObject

```json
{
  "account_id": "string",
  "state_json": {},
  "commitment": "string",
  "created_at": "string",
  "updated_at": "string",
  "auth_scheme": "falcon"
}
```

`auth_scheme` may be `"falcon"` or `"ecdsa"` when present.

### DeltaObject

```json
{
  "account_id": "string",
  "nonce": 0,
  "prev_commitment": "string",
  "new_commitment": "string",
  "delta_payload": {},
  "ack_sig": "string",
  "ack_pubkey": "string",
  "ack_scheme": "falcon",
  "status": { "status": "candidate", "timestamp": "string", "retry_count": 0 }
}
```

`status` is one of:

- `{ "status": "pending", "timestamp": string, "proposer_id": string, "cosigner_sigs": CosignerSignature[] }`
- `{ "status": "candidate", "timestamp": string, "retry_count": number }`
- `{ "status": "canonical", "timestamp": string }`
- `{ "status": "discarded", "timestamp": string }`

### Proposal Payloads

Miden delta proposals use:

```json
{
  "tx_summary": { "data": "base64-transaction-summary" },
  "metadata": { "proposal_type": "p2id" },
  "signatures": []
}
```

EVM proposals use EVM-specific request and response shapes under `/evm/proposals`. They do not use `DeltaObject` or the `/delta/proposal` envelope.

EVM proposal creation request:

```json
{
  "account_id": "evm:31337:0x...",
  "user_op_hash": "0x...",
  "payload": "{\"packedUserOperation\":{}}",
  "nonce": "0",
  "ttl_seconds": 900,
  "signature": "0x..."
}
```

EVM proposal response:

```json
{
  "proposal_id": "0x...",
  "account_id": "evm:31337:0x...",
  "chain_id": 31337,
  "smart_account_address": "0x...",
  "validator_address": "0x...",
  "user_op_hash": "0x...",
  "payload": "{\"packedUserOperation\":{}}",
  "nonce": "0",
  "nonce_key": "0",
  "proposer": "0x...",
  "signer_snapshot": ["0x..."],
  "threshold": 2,
  "signatures": [
    { "signer": "0x...", "signature": "0x...", "signed_at": 1700000000000 }
  ],
  "created_at": 1700000000000,
  "expires_at": 1700000900000
}
```

- The payload is opaque application data supplied by the client.
- `user_op_hash` is the 32-byte hash that EVM signers sign.
- `nonce` is the full uint256 EntryPoint nonce as a decimal string or `0x`-prefixed hex string.
- Guardian snapshots signer EOAs and threshold through Alloy, verifies signatures against `user_op_hash`, and stores an EVM proposal record in a domain-specific proposal store.
- EVM signatures are verified against the client-supplied 32-byte hash.
- Guardian does not build UserOperations, decode payloads, or submit transactions on-chain.

### Proposal Signatures

```json
{
  "signer_id": "0x...",
  "signature": { "scheme": "falcon", "signature": "0x..." }
}
```

```json
{
  "signer_id": "0x...",
  "signature": { "scheme": "ecdsa", "signature": "0x...", "public_key": "0x..." }
}
```

- Miden Falcon signer IDs are signer commitments.
- Miden ECDSA signer IDs are signer commitments.
- EVM proposal signatures use `EvmProposalSignature` records: `{ signer, signature, signed_at }`.
- EVM create/approve request bodies carry raw ECDSA signatures. Signer identity is derived from `guardian_evm_session`.
- Stored EVM signers are normalized EOA addresses.
- EVM proposal signatures are verified with `ecrecover(hash, signature)`.

### DeltaProposalEnvelope

```json
{ "delta": {}, "commitment": "0x..." }
```

- Miden proposal IDs are derived by the configured Miden network client from `(account_id, nonce, tx_summary)`.

## HTTP Endpoints

### Rate Limiting

- HTTP endpoints are rate limited by client IP.
- Burst limits are applied per IP and endpoint path.
- Sustained limits are applied per IP and per IP+account/signer when available.
- Client IP detection prefers `X-Forwarded-For`, then `X-Real-IP`, then the socket peer IP.
- Exceeded limits return `429 Too Many Requests` and include `Retry-After`.

### Request Size Limits

- HTTP request bodies are limited to a configurable maximum size (default: 1 MB).
- Requests exceeding this limit return `413 Payload Too Large`.

### POST /configure

- Headers: `x-pubkey`, `x-signature`, `x-timestamp`.
- Body: `{ account_id: string, auth: AuthConfig, network_config?: NetworkConfig, initial_state: object }`.
- Miden behavior: validates initial state and Guardian acknowledgement binding, stores account state, and stores metadata.
- EVM behavior: unsupported. EVM accounts are registered through `/evm/accounts`.
- 200: `{ success: true, message: string, ack_pubkey: string, ack_commitment: string }`.
- Error: `{ success: false, message: string, ack_pubkey: null, ack_commitment: null, code?: string }`.

### POST /delta

- Headers: `x-pubkey`, `x-signature`, `x-timestamp`.
- Body: `DeltaObject` with client-set `account_id`, `nonce`, `prev_commitment`, and `delta_payload`.
- Miden behavior: validates, applies, acknowledges, and persists the delta.
- EVM behavior: unsupported for EVM accounts in v1.
- 200: `DeltaObject`.

### GET /delta

- Headers: `x-pubkey`, `x-signature`, `x-timestamp`.
- Query: `account_id`, `nonce`.
- 200: `DeltaObject`.
- 404: not found.

### GET /delta/since

- Headers: `x-pubkey`, `x-signature`, `x-timestamp`.
- Query: `account_id`, `nonce` where `nonce` is the starting nonce.
- 200: merged canonical `DeltaObject`.
- 404: not found.

### GET /state

- Headers: `x-pubkey`, `x-signature`, `x-timestamp`.
- Query: `account_id`.
- 200: `StateObject`.
- 404: not found.

### GET /state/lookup

- Headers: `x-pubkey`, `x-signature`, `x-timestamp` — signed via the **Lookup Request Signing** format above (NOT the per-account format).
- Query: `key_commitment` (`0x`-prefixed lowercase hex, 32 bytes).
- Authentication: proof-of-possession of the queried commitment. Identity is derived from the signature itself (Falcon embeds the pubkey, ECDSA recovers it); the server then requires the derived key's commitment to equal `key_commitment`. `x-pubkey` is part of the wire format for parity with other endpoints but is not consulted here.
- Errors propagate via the structured `GuardianError` envelope (no legacy per-endpoint body shape).
- 200: `{ accounts: [ { account_id: string } ] }`. The list may be empty when no account authorizes the queried commitment — empty list is a successful response, NOT a not-found error. Distinguishing "no account" from "wrong key" would leak account presence to non-key-holders.
- Common errors: `invalid_input` (malformed `key_commitment`), `authentication_failed` (signature verification failure, derived-key commitment mismatch, timestamp outside skew window, signature did not parse as Falcon or ECDSA), `storage_error`.
- EVM accounts are excluded from results regardless of commitment value (their authorization shape uses `signers`, not `cosigner_commitments`).

### POST /delta/proposal

- Headers: `x-pubkey`, `x-signature`, `x-timestamp`.
- Body: `{ account_id: string, nonce: u64, delta_payload: MidenProposalPayload }`.
- Miden behavior: validates proposer credentials, validates `tx_summary` against the latest persisted state, derives a proposal ID via the network client, and persists a pending proposal.
- EVM behavior: unsupported. EVM proposals use `/evm/proposals`.
- 200: `DeltaProposalEnvelope`.
- Common errors: `invalid_delta`, `account_not_found`, `authentication_failed`, `conflict_pending_delta`, `pending_proposals_limit`, `unsupported_for_network`.

### GET /delta/proposal

- Headers: `x-pubkey`, `x-signature`, `x-timestamp`.
- Query: `account_id`.
- Returns only pending proposals, ordered by nonce.
- EVM behavior: unsupported. EVM proposals use `/evm/proposals`.
- 200: `{ proposals: DeltaObject[] }`.
- Missing accounts or storage errors return an empty list to avoid leaking account existence.

### GET /delta/proposal/single

- Headers: `x-pubkey`, `x-signature`, `x-timestamp`.
- Query: `account_id`, `commitment`.
- 200: `DeltaObject`.
- 404: not found.

### PUT /delta/proposal

- Headers: `x-pubkey`, `x-signature`, `x-timestamp`.
- Body: `{ account_id: string, commitment: string, signature: ProposalSignature }`.
- Miden behavior: loads the pending proposal, derives the signer commitment from the caller's public key, rejects duplicate signatures, and appends the signature to both `status.pending.cosigner_sigs` and `delta_payload.signatures`.
- EVM behavior: unsupported. EVM proposal approvals use `/evm/proposals/{proposal_id}/approve`.
- 200: `DeltaObject`.
- Common errors: `proposal_not_found`, `proposal_already_signed`, `invalid_proposal_signature`, `signer_not_authorized`, `unsupported_for_network`.

### GET /evm/auth/challenge

- Query: `address`.
- Issues a time-limited EIP-712 session challenge for the normalized EOA address.
- 200: `{ address, nonce, issued_at, expires_at, typed_data }`.
- Available only when the server is built with the `evm` feature.

### POST /evm/auth/verify

- Body: `{ address, nonce, signature }`.
- Recovers the signer from the challenge typed-data hash, consumes the challenge once, and sets `guardian_evm_session`.
- 200: `{ address, expires_at }`.
- Common errors: `authentication_failed`, `invalid_input`.

### POST /evm/auth/logout

- Requires `guardian_evm_session`.
- Clears the cookie-backed EVM session.
- 200: `{ success: true }`.

### POST /evm/accounts

- Requires `guardian_evm_session`.
- Body: `{ chain_id: number, account_address: string, multisig_validator_address: string }`.
- Guardian derives the canonical `account_id`, resolves RPC by `chain_id`, uses the configured shared EntryPoint address, verifies the session EOA is a current validator signer, verifies `isModuleInstalled(1, validator, 0x)`, snapshots validator signer EOAs and threshold, and stores account metadata. No Miden state snapshot or Guardian acknowledgement key is created.
- 200: `{ account_id, chain_id, account_address, multisig_validator_address, signers, threshold }`.
- Common errors: `unsupported_evm_chain`, `rpc_unavailable`, `rpc_validation_failed`, `signer_not_authorized`, `invalid_network_config`.

### POST /evm/proposals

- Requires `guardian_evm_session`.
- Body: `{ account_id, user_op_hash, payload, nonce, ttl_seconds, signature }`.
- Guardian loads the registered EVM account, verifies the session EOA is a configured signer, refreshes validator installation and signer/threshold data through Alloy, verifies `signature` over `user_op_hash`, derives a deterministic proposal ID from `(account_id, validator_address, user_op_hash, nonce)`, and stores an active EVM proposal.
- Duplicate active creates with the same deterministic proposal ID are idempotent.
- 200: `EvmProposal`.
- Common errors: `account_not_found`, `unsupported_evm_chain`, `rpc_unavailable`, `rpc_validation_failed`, `signer_not_authorized`, `invalid_proposal_signature`, `invalid_evm_proposal`.

### GET /evm/proposals

- Requires `guardian_evm_session`.
- Query: `account_id`.
- Returns active EVM proposals for the account where the session EOA is in the stored signer snapshot.
- Lazily deletes expired proposals and proposals whose EntryPoint nonce has advanced past the stored nonce.
- 200: `{ proposals: EvmProposal[] }`.

### GET /evm/proposals/{proposal_id}

- Requires `guardian_evm_session`.
- Query: `account_id`.
- Returns an active proposal when the session EOA is in the stored signer snapshot.
- 200: `EvmProposal`.
- Common errors: `proposal_not_found`, `signer_not_authorized`.

### POST /evm/proposals/{proposal_id}/approve

- Requires `guardian_evm_session`.
- Body: `{ account_id, signature }`.
- Guardian derives signer identity from the session EOA, verifies the signer is in the stored snapshot, verifies `signature` over the stored `user_op_hash`, and rejects duplicate signer approvals.
- 200: `EvmProposal`.
- Common errors: `proposal_not_found`, `proposal_already_signed`, `invalid_proposal_signature`, `signer_not_authorized`.

### GET /evm/proposals/{proposal_id}/executable

- Requires `guardian_evm_session`.
- Query: `account_id`.
- Returns `{ hash, payload, signatures, signers }` once stored signatures meet the snapshot threshold.
- Before threshold, returns `insufficient_signatures`.

### POST /evm/proposals/{proposal_id}/cancel

- Requires `guardian_evm_session`.
- Body: `{ account_id }`.
- Proposer-only. Deletes the active EVM proposal.
- 200: `{ success: true }`.

### GET /pubkey

- No authentication.
- Query: optional `scheme=falcon|ecdsa`.
- Default Falcon response: `{ "commitment": "0x..." }`.
- ECDSA response: `{ "commitment": "0x...", "pubkey": "0x..." }`.

## Errors

Stable error codes include:

- `account_not_found`
- `account_already_exists`
- `account_data_unavailable`
- `invalid_account_id`
- `state_not_found`
- `delta_not_found`
- `invalid_delta`
- `conflict_pending_delta`
- `conflict_pending_proposal`
- `pending_proposals_limit`
- `commitment_mismatch`
- `invalid_commitment`
- `authentication_failed`
- `authorization_failed`
- `invalid_input`
- `storage_error`
- `network_error`
- `signing_error`
- `configuration_error`
- `proposal_not_found`
- `proposal_already_signed`
- `invalid_proposal_signature`
- `unsupported_for_network`
- `unsupported_evm_chain`
- `invalid_network_config`
- `rpc_unavailable`
- `rpc_validation_failed`
- `signer_not_authorized`
- `invalid_evm_proposal`
- `insufficient_signatures`
- `rate_limit_exceeded`

HTTP endpoints that return structured error envelopes include `code` when available. gRPC responses include `error_code` in response messages and use matching gRPC status codes for transport errors.

## gRPC

The gRPC surface mirrors the Miden state/delta methods. EVM account registration, session auth, and proposal coordination are HTTP-only under `/evm/*`; gRPC proposal methods remain Miden-oriented and reject EVM inputs with `unsupported_for_network`.

- `Configure(ConfigureRequest) -> ConfigureResponse`
- `PushDelta(PushDeltaRequest) -> PushDeltaResponse`
- `GetDelta(GetDeltaRequest) -> GetDeltaResponse`
- `GetDeltaSince(GetDeltaSinceRequest) -> GetDeltaSinceResponse`
- `GetState(GetStateRequest) -> GetStateResponse`
- `GetPubkey(GetPubkeyRequest) -> GetPubkeyResponse`
- `PushDeltaProposal(PushDeltaProposalRequest) -> PushDeltaProposalResponse`
- `GetDeltaProposals(GetDeltaProposalsRequest) -> GetDeltaProposalsResponse`
- `GetDeltaProposal(GetDeltaProposalRequest) -> GetDeltaProposalResponse`
- `SignDeltaProposal(SignDeltaProposalRequest) -> SignDeltaProposalResponse`
- `GetAccountByKeyCommitment(GetAccountByKeyCommitmentRequest) -> GetAccountByKeyCommitmentResponse`

`GetAccountByKeyCommitment` mirrors the HTTP `GET /state/lookup` route. Authentication is carried in gRPC metadata (`x-pubkey`, `x-signature`, `x-timestamp`) and signed under the **Lookup Request Signing** format. Errors propagate as `tonic::Status` via the structured `GuardianError` mapping (`InvalidInput → INVALID_ARGUMENT`, `AuthenticationFailed → UNAUTHENTICATED`, `StorageError → INTERNAL`); the response contains a `repeated AccountRef accounts` field, with empty list as the success-with-no-matches signal.

## Idempotency and Ordering

- `push_delta` MAY be retried by clients; identical Miden deltas SHOULD be treated as idempotent when possible.
- Miden `push_delta` enforces `prev_commitment` match.
- EVM proposal create is idempotent for duplicate active proposals with the same deterministic proposal ID.
- EVM proposals remain active/pending-only in the EVM proposal store; expired or finalized proposals are lazily deleted.

## Examples

### Miden Configure

```bash
curl -X POST http://localhost:3000/configure \
  -H 'content-type: application/json' \
  -H 'x-pubkey: 0x...' \
  -H 'x-signature: 0x...' \
  -H 'x-timestamp: 1700000000000' \
  -d '{
    "account_id": "0x...",
    "auth": { "MidenFalconRpo": { "cosigner_commitments": ["0x..."] } },
    "network_config": { "kind": "miden", "network_type": "testnet" },
    "initial_state": { "...": "..." }
  }'
```

### Miden Proposal Create

```bash
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
      "metadata": { "proposal_type": "p2id" },
      "signatures": []
    }
  }'
```

### EVM Account Registration And Proposal Create

```bash
curl 'http://localhost:3000/evm/auth/challenge?address=0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266'
```

```bash
curl -X POST http://localhost:3000/evm/accounts \
  -H 'content-type: application/json' \
  -H 'cookie: guardian_evm_session=...' \
  -d '{
    "chain_id": 31337,
    "account_address": "0x1111111111111111111111111111111111111111",
    "multisig_validator_address": "0x2222222222222222222222222222222222222222"
  }'
```

```bash
curl -X POST http://localhost:3000/evm/proposals \
  -H 'content-type: application/json' \
  -H 'cookie: guardian_evm_session=...' \
  -d '{
    "account_id": "evm:31337:0x1111111111111111111111111111111111111111",
    "user_op_hash": "0x...",
    "payload": "{\"packedUserOperation\":{}}",
    "nonce": "0",
    "ttl_seconds": 900,
    "signature": "0x..."
  }'
```
