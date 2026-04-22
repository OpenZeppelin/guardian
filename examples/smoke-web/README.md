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
- Para and Miden Wallet parity is preserved through React providers, but the primary interface is still `window.smoke`.

## Setup

```bash
cd /Users/marcos/repos/guardian/examples/smoke-web
npm install
npm run typecheck
npm run dev
```

Optional env vars:

```bash
VITE_PARA_API_KEY=...
VITE_PARA_ENVIRONMENT=development
```

The page follows the `examples/web` lifecycle:
- it clears the Miden IndexedDB state and boots once on page load
- `window.smoke.status()` exposes `bootStatus` and `bootError`
- use `initSession(...)` to reinitialize in place with a different config
- use a full page reload for the closest equivalent to the `examples/web` reset path

## Console API

The app exposes `window.smoke` with JSON-safe methods:

- `initSession({ guardianEndpoint, midenRpcEndpoint, signerSource, signatureScheme, browserLabel })`
- `connectPara()`
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
- `signProposal({ proposalId })`
- `executeProposal({ proposalId })`
- `exportProposal({ proposalId })`
- `signProposalOffline({ proposalId, json })`
- `importProposal({ json })`
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

## Verification Targets

Use this harness for manual smoke flows that need:
- local Falcon and ECDSA signers
- Para or Miden Wallet connectivity checks
- create/load/register/sync/state verification
- proposal create/sign/execute loops
- offline export/import/sign flows
- switch-GUARDIAN proposal orchestration

The UI is intentionally plain. Agents should prefer `window.smoke` over DOM clicking.
