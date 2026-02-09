# PSM Demo (Terminal UI)

Interactive CLI that exercises the `miden-multisig-client` SDK end-to-end: generate keys, create/register multisig accounts, list notes, coordinate proposals, export/import offline files, and execute transactions.

The demo supports both PSM signature schemes:
- Falcon
- ECDSA (secp256k1 / keccak)

## Requirements

- Private State Manager server (default `http://localhost:50051`)
- Miden node (default `https://rpc.testnet.miden.io:443` for testnet)
- Workspace dependencies aligned to Miden `0.13.x`

## Run

```bash
cargo run -p psm-demo
```

At startup you can override the Miden/PSM endpoints and choose your signature scheme (Falcon or ECDSA).

## Typical Flow

1. Choose signature scheme and generate a keypair (shows your signer commitment).
2. Create multisig account (choose threshold and enter cosigner commitments).
3. Register the account on PSM (makes it visible to other cosigners).
4. Pull/register the account from another terminal and sign proposals.
5. Create proposals (transfer, consume notes, switch PSM) and gather signatures.
6. Execute once the threshold is satisfied, or export/import proposals for offline signing.

All of these steps are surfaced via the interactive menu—run it in multiple terminals to simulate different cosigners.

## Tips

- Copy the full commitment hex shown when generating keys; you’ll need it for account creation.
- All cosigners in a multisig must use commitments produced for the same signature scheme.
- Ensure the PSM server and Miden node are running before launching the demo.
- Each run stores its miden-client database under `~/.psm-demo` (configurable via the prompts).

## File Layout

- `state.rs` – session state (connections, accounts, keys)
- `menu.rs` – interactive menu + input handling
- `actions/` – individual action handlers (create, sign, export, etc.)
- `display.rs` – UI helpers for printing sections, tables, etc.
- `main.rs` – entry point (`cargo run -p psm-demo`)
