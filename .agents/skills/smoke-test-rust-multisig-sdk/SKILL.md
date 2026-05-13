---
name: smoke-test-rust-multisig-sdk
description: Drive manual smoke testing of the Rust `miden-multisig-client` SDK in this repository through the interactive `examples/demo` CLI and targeted Rust checks. Use when Codex needs to verify account creation, cosigner sync, proposal creation/sign/execute, offline export/import, state-commitment verification, or Falcon/ECDSA behavior after changes in `crates/miden-multisig-client`, `crates/client`, `crates/server`, or `examples/demo`.
---

# Smoke Test Rust Multisig SDK

Use `cargo run -p guardian-demo` as the primary smoke harness for `miden-multisig-client`. Start with the smallest workflow that covers the changed codepath, then expand to adjacent workflows when a lower-layer change can affect GUARDIAN coordination, signature handling, or proposal execution.

## Deployment Targets

Pick the target before picking a workflow. All three are valid smoke targets — the choice depends on what's being verified.

| Target | GUARDIAN endpoint | Miden RPC | When to use |
| --- | --- | --- | --- |
| **Local dev** | `http://localhost:50051` (gRPC) or `http://localhost:3000` (HTTP) | `http://localhost:57291` or devnet | default for in-repo changes not yet released |
| **Staging (devnet)** | `https://guardian-stg.openzeppelin.com` | `https://rpc.devnet.miden.io` | verify a candidate before release; mirrors prod topology on devnet |
| **Production (testnet)** | `https://guardian.openzeppelin.com` | `https://rpc.testnet.miden.io` | smoke the published SDK version against live prod |

Sanity-check the GUARDIAN target before every deployed-env run — a non-200 response or commitment mismatch means the ALB or server is unhealthy and blocks the rest of the canary:

```bash
curl https://guardian.openzeppelin.com/pubkey
curl 'https://guardian.openzeppelin.com/pubkey?scheme=ecdsa'
# staging
curl https://guardian-stg.openzeppelin.com/pubkey
# gRPC over TLS (matches what the Rust SDK uses)
grpcurl -import-path crates/server/proto -proto guardian.proto \
  -d '{}' guardian.openzeppelin.com:443 guardian.Guardian/GetPubkey
```

Record the returned commitment — you'll paste it into the demo's "GUARDIAN commitment" prompt during `Switch GUARDIAN` canaries, and you'll use it to verify `s` output.

## Testing Deployed SDKs

`examples/demo` depends on `miden-multisig-client` via a workspace `path = "../../crates/miden-multisig-client"`, so running it smokes the **workspace source**, not the published crate. When the user asks to smoke "the deployed SDK" or "the released version":

1. Create a scratch cargo project outside the workspace (e.g. `/tmp/guardian-rust-smoke-<version>`).
2. Declare the published crates as registry dependencies pinned to the release under test:
   ```toml
   [dependencies]
   miden-multisig-client = "0.14.6"
   guardian-client       = "0.14.6"
   miden-client          = "0.14.5"
   tokio                 = { version = "1", features = ["full"] }
   ```
3. Port or copy the exact menu/action code from `examples/demo/src/` into that scratch project. Keep the prompts identical so the workflows in `references/workflow-matrix.md` still apply verbatim.
4. Run `cargo build` to confirm the resolved dependency graph is the published release — not a transitive workspace override.
5. Record the exact versions printed by `cargo tree -p miden-multisig-client --depth 0` in the result.

Treat workspace-path demo runs and deployed-crate scratch-project runs as different smoke targets. Never collapse them in the report.

## Quick Start

1. Read the current demo surface before assuming prompts or menu labels:
   - `examples/demo/src/menu.rs`
   - `examples/demo/src/actions/create_account.rs`
   - `examples/demo/src/actions/sync_account.rs`
   - `examples/demo/src/actions/proposal_management.rs`
2. Run targeted Rust validation before manual smoke:
   ```bash
   cargo test -p miden-multisig-client
   cargo test -p guardian-demo
   ```
3. Start one GUARDIAN server session.
4. Start three demo sessions from the repo root:
   ```bash
   cargo run -p guardian-demo
   ```
5. Point all demo sessions at the local GUARDIAN server and Miden devnet.
6. Default to Falcon unless the prompt explicitly asks for ECDSA.
7. Record each session's displayed signer commitment before creating the account.
8. Create the multisig account in one session by pasting the other sessions' commitments.
9. Pull and sync the account in the other sessions.
10. Choose the minimal workflow from `references/workflow-matrix.md`.
11. Record the exact network, GUARDIAN endpoint, signature scheme, workflow, observed result, and timing data.
12. Compare the recorded timings with `references/timing-baseline.md`.

## Baseline Harness

Use this as the default setup unless the prompt explicitly asks for something else:

- one tab for the GUARDIAN server (skip when targeting Staging or Production — the deployed server is already up)
- three tabs running `cargo run -p guardian-demo` (or three runs of the scratch deployed-SDK binary when smoking the published crate)
- GUARDIAN endpoint matching the chosen target from the Deployment Targets table
- Miden RPC matching the chosen target
- Falcon signature scheme unless the prompt explicitly asks for ECDSA

Treat the three demo tabs as three cosigners of the same account. Capture the commitments shown at startup, then use one tab to create the account and the other tabs to pull it with `Sync account`. Expect initial sync to take time; allow for retries before calling it a failure.

When the first canary is `add cosigner`, reserve one tab as the signer to add later. In that case, create the initial account with the other two tabs, then use the reserved tab's commitment in the proposal workflow.

## Workflow Selection

- Run `three-cosigner-baseline` first when the prompt asks for a general smoke test or does not narrow the target behavior.
- Run `add-cosigner-canary` first when the prompt asks for a default create/sign/execute smoke test.
- Run `payment-roundtrip-canary` when the prompt asks to test note receipt, note consumption, or sending a payment.
- Run `switch-guardian-offline-canary` when the prompt asks to test switching providers or offline `Switch GUARDIAN` execution.
- Run `account-bootstrap` when builder setup, account creation, push/pull registration, or threshold configuration changes.
- Run `cosigner-sync` when account import, delta retrieval, sync, retry/reinitialize logic, or store recovery paths change.
- Run `online-proposal-lifecycle` when proposal parsing, proposal metadata, thresholds, signing, execution, or post-execution sync changes.
- Run `offline-switch-guardian` when export/import, offline signing, offline execution, `SwitchGuardian`, or fallback behavior changes.
- Run `state-verification` when commitment comparison or sync-after-execute behavior changes.
- Run `recover-by-key-canary` when key-commitment lookup, `recover_by_key`, `lookup_account_by_key_commitment`, or any related code path changes.
- Run at least one Falcon pass and one ECDSA pass when key management, commitment parsing, signature encoding, or scheme selection changes.

## Execution Rules

- Prefer the baseline harness unless the task explicitly requires a different setup.
- Use the local GUARDIAN server and Miden devnet as the default endpoint pair for manual smoke.
- Treat `examples/demo` as the required manual smoke surface for SDK behavior; crate tests support this but do not replace it.
- Use three demo sessions for the standard cosigner flow. If fewer sessions are used, say which cosigner paths were not exercised.
- Trust current source in `examples/demo/src/` over README text when they disagree, and note the mismatch in your result.
- Do not mark a workflow as passed unless the demo reached the success state or printed the expected confirmation for that path.
- If the change touches transport or auth, rerun the minimal affected workflow against the relevant GUARDIAN endpoint choice from startup.

## Timing Discipline

- Time every blocking operation that can regress materially:
  - client initialization
  - account creation and GUARDIAN registration
  - initial account pull or sync
  - proposal creation
  - proposal signing
  - proposal execution
  - post-execution sync
  - note visibility after faucet mint or self-P2ID
  - account recovery by key (`Recover by key` menu action: lookup + per-match `get_state`)
  - offline fallback prompt after GUARDIAN failure
  - offline proposal import, sign, and execute
- Start timing when the final input for that step is submitted and the demo begins blocking work.
- Stop timing when the demo prints a success line, an error line, or the next actionable prompt.
- If a step fails and later recovers, record both:
  - time to first failure
  - time to eventual recovery
- For proof-generation steps, explicitly note that the time is proof-generation dominated rather than generic waiting.
- In the GUARDIAN-ack flow, remember that the client pushes the delta to GUARDIAN before the final transaction is submitted on-chain. An early canonicalization poll can therefore see the previous or zero on-chain commitment while proving or submission is still in flight.
- For canonicalization-sensitive steps, record the lag separately from the execute time. Example: time from `Transaction executed successfully!` to the first successful pull by a newly-added cosigner.
- Apply the same rule to the first post-submit sync or pull. A temporary nonce/state mismatch or newly-added cosigner authorization failure is expected pending state by itself until canonicalization catches up.
- Use exact timestamps when possible. If the timing was collected from manual polling instead of a stopwatch, mark it as approximate.
- Compare each captured duration with `references/timing-baseline.md`. If no baseline exists yet for that exact step, append the new timing as the first reference sample.
- Treat timing regression as reportable when a step exceeds the baseline by more than 2x or by more than 60 seconds, whichever is larger. Treat more than 3x or timeout/retry loops as severe degradation.

## High-Risk Assertions

- Verify collected vs required signatures before execute.
- Verify proposal IDs and account IDs stay stable across export/import.
- Verify nonce or state commitment changes after a successful execute.
- Verify all existing cosigner tabs resync after execute.
- Treat an early post-`push_delta` or post-execute on-chain `0x000...0` commitment, or a first canonicalization mismatch, as expected pending state by itself. Only treat it as a product failure if the account never converges after the proving/submission window or reaches a terminal discard/timeout.
- Treat newly-added cosigner pulls as canonicalization-sensitive:
  - poll until GUARDIAN canonicalization catches up
  - record time from execute success to first successful pull
  - do not treat an immediate post-execute authorization failure as a product bug by itself
- Treat an immediate post-execute sync mismatch in an existing cosigner tab the same way: report it, keep polling, and only fail the workflow if the tab never converges to the expected canonicalized state.
- Verify public-note receipt after faucet mint and post-sync before attempting `Consume notes`.
- Verify the vault gains assets after `Consume notes` executes.
- Verify a self-addressed P2ID transfer produces a new received note after the final sync.
- Verify `Switch GUARDIAN` uses the exact new endpoint string and the actual commitment from the replacement server.
- Verify the executing tab shows the new GUARDIAN endpoint after a successful switch.
- Verify at least one relaunched or newly-started demo tab can sync the account from the replacement GUARDIAN.
- Verify offline execution only for `SwitchGuardian`; reject broader offline claims unless code changed to support them.
- Verify Falcon and ECDSA behavior separately whenever signature-scheme code changes.
- Verify `Recover by key` returns the just-created `account_id` when called from the same demo tab that created the account (each `cargo run` regenerates the keypair; the canary asserts the SDK round-trip, not seed-derivation). Verify the recovered account loads + syncs to the same `state_commitment`.

## Canary Failure Policy

- Treat any error during setup, create, sync, sign, execute, or post-execution sync as reportable.
- Report transient errors even if a retry or later sync succeeds.
- Include the tab, workflow step, menu action, full error text, whether a retry was attempted, and whether the run eventually recovered.
- Do not collapse multiple failures into one summary line; preserve sequence so the user can see where the canary first degraded.

## Reporting

Report:
- commands run
- endpoints and signature scheme
- workflows exercised
- pass/fail per workflow
- elapsed time per major step
- baseline comparison and delta for each timed step
- every observed error, even if recovered
- regressions with concrete file paths or commands
- skipped coverage with reason
