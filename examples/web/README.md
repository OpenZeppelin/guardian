# Minimal Miden web client

This example shows how to use `@openzeppelin/miden-multisig-client` from a browser. It wires a Miden `WebClient`, generates a Falcon signer, talks to a Guardian, and drives multisig proposals end to end.

## How this demo works

1) **Initialize**: create a `WebClient` (points at the Miden RPC), sync state, and generate a Falcon signer stored in the web keystore.
2) **Connect to GUARDIAN**: fetch the GUARDIAN pubkey from the configured endpoint, keep it for multisig config.
3) **Create or load multisig**:
   - Create: build a config with your signer + other commitments, use `MultisigClient.create`, then register on GUARDIAN.
   - Load: fetch state from GUARDIAN and wrap it with `MultisigClient.load`.
4) **Work with proposals**:
   - Create proposals (add/remove signer, change threshold, switch GUARDIAN, consume notes, P2ID).
   - Sync proposals from GUARDIAN, sign them, and execute when ready.
5) **Inspect account**: read state/proposals, and list consumable notes.
