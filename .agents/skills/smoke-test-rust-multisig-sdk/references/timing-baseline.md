# Timing Baseline

## Use This File

Use this file as the timing reference for future smoke runs of `guardian-demo` and related offline workflows.

- Prefer exact timings captured during the run.
- If the current run only has coarse manual timings, still record them and mark them approximate.
- When multiple clean samples exist for the same step, prefer the median rather than the single fastest run.

## Measurement Rules

- Start the timer when the final input for the step is submitted and the demo starts blocking work.
- Stop the timer when the demo prints the success line, error line, or next actionable prompt.
- Record retries separately from the eventual recovered duration.
- For canonicalization-sensitive behavior, time the lag after execute separately from the execute itself.
- For proof-generation-heavy executes, label the step as proof-generation dominated.

## Comparison Rules

- `normal`: within 2x of baseline and within baseline + 60s
- `slow`: greater than 2x baseline or greater than baseline + 60s
- `severe`: greater than 3x baseline, repeated retry loops, or timeout
- Recovered failures still count as failures for canary reporting; the recovered duration is additional context, not a pass override.

## Reference Environment

- Date: 2026-03-19
- Repo: `/Users/marcos/repos/guardian`
- GUARDIAN endpoint: local gRPC `http://localhost:50051`
- Network: Miden testnet
- Signature scheme: Falcon
- Collection method: manual live run with coarse polling, not a dedicated stopwatch

## Baseline Samples

| Workflow | Operation | Elapsed | Confidence | Outcome | Notes |
| --- | --- | --- | --- | --- | --- |
| add-cosigner-canary | client initialization to main menu | ~6s | medium | pass | New Falcon keypair generation on startup. |
| add-cosigner-canary | create 2-of-2 account and register on GUARDIAN | ~1s | low | pass | Includes account creation and initial GUARDIAN configuration. |
| add-cosigner-canary | second cosigner pull existing account | ~1s | low | pass | Initial `Sync account` pull by tab B. |
| add-cosigner-canary | create add-cosigner proposal | ~5s | medium | pass | Proposal created on GUARDIAN and auto-signed by creator. |
| add-cosigner-canary | second cosigner sign proposal | ~1s | low | pass | Signatures reached threshold immediately. |
| add-cosigner-canary | execute add-cosigner proposal | >20s | low | pass | Proof-generation dominated. Exact elapsed was not captured in the first run. |
| add-cosigner-canary | canonicalization lag before new cosigner could pull | ~15s | low | recovered | Newly-added cosigner first saw unauthorized auth failure, then could pull after canonicalization. |
| add-cosigner-canary | post-execute sync recovery after delta/store failure | ~20s | low | recovered | Included delta commitment mismatch, store overflow panic, reinitialize, and re-pull. |
| payment-roundtrip-canary | client initialization to main menu | ~6s | medium | pass | Same startup path as other Falcon sessions. |
| payment-roundtrip-canary | create 2-of-2 account and register on GUARDIAN | ~1s | low | pass | Account creation tab. |
| payment-roundtrip-canary | second cosigner pull existing account | ~1s | low | pass | Initial sync pull by tab B. |
| payment-roundtrip-canary | faucet PoW solve for 100-token public note | 0.817s | high | pass | Captured programmatically from terminal-driven faucet flow. |
| payment-roundtrip-canary | faucet `get_tokens` request after PoW | <1s | medium | pass | Returned `tx_id` and `note_id` immediately. |
| payment-roundtrip-canary | first demo sync after faucet mint | ~10s | medium | pass | Account sync completed with nonce unchanged. |
| payment-roundtrip-canary | note visibility in `List consumable notes` | ~5s | medium | pass | Minted note appeared as `ConsumableWithAuthorization`. |
| payment-roundtrip-canary | create consume-notes proposal | ~5s | medium | pass | Proposal created on GUARDIAN and auto-signed. |
| payment-roundtrip-canary | second cosigner sign consume-notes proposal | ~1s | low | pass | Signatures reached `2/2`. |
| payment-roundtrip-canary | execute consume-notes proposal | ~120s | high | pass | Proof-generation dominated; long CPU-bound wait before success. |
| payment-roundtrip-canary | simple post-consume sync | ~1s | low | pass | Clean sync on the executing tab. |
| payment-roundtrip-canary | recovered post-consume sync after delta/store failure | ~30s | medium | recovered | Included delta commitment mismatch, store overflow panic, reinitialize, and re-pull on the other tab. |
| payment-roundtrip-canary | create self-P2ID proposal | ~5s | medium | pass | Proposal created on GUARDIAN and auto-signed. |
| payment-roundtrip-canary | second cosigner sign self-P2ID proposal | ~5s | low | pass | Includes menu navigation plus sign action. |
| payment-roundtrip-canary | execute self-P2ID proposal | ~100s | high | pass | Proof-generation dominated. |
| payment-roundtrip-canary | final sync and received-note visibility | ~10s | medium | pass | New received note visible on both tabs after sync. |
| switch-guardian-offline-canary | start replacement GUARDIAN after temporary port change | ~10s | medium | pass | Includes rebuild/startup until `/pubkey` answered. |
| switch-guardian-offline-canary | create 2-of-2 account and register on GUARDIAN A | ~1s | low | pass | Fresh account on default GUARDIAN. |
| switch-guardian-offline-canary | second cosigner pull existing account | ~1s | low | pass | Initial sync pull by tab B. |
| switch-guardian-offline-canary | online create failure to offline fallback prompt | ~30s | high | recovered | GUARDIAN A was down; demo eventually offered offline creation. |
| switch-guardian-offline-canary | offline switch proposal creation and save | ~5s | medium | pass | Exported JSON proposal created locally. |
| switch-guardian-offline-canary | import offline proposal on second cosigner | ~5s | medium | fail | Failed with `recency condition error: The client is too far behind the chain tip to execute the transaction`. |
| switch-guardian-offline-canary | import offline proposal on second cosigner after network-refresh retry fix | ~60s | medium | pass | Import retried after network-only refresh and then succeeded. |
| switch-guardian-offline-canary | sign imported switch proposal | ~1s | low | pass | Second cosigner added the final signature. |
| switch-guardian-offline-canary | execute imported switch proposal | ~130s | medium | pass | Proof-generation dominated offline `Switch GUARDIAN` execution. |
| switch-guardian-offline-canary | verify executing tab shows new GUARDIAN endpoint | ~1s | low | pass | `s` showed `http://127.0.0.1:50052`. |

## Future Samples

Append future runs in the same format. Once at least three clean samples exist for a step under the same network and signature scheme, use the median as the working baseline in reports.
