# Minimal Miden web client

This example shows how to use `@openzeppelin/miden-multisig-client` from a browser. It wires a `MidenClient`, generates a Falcon signer, talks to a Guardian, and drives multisig proposals end to end.

## Setup

Install the shared TypeScript workspace dependencies once from the repository
root. The example's `dev` and `build` commands rebuild the local Guardian and
Miden multisig packages automatically.

```bash
cd packages
npm ci

cd ../examples/web
npm ci
npm run dev
```

## How this demo works

1) **Initialize**: create a `MidenClient` pointed at Miden devnet with `useWorker: false`, sync state, and generate a Falcon signer stored in the web keystore. The app can also generate an ECDSA signer; pick ECDSA when the Guardian you register with restricts new accounts to it (`GUARDIAN_ALLOWED_ACCOUNT_SCHEMES=ecdsa`, which the production templates set), otherwise registration fails with `signature_scheme_not_allowed`.
2) **Connect to GUARDIAN**: fetch the GUARDIAN pubkey from the configured endpoint, keep it for multisig config.
3) **Create or load multisig**:
   - Create: build a config with your signer + other commitments, use `MultisigClient.create`, then register on GUARDIAN.
   - Load: fetch state from GUARDIAN and wrap it with `MultisigClient.load`.
4) **Work with proposals**:
   - Create proposals (add/remove signer, change threshold, switch GUARDIAN, consume notes, P2ID).
   - Sync proposals from GUARDIAN, sign them, and execute when ready.
5) **Inspect account**: read state/proposals, and list consumable notes.

The client runs with `useWorker: false`, so its WASM work, local proving included, runs on the page's main thread and the page stops responding while it runs. A worker-mode client breaks the multisig accounts it loads or syncs; see [`MIDEN_COMPATIBILITY.md`](../../docs/MIDEN_COMPATIBILITY.md#open-upstream-items).
