# Workflow Matrix

## Use This File

Map the changed area to the smallest browser smoke workflow that still proves the behavior. Combine workflows when a shared-layer or SDK change crosses multiple categories.

## Environment

Run validation first:

```bash
cd packages/miden-multisig-client && npm test
cd examples/smoke-web && npm run typecheck && npm run build
cd examples/web && npm run build
```

Primary smoke server commands:

```bash
cargo run -p guardian-server --bin server
cd examples/smoke-web && npm run dev
```

Default startup choices:

- one GUARDIAN at the chosen Deployment Target (see the table in `SKILL.md`):
  - Local dev: `http://localhost:3000`
  - Staging (devnet): `https://guardian-stg.openzeppelin.com`
  - Production (testnet): `https://guardian.openzeppelin.com`
- one `examples/smoke-web` dev server at `http://localhost:3002` (or the scratch deployed-SDK project when smoking the published npm package)
- one real browser or fully isolated browser profile per cosigner (Chrome MCP's `tabs_create_mcp` tabs count as one profile, not three — see Browser Automation in `SKILL.md`)
- Miden RPC matching the chosen target:
  - Local dev or Staging: `https://rpc.devnet.miden.io`
  - Production: `https://rpc.testnet.miden.io`
- signer source: `local`
- signature scheme: Falcon unless the task specifically targets ECDSA, Para, or Miden Wallet

If `3002` is occupied by another local app, start `examples/smoke-web` on a free port and use that exact URL consistently in every browser and automation command.

Default session bootstrap in each browser console when page-load bootstrap did not already succeed, or when you need to override the defaults:

```js
await window.smoke.initSession({
  guardianEndpoint: 'http://localhost:3000',
  midenRpcEndpoint: 'https://rpc.devnet.miden.io',
  signerSource: 'local',
  signatureScheme: 'falcon',
  browserLabel: 'A',
});
```

Record commitments from `await window.smoke.status()`. For local signers, use `status.localSigners.falconCommitment` or `status.localSigners.ecdsaCommitment`. For Para or Miden Wallet, use `status.para.commitment` or `status.midenWallet.commitment`.

Use `await window.smoke.events()` as the primary timing source for command durations. Record extra manual timings for wallet modal latency, faucet confirmation, and canonicalization lag.

If a `window.smoke.*` call throws an opaque value such as `[object Object]`, read the newest matching entry from `await window.smoke.events()` and classify the error from that event instead of from the thrown value.

If page-load bootstrap fails with `ConstraintError: Key already exists in the object store`, record that failure, then retry with an explicit `await window.smoke.initSession(...)` before abandoning the workflow. If page-load bootstrap already reached `ready`, avoid an immediate second `initSession()` on that same profile.

## `browser-baseline`

Use when:

- the prompt asks for a general smoke test
- the change spans multiple TS/browser multisig flows
- the prompt does not narrow the target behavior yet

Steps:

1. Start GUARDIAN and `examples/smoke-web`.
2. Open browser/profile A, B, and C at the active smoke harness URL.
3. Run `initSession` in each browser with unique `browserLabel`.
4. Capture each browser's commitment from `status()`.
5. In browser A, create the multisig using B and C commitments:
   ```js
   await window.smoke.createAccount({
     threshold: 2,
     otherCommitments: ['0x...', '0x...'],
   });
   ```
6. Capture the `accountId` from `status().multisig.accountId` in browser A.
7. In browsers B and C, load and sync the account:
   ```js
   await window.smoke.loadAccount({ accountId: '0x...' });
   await window.smoke.sync();
   ```

Expect:

- all browsers point to the same GUARDIAN server and testnet
- account creation succeeds in browser A
- the other browsers can load and sync the same account
- all browsers now represent cosigners of the same multisig

Canary checks:

- report any bootstrap failure before a recovered `initSession()` as a transient but real canary failure
- prefer Chrome + Brave or Chrome + Firefox over multiple tabs in one browser

## `online-proposal-canary`

Use when:

- the prompt asks for a default create/sign/execute canary
- proposal creation, signing, execution, or post-execute sync changed
- you need a smoke test that does not depend on notes or external faucet state

Setup:

1. Start from the baseline harness with browsers A, B, and C.
2. Create the initial multisig with A and B only.
3. Keep C unbound to the account so its commitment can be added later.

Steps:

1. In A, create the add-signer proposal:
   ```js
   const created = await window.smoke.createProposal({
     type: 'add_signer',
     commitment: '0xC_COMMITMENT',
     increaseThreshold: false,
   });
   ```
2. In B, sync if needed, then sign:
   ```js
   await window.smoke.sync();
   await window.smoke.signProposal({ proposalId: created.proposal.id });
   ```
3. In A, sign too. In the default 2-of-2 initial account, both existing cosigners must sign before the add-signer proposal becomes executable:
   ```js
   await window.smoke.signProposal({ proposalId: created.proposal.id });
   ```
4. In A or B, execute:
   ```js
   await window.smoke.executeProposal({ proposalId: created.proposal.id });
   ```
5. If execute returns `Refusing to overwrite local state: incoming nonce ... is not greater than local nonce ...`, record the error and keep syncing A and B until the canonicalized 2-of-3 state appears. Treat the first post-execute `sync()` nonce-overwrite the same way.
6. In C, load the updated account using the shared `accountId`.

Expect:

- proposal creation succeeds in A
- B can sync, see the proposal, and sign it
- A also signs before execute in the default 2-of-2 setup; B signing alone is not enough to move the proposal to `ready`
- execution may return a reportable nonce-overwrite error before server canonicalization finishes; the pass condition is eventual convergence to the updated signer set
- existing cosigners may see temporary nonce-overwrite sync failures before canonicalization finishes, then resync successfully
- C may initially fail `loadAccount()` with unauthorized auth before canonicalization finishes, then load the updated account successfully

Canary checks:

- report canonicalization lag separately from execute time
- if execute first fails with the nonce-overwrite error but later sync converges to the expected state, report it as a recovered failure rather than a terminal canary failure
- if post-execute `sync()` or `loadAccount()` first fails with nonce-overwrite or unauthorized auth but later converges, report the first failure and the later recovery rather than treating it as a terminal canary failure
- if C initially cannot load the account, report the first failure and the eventual recovery
- if A or B executes successfully but C never becomes able to load, mark the canary failed

## `payment-roundtrip-canary`

Use when:

- the prompt asks to send a payment
- note ingestion, note consumption, or P2ID transfer behavior changed
- vault updates or post-execute received-note behavior changed

Setup:

1. Start two browsers, A and B.
2. Create a 2-of-2 account and load it in both browsers.
3. Record the `accountId` from `status().multisig.accountId`.

Steps:

1. Open [Miden Faucet](https://faucet.testnet.miden.io/).
2. Paste the `accountId` into `Recipient address` and send a public note.
3. In A and B, poll `sync()` and `listConsumableNotes()` until the note appears.
4. In A, create a consume-notes proposal with the note ID:
   ```js
   const notes = await window.smoke.listConsumableNotes();
   const consume = await window.smoke.createProposal({
     type: 'consume_notes',
     noteIds: [notes[0].id],
   });
   ```
5. In B, sign the proposal. Then execute from A or B.
6. If execute returns `Refusing to overwrite local state: incoming nonce ... is not greater than local nonce ...`, record it and keep syncing until the vault becomes non-empty.
7. Create a self-addressed P2ID proposal using the same `accountId` and a vault asset:
   ```js
   const { detectedConfig, multisig } = await window.smoke.status();
   const asset = detectedConfig.vaultBalances[0];
   const payment = await window.smoke.createProposal({
     type: 'p2id',
     recipientId: multisig.accountId,
     faucetId: asset.faucetId,
     amount: asset.amount,
   });
   ```
8. Sign and execute the P2ID proposal.
9. If execute returns the nonce-overwrite error, record it and keep syncing until a new received note is visible.

Expect:

- faucet mint succeeds before browser-side sync starts
- the new note becomes visible after sync
- consume-notes executes and the vault gains assets
- the self-P2ID executes successfully
- a new received note becomes visible after the final sync

Canary checks:

- report faucet page or confirmation failures explicitly
- if the note never appears after repeated sync, report the attempt count and wait time
- if either execute first returns the nonce-overwrite error but later sync converges, report it as a recovered failure with the canonicalization lag
- if consume executes but the vault remains empty, mark the canary failed
- if self-P2ID executes but no new note appears after final sync, mark the canary failed

## `switch-guardian-offline-canary`

Use when:

- the prompt asks to switch GUARDIAN providers
- `Switch GUARDIAN` transaction behavior changed
- export/import/offline sign behavior changed

Important:

- the browser harness uses GUARDIAN HTTP endpoints, not gRPC endpoints
- keep host literals consistent; prefer all `127.0.0.1` or all `localhost`
- the current smoke harness does not expose the active post-switch GUARDIAN endpoint in `status()`, so verification must be behavior-based

Setup:

1. Start GUARDIAN A on the default ports.
2. Temporarily change `crates/server/src/main.rs` so GUARDIAN B uses alternate HTTP and gRPC ports.
3. Start GUARDIAN B with distinct storage directories.
4. Fetch GUARDIAN B's commitment from its HTTP endpoint. Match the query to the active signer scheme:
   ```bash
   curl http://127.0.0.1:3001/pubkey
   ```
   For ECDSA runs, use:
   ```bash
   curl 'http://127.0.0.1:3001/pubkey?scheme=ecdsa'
   ```
5. Start browsers A and B against GUARDIAN A.
6. Create a 2-of-2 multisig and load it in both browsers.
7. Kill GUARDIAN A.

Steps:

1. In A, create the switch proposal:
   ```js
   const created = await window.smoke.createProposal({
     type: 'switch_guardian',
     newGuardianEndpoint: 'http://127.0.0.1:3001',
     newGuardianPubkey: '0xNEW_GUARDIAN_COMMITMENT',
   });
   ```
2. Export it:
   ```js
   const exported = await window.smoke.exportProposal({ proposalId: created.proposal.id });
   ```
3. In B, import it and offline-sign it:
   ```js
   await window.smoke.importProposal({ json: exported.json });
   const signedByB = await window.smoke.signProposalOffline({ proposalId: created.proposal.id });
   ```
4. In A, import B's signed JSON, offline-sign it too, then import the fully-signed JSON:
   ```js
   await window.smoke.importProposal({ json: signedByB.json });
   const signedByA = await window.smoke.signProposalOffline({ proposalId: created.proposal.id });
   await window.smoke.importProposal({ json: signedByA.json });
   ```
5. Execute the fully-signed proposal from A.
6. With GUARDIAN A still down, attempt a post-execute `sync()` or fresh account load that can only succeed if the account is now using GUARDIAN B.

Expect:

- switch proposal creation/export/import/offline sign succeed
- both required offline signatures are present before execute
- execute succeeds
- follow-up sync or account interaction succeeds while GUARDIAN A remains down and GUARDIAN B remains up

Canary checks:

- if post-switch behavior cannot distinguish A from B, report that as a harness gap
- if import or offline sign fails due stale chain state, report the exact error and whether an immediate pre-import `sync()` recovered it
- if execute is attempted before the proposal is fully signed, report that as test-sequencing failure rather than SDK failure
- if execute succeeds but post-switch sync still depends on the dead GUARDIAN, mark the canary failed

## `para-connectivity`

Use when:

- Para integration changed
- ECDSA external signer resolution changed
- the prompt explicitly asks for Para validation

Steps:

1. Initialize the session in a clean browser.
2. Run:
   ```js
   await window.smoke.connectPara();
   const status = await window.smoke.status();
   ```
3. Verify `status.signerSource === 'para'`, `status.signatureScheme === 'ecdsa'`, and `status.para.connected === true`.
4. If the changed code path affects signing, create or load a small multisig and run at least one sign operation with Para.

Expect:

- connection succeeds through the Para modal flow
- commitment and public key are visible in `status().para`
- any requested signing path succeeds with Para as the active signer source

## `miden-wallet-connectivity`

Use when:

- Miden Wallet integration changed
- wallet extension detection or external signing changed
- the prompt explicitly asks for wallet validation

Steps:

1. Initialize the session in a clean browser with the extension installed.
2. Run:
   ```js
   await window.smoke.connectMidenWallet();
   const status = await window.smoke.status();
   ```
3. Verify `status.signerSource === 'miden-wallet'` and `status.midenWallet.connected === true`.
4. If the changed code path affects signing, create or load a small multisig and run at least one sign operation through the wallet.

Expect:

- wallet connection succeeds
- commitment, public key, and scheme are visible in `status().midenWallet`
- any requested signing path succeeds with the wallet as the active signer source

## `state-verification`

Use when:

- commitment verification changed
- sync-after-execute behavior changed
- the prompt explicitly asks to compare local vs on-chain state

Steps:

1. Load a multisig and run `sync()`.
2. Fetch decoded state:
   ```js
   await window.smoke.fetchState();
   ```
3. Verify commitments:
   ```js
   await window.smoke.verifyStateCommitment();
   ```
4. If the task changed execute behavior, run the verification again after a proposal execute and post-execute sync.

Expect:

- state fetch succeeds
- local and on-chain commitments match
- post-execute state verification reflects the updated account state
