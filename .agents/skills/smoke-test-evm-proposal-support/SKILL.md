---
name: smoke-test-evm-proposal-support
description: Run or guide smoke testing of Guardian feature-gated EVM proposal support with Anvil, an EVM-enabled guardian-server, `@openzeppelin/guardian-evm-client`, and `examples/evm-smoke-web`. Use when Codex needs to verify EVM account configuration, EIP-712 request auth, proposal create/list/get/sign, collecting multiple signatures, or on-chain submit against an ERC-7579-style module.
---

# Smoke Test EVM Proposal Support

Use this skill for local EVM proposal smoke tests. Keep EVM client checks isolated to `packages/guardian-evm-client`; do not modify or validate through the base Rust `guardian-client` crate or the base TypeScript `packages/guardian-client` package unless the user explicitly asks.

## Preflight

Read current source before assuming labels, endpoints, or response shapes:

- `packages/guardian-evm-client/src/index.ts`
- `examples/evm-smoke-web/src/App.tsx`
- `crates/server/src/evm.rs`
- `crates/server/src/services/configure_account.rs`
- `crates/server/src/services/push_delta_proposal.rs`
- `crates/server/src/services/sign_delta_proposal.rs`

Run the focused checks before manual browser work:

```bash
cargo test -p guardian-server
cargo test -p guardian-server --features evm
cd packages/guardian-evm-client && npm test && npm run build
cd examples/evm-smoke-web && npm run typecheck && npm run build
```

Use `git diff -- packages/guardian-client crates/client crates/shared` when the user wants the EVM client isolated from the base clients. Those paths should stay unchanged unless a separate contract change requires them.

## Local Stack

Start or verify Anvil:

```bash
anvil --host 127.0.0.1 --port 8545
cast chain-id --rpc-url http://127.0.0.1:8545
```

Start Guardian with the EVM feature and isolated runtime state. Set `GUARDIAN_NETWORK_TYPE=MidenTestnet` when no local Miden node is running; the EVM smoke still targets `EVM_CHAIN_ID`, but Guardian initializes its default Miden client at startup and `MidenLocal` requires a node at `http://localhost:57291`.

```bash
SMOKE_DIR=$(mktemp -d /tmp/guardian-evm-smoke.XXXXXX)
mkdir -p "$SMOKE_DIR/storage" "$SMOKE_DIR/metadata" "$SMOKE_DIR/keystore"

GUARDIAN_STORAGE_PATH="$SMOKE_DIR/storage" \
GUARDIAN_METADATA_PATH="$SMOKE_DIR/metadata" \
GUARDIAN_KEYSTORE_PATH="$SMOKE_DIR/keystore" \
GUARDIAN_NETWORK_TYPE=MidenTestnet \
GUARDIAN_EVM_ALLOWED_CHAIN_IDS=31337 \
RUST_LOG=info \
cargo run -p guardian-server --features evm --bin server
```

If port `3000` or `50051` is occupied, inspect the owner first:

```bash
lsof -nP -iTCP:3000 -iTCP:50051 -sTCP:LISTEN
```

Reuse the process only if it was started from the current EVM-enabled build with the intended storage and chain allowlist. Otherwise stop it or use a clean process.

## Mock Module

The smoke needs an ERC-7579-style module that exposes:

- `getSigners(address,uint256,uint256) returns (bytes[])`
- `getSignerCount(address) returns (uint256)`
- `isSigner(address,bytes) returns (bool)`
- `threshold(address) returns (uint64)`
- `submitProposal(address,bytes32,bytes,bytes[])`
- `submitted(bytes32) returns (bool)`
- `submittedSignatureCounts(bytes32) returns (uint256)`

Deploy the bundled local mock to Anvil:

```bash
EVM_RPC_URL=http://127.0.0.1:8545 \
EVM_ACCOUNT_ADDRESS=0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC \
EVM_SIGNER_ONE=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266 \
EVM_SIGNER_TWO=0x70997970C51812dc3A010C7d01b50e0d17dc79C8 \
bash .agents/skills/smoke-test-evm-proposal-support/scripts/deploy-smoke-module.sh
```

Record the printed `EVM_MODULE_ADDRESS`. The default account and signer addresses are Anvil's standard first three accounts.

## Client Smoke

After Guardian, Anvil, and a module are live, run the bundled client smoke from the repo root:

```bash
GUARDIAN_URL=http://127.0.0.1:3000 \
EVM_RPC_URL=http://127.0.0.1:8545 \
EVM_CHAIN_ID=31337 \
EVM_ACCOUNT_ADDRESS=0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC \
EVM_MODULE_ADDRESS=<module-address> \
EVM_SIGNER_ONE=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266 \
EVM_SIGNER_TWO=0x70997970C51812dc3A010C7d01b50e0d17dc79C8 \
node .agents/skills/smoke-test-evm-proposal-support/scripts/run-evm-client-smoke.mjs
```

The script uses the workspace build output at `packages/guardian-evm-client/dist/index.js`, so run `npm run build` in that package first after TypeScript edits.

## Browser Smoke

Use the Vite app when the user asks for a browser or injected-wallet pass:

```bash
cd examples/evm-smoke-web
npm run dev -- --host 127.0.0.1 --port 5173
```

Open `http://127.0.0.1:5173/` in a browser with an injected EVM wallet connected to Anvil. Fill in Guardian URL, chain ID, account address, module address, RPC endpoint, mode, and execution calldata. Use different signer accounts to collect more than one signature over the same proposal, then submit the fetched proposal on-chain.

For browser wallet prompts, always let the human complete passwords or confirmations when needed. After the user says a prompt is confirmed, continue with the next action and confirm future wallet prompts without asking only when the user explicitly told Codex to do so for that run.

## Assertions

Mark the smoke passed only when all relevant assertions hold:

- Guardian was started with `--features evm`.
- The default non-EVM server path still rejects EVM requests with stable code `evm_support_disabled` when that negative gate is part of the task.
- `configure` succeeds for `evm:<chain_id>:<account_address>`.
- The module signer set and threshold match the intended signers.
- Proposal creation returns a deterministic commitment/proposal ID for `(chain_id, account_address, mode, keccak256(execution_calldata))`.
- `listProposals` or `getProposal` sees the created proposal.
- Two distinct signer addresses sign the same proposal.
- The fetched proposal contains two stored ECDSA signatures.
- On-chain `submitProposal` succeeds.
- `submitted(proposal_id)` is true and `submittedSignatureCounts(proposal_id)` is at least 2.

## Failure Triage

- `evm_support_disabled`: Guardian is running without the `evm` feature or the stale server process is still bound to port `3000`.
- `chain_id ... is not allowed`: set `GUARDIAN_EVM_ALLOWED_CHAIN_IDS=31337` or include the target chain ID.
- `Failed to connect to http://localhost:57291`: the server was started with `GUARDIAN_NETWORK_TYPE=MidenLocal` without a local Miden node. Use `MidenTestnet` for the EVM smoke unless the local node is intentionally part of the run.
- `SignerNotAuthorized`: compare the signer addresses in Guardian auth, wallet, and the module constructor; normalize all EVM addresses.
- RPC validation failures during configure usually mean the module address is wrong, the ABI does not match the expected reader functions, or `getSigners` is not returning 20-byte EOA signer bytes.
- EIP-712 signature recovery failures usually mean the wallet is on the wrong chain, the account address in the EIP-712 domain differs from the canonical `account_address`, or the proposal payload changed between create and sign.
- Duplicate submit failures are expected if the same execution calldata is reused after a successful on-chain submit. Generate fresh calldata for another pass.

## Report

Report:

- commands run and whether each passed
- Guardian URL, RPC URL, chain ID, account address, module address
- signer addresses and module threshold
- proposal commitment/proposal ID
- number of signatures collected and the signer IDs
- submit transaction hash and on-chain `submitted`/signature-count values
- browser and wallet used when running the Vite app
- every setup or smoke error observed, including recovered errors
- checks skipped with reason
