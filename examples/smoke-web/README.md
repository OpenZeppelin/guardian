# `examples/smoke-web`

Minimal browser smoke harness for `@openzeppelin/miden-multisig-client`.

This app is the browser analogue of the Rust CLI smoke surface:
- minimal UI
- structured event log
- stable `window.smoke` command API
- no dependence on the product-style `examples/web` UI flow

## Constraints

- Use one browser or browser profile per cosigner session.
- Same-browser concurrent tabs are out of scope because the current browser client path does not expose safe per-session IndexedDB isolation.
- The Miden client runs with `useWorker: false`, so its WASM work, local proving included, runs on the page's main thread and the page stops responding while it runs; a worker-mode client breaks the multisig accounts it loads or syncs (see [`MIDEN_COMPATIBILITY.md`](../../docs/MIDEN_COMPATIBILITY.md#open-upstream-items)).
- Miden Wallet parity is reached through `window.smoke.connectMidenWallet()`; no wallet provider wraps the app, and `window.smoke` stays the primary interface.

## Setup

Install the shared TypeScript workspace dependencies once from the repository
root. The example's `dev`, `build`, and `typecheck` commands rebuild their
Guardian SDK dependencies automatically.

```bash
cd packages
npm ci

cd ../examples/_shared/multisig-browser
npm ci

cd ../../smoke-web
npm ci
npm run typecheck
npm run dev
```

Optional env vars:

```bash
VITE_PROVER_URL=...
VITE_PROVER_MAX_ATTEMPTS=2
VITE_RPC_MAX_ATTEMPTS=2
```

The page follows the `examples/web` lifecycle:
- it clears the Miden IndexedDB state and boots once on page load
- `window.smoke.status()` exposes `bootStatus` and `bootError`
- use `initSession(...)` to reinitialize in place with a different config
- use a full page reload for the closest equivalent to the `examples/web` reset path

## Console API

The app exposes `window.smoke` with JSON-safe methods:

- `initSession({ guardianEndpoint, midenRpcEndpoint, signerSource, signatureScheme, executionMode, browserLabel })`
- `connectMidenWallet()`
- `status()`
- `createAccount({ threshold, otherCommitments, guardianCommitment, procedureThresholds })`
- `loadAccount({ accountId })`
- `registerOnGuardian({ stateDataBase64 })`
- `sync()`
- `fetchState()`
- `verifyStateCommitment()`
- `listConsumableNotes()`
- `listProposals()`
- `createProposal({ type, ...payload })`
- `createCustomProposal({ recipientId, faucetId, amount, label })`
- `executeCustomProposal({ proposalId })` or `executeCustomProposal({ recipe })`
- `signProposal({ proposalId })`
- `executeProposal({ proposalId })`
- `guardianExecutionStatus({ proposalId? })`
- `exportProposal({ proposalId })`
- `signProposalOffline({ proposalId, json })`
- `importProposal({ json })`
- `recoverByKey()`
- `recoverNotes({ transportDrain?, proposalImport?, publicBackfill?, fromBlock?, toBlock?, syncAfter? })`
- `clearLocalState()`
- `events()`

Example:

```js
await window.smoke.status();

await window.smoke.initSession({
  guardianEndpoint: 'http://localhost:3000',
  midenRpcEndpoint: 'https://rpc.devnet.miden.io',
  signerSource: 'local',
  signatureScheme: 'falcon',
  browserLabel: 'chrome-a',
});

await window.smoke.createAccount({
  threshold: 2,
  otherCommitments: ['0x...'],
});

await window.smoke.createProposal({
  type: 'add_signer',
  commitment: '0x...',
  increaseThreshold: false,
});
```

### GUARDIAN execution

`executionMode` chooses who proves and submits the proposals the session creates: `'self_executed'` (default, the browser) or `'guardian_executable'` (GUARDIAN). The UI offers the same choice as **Proposal execution** in the session form.

In `guardian_executable` mode, proposals carry the transaction request GUARDIAN reproduces, and `executeProposal({ proposalId })` asks GUARDIAN to execute and waits until the execution is `committed` or `failed`. A failure throws with the error code and says whether the proposal is kept (execute again once the cause is fixed) or removed (create a new one). `guardianExecutionStatus({ proposalId })` returns the account's in-flight execution (`current`) and, when a proposal ID is given, that proposal's latest execution (`proposal`).

To test it:

1. Run the GUARDIAN server with `GUARDIAN_TX_PROVER_URL` set and check that `GET /status` reports `"execution":{"enabled":true}`.
2. Initialize the session with `executionMode: 'guardian_executable'`, create the multisig, and fund it with the network's fee asset: GUARDIAN executes with the account paying its own fee, and an unfunded account fails with `GUARDIAN_EXECUTION_INSUFFICIENT_FEE`.
3. Create the proposal from that session; cosigners can sign it from a session in either mode.
4. Call `executeProposal({ proposalId })` (or **Execute via GUARDIAN** in the UI).

A proposal created by a session that executes itself carries no request, and GUARDIAN refuses it with `GUARDIAN_PROPOSAL_MISSING_TRANSACTION_REQUEST`.

```js
await window.smoke.initSession({
  guardianEndpoint: 'http://localhost:3000',
  midenRpcEndpoint: 'https://rpc.devnet.miden.io',
  signerSource: 'local',
  signatureScheme: 'ecdsa',
  executionMode: 'guardian_executable',
  browserLabel: 'chrome-a',
});

await window.smoke.executeProposal({ proposalId: '0x...' });
await window.smoke.guardianExecutionStatus({ proposalId: '0x...' });
```

### Custom proposal producer API

A custom (`'custom'`-bucket) proposal is created with a free-form label and
executed by the integration, not the SDK. The producer builds a serialized transaction
request, proposes it via `createCustomProposal`, and after threshold calls
`prepareCustomExecution` to get the validated advice, which the harness injects
into a rebuilt request before submitting on-chain. The `recipe` returned by
`createCustomProposal` is what the producer keeps to reproduce the exact
transaction at execute time (request inputs, the original salt, and the anchor
block). Both builds pass that salt and block into
`feeAwareTransactionRequestBuilder`; the Miden client derives the native fee
conversion info and leaves the three-word multisig auth args in place.

```js
// Producer tab: create
const { recipe } = await window.smoke.createCustomProposal({
  recipientId: '0x...',
  faucetId: '0x...',
  amount: '100',
  label: 'b2agg',
});

// Cosigner tabs: sign the proposal like any other
await window.smoke.signProposal({ proposalId: recipe.proposalId });

// Producer tab: prepare advice + inject + submit
await window.smoke.executeCustomProposal({ recipe });
```

The producer tab can also execute by id alone (`executeCustomProposal({
proposalId })`) when the recipe is still cached in that session; pass `recipe`
explicitly to drive execution from a fresh session.

## Verification Targets

Use this harness for manual smoke flows that need:
- local Falcon and ECDSA signers
- Miden Wallet connectivity checks
- create/load/register/sync/state verification
- proposal create/sign/execute loops, self-executed or executed by GUARDIAN
- custom (producer-API) propose/sign/prepare/submit loops
- offline export/import/sign flows
- switch-GUARDIAN proposal orchestration
- key-based account recovery plus the note-recovery flow (`recoverNotes`)

The UI is intentionally plain. Agents should prefer `window.smoke` over DOM clicking.
