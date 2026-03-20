# Timing Baseline

## Use This File

Use this file as the timing reference for future smoke runs of `examples/smoke-web` and related browser-only workflows.

- Prefer exact timings from `await window.smoke.events()` whenever the step is driven by the command API.
- If the current run only has coarse manual timings for faucet, wallet, or browser actions, still record them and mark them approximate.
- When multiple clean samples exist for the same step, prefer the median rather than the single fastest run.

## Measurement Rules

- Start the timer when the final user action for the step is submitted.
- Stop the timer when the promise resolves or rejects, or when the next actionable browser state appears.
- Use `durationMs` from `window.smoke.events()` for command timings.
- Record retries separately from the eventual recovered duration.
- For canonicalization-sensitive behavior, time the lag after execute separately from execute itself.
- For proof-generation-heavy executes, label the step as proof-generation dominated.

## Comparison Rules

- `normal`: within 2x of baseline and within baseline + 60s
- `slow`: greater than 2x baseline or greater than baseline + 60s
- `severe`: greater than 3x baseline, repeated retry loops, or timeout
- Recovered failures still count as failures for canary reporting; the recovered duration is additional context, not a pass override.

## Reference Environment

- Repo: current Guardian workspace
- GUARDIAN endpoint: local HTTP `http://localhost:3000`
- Smoke harness: local isolated-browser canary harness on a free local port
- Network: Miden testnet
- Default signer source: local
- Default signature scheme: Falcon
- Timing source preference: `window.smoke.events()` first, manual stopwatch second

## Baseline Samples

First verified isolated-browser sample recorded on March 20, 2026 using Chrome + Brave.

| Workflow | Operation | Elapsed | Confidence | Outcome | Notes |
| --- | --- | --- | --- | --- | --- |
| `browser-baseline` | Chrome bootstrap | `1213ms` | medium | recovered failure | Initial auto-bootstrap failed with `ConstraintError: Key already exists in the object store`; explicit `initSession()` recovered |
| `browser-baseline` | Chrome `initSession` | `1218ms` | high | pass | Recovered the failed Chrome bootstrap |
| `browser-baseline` | Chrome `createAccount` | `121ms` | high | pass | 2-of-2 local Falcon account |
| `browser-baseline` | Chrome `registerOnGuardian` | `561ms` | high | pass | Local GUARDIAN registration |
| `browser-baseline` | Brave bootstrap | `3623ms` | high | pass | Clean page-load bootstrap |
| `browser-baseline` | Brave `loadAccount` | `424ms` | high | pass | Loaded Chrome-created account |
| `browser-baseline` | Brave `sync` | `343ms` | high | pass | Synced to the registered 2-of-2 state |

Recovery-aware local Falcon samples recorded later on March 20, 2026 using isolated Chrome profiles only.

| Workflow | Operation | Elapsed | Confidence | Outcome | Notes |
| --- | --- | --- | --- | --- | --- |
| `online-proposal-canary` | A `initSession` | `1620ms` | high | pass | Clean init |
| `online-proposal-canary` | B bootstrap timeout + recovered `initSession` | `30000ms` + `1082ms` | high | recovered failure | Page bootstrap timed out, explicit `initSession()` recovered |
| `online-proposal-canary` | C `initSession` | `1446ms` | high | pass | Clean init |
| `online-proposal-canary` | `createProposal(add_signer)` | `414ms` | high | pass | A created add-signer proposal |
| `online-proposal-canary` | B `signProposal` | `365ms` | high | pass | First signature |
| `online-proposal-canary` | A `signProposal` | `334ms` | high | pass | Proposal moved to `ready` |
| `online-proposal-canary` | `executeProposal` first response | `19918ms` | high | recovered failure | Returned nonce-overwrite error before canonicalization |
| `online-proposal-canary` | Post-execute convergence | `9166ms` | high | pass | A/B/C converged to 2-of-3 and C could load/sync |
| `online-proposal-canary` | `verifyStateCommitment` | `644ms` | high | pass | Local and on-chain commitments matched after convergence |
| `payment-roundtrip-canary` | A bootstrap timeout + recovered `initSession` | `30000ms` + `1239ms` | high | recovered failure | Page bootstrap timed out, explicit `initSession()` recovered |
| `payment-roundtrip-canary` | B `initSession` | `1554ms` | high | pass | Clean init |
| `payment-roundtrip-canary` | Faucet note visibility | `16491ms` | high | pass | Public note visible in both browsers after sync |
| `payment-roundtrip-canary` | `consume_notes` first execute response | `22768ms` | high | recovered failure | Returned nonce-overwrite error before canonicalization |
| `payment-roundtrip-canary` | Vault convergence after consume | `14067ms` | high | pass | Both browsers showed non-empty vault balances |
| `payment-roundtrip-canary` | `p2id` first execute response | `19099ms` | high | recovered failure | Returned nonce-overwrite error before canonicalization |
| `payment-roundtrip-canary` | Received note after self-P2ID | `9086ms` | high | pass | New note appeared in both browsers after sync |
| `payment-roundtrip-canary` | `verifyStateCommitment` | `567ms` | high | pass | Local and on-chain commitments matched after final sync |
| `switch-guardian-offline-canary` | A recovered `initSession` | `1722ms` | high | recovered failure | Initial bootstrap hit `ConstraintError`; explicit `initSession()` recovered |
| `switch-guardian-offline-canary` | B recovered `initSession` | `2069ms` | high | recovered failure | Initial bootstrap timed out; explicit `initSession()` recovered |
| `switch-guardian-offline-canary` | `executeProposal` | `20277ms` | high | pass | Fully-signed 2-of-2 offline switch executed directly |
| `switch-guardian-offline-canary` | Post-switch `sync` with GUARDIAN A down | `383ms` | high | pass | Sync succeeded only after the account switched to GUARDIAN B |

Local ECDSA samples recorded later on March 20, 2026 using isolated Chrome profiles only.

| Workflow | Operation | Elapsed | Confidence | Outcome | Notes |
| --- | --- | --- | --- | --- | --- |
| `online-proposal-canary` | A `initSession` | `1076ms` | high | pass | Clean init |
| `online-proposal-canary` | B `initSession` | `1344ms` | high | pass | Clean init |
| `online-proposal-canary` | C `initSession` | `1102ms` | high | pass | Clean init |
| `online-proposal-canary` | `createProposal(add_signer)` | `382ms` | high | pass | A created add-signer proposal |
| `online-proposal-canary` | B `signProposal` | `759ms` | high | pass | First signature |
| `online-proposal-canary` | A `signProposal` | `262ms` | high | pass | Proposal moved to `ready` |
| `online-proposal-canary` | `executeProposal` first response | `5778ms` | high | recovered failure | Returned nonce-overwrite error before canonicalization |
| `online-proposal-canary` | Post-execute convergence | `9182ms` | high | pass | A/B converged to 2-of-3 and C loaded the updated account |
| `online-proposal-canary` | `verifyStateCommitment` | `514ms` | high | pass | Local and on-chain commitments matched after convergence |
| `payment-roundtrip-canary` | A `initSession` | `1178ms` | high | pass | Clean init |
| `payment-roundtrip-canary` | B `initSession` | `1179ms` | high | pass | Clean init |
| `payment-roundtrip-canary` | Faucet note visibility | `16922ms` | high | pass | Public note visible in both browsers after sync |
| `payment-roundtrip-canary` | `consume_notes` first execute response | `5108ms` | high | recovered failure | Returned nonce-overwrite error before canonicalization |
| `payment-roundtrip-canary` | Vault convergence after consume | `10070ms` | high | pass | Both browsers showed non-empty vault balances |
| `payment-roundtrip-canary` | `p2id` first execute response | `4789ms` | high | recovered failure | Returned nonce-overwrite error before canonicalization |
| `payment-roundtrip-canary` | Received note after self-P2ID | `29310ms` | high | pass | New note appeared in both browsers after sync |
| `payment-roundtrip-canary` | `verifyStateCommitment` | `323ms` | high | pass | Local and on-chain commitments matched after final sync |
| `switch-guardian-offline-canary` | A `initSession` | `1141ms` | high | pass | Clean init on GUARDIAN A |
| `switch-guardian-offline-canary` | B `initSession` | `1476ms` | high | pass | Clean init on GUARDIAN A |
| `switch-guardian-offline-canary` | `createProposal(switch_guardian)` | `104ms` | high | pass | Used the ECDSA-specific `pubkey?scheme=ecdsa` commitment for GUARDIAN B |
| `switch-guardian-offline-canary` | B `signProposalOffline` | `216ms` | high | pass | Second cosigner signed imported JSON offline |
| `switch-guardian-offline-canary` | A `signProposalOffline` | `151ms` | high | pass | Proposal became fully signed offline |
| `switch-guardian-offline-canary` | `executeProposal` | `4369ms` | high | pass | Fully-signed 2-of-2 offline switch executed directly |
| `switch-guardian-offline-canary` | Post-switch `sync` with GUARDIAN A down | `355ms` | high | pass | Sync succeeded only after the account switched to GUARDIAN B |

## Future Samples

Append future runs in the same format. Once at least three clean samples exist for a step under the same network, signer source, and signature scheme, use the median as the working baseline in reports.
