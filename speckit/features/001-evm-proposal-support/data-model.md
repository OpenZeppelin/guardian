# Data Model: Add generic EVM proposal sharing and signing support

## 1. AccountMetadata

Persisted per account in filesystem and Postgres metadata stores.

| Field | Type | Notes |
|-------|------|-------|
| `account_id` | `String` | Existing account identifier; for EVM this is canonical `evm:<chain_id>:<normalized_contract_address>` |
| `auth` | `Auth` | Persisted auth policy and signer snapshot |
| `network_config` | `NetworkConfig` | New per-account network selection and config |
| `created_at` | `String` | Existing RFC3339 timestamp |
| `updated_at` | `String` | Existing RFC3339 timestamp |
| `has_pending_candidate` | `bool` | Existing canonicalization flag |
| `last_auth_timestamp` | `Option<i64>` | Existing replay-protection CAS field |

### Validation Rules

- `network_config` is required.
- Missing `network_config` is invalid rather than implicitly mapped to a legacy
  server-global Miden setting.
- For `network_config.kind = "evm"`, `account_id` must match the canonical
  identity derived from `chain_id + contract_address`.
- Filesystem and Postgres metadata encodings must remain semantically
  equivalent.

## 2. NetworkConfig

Tagged per-account network configuration.

### `MidenNetworkConfig`

| Field | Type | Notes |
|-------|------|-------|
| `kind` | `"miden"` | Discriminator |
| `network_type` | `MidenNetworkType` | `local`, `devnet`, or `testnet` |

### `EvmNetworkConfig`

| Field | Type | Notes |
|-------|------|-------|
| `kind` | `"evm"` | Discriminator |
| `chain_id` | `u64` | Part of canonical account identity |
| `contract_address` | `String` | Normalized hex address; part of canonical account identity |
| `rpc_endpoint` | `String` | Required RPC authority for signer validation |

### Validation Rules

- `chain_id` must be a positive integer.
- `contract_address` must be a normalized `0x`-prefixed 20-byte lowercase hex
  address and validated at the boundary.
- `rpc_endpoint` must be a valid URL string and is treated as trusted account
  configuration in v1.

## 3. Auth

Persisted auth policy plus signer snapshot.

| Variant | Fields | Notes |
|---------|--------|-------|
| `MidenFalconRpo` | `cosigner_commitments: Vec<String>` | Existing Miden Falcon path |
| `MidenEcdsa` | `cosigner_commitments: Vec<String>` | Existing Miden ECDSA path |
| `EvmEcdsa` | `signers: Vec<String>` | New EVM ECDSA path; normalized EOA address snapshot refreshed from RPC |

### Validation Rules

- Request-signature verification must be separated from signer authorization.
- EVM authorization uses RPC as the source of truth on every relevant action.
- Stored EVM `signers` may be used as a snapshot or cache, but not as the only
  authority source.
- EVM `signers` are normalized EOA addresses only in v1.

## 4. Proposal Record

V1 keeps the current proposal transport/storage envelope to constrain blast
radius, but proposal semantics become network-aware.

| Field | Type | Notes |
|-------|------|-------|
| `account_id` | `String` | Existing account namespace |
| `nonce` | `u64` | Ordering field; for EVM this is Guardian-local ordering only |
| `commitment` | `String` | Deterministic proposal identifier; currently carried as response `commitment` |
| `delta_payload` | `serde_json::Value` / JSON object | Network-specific proposal payload plus signatures/metadata |
| `status` | `DeltaStatus` | Pending proposal state remains shared |
| `proposer_id` | `String` | Stored inside pending status |
| `cosigner_sigs` | `Vec<CosignerSignature>` | Stored inside pending status |

### Interpretation Rules

- For Miden proposals, current `tx_summary`-driven semantics remain unchanged.
- For EVM proposals, the inner payload is the normalized ERC-7579 execution
  shape described below and is normalized before hashing or persistence.
- For EVM proposals, `prev_commitment`, `new_commitment`, `ack_sig`,
  `ack_pubkey`, and `ack_scheme` remain unused in v1.

### `EvmProposalPayload`

| Field | Type | Notes |
|-------|------|-------|
| `kind` | `"evm"` | Discriminator |
| `mode` | `String` | `0x`-prefixed 32-byte hex ERC-7579 mode |
| `execution_calldata` | `String` | `0x`-prefixed hex bytes for ERC-7579 execution calldata |
| `signatures` | `Vec<SubmittedSignature>` | Empty on create; appended by sign flow |

### EVM Proposal Validation Rules

- EVM proposal payloads model ERC-7579 `execute(mode, executionCalldata)` only.
- Supported v1 modes are single-call and batch-call with default exec type and
  zero selector/mode payload.
- Delegatecall, try-mode, and custom selector/payload modes are unsupported in
  v1.
- `signatures` must be empty when the proposal is first created.
- EVM submitted signatures carry `signer_id` as a normalized EOA address.

## 5. Proposal Identifier

Derived value, not an independently configured field.

| Field | Type | Notes |
|-------|------|-------|
| `proposal_id` | `String` | Deterministic hash-based identifier |

### Validation Rules

- The identifier is a `0x`-prefixed lowercase 32-byte hex value derived from
  `keccak256(abi.encode(chain_id, contract_address, mode,
  keccak256(execution_calldata)))`.
- The identifier must be derived from normalized inputs, not raw incoming JSON.
- HTTP and gRPC must yield the same identifier for semantically equivalent
  proposal payloads.
- Rust and TypeScript must model the same identifier semantics.
- The identifier excludes the local proposal nonce, collected signatures, and
  timestamps.

## 6. Network Capabilities

Internal design model for network-specific behavior.

| Capability | Purpose |
|------------|---------|
| `AccountConfigCapability` | Validate account configuration and initial signer state |
| `SignerAuthorityCapability` | Resolve or verify authorized signers for an account |
| `ProposalCapability` | Normalize proposal payloads, compute proposal identifiers, and validate proposal-specific operations |
| `DeltaStateCapability` | Handle state, delta, and canonicalization operations where supported |

### Capability Rules

- Miden implements all capabilities needed by existing flows.
- EVM v1 implements account configuration, signer authority, and proposal
  capabilities only.
- Unsupported EVM delta/state capabilities return explicit errors.

## 7. State Transitions

### Miden

- Existing pending -> candidate -> canonical/discarded lifecycle remains intact.

### EVM v1

- `pending` is the only supported proposal state.
- Candidate, canonical, discarded, or auto-reconciled proposal transitions are
  out of scope for this feature.
- Unsupported lifecycle requests must fail explicitly.
- Re-submitting the same normalized EVM proposal is idempotent and returns the
  existing pending proposal.
- Pending EVM proposals stay pending until a future explicit cleanup or
  reconciliation feature is added, subject to the per-account pending-proposal
  cap.
