# Quickstart: Add generic EVM proposal sharing and signing support

This quickstart is a validation-oriented walkthrough for the planned feature.
It focuses on the safe refactor path that can proceed before the final EVM
contract details are settled.

## 1. Configure a Miden account

Expected result:

- request includes `network_config.kind = "miden"`
- existing Miden auth and state validation still work
- account metadata persists Miden-specific network configuration

## 2. Configure an EVM account

Expected request shape:

```json
{
  "account_id": "evm-account-placeholder",
  "auth": {
    "EvmEcdsa": {
      "cosigner_commitments": []
    }
  },
  "network_config": {
    "kind": "evm",
    "chain_id": 1,
    "contract_address": "0x0000000000000000000000000000000000000000",
    "rpc_endpoint": "https://rpc.example"
  },
  "initial_state": {}
}
```

Expected result:

- account configuration succeeds only if RPC-backed signer validation succeeds
- account metadata persists `network_config`
- request-auth headers and replay protection still apply

## 3. Create an EVM proposal

Expected result:

- proposal create routes through the EVM proposal capability
- signer authority is re-validated through RPC
- proposal is stored as `pending`
- response returns a deterministic hash-based proposal identifier

Note:

- the exact inner EVM proposal payload remains pending the contract-team answer
- contract drafts currently treat the EVM executable payload as a normalized
  object placeholder

## 4. List, get, and sign an EVM proposal

Expected result:

- list/get/sign routes stay aligned between HTTP and gRPC
- repeated signatures by the same signer are rejected explicitly
- request auth remains explicit and replay-protected

## 5. Verify unsupported EVM flows

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
cargo test -p private-state-manager-server
cargo test -p private-state-manager-client
cd packages/psm-client && npm test
```

Run example smoke checks only if the base-client changes propagate into example
surfaces.
