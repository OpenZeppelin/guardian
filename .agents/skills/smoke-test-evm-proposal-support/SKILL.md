---
name: smoke-test-evm-proposal-support
description: Run or guide smoke testing of Guardian feature-gated EVM proposal support with Anvil, an EVM-enabled guardian-server, `@openzeppelin/guardian-evm-client`, and `examples/evm-smoke-web`. Use when Codex needs to verify EVM session auth, proposal create/list/get/approve/executable, collecting multiple signatures, or lazy expiry/finality behavior.
---

# Smoke Test EVM Proposal Support

Use this skill for local EVM proposal smoke tests. Keep EVM client checks isolated to `packages/guardian-evm-client`; do not modify or validate through the base Rust `guardian-client` crate or the base TypeScript `packages/guardian-client` package unless the user explicitly asks.

## Preflight

Read current source before assuming labels, endpoints, or response shapes:

- `packages/guardian-evm-client/src/`
- `examples/evm-smoke-web/src/App.tsx`
- `crates/server/src/api/evm.rs`
- `crates/server/src/evm/`

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
GUARDIAN_EVM_RPC_URLS=31337=http://127.0.0.1:8545 \
GUARDIAN_EVM_ENTRYPOINTS=31337=<entrypoint-address> \
RUST_LOG=info \
cargo run -p guardian-server --features evm --bin server
```

If port `3000` or `50051` is occupied, inspect the owner first:

```bash
lsof -nP -iTCP:3000 -iTCP:50051 -sTCP:LISTEN
```

Reuse the process only if it was started from the current EVM-enabled build with the intended storage, RPC map, and EntryPoint map. Otherwise stop it or use a clean process.

## Smoke Contracts

The smoke needs:

- a smart account exposing `isModuleInstalled(uint256,address,bytes)`
- an ERC-7579-style validator exposing:
- `getSigners(address,uint256,uint256) returns (bytes[])`
- `getSignerCount(address) returns (uint256)`
- `threshold(address) returns (uint64)`
- an EntryPoint exposing `getNonce(address,uint192) returns (uint256)`

Deploy the bundled local mock to Anvil:

```bash
EVM_RPC_URL=http://127.0.0.1:8545 \
EVM_SIGNER_ONE=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266 \
EVM_SIGNER_TWO=0x70997970C51812dc3A010C7d01b50e0d17dc79C8 \
bash .agents/skills/smoke-test-evm-proposal-support/scripts/deploy-smoke-module.sh
```

Record the printed `EVM_ACCOUNT_ADDRESS`, `EVM_VALIDATOR_ADDRESS`, and `EVM_ENTRYPOINT_ADDRESS`. The default signer keys are Anvil's standard first two accounts.

## Sepolia Deployed Smoke

For a deployed Guardian target, use Sepolia instead of local Anvil when you need a public-chain canary. Generate a fresh deployer key, fund only that address with Sepolia ETH, and keep the private key in an ignored local file or shell session. Do not fund Anvil's well-known default keys on public testnets.

The deploy script supports either Anvil unlocked accounts or a funded private key. For Sepolia, set `EVM_PRIVATE_KEY` and use signer one as the deployer address; signer two can be a fresh unfunded key because it only signs proposals off-chain:

```bash
source /tmp/guardian-evm-sepolia-deployer.env

EVM_RPC_URL=https://ethereum-sepolia-rpc.publicnode.com \
EVM_PRIVATE_KEY="$EVM_SEPOLIA_DEPLOYER_PRIVATE_KEY" \
EVM_DEPLOYER_ADDRESS="$EVM_SEPOLIA_DEPLOYER_ADDRESS" \
EVM_SIGNER_ONE="$EVM_SEPOLIA_DEPLOYER_ADDRESS" \
EVM_SIGNER_TWO="$EVM_SEPOLIA_SIGNER_TWO_ADDRESS" \
EVM_THRESHOLD=2 \
bash .agents/skills/smoke-test-evm-proposal-support/scripts/deploy-smoke-module.sh
```

Then run the client smoke against the deployed Guardian endpoint and Sepolia:

```bash
source /tmp/guardian-evm-sepolia-deployer.env

GUARDIAN_URL=https://guardian-evm.openzeppelin.com \
EVM_RPC_URL=https://ethereum-sepolia-rpc.publicnode.com \
EVM_CHAIN_ID=11155111 \
EVM_ACCOUNT_ADDRESS=<printed-smoke-account> \
EVM_VALIDATOR_ADDRESS=<printed-smoke-validator> \
EVM_ENTRYPOINT_ADDRESS=0x0000000071727De22E5E9d8BAf0edAc6f37da032 \
EVM_SIGNER_ONE_PRIVATE_KEY="$EVM_SEPOLIA_DEPLOYER_PRIVATE_KEY" \
EVM_SIGNER_TWO_PRIVATE_KEY="$EVM_SEPOLIA_SIGNER_TWO_PRIVATE_KEY" \
node .agents/skills/smoke-test-evm-proposal-support/scripts/run-evm-client-smoke.mjs
```

If Guardian is still using in-memory EVM sessions, keep the deployed ECS service at one running task or enable sticky/shared sessions before running the smoke. With two or more tasks, challenge verification can fail because the challenge and verify requests land on different tasks.

## Client Smoke

After Guardian, Anvil, and a module are live, run the bundled client smoke from the repo root:

```bash
GUARDIAN_URL=http://127.0.0.1:3000 \
EVM_RPC_URL=http://127.0.0.1:8545 \
EVM_CHAIN_ID=31337 \
EVM_ACCOUNT_ADDRESS=<smart-account-address> \
EVM_VALIDATOR_ADDRESS=<validator-address> \
EVM_ENTRYPOINT_ADDRESS=<entrypoint-address> \
node .agents/skills/smoke-test-evm-proposal-support/scripts/run-evm-client-smoke.mjs
```

The script uses the workspace build output at `packages/guardian-evm-client/dist/index.js`, so run `npm run build` in that package first after TypeScript edits.

## Browser Smoke

Use the Vite app when the user asks for a browser or injected-wallet pass:

```bash
cd examples/evm-smoke-web
npm run dev -- --host 127.0.0.1 --port 5173
```

Open `http://127.0.0.1:5173/` in a browser with an injected EVM wallet connected to Anvil. Leave Guardian URL blank to use the Vite proxy for `/evm`, then fill in chain ID, smart account, validator, UserOp hash, nonce, TTL, and opaque payload. Use different signer accounts to approve the same proposal, then fetch the executable payload/signature bundle.

For browser wallet prompts, always let the human complete passwords or confirmations when needed. After the user says a prompt is confirmed, continue with the next action and confirm future wallet prompts without asking only when the user explicitly told Codex to do so for that run.

## Assertions

Mark the smoke passed only when all relevant assertions hold:

- Guardian was started with `--features evm`.
- The default non-EVM server does not register the `/evm/*` routes when that negative gate is part of the task.
- EVM login creates a cookie-backed session derived from an EIP-712 wallet signature.
- EVM account registration stores account metadata through `/evm/accounts`.
- The validator is installed on the smoke smart account.
- The validator signer set and threshold match the intended signers.
- Proposal creation stores the opaque payload and initial signature.
- `listProposals` or `getProposal` sees the created proposal.
- Two distinct signer addresses approve the same proposal.
- The fetched proposal contains two stored ECDSA signatures.
- `getExecutableProposal` returns the original hash, payload, signatures, and signers once threshold is met.

## Failure Triage

- `404` on `/evm/*`: Guardian is running without the `evm` feature or the stale server process is still bound to port `3000`.
- `unsupported_evm_chain`: set both `GUARDIAN_EVM_RPC_URLS` and `GUARDIAN_EVM_ENTRYPOINTS` for the target chain ID.
- `Failed to connect to http://localhost:57291`: the server was started with `GUARDIAN_NETWORK_TYPE=MidenLocal` without a local Miden node. Use `MidenTestnet` for the EVM smoke unless the local node is intentionally part of the run.
- `SignerNotAuthorized`: compare the session wallet, proposal signer, and validator signer snapshot; normalize all EVM addresses.
- RPC validation failures during create usually mean the smart account, validator, or EntryPoint address is wrong, the validator is not installed, the ABI does not match, or `getSigners` is not returning 20-byte EOA signer bytes.
- EIP-712 session recovery failures usually mean the wallet rejected or altered the challenge typed data.
- Proposal signature recovery failures usually mean the app signed a different hash from the one sent to Guardian.

## Report

Report:

- commands run and whether each passed
- Guardian URL, RPC URL, chain ID, smart account, validator, EntryPoint
- signer addresses and validator threshold
- proposal ID
- number of signatures collected and the signer IDs
- executable payload/signature result
- browser and wallet used when running the Vite app
- every setup or smoke error observed, including recovered errors
- checks skipped with reason
