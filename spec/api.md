# API (HTTP and gRPC)

## Authentication

- Per-account authentication: requests MUST include credentials authorised by the account's policy.
- Credentials are provided via HTTP headers `x-pubkey`, `x-signature`, `x-timestamp` and the same keys in gRPC metadata.
- `x-pubkey` is interpreted by the account auth policy:
  - Miden Falcon/ECDSA accounts use the serialized public key or its commitment.
  - EVM accounts use the normalized EOA signer address.
- The authenticated signer is checked against the account's allowlist or network-owned signer set.
- Replay protection applies to every authenticated request.

### Replay Protection

- The signed payload includes a Unix timestamp in milliseconds.
- The server enforces a maximum clock skew window of **300,000 milliseconds** (5 minutes).
- The server tracks `last_auth_timestamp` per account; requests with a timestamp less than or equal to the last accepted timestamp are rejected.
- `last_auth_timestamp` is updated atomically when authentication succeeds.

### Miden Request Signing

- HTTP request payload digest: RPO256 over canonical JSON bytes of the request payload (`body` for `POST`/`PUT`, query object for `GET`).
- gRPC request payload digest: RPO256 over protobuf-encoded request bytes.
- Signed message format: `RPO256_hash([account_id_prefix, account_id_suffix, timestamp_ms, payload_hash_0, payload_hash_1, payload_hash_2, payload_hash_3])`.

### EVM Request Signing

- EVM support is behavior-gated by the server `evm` feature. Schema variants are visible in all builds, but default builds reject EVM configuration, auth, and proposal requests with `evm_support_disabled` before storage mutation.
- EVM request auth uses `eth_signTypedData_v4` over EIP-712 typed data:
  - Domain: `{ name: "Guardian EVM Request", version: "1", chainId, verifyingContract: account_address }`.
  - Message: `{ account_id, timestamp, request_hash }`.
  - `request_hash` is `keccak256` of the transport's canonical request payload bytes.
- The recovered address MUST match `x-pubkey`.

## Data Shapes

### Account Identifiers

- Miden account IDs use the existing Miden account identifier format.
- EVM account IDs are canonical strings: `evm:<chain_id>:<normalized_account_address>`.
- `account_address` is the identity and EIP-712 verifying contract address.
- `multisig_module_address` is the ERC-7579-style module address used for signer and threshold reads.

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

gRPC uses `AuthConfig::{miden_falcon_rpo, miden_ecdsa, evm_ecdsa}`.

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
  "multisig_module_address": "0x...",
  "rpc_endpoint": "http://127.0.0.1:8545"
}
```

- `network_config` is optional for legacy Miden requests and defaults to `{ "kind": "miden", "network_type": "local" }`.
- EVM `chain_id` MUST be greater than zero.
- EVM addresses are normalized to lowercase `0x`-prefixed 20-byte addresses.
- EVM `rpc_endpoint` MUST be `http://` or `https://`.
- When `GUARDIAN_EVM_ALLOWED_CHAIN_IDS` is set, the EVM chain ID MUST be included in that comma-separated allowlist.

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

EVM delta proposals use:

```json
{
  "kind": "evm",
  "mode": "0x0000000000000000000000000000000000000000000000000000000000000000",
  "execution_calldata": "0x",
  "signatures": []
}
```

- EVM create requests MUST include an empty `signatures` array.
- EVM `mode` MUST be a 32-byte hex value.
- EVM v1 supports single-call or batch-call ERC-7579 modes with default exec type and zero selector/payload.
- `execution_calldata` MUST be `0x`-prefixed hex.

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
- EVM signer IDs are normalized EOA addresses.
- EVM proposal signatures are EIP-712 signatures over:
  - Domain: `{ name: "Guardian EVM Proposal", version: "1", chainId, verifyingContract: account_address }`.
  - Message: `{ mode, execution_calldata_hash }`.

### DeltaProposalEnvelope

```json
{ "delta": {}, "commitment": "0x..." }
```

- Miden proposal IDs are derived by the configured Miden network client from `(account_id, nonce, tx_summary)`.
- EVM proposal IDs are `keccak256(abi.encode(chain_id, account_address, mode, keccak256(execution_calldata)))`.
- EVM duplicate create is idempotent when the existing proposal is still pending.

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
- EVM behavior with `evm` feature: requires canonical `evm:<chain_id>:<account_address>` account ID, `EvmEcdsa` auth, and empty `initial_state`; reads signers and threshold from the configured module; verifies the request signer is authorized; stores normalized EVM metadata only.
- EVM behavior without `evm` feature: rejects before persistence with `evm_support_disabled`.
- 200: `{ success: true, message: string, ack_pubkey: string, ack_commitment: string }`.
- EVM configure responses return empty acknowledgement fields because EVM proposal coordination does not use Guardian delta acknowledgements in v1.
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

### POST /delta/proposal

- Headers: `x-pubkey`, `x-signature`, `x-timestamp`.
- Body: `{ account_id: string, nonce: u64, delta_payload: MidenProposalPayload | EvmProposalPayload }`.
- Miden behavior: validates proposer credentials, validates `tx_summary` against the latest persisted state, derives a proposal ID via the network client, and persists a pending proposal.
- EVM behavior with `evm` feature: validates EVM payload shape, verifies the caller is an authorized module signer, computes the deterministic EVM proposal ID, and stores a pending proposal with empty `prev_commitment`, `new_commitment`, `ack_sig`, `ack_pubkey`, and `ack_scheme`.
- EVM behavior without `evm` feature: rejects before persistence with `evm_support_disabled`.
- 200: `DeltaProposalEnvelope`.
- Common errors: `invalid_delta`, `invalid_evm_proposal`, `account_not_found`, `authentication_failed`, `conflict_pending_delta`, `pending_proposals_limit`, `evm_support_disabled`.

### GET /delta/proposal

- Headers: `x-pubkey`, `x-signature`, `x-timestamp`.
- Query: `account_id`.
- Returns only pending proposals, ordered by nonce.
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
- EVM behavior with `evm` feature: loads the pending proposal, verifies the EIP-712 proposal signature, checks the recovered signer matches the authenticated request signer, rejects duplicates, and appends the ECDSA signature.
- 200: `DeltaObject`.
- Common errors: `proposal_not_found`, `proposal_already_signed`, `invalid_proposal_signature`, `signer_not_authorized`, `evm_support_disabled`.

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
- `evm_support_disabled`
- `unsupported_for_network`
- `invalid_network_config`
- `rpc_unavailable`
- `rpc_validation_failed`
- `signer_not_authorized`
- `invalid_evm_proposal`
- `insufficient_signatures`
- `rate_limit_exceeded`

HTTP endpoints that return structured error envelopes include `code` when available. gRPC responses include `error_code` in response messages and use matching gRPC status codes for transport errors.

## gRPC

The gRPC surface mirrors HTTP methods and data shapes. Credentials are provided via metadata headers.

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

## Idempotency and Ordering

- `push_delta` MAY be retried by clients; identical Miden deltas SHOULD be treated as idempotent when possible.
- Miden `push_delta` enforces `prev_commitment` match.
- EVM proposal create is idempotent for duplicate pending proposals with the same `(chain_id, account_address, mode, keccak256(execution_calldata))`.
- EVM proposals are pending-only in v1; execution tracking and reconciliation are outside this contract.

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

### EVM Configure

```bash
curl -X POST http://localhost:3000/configure \
  -H 'content-type: application/json' \
  -H 'x-pubkey: 0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266' \
  -H 'x-signature: 0x...' \
  -H 'x-timestamp: 1700000000000' \
  -d '{
    "account_id": "evm:31337:0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc",
    "auth": { "EvmEcdsa": { "signers": ["0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"] } },
    "network_config": {
      "kind": "evm",
      "chain_id": 31337,
      "account_address": "0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc",
      "multisig_module_address": "0x...",
      "rpc_endpoint": "http://127.0.0.1:8545"
    },
    "initial_state": {}
  }'
```

### EVM Proposal Create

```bash
curl -X POST http://localhost:3000/delta/proposal \
  -H 'content-type: application/json' \
  -H 'x-pubkey: 0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266' \
  -H 'x-signature: 0x...' \
  -H 'x-timestamp: 1700000000000' \
  -d '{
    "account_id": "evm:31337:0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc",
    "nonce": 1,
    "delta_payload": {
      "kind": "evm",
      "mode": "0x0000000000000000000000000000000000000000000000000000000000000000",
      "execution_calldata": "0x",
      "signatures": []
    }
  }'
```
