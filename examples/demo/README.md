# GUARDIAN Demo (Terminal UI)

Interactive CLI that exercises the `miden-multisig-client` SDK end-to-end: generate keys, create/register multisig accounts, list notes, coordinate proposals, export/import offline files, and execute transactions.

On a fee-charging chain (`verification_base_fee` non-zero) fund the demo account with the
native fee asset before the first execute: the guarded auth procedure pays the fee before
the transaction summary exists, so an unfunded vault aborts there rather than reaching
signing.

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

1. Generate a keypair (shows your signer commitment). The prompt defaults to Falcon; pick ECDSA when the target Guardian restricts new accounts with `GUARDIAN_ALLOWED_ACCOUNT_SCHEMES=ecdsa` (the production templates do), otherwise registration fails with `signature_scheme_not_allowed`.
2. Create multisig account (choose threshold and enter cosigner commitments).
3. Register the account on GUARDIAN (makes it visible to other cosigners).
4. Pull/register the account from another terminal and sign proposals.
5. Create proposals (transfer, consume notes, switch GUARDIAN) and gather signatures.
6. Execute once the threshold is satisfied, or export/import proposals for offline signing.
7. After recovering an account on a fresh device (`r` then a sync/pull), run `n` — "Recover notes" — to restore pending notes via the transport drain, proposal import, and public backfill in one flow.

All of these steps are surfaced via the interactive menu—run it in multiple terminals to simulate different cosigners.

## GUARDIAN Executes

At startup the demo asks who executes the proposals the session creates. Pick **[2] GUARDIAN proves and submits them** to test server-side execution:

1. Run the GUARDIAN server with `GUARDIAN_TX_PROVER_URL` set (for devnet, `https://tx-prover.devnet.miden.io`) and check that `GET /status` reports `"execution":{"enabled":true}`.
2. Fund the multisig with the network's fee asset: GUARDIAN executes with the account paying its own fee, and an unfunded account fails with `GUARDIAN_EXECUTION_INSUFFICIENT_FEE`.
3. Create the proposal from the session in GUARDIAN-executes mode, so it carries the transaction request GUARDIAN reproduces. Cosigners can sign it from a session in either mode.
4. **Execute a proposal** (`4` in Proposal Management) then asks GUARDIAN to execute it and waits until it is `committed` or `failed`. A failure says whether the proposal is kept (execute again once the cause is fixed) or removed (create a new one).
5. **Show GUARDIAN execution status** (`9`) shows the execution in flight on the account and, for a proposal ID you enter, its latest execution.

A proposal created by a session that executes itself carries no request, and GUARDIAN refuses it with `GUARDIAN_PROPOSAL_MISSING_TRANSACTION_REQUEST`.

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
