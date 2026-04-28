# Guardian EVM Smoke Web

Private browser smoke app for feature-gated EVM proposal support.

Use this app with an injected EVM wallet connected to a local Anvil chain. The
Guardian server must be running with the Rust `evm` feature enabled and an
allowlist that includes the smoke chain ID.

## Start Guardian

```bash
GUARDIAN_NETWORK_TYPE=MidenTestnet \
GUARDIAN_EVM_ALLOWED_CHAIN_IDS=31337 \
cargo run -p guardian-server --features evm --bin server
```

`GUARDIAN_NETWORK_TYPE=MidenTestnet` avoids requiring a local Miden node while
the smoke targets Anvil through the EVM RPC URL.

## Start Anvil And Deploy The Mock Module

```bash
anvil --host 127.0.0.1 --port 8545
```

From the repository root:

```bash
EVM_RPC_URL=http://127.0.0.1:8545 \
EVM_ACCOUNT_ADDRESS=0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC \
EVM_SIGNER_ONE=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266 \
EVM_SIGNER_TWO=0x70997970C51812dc3A010C7d01b50e0d17dc79C8 \
bash .agents/skills/smoke-test-evm-proposal-support/scripts/deploy-smoke-module.sh
```

Record the printed `EVM_MODULE_ADDRESS`.

## Run The App

```bash
npm install
npm run dev
```

Open the Vite URL in a browser with the wallet connected to Anvil. Use:

- Guardian URL: `http://127.0.0.1:3000`
- RPC URL: `http://127.0.0.1:8545`
- Chain ID: `31337`
- Account address: `0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC`
- Module address: the recorded `EVM_MODULE_ADDRESS`

For the full agent-driven checklist, use
`.agents/skills/smoke-test-evm-proposal-support/SKILL.md`.
