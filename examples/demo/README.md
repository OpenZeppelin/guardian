# GUARDIAN Demo (Terminal UI)

Interactive CLI that exercises the `miden-multisig-client` SDK end-to-end: generate keys, create/register multisig accounts, list notes, coordinate proposals, export/import offline files, and execute transactions.

## Requirements

- Guardian server (default `http://localhost:50051`)
- Miden node (default public endpoint `https://rpc.devnet.miden.io`)

## Run

```bash
cargo run -p guardian-demo
```

At startup you can override the Miden/GUARDIAN endpoints, select an optional
custom remote prover, and set the total proof-attempt budget. Leaving the prover
selection and attempt prompt at their defaults preserves the network's prover
selection and uses two total remote proof attempts. Local proving always runs
once.

## Typical Flow

1. Generate Falcon keypair (shows your signer commitment).
2. Create multisig account (choose threshold and enter cosigner commitments).
3. Register the account on GUARDIAN (makes it visible to other cosigners).
4. Pull/register the account from another terminal and sign proposals.
5. Create proposals (transfer, consume notes, switch GUARDIAN) and gather signatures.
6. Execute once the threshold is satisfied, or export/import proposals for offline signing.
7. After recovering an account on a fresh device (`r` then a sync/pull), run `n` — "Recover notes" — to restore pending notes via the transport drain, proposal import, and public backfill in one flow.

All of these steps are surfaced via the interactive menu—run it in multiple terminals to simulate different cosigners.

## Tips

- Copy the full commitment hex shown when generating keys; you’ll need it for account creation.
- Ensure the GUARDIAN server and Miden node are running before launching the demo.
- Each run stores its miden-client database under `~/.guardian-demo` (configurable via the prompts).

## File Layout

- `state.rs` – session state (connections, accounts, keys)
- `menu.rs` – interactive menu + input handling
- `actions/` – individual action handlers (create, sign, export, etc.)
- `display.rs` – UI helpers for printing sections, tables, etc.
- `main.rs` – entry point (`cargo run -p guardian-demo`)
