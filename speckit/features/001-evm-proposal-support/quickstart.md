# Quickstart: Domain-separated EVM proposal support

## 1. Default Server Gate

Run the default server and call any `/evm/*` route.

Expected result:

- the route is absent from the default router
- no EVM session, metadata, contract read, or proposal write occurs
- existing Miden `/configure`, `/delta`, `/delta/proposal`, and `/state` flows
  continue to work

## 2. EVM-enabled Server

```bash
GUARDIAN_EVM_RPC_URLS=31337=http://127.0.0.1:8545 \
GUARDIAN_EVM_ENTRYPOINTS=31337=0x... \
cargo run -p guardian-server --features evm --bin server
```

## 3. Wallet Session

```text
GET  /evm/auth/challenge?address=0x...
POST /evm/auth/verify { address, nonce, signature }
POST /evm/auth/logout
```

Expected result:

- Guardian recovers the EOA from the EIP-712 challenge signature
- the nonce is consumed once
- `guardian_evm_session` is set as an expiring cookie

## 4. Register Account

```text
POST /evm/accounts
```

```json
{
  "chain_id": 31337,
  "account_address": "0x...",
  "multisig_validator_address": "0x..."
}
```

Expected result:

- Guardian derives `evm:<chain_id>:<account_address>`
- RPC and EntryPoint are resolved from server env maps
- `isModuleInstalled(1, validator, 0x)` succeeds
- the session EOA is a current validator signer
- signer snapshot and threshold are stored in metadata

## 5. Coordinate Proposal

```text
POST /evm/proposals
GET  /evm/proposals?account_id=...
GET  /evm/proposals/{proposal_id}?account_id=...
POST /evm/proposals/{proposal_id}/approve
GET  /evm/proposals/{proposal_id}/executable?account_id=...
POST /evm/proposals/{proposal_id}/cancel
```

Create request:

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

Expected result:

- initial and approval signatures recover to the session EOA
- duplicate approvals fail explicitly
- executable export fails with `insufficient_signatures` before threshold
- executable export returns `{ hash, payload, signatures, signers }` after
  threshold
- expired/finalized proposals are lazily removed

## 6. Validation

```bash
cargo test -p guardian-server
cargo test -p guardian-server --features evm
cargo check -p guardian-server --features postgres
cargo check -p guardian-server --features postgres,evm
cd packages/guardian-evm-client && npm test && npm run build
cd examples/evm-smoke-web && npm run typecheck && npm run build
```
