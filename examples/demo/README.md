# PSM Demo (Terminal UI)

Interactive CLI that exercises the `miden-multisig-client` SDK end-to-end: generate keys, create/register multisig accounts, list notes, coordinate proposals, export/import offline files, and execute transactions.

## Requirements

- Private State Manager server (default `http://localhost:50051`)
- Miden node (default `http://localhost:57291`)

## Run

```bash
cargo run -p psm-demo
```

At startup you can override the Miden/PSM endpoints if needed.

## Typical Flow

1. Generate Falcon keypair (shows your signer commitment).
2. Create multisig account (choose threshold and enter cosigner commitments).
3. Register the account on PSM (makes it visible to other cosigners).
4. Pull/register the account from another terminal and sign proposals.
5. Create proposals (transfer, consume notes, switch PSM) and gather signatures.
6. Execute once the threshold is satisfied, or export/import proposals for offline signing.

All of these steps are surfaced via the interactive menu—run it in multiple terminals to simulate different cosigners.

## Tips

- Copy the full commitment hex shown when generating keys; you’ll need it for account creation.
- Ensure the PSM server and Miden node are running before launching the demo.
- Each run stores its miden-client database under `~/.psm-demo` (configurable via the prompts).

## File Layout

- `state.rs` – session state (connections, accounts, keys)
- `menu.rs` – interactive menu + input handling
- `actions/` – individual action handlers (create, sign, export, etc.)
- `display.rs` – UI helpers for printing sections, tables, etc.
- `main.rs` – entry point (`cargo run -p psm-demo`)
