---
name: smoke-test-ts-multisig-sdk
description: Drive manual smoke testing of the TypeScript `@openzeppelin/miden-multisig-client` SDK in this repository through the browser-only `examples/smoke-web` harness and targeted TypeScript checks. Use when Codex needs to verify browser multisig account creation, cosigner sync, proposal creation/sign/execute, offline export/import, state verification, or local/Para/Miden Wallet behavior after changes in `packages/miden-multisig-client`, `packages/guardian-client`, `examples/_shared/multisig-browser`, `examples/smoke-web`, or `examples/web`.
---

# Smoke Test TS Multisig SDK

Use `examples/smoke-web` as the primary smoke surface for `@openzeppelin/miden-multisig-client`. Treat `examples/web` as the parity target for the shared browser orchestration layer, not as the main canary surface.

## Deployment Targets

Pick the target before picking a workflow. All three are valid smoke targets — the choice depends on what's being verified.

| Target | GUARDIAN endpoint | Miden RPC | When to use |
| --- | --- | --- | --- |
| **Local dev** | `http://localhost:3000` | `https://rpc.devnet.miden.io` or local `http://localhost:57291` | default for in-repo changes not yet released |
| **Staging (devnet)** | `https://guardian-stg.openzeppelin.com` | `https://rpc.devnet.miden.io` | verify a candidate before release; mirrors prod topology on devnet |
| **Production (testnet)** | `https://guardian.openzeppelin.com` | `https://rpc.testnet.miden.io` | smoke the published npm SDK against live prod |

Sanity-check GUARDIAN before every deployed-env run:

```bash
curl https://guardian.openzeppelin.com/pubkey
curl 'https://guardian.openzeppelin.com/pubkey?scheme=ecdsa'
curl https://guardian-stg.openzeppelin.com/pubkey
```

If the commitment does not round-trip into `status().multisig.guardianPubkey` after `createAccount`, report the exact mismatch.

## Testing Deployed SDKs

`examples/smoke-web/package.json` pins `@openzeppelin/miden-multisig-client` and `@openzeppelin/guardian-client` to `file:../../packages/...`, so `npm run dev` smokes the **workspace source**, not the npm release. When the user asks to smoke "the deployed SDK" or "the released version":

1. Create a scratch Vite (or equivalent) project outside the workspace (e.g. `/tmp/guardian-ts-smoke-<version>`).
2. Declare the published packages pinned to the release under test:
   ```json
   {
     "dependencies": {
       "@openzeppelin/miden-multisig-client": "0.14.3",
       "@openzeppelin/guardian-client": "0.14.3",
       "@miden-sdk/miden-sdk": "0.14.3"
     }
   }
   ```
3. Port the smoke surface (`smokeHarness.ts`, `initClient.ts`, `multisigApi.ts`, the `window.smoke` shim) as-is so the workflow matrix still applies verbatim. Avoid re-implementing flows; lexical parity lets baseline timings stay comparable.
4. Run `npm install` and record the actual resolved version from `npm ls @openzeppelin/miden-multisig-client`.
5. Point the scratch harness at the chosen Deployment Target and run the workflow.

Treat workspace-path smoke-web runs and deployed-npm scratch-project runs as different smoke targets. Never collapse them in the report.

## Browser Automation

The smoke harness is designed to be driven from DevTools (`window.smoke.*`), which makes it drivable by browser-automation tools when available. Prefer in this order:

1. **`mcp__Claude_in_Chrome__*`** (Chrome MCP) — drives real Chrome tabs. Use `tabs_context_mcp` to get the tab group, `tabs_create_mcp` to open one tab per cosigner, `navigate` to load the smoke URL, `javascript_tool` to run `await window.smoke.createAccount(...)` etc. Supports ECDSA local-signer flows end-to-end. `browser_batch` lets you chain `navigate → evaluate → wait` in one round-trip. Does **not** bypass wallet modals — Para and Miden Wallet still need a human at the keyboard for the approval UI.
2. **`mcp__Claude_Preview__*`** (Claude Preview) — scoped to a single dev-server preview, good for the 1-cosigner happy path or for `preview_console_logs` and `preview_network` during a handoff. Less suited to concurrent cosigner flows because it is single-tab.
3. **Manual DevTools** — default fallback. Required for wallet-signer flows that need real user approval.

Isolation rules stay identical when using browser MCPs:

- Same-browser tabs in the MCP tab group do **not** give true IndexedDB isolation. For 3-cosigner runs, use Chrome MCP for one cosigner and drive the other two in a second real browser binary (Brave, Firefox) that the user controls manually. Same-browser concurrent cosigners are a known harness gap, not a supported canary path.
- Always call `await window.smoke.status()` via `javascript_tool` after any state transition; use that as the source of truth, not DOM state.
- When an MCP tool surfaces the opaque `[object Object]` error from `window.smoke.*`, re-query `await window.smoke.events()` and classify the failure from the newest matching event — the raw thrown value is noise.

When reporting, capture the concrete tool invocation path used (Chrome MCP vs Claude Preview vs manual) alongside the browser label so another agent can reproduce the run.

## Quick Start

1. Read the current browser harness before assuming command names or result shapes:
   - `examples/smoke-web/src/smokeHarness.ts`
   - `examples/_shared/multisig-browser/src/multisigApi.ts`
   - `examples/_shared/multisig-browser/src/initClient.ts`
   - `examples/smoke-web/src/App.tsx`
2. Run targeted TypeScript validation before manual smoke:
   ```bash
   cd packages/miden-multisig-client && npm test
   cd examples/smoke-web && npm run typecheck && npm run build
   cd examples/web && npm run build
   ```
3. Start one GUARDIAN server from the repo root:
   ```bash
   cargo run -p guardian-server --bin server
   ```
4. Start the smoke harness:
   ```bash
   cd examples/smoke-web && npm run dev
   ```
5. Open the smoke harness in separate real browsers or fully isolated browser profiles. Do not rely on same-profile parallel tabs. Prefer Chrome + Brave or Chrome + Firefox when driving concurrent cosigners.
6. Let the page-load bootstrap settle first. If `await window.smoke.status()` is not `ready`, or you need to override the default endpoints or signer settings, initialize the session with `window.smoke.initSession(...)`.
7. Default to local signers and Falcon unless the prompt explicitly asks for ECDSA, Para, or Miden Wallet.
8. Record each browser's signer commitment from `await window.smoke.status()` before account creation.
9. Create the multisig in one browser by passing the other browsers' commitments to `createAccount`.
10. Load and sync the account in the other browsers with `loadAccount` and `sync`.
11. Choose the smallest workflow from `references/workflow-matrix.md`.
12. Record the exact browser/profile labels, endpoints, signer source, signature scheme, workflow, observed result, and timing data.
13. Compare the recorded timings with `references/timing-baseline.md`.

## Baseline Harness

Use this as the default setup unless the prompt explicitly asks for something else:

- one GUARDIAN server (skip when targeting Staging or Production — the deployed server is already up)
- one `examples/smoke-web` dev server (or the scratch deployed-SDK project when smoking the published npm package)
- one browser or browser profile per cosigner session
- GUARDIAN endpoint matching the chosen target from the Deployment Targets table
- Miden RPC endpoint matching the chosen target
- signer source: `local`
- signature scheme: `falcon`

Treat each browser or browser profile as one cosigner. Prefer distinct browser binaries over multiple profiles when available. `initSession` clears the local IndexedDB state for that profile, so do not share one profile across concurrent cosigner sessions. Use `browserLabel` to make reports readable.

`examples/smoke-web` attempts a bootstrap on page load. For automation or console-driven smoke, check `await window.smoke.status()` before calling `initSession()`. Use `initSession()` as recovery or explicit reconfiguration, not as the automatic first step for an already-ready profile.

## Workflow Selection

- Run `browser-baseline` first when the prompt asks for a general smoke test or does not narrow the target behavior yet.
- Run `online-proposal-canary` first when the prompt asks for a default create/sign/execute canary.
- Run `payment-roundtrip-canary` when the prompt asks to test note receipt, note consumption, or self-P2ID payment flow.
- Run `switch-guardian-offline-canary` when the prompt asks to test switching providers or offline export/import/sign behavior.
- Run `para-connectivity` when signer resolution, Para integration, or ECDSA wallet flow changed.
- Run `miden-wallet-connectivity` when Miden Wallet connection, signing, or extension behavior changed.
- Run `state-verification` when commitment comparison, sync-after-execute, or account-state inspection changed.
- Run at least one Falcon pass and one ECDSA pass when key management, commitment parsing, signature encoding, or signer-source selection changed.

## Execution Rules

- Prefer `window.smoke` over DOM clicking. The UI is a status surface, not the canonical test interface.
- Prefer truly isolated browsers for concurrent cosigners. Same-browser automation is not a supported canary path unless it uses proven storage isolation.
- Use `await window.smoke.status()` after any meaningful state transition.
- Use `await window.smoke.events()` as the default command-history and timing source.
- When a `window.smoke.*` command throws an opaque JS value such as `[object Object]`, classify the failure from the newest matching `window.smoke.events()` entry rather than from the thrown value.
- If page-load bootstrap already reached `ready`, do not immediately call `initSession()` again on the same profile unless you are intentionally changing config or recovering from a failed bootstrap.
- Treat `examples/smoke-web` as the required manual smoke surface for browser SDK behavior; package tests and builds support this but do not replace it.
- Build `examples/web` whenever shared browser code changes; if the change is risky, note whether `examples/web` was also launched manually.
- Trust current source in `examples/smoke-web/src/` and `examples/_shared/multisig-browser/src/` over README text when they disagree, and note the mismatch in your result.
- Do not mark a workflow as passed unless the expected state transition is visible in `status()`, `events()`, proposal lists, notes, or explicit command output.
- For external actions that happen outside the command API, such as faucet submission or wallet modal interaction, pair the manual step with an immediate `status()` or `events()` capture.
- In the GUARDIAN-ack flow, remember that the browser client pushes the delta to GUARDIAN before the final transaction is submitted on-chain. Early canonicalization polls can therefore see the previous or zero on-chain commitment while proving or submission is still in flight.
- When `executeProposal` fails with `Refusing to overwrite local state: incoming nonce ... is not greater than local nonce ...`, treat that as a reportable pre-canonicalization state, not an immediate terminal failure. Keep syncing until the expected account state converges or the workflow times out.
- Apply the same canonicalization rule to the first post-submit `sync()` and to a newly-added cosigner's `loadAccount()`. A temporary nonce-overwrite or unauthorized failure is expected pending state by itself; the workflow only fails if later sync/load never converges.

## Timing Discipline

- Prefer the built-in `durationMs` values from `window.smoke.events()` for command timings:
  - `initSession`
  - `connectPara`
  - `connectMidenWallet`
  - `createAccount`
  - `loadAccount`
  - `registerOnGuardian`
  - `sync`
  - `fetchState`
  - `verifyStateCommitment`
  - `createProposal`
  - `signProposal`
  - `executeProposal`
  - `exportProposal`
  - `signProposalOffline`
  - `importProposal`
- Time non-command steps manually:
  - browser/profile startup
  - wallet approval modal latency
  - faucet confirmation and note visibility lag
  - canonicalization lag after execute when a newly-added cosigner is waiting to load the account
- Start manual timing when the final user action is submitted and stop when the next actionable browser or console state appears.
- If a step fails and later recovers, record both:
  - time to first failure
  - time to eventual recovery
- For proof-generation steps, explicitly note that the time is proof-generation dominated.
- Compare each captured duration with `references/timing-baseline.md`. If no baseline exists yet for that exact step, append the new timing as the first reference sample.
- Treat timing regression as reportable when a step exceeds the baseline by more than 2x or by more than 60 seconds, whichever is larger. Treat more than 3x or timeout/retry loops as severe degradation.

## High-Risk Assertions

- Verify each browser/profile is using the intended signer source and signature scheme.
- Verify the reported commitments match the expected browser session before account creation.
- Verify `createAccount` produces the expected threshold and signer set.
- Verify `loadAccount` and `sync` converge to the same account state across cosigners.
- Verify proposal IDs stay stable across export, import, offline sign, and execute.
- Verify collected vs required signatures before execute.
- Verify execute changes account state, proposal visibility, nonce-sensitive behavior, or detected vault state.
- Treat an early post-`push_delta` or post-execute on-chain `0x000...0` commitment, or a first canonicalization mismatch, as expected pending state by itself. Only treat it as a product failure if the account never converges after the proving/submission window or reaches a terminal discard/timeout.
- Verify note visibility before `consume_notes` and after self-P2ID transfer.
- Verify Para and Miden Wallet sessions expose commitment, public key, and connected state after connect.
- Verify `Switch GUARDIAN` proves real post-switch behavior, not just local proposal mutation.
- For a 2-of-2 offline `Switch GUARDIAN` run, verify both cosigners have offline-signed before execute. The proven sequence is: A exports, B imports and offline-signs, A imports B's signed JSON, A offline-signs, A imports the fully-signed JSON, then A executes.

## Canary Failure Policy

- Treat any error during setup, connect, create, load, sync, sign, execute, import, export, or post-execute sync as reportable.
- Report transient errors even if a retry or later sync succeeds.
- Treat bootstrap `ConstraintError: Key already exists in the object store` as reportable even if a later `initSession()` succeeds and the workflow recovers.
- Treat the first post-submit nonce-overwrite or unauthorized sync/load error as a recovered failure only if later sync/load proves the expected canonicalized state. If the state never converges, mark the workflow failed.
- Include the browser label, workflow step, command name, full error text, relevant event entries, whether a retry was attempted, and whether the run eventually recovered.
- Do not collapse multiple failures into one summary line; preserve the sequence so the user can see where the canary first degraded.
- If the smoke harness itself lacks a direct verification surface for a claimed SDK behavior, report that as a harness gap rather than silently marking the workflow passed.

## Reporting

Report:

- commands run
- endpoints, signer source, and signature scheme
- browser/profile labels used as cosigners
- workflows exercised
- pass/fail per workflow
- elapsed time per major step
- baseline comparison and delta for each timed step
- every observed error, even if recovered
- relevant `window.smoke.events()` entries for failures or slow steps
- whether `examples/smoke-web` and `examples/web` builds passed
- skipped coverage with reason
