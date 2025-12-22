# Minimal Miden web client

This example shows how to use `@openzeppelin/miden-multisig-client` from a browser. It wires a Miden `WebClient`, generates a Falcon signer, talks to a Private State Manager (PSM), and drives multisig proposals end to end.

## How this demo works

1) **Initialize**: create a `WebClient` (points at the Miden RPC), sync state, and generate a Falcon signer stored in the web keystore.
2) **Connect to PSM**: fetch the PSM pubkey from the configured endpoint, keep it for multisig config.
3) **Create or load multisig**:
   - Create: build a config with your signer + other commitments, use `MultisigClient.create`, then register on PSM.
   - Load: fetch state from PSM and wrap it with `MultisigClient.load`.
4) **Work with proposals**:
   - Create proposals (add/remove signer, change threshold, switch PSM, consume notes, P2ID).
   - Sync proposals from PSM, sign them, and execute when ready.
5) **Inspect account**: read state/proposals, and list consumable notes.
