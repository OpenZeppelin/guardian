# API (HTTP and gRPC)

## Authentication

- Per-account Miden requests MUST include credentials authorized by the account's policy.
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

`metadata.proposal_type` is required and must be a non-empty string, but its value is **not** restricted to a fixed set: the server accepts any label (issue #266). The first-party multisig operations use `add_signer`, `remove_signer`, `change_threshold`, `update_procedure_threshold`, `switch_guardian`, `consume_notes`, and `p2id`; any other label is accepted and surfaced verbatim. Clients that do not model a given label bucket it as `custom` while preserving the original string for display. The server makes no security decision based on `proposal_type` — integrity comes from the tx_summary/state-commitment check, the cosigner threshold, and the GUARDIAN ack. Restricting which types an account may submit is a policy-layer concern, not a core-server one.

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

### Endpoint catalog

The authoritative, machine-readable description of every HTTP endpoint —
request/response shapes, status codes, query/path parameters, and auth
schemes — is the generated **OpenAPI spec**, not this document. See
[`docs/OPENAPI.md`](../docs/OPENAPI.md); the spec files live at
[`docs/openapi.json`](../docs/openapi.json) (combined) and the per-surface
[`docs/openapi-client.json`](../docs/openapi-client.json),
[`docs/openapi-dashboard.json`](../docs/openapi-dashboard.json), and
[`docs/openapi-evm.json`](../docs/openapi-evm.json). The gRPC contract is
[`crates/server/proto/guardian.proto`](../crates/server/proto/guardian.proto).

To avoid drift, this section does **not** restate per-endpoint
request/response shapes — it is an index plus the cross-cutting behavior
the OpenAPI spec cannot express. Wire shapes are defined once under
[Data Shapes](#data-shapes) (also covering gRPC) and in the OpenAPI
component schemas.

| Surface | Method & path | Auth | Summary |
| --- | --- | --- | --- |
| client | `POST /configure` | signed headers | Register an account with its auth set and initial state |
| client | `POST /delta` | signed headers | Push a signed single-key delta |
| client | `GET /delta` | signed headers | Fetch the delta at a nonce |
| client | `GET /delta/since` | signed headers | Merged delta since a nonce |
| client | `GET /state` | signed headers | Latest canonical state |
| client | `GET /state/lookup` | lookup signing (PoP) | Resolve a key commitment to account IDs |
| client | `GET /pubkey` | public | ACK public key / commitment |
| client | `POST /delta/proposal` | signed headers | Create a multisig proposal |
| client | `GET /delta/proposal` | signed headers | List pending proposals |
| client | `GET /delta/proposal/single` | signed headers | Fetch one proposal by commitment |
| client | `PUT /delta/proposal` | signed headers | Add a cosigner signature |
| dashboard | `GET /auth/challenge` | public | Operator login challenge |
| dashboard | `POST /auth/verify` | public | Verify challenge, establish session |
| dashboard | `POST /auth/logout` | session | Invalidate the operator session |
| dashboard | `GET /dashboard/accounts` | session + `dashboard:read` | Paginated account list |
| dashboard | `GET /dashboard/accounts/{account_id}` | session + `dashboard:read` | Account detail |
| dashboard | `GET /dashboard/accounts/{account_id}/snapshot` | session + `dashboard:read` | Decoded vault snapshot |
| dashboard | `GET /dashboard/accounts/{account_id}/deltas` | session + `dashboard:read` | Per-account delta feed |
| dashboard | `GET /dashboard/accounts/{account_id}/deltas/{nonce}` | session + `dashboard:read` | Decoded delta detail |
| dashboard | `GET /dashboard/accounts/{account_id}/proposals` | session + `dashboard:read` | Per-account proposal queue |
| dashboard | `POST /dashboard/accounts/{account_id}/pause` | session + `accounts:pause` | Pause an account |
| dashboard | `POST /dashboard/accounts/{account_id}/unpause` | session + `accounts:pause` | Unpause an account |
| dashboard | `GET /dashboard/info` | session + `dashboard:read` | Inventory & lifecycle summary |
| dashboard | `GET /dashboard/session` | session | Session introspection |
| dashboard | `GET /dashboard/deltas` | session + `dashboard:read` | Cross-account delta feed |
| dashboard | `GET /dashboard/proposals` | session + `dashboard:read` | Cross-account proposal feed |
| evm | `GET /evm/auth/challenge` | public | EIP-712 session challenge |
| evm | `POST /evm/auth/verify` | public | Verify challenge, establish session |
| evm | `POST /evm/auth/logout` | evm session | Invalidate the EVM session |
| evm | `POST /evm/accounts` | evm session | Register an EVM smart-account |
| evm | `POST /evm/proposals` | evm session | Create an EVM proposal |
| evm | `GET /evm/proposals` | evm session | List EVM proposals |
| evm | `GET /evm/proposals/{proposal_id}` | evm session | Fetch an EVM proposal |
| evm | `POST /evm/proposals/{proposal_id}/approve` | evm session | Approve an EVM proposal |
| evm | `GET /evm/proposals/{proposal_id}/executable` | evm session | Executable (threshold-met) proposal |
| evm | `POST /evm/proposals/{proposal_id}/cancel` | evm session | Cancel an EVM proposal |

"signed headers" = `x-pubkey` + `x-signature` + `x-timestamp` (see
[Miden Request Signing](#miden-request-signing)); "session" = the
`guardian_operator_session` cookie; "evm session" = the
`guardian_evm_session` cookie. EVM endpoints exist only when the server
is built with the `evm` feature.

### HTTP behavioral notes

Semantics not captured by the OpenAPI shapes:

- **Pagination.** Paginated dashboard endpoints return
  `{ items, next_cursor }`; `limit` defaults to 50 and is capped at 500
  (`invalid_limit` outside `[1, 500]`). Cursors are opaque and signed;
  tampered/stale cursors return `invalid_cursor`. Per-account feeds key
  the cursor on immutable fields (`nonce`, `(nonce, commitment)`) and are
  fully stable; cross-account feeds order by `status_timestamp` /
  `originating_timestamp` and MAY skip or repeat an entry whose timestamp
  is bumped mid-traversal (FR-005).
- **`/state/lookup`.** An empty `accounts` list is a successful response,
  not a 404 — distinguishing "no account" from "wrong key" would leak
  account presence to non-key-holders. Authentication is proof-of-possession
  of the queried commitment (see [Lookup Request Signing](#lookup-request-signing)).
- **EVM exclusions (v1).** EVM accounts return an empty page on the
  per-account proposal queue and never appear on the global proposal feed
  (FR-017); the account snapshot returns `unsupported_for_network` for EVM
  accounts (no Miden vault to decode).
- **Aggregate degradation.** On the filesystem backend, cross-account
  aggregates (`/dashboard/info`, `/dashboard/deltas`, `/dashboard/proposals`)
  short-circuit to `data_unavailable` (503) above the configured
  `filesystem_aggregate_threshold` (default 1,000 accounts) rather than
  full-scan the inventory; `total_account_count` is always returned (FR-029).
- **Account detail / snapshot.** Both are decode-only views of Guardian's
  stored state at the last-canonicalized commitment — no live Miden RPC and
  no cross-account joins. `has_pending_candidate: true` means the decoded
  vault may already be stale relative to the chain.
- **Cross-cutting errors.** Every endpoint may also return `429`
  (rate limit, with `Retry-After`) and `413` (body size limit); see
  [Rate Limiting](#rate-limiting) and [Request Size Limits](#request-size-limits).
  All errors use the structured `GuardianError` envelope (see [Errors](#errors)).

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
- `invalid_cursor` (dashboard pagination, see feature `005-operator-dashboard-metrics`)
- `invalid_limit`
- `invalid_status_filter`
- `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`
- `GUARDIAN_ACCOUNT_PAUSED`
- `data_unavailable`

HTTP endpoints that return structured error envelopes include `code` when available.
`GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` additionally carries
`missing_permissions: string[]` and `retryable: false`.
`GUARDIAN_ACCOUNT_PAUSED` additionally carries `paused_at`,
`paused_reason`, and `retryable: false`. gRPC responses include
`error_code` in response messages and use matching gRPC status codes for
transport errors.

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
