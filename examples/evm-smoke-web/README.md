# Guardian EVM Smoke Web

Private browser smoke app for Guardian's domain-separated EVM proposal flow.

Use this app with an injected EVM wallet connected to Anvil. The Guardian
server must be running with the Rust `evm` feature enabled and server-owned RPC
and EntryPoint mappings for the smoke chain.

## Start Guardian

After deploying the smoke contracts, start Guardian with the printed addresses:

```bash
GUARDIAN_NETWORK_TYPE=MidenTestnet \
GUARDIAN_EVM_RPC_URLS=31337=http://127.0.0.1:8545 \
GUARDIAN_EVM_ENTRYPOINTS=31337=$EVM_ENTRYPOINT_ADDRESS \
cargo run -p guardian-server --features evm --bin server
```

`GUARDIAN_NETWORK_TYPE=MidenTestnet` avoids requiring a local Miden node while
the smoke targets Anvil through the EVM RPC URL.

## Start Anvil And Deploy Smoke Contracts

```bash
anvil --host 127.0.0.1 --port 8545
```

From the repository root:

```bash
EVM_RPC_URL=http://127.0.0.1:8545 \
EVM_SIGNER_ONE=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266 \
EVM_SIGNER_TWO=0x70997970C51812dc3A010C7d01b50e0d17dc79C8 \
bash .agents/skills/smoke-test-evm-proposal-support/scripts/deploy-smoke-module.sh
```

Record the printed `EVM_ACCOUNT_ADDRESS`, `EVM_VALIDATOR_ADDRESS`, and
`EVM_ENTRYPOINT_ADDRESS`.

## Run The App

```bash
npm install
npm run dev
```

Open the Vite URL in a browser with the wallet connected to Anvil. The default
blank Guardian URL uses the Vite `/evm` proxy to `http://127.0.0.1:3000`, which
keeps the cookie session same-origin for local smoke testing.

Use:

- Chain ID: `31337`
- Smart account: the printed `EVM_ACCOUNT_ADDRESS`
- Multisig validator: the printed `EVM_VALIDATOR_ADDRESS`
- UserOp hash: any 32-byte hash for local smoke
- Payload: any opaque JSON string

For the full agent-driven checklist, use
`.agents/skills/smoke-test-evm-proposal-support/SKILL.md`.
