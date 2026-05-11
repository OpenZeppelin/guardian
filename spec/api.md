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

### GET /dashboard/accounts

- Requires `guardian_operator_session`.
- **Breaking change vs. `003-operator-account-apis`** (feature
  `005-operator-dashboard-metrics` US1 / FR-001..FR-008): the endpoint
  is now always paginated. The previous unparameterized full-inventory
  mode and the `total_count` field are removed. Aggregate inventory
  totals are now exposed only via `GET /dashboard/info`.
- Query: optional `limit` (default 50, max 500), optional `cursor`
  (opaque token, from a prior page's `next_cursor`).
- Ordered by `(updated_at DESC, account_id ASC)`. Cursor is stable
  under concurrent inserts; concurrent updates to `updated_at` MAY
  cause an account to be skipped or repeated across a traversal
  (FR-005 caveat — applies to this endpoint only because the sort key
  is mutable).
- 200 envelope: `{ items: DashboardAccountSummary[], next_cursor: string | null }`.
- Each entry shape unchanged from `003-operator-account-apis` per
  FR-006 (superset compatibility): `account_id`, `auth_scheme`,
  `authorized_signer_count`, `has_pending_candidate`,
  `current_commitment`, `state_status`, `created_at`, `updated_at`.
- 400: `invalid_limit` for `limit ∉ [1, 500]`; `invalid_cursor` for
  tampered, malformed, or stale cursor.
- 401: `authentication_failed` for missing or invalid operator session.

### GET /dashboard/info

- Requires `guardian_operator_session`.
- Returns a single point-in-time inventory and lifecycle health
  snapshot for the Guardian instance per feature
  `005-operator-dashboard-metrics` US2 / FR-008..FR-012.
- Response shape:
  - `service_status`: `"healthy" | "degraded"`.
  - `environment`: deployment environment identifier (e.g.
    `mainnet`, `testnet`). Set via `GUARDIAN_ENVIRONMENT`.
  - `total_account_count`: total configured accounts.
  - `latest_activity`: greater of the most recent delta status
    timestamp and the most recent in-flight proposal originating
    timestamp across all accounts; `null` when the inventory has
    produced no activity OR when this aggregate is degraded.
  - `delta_status_counts`: `{ candidate, canonical, discarded }`
    counts of persisted deltas grouped by lifecycle status.
  - `in_flight_proposal_count`: count of `Pending` rows in
    `delta_proposals` across all accounts.
  - `degraded_aggregates`: stable string identifiers of any
    cross-account aggregates that returned a degraded marker on this
    response. Possible values: `delta_status_counts`,
    `in_flight_proposal_count`, `latest_activity`.
- Per FR-009 the response intentionally **does not** carry per-network
  account counts or a singular "the network" field. v1 is
  Miden-oriented (Guardian default build); the dashboard knows its
  own deployment context.
- Per FR-029, on the filesystem backend above the configured
  `filesystem_aggregate_threshold` (default 1,000 accounts) the
  cross-account aggregates may be marked degraded rather than
  full-scanning the on-disk inventory; `total_account_count` is
  always returned.
- 401: `authentication_failed`.

### GET /dashboard/accounts/{account_id}/deltas

- Requires `guardian_operator_session` (operator dashboard auth).
- Query: optional `limit` (default 50, max 500), optional `cursor` (opaque, from a prior page's `next_cursor`).
- Returns the per-account delta feed, paginated newest-first by `nonce DESC`. The per-account `nonce` is a domain sequence number set at insert and never mutated (it is distinct from the table's `id` bigserial), so cursors are fully stable per FR-005.
- Surfaces only the lifecycle statuses persisted in `deltas`: `candidate`, `canonical`, `discarded`. `pending` proposals are exposed via `/dashboard/accounts/{id}/proposals`.
- 200 envelope: `{ items: DashboardDeltaEntry[], next_cursor: string | null }`.
- Each entry: `{ nonce, status, status_timestamp, prev_commitment, new_commitment | null, retry_count? }`. `retry_count` is present (default `0`) on `candidate` entries only.
- 400: `invalid_limit` for `limit ∉ [1, 500]`; `invalid_cursor` for tampered, malformed, or stale cursor.
- 404: `account_not_found`.
- 503: `data_unavailable` when metadata exists but delta records cannot be loaded.

### GET /dashboard/accounts/{account_id}/proposals

- Requires `guardian_operator_session`.
- Query: optional `limit` (default 50, max 500), optional `cursor`.
- Returns the in-flight multisig proposal queue for one account (i.e. `DeltaStatus::Pending` rows in `delta_proposals`), paginated newest-first by `(nonce DESC, commitment DESC)` — both fields immutable, fully stable cursors.
- Single-key Miden accounts and EVM accounts (`Auth::EvmEcdsa`) always return an empty page; EVM proposals do not flow through `delta_proposals` in v1 (see feature `005-operator-dashboard-metrics` FR-017).
- 200 envelope: `{ items: DashboardProposalEntry[], next_cursor: string | null }`.
- Each entry: `{ commitment, nonce, proposer_id, originating_timestamp, signatures_collected, signatures_required, prev_commitment, new_commitment | null }`. `signatures_required` is derived from the account's auth policy (`cosigner_commitments.len()` for `MidenFalconRpo` / `MidenEcdsa`).
- No raw signature bytes and no per-cosigner identity list are exposed (FR-021).
- Errors mirror the deltas endpoint: 400 `invalid_limit` / `invalid_cursor`, 404 `account_not_found`, 503 `data_unavailable`.

### GET /dashboard/accounts/{account_id}/snapshot

- Requires `guardian_operator_session`.
- Returns a **decoded snapshot** of Guardian's stored state for one account at the commitment Guardian last canonicalized. v1 surface exposes the Miden `AssetVault` (fungible + non-fungible entries). Spec reference: feature `005-operator-dashboard-metrics` FR-043..FR-046.
- The endpoint does **not** make live Miden RPC calls, perform cross-account aggregations, or join with delta history — the response is derived purely from `states.state_json` for the given account. New fields land on this response as additive top-level keys derivable from the same stored blob (FR-046).
- 200 shape:
  - `commitment`: hex state commitment the snapshot was decoded from. Equals the detail endpoint's `current_commitment` for the same account at the same point in time.
  - `updated_at`: RFC3339; equals the detail endpoint's `state_updated_at`.
  - `has_pending_candidate`: boolean. `true` means a candidate delta is in flight and has not yet been canonicalized — the vault below may already be stale relative to the chain.
  - `vault`:
    - `fungible`: array of `{ faucet_id: string, amount: string }`. Amounts are strings to preserve `u64` precision across JS clients (`Number.MAX_SAFE_INTEGER` is 2^53 − 1). Decimal handling and value/USD derivation are dashboard-client concerns.
    - `non_fungible`: array of `{ faucet_id: string, vault_key: string }`. `vault_key` is the canonical Word hex form for the asset entry.
- 400: `unsupported_for_network` when the account's `network_config` is EVM. EVM accounts have no Miden `AssetVault` to decode and the condition is permanent for this surface, so it is reported separately from `data_unavailable` (which implies "retry later"). Detection uses `metadata.network_config.is_evm()` per AGENTS.md §5.
- 404: `account_not_found`.
- 503: `data_unavailable` when metadata exists but the state row cannot be loaded, or when the stored blob fails to deserialize as a Miden `Account`. Both are transient/recoverable conditions.

### GET /dashboard/deltas

- Requires `guardian_operator_session`.
- Query: optional `limit` (default 50, max 500), optional `cursor`, optional `status` (comma-separated subset of `{candidate, canonical, discarded}`, e.g. `status=candidate,canonical`).
- Cross-account delta feed paginated newest-first by `status_timestamp DESC` with `(account_id ASC, nonce ASC)` as the stable tie-breaker. Per FR-005, cursor traversal is stable under concurrent inserts but a delta whose `status_timestamp` is bumped mid-traversal (e.g. `candidate → canonical`) MAY be skipped or repeated.
- 200 envelope: `{ items: DashboardGlobalDeltaEntry[], next_cursor: string | null }`. Each entry has every field of a per-account delta entry (per US3) plus `account_id`. `pending` entries are not surfaced here — they live on the global proposal feed.
- 400: `invalid_limit`, `invalid_cursor`, `invalid_status_filter` for unknown values in the `?status=` filter or empty CSV tokens.
- 503: `data_unavailable` above the configured `filesystem_aggregate_threshold` (default 1,000 accounts) per FR-029, OR when a per-account delta read fails.
- Smallest priority slice of feature `005-operator-dashboard-metrics` (US6 / FR-031..FR-035, FR-040).

### GET /dashboard/proposals

- Requires `guardian_operator_session`.
- Query: optional `limit` (default 50, max 500), optional `cursor`. **No** `status` filter — every entry is in-flight by definition (FR-035).
- Cross-account in-flight proposal feed paginated newest-first by `originating_timestamp DESC` with `(account_id ASC, commitment ASC)` as the stable tie-breaker. Originating timestamp is immutable while the proposal remains in the queue, so cursor traversal is fully stable for the lifetime of a queued proposal.
- 200 envelope: `{ items: DashboardGlobalProposalEntry[], next_cursor: string | null }`. Each entry has every field of a per-account proposal entry (per US4) plus `account_id`.
- EVM accounts (`Auth::EvmEcdsa`) do not appear in the feed in v1 (FR-017).
- 400: `invalid_limit`, `invalid_cursor`. 503: `data_unavailable` above the threshold per FR-029.
- Smallest priority slice of feature `005-operator-dashboard-metrics` (US7 / FR-035..FR-037, FR-040).

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
- `data_unavailable`

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
