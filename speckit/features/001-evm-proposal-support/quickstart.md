# Quickstart: Add generic EVM proposal sharing and signing support

This quickstart is a validation-oriented walkthrough for the planned feature.
It focuses on the agreed v1 shape for network-aware EVM proposal sharing and
signing. EVM support is opt-in: a default server is expected to reject EVM
account, auth, and proposal inputs with `evm_support_disabled`.

## 0. Enable EVM support for EVM validation

Run EVM-specific validation only against a server built or deployed with the
server-side `evm` feature enabled.

Expected default-server result:

- Miden account configuration continues to work
- `network_config.kind = "evm"` is rejected with `evm_support_disabled`
- EVM proposal payloads are rejected with `evm_support_disabled`
- no EVM account metadata or proposal record is persisted

## 1. Configure a Miden account

Expected result:

- request includes `network_config.kind = "miden"`
- existing Miden auth and state validation still work
- account metadata persists Miden-specific network configuration

## 2. Configure an EVM account

Precondition: the server-side `evm` feature is enabled.

Expected request shape:

```json
{
  "account_id": "evm:1:0x0000000000000000000000000000000000000000",
  "auth": {
    "EvmEcdsa": {
      "signers": []
    }
  },
  "network_config": {
    "kind": "evm",
    "chain_id": 1,
    "account_address": "0x0000000000000000000000000000000000000000",
    "multisig_module_address": "0x0000000000000000000000000000000000000001",
    "rpc_endpoint": "https://rpc.example"
  },
  "initial_state": {}
}
```

Expected result:

- account configuration succeeds only if RPC-backed signer validation succeeds
- `account_id` matches the canonical `chain_id + account_address` identity
- the server derives the EVM signer snapshot and threshold view from the
  configured ERC-7579 multisig module via RPC
- account metadata persists `network_config`
- request-auth headers and replay protection still apply
- for EVM accounts, request auth uses EIP-712 over a server-reconstructed
  `GuardianRequest(account_id, timestamp, request_hash)` message

## 3. Create an EVM proposal

Precondition: the server-side `evm` feature is enabled.

Expected request shape:

```json
{
  "account_id": "evm:1:0x0000000000000000000000000000000000000000",
  "nonce": 1,
  "delta_payload": {
    "kind": "evm",
    "mode": "0x0000000000000000000000000000000000000000000000000000000000000000",
    "execution_calldata": "0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
    "signatures": []
  }
}
```

Expected result:

- proposal create routes through the EVM proposal capability
- signer authority is re-validated through RPC
- payload is validated as an ERC-7579 `execute(mode, executionCalldata)` shape
- non-empty submitted signature arrays are rejected on create
- proposal is stored as `pending`
- response returns a deterministic hash-based proposal identifier
- repeated create of the same normalized proposal is idempotent

## 4. List, get, and sign an EVM proposal

Precondition: the server-side `evm` feature is enabled.

Expected result:

- list/get/sign routes stay aligned between HTTP and gRPC
- proposal signatures use EIP-712 over `(mode, keccak256(execution_calldata))`
- signer identities are normalized EOA addresses
- repeated signatures by the same signer are rejected explicitly
- request auth remains explicit and replay-protected

## 5. Verify unsupported EVM flows

Precondition: the server-side `evm` feature is enabled.

Expected result:

- `push_delta`
- `get_delta`
- `get_delta_since`
- `get_state`
- canonicalization paths

all return explicit unsupported errors for EVM accounts and do not fall back to
Miden behavior.

## 6. Run validation

```bash
cargo test -p guardian-server
cargo test -p guardian-server --features evm
cargo test -p guardian-client
cd packages/guardian-client && npm test
cd packages/guardian-evm-client && npm test && npm run build
cd examples/evm-smoke-web && npm run typecheck && npm run build
```

Run the browser smoke when changing wallet signing, proposal collection, or
on-chain submission behavior.
