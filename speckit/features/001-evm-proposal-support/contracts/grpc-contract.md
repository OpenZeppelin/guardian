# gRPC Contract Draft: Add generic EVM proposal sharing and signing support

This document captures the expected gRPC contract direction before the final
proto edits are made.

## Goals

- keep the existing service shape and method names in v1
- extend `ConfigureRequest` with account-level `network_config`
- keep proposal create/list/get/sign available through current RPC names
- preserve Miden behavior and make unsupported EVM delta/state flows explicit

## Proposed Proto Changes

### 1. Add `NetworkConfig`

```proto
message NetworkConfig {
  oneof config {
    MidenNetworkConfig miden = 1;
    EvmNetworkConfig evm = 2;
  }
}

message MidenNetworkConfig {
  string network_type = 1; // local | devnet | testnet
}

message EvmNetworkConfig {
  uint64 chain_id = 1;
  string contract_address = 2;
  string rpc_endpoint = 3;
}
```

### 2. Extend `ConfigureRequest`

```proto
message ConfigureRequest {
  string account_id = 1;
  AuthConfig auth = 2;
  string initial_state = 3;
  NetworkConfig network_config = 4;
}
```

For EVM accounts, `account_id` uses the canonical form
`evm:<chain_id>:<normalized_contract_address>`.

### 3. Extend `AuthConfig`

```proto
message AuthConfig {
  oneof auth_type {
    MidenFalconRpoAuth miden_falcon_rpo = 1;
    MidenEcdsaAuth miden_ecdsa = 2;
    EvmEcdsaAuth evm_ecdsa = 3;
  }
}

message EvmEcdsaAuth {
  repeated string signers = 1;
}
```

For EVM accounts, `signers` are normalized EOA addresses. V1 does not support
ERC-1271 or generic ERC-7913 verifier-key signers.

## Request Authentication

Transport metadata remains:

- `x-pubkey`
- `x-signature`
- `x-timestamp`

For EVM accounts, `x-pubkey` keeps its legacy name and carries the normalized
signer address. EVM request auth uses EIP-712 over a server-reconstructed typed
message:

```text
Domain:
  name = "Guardian Request"
  version = "1"
  chainId = network_config.chain_id
  verifyingContract = network_config.contract_address

AuthRequest(
  string accountId,
  uint64 timestampMs,
  bytes32 payloadHash
)
```

For gRPC, `payloadHash = keccak256(protobuf_request_bytes)`.

## Proposal RPC Direction

For v1, keep these methods:

- `PushDeltaProposal`
- `GetDeltaProposals`
- `GetDeltaProposal`
- `SignDeltaProposal`

The outer RPC names stay stable. The inner `delta_payload` JSON becomes
network-aware:

- Miden keeps its current `tx_summary`-driven JSON shape.
- EVM uses the exact normalized JSON shape:

```json
{
  "kind": "evm",
  "mode": "0x...",
  "execution_calldata": "0x...",
  "signatures": []
}
```

- EVM `mode` encodes ERC-7579 execution and v1 supports only single-call and
  batch-call modes with default exec type and zero selector/mode payload.
- EVM proposal creation rejects non-empty `signatures`.
- EVM proposal `nonce` remains a Guardian-local ordering field only.

## EVM Proposal Signature Meaning

EVM proposal cosigners sign a Guardian-defined EIP-712 coordination message:

```text
Domain:
  name = "Guardian EVM Proposal"
  version = "1"
  chainId = network_config.chain_id
  verifyingContract = network_config.contract_address

PsmEvmProposal(
  bytes32 mode,
  bytes32 executionCalldataHash
)
```

Where `executionCalldataHash = keccak256(execution_calldata)`.

The signer address is recovered from the ECDSA signature and recorded as the
normalized EOA `signer_id`.

## Unsupported EVM RPC Behavior

These methods remain available for Miden but must return explicit unsupported
behavior for EVM accounts in this feature:

- `PushDelta`
- `GetDelta`
- `GetDeltaSince`
- `GetState`

Canonicalization-related flows also remain unsupported for EVM accounts.

## Response Semantics

- `PushDeltaProposalResponse.commitment` remains the outward proposal identifier.
- For EVM v1, that identifier is
  `keccak256(abi.encode(chain_id, contract_address, mode, keccak256(execution_calldata)))`.
- HTTP and gRPC must produce the same proposal identifier for equivalent
  normalized EVM proposals.
- The identifier excludes local proposal nonce, collected signatures, and
  timestamps.
- Re-submitting the same EVM proposal is idempotent and returns the existing
  pending proposal.

## Stable Application Error Codes

Both transports should expose the same application-level error codes alongside
their native HTTP or gRPC status:

- `unsupported_for_network`
- `invalid_network_config`
- `rpc_unavailable`
- `rpc_validation_failed`
- `signer_not_authorized`
- `invalid_evm_proposal`
- `invalid_proposal_signature`
- `proposal_already_signed`

## Deferred Topics

- RPC endpoint replacement or rotation policy is deferred in v1.
