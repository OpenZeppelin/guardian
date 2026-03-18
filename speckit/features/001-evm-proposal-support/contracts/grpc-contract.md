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
  repeated string cosigner_commitments = 1;
}
```

## Proposal RPC Direction

For v1, keep these methods:

- `PushDeltaProposal`
- `GetDeltaProposals`
- `GetDeltaProposal`
- `SignDeltaProposal`

The outer RPC names stay stable. The inner `delta_payload` JSON becomes
network-aware:

- Miden keeps its current `tx_summary`-driven JSON shape.
- EVM uses normalized JSON representing the executable proposal payload plus
  signature entries.
- The exact EVM inner JSON fields remain pending the contract-team answer.

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
- For EVM v1, that identifier is a deterministic hash-based PSM value.
- HTTP and gRPC must produce the same proposal identifier for equivalent
  normalized EVM proposals.

## Remaining Team Inputs

- exact multisig contract reads required over RPC
- final EVM proposal payload structure
- exact signed-bytes definition for EVM cosigners
- final lifecycle behavior for pending proposals
- final RPC failure and endpoint-rotation policy
