# Workflow Matrix

## Use This File

Map the changed area to the smallest smoke workflow that still proves the behavior. Combine workflows when a lower-layer change crosses multiple categories.

## Environment

Run from the repo root:

```bash
cargo test -p miden-multisig-client
cargo test -p guardian-demo
```

Primary smoke command:

```bash
cargo run -p guardian-demo
```

Default startup choices:

- One GUARDIAN server tab
- Three demo tabs running `cargo run -p guardian-demo`
- Miden testnet
- GUARDIAN local gRPC: `http://localhost:50051` unless the task explicitly targets HTTP
- Signature scheme: Falcon unless the task specifically targets ECDSA

Record the signer commitment shown in each demo tab before account creation. Use one demo tab to create the account and paste the commitments from the other demo tabs into the cosigner list. Then use the remaining demo tabs to pull and sync the shared account.

If the first canary is `add-cosigner-canary`, reserve tab C as the future signer to add. Create the initial multisig with tabs A and B only, then add tab C through the proposal flow.

Also record timings for each major blocking step and compare them with `timing-baseline.md`.

## `three-cosigner-baseline`

Use when:

- the prompt asks for a general smoke test
- the change spans multiple multisig flows
- the prompt does not narrow the target behavior yet

Steps:

1. Start the GUARDIAN server in one tab.
2. Start three demo tabs with `cargo run -p guardian-demo`.
3. In every demo tab, choose the local GUARDIAN server.
4. In every demo tab, choose Miden testnet.
5. Choose the requested signature scheme or default to Falcon.
6. Record the signer commitment shown in each tab.
7. In demo tab A, select `[1] Create multisig account`.
8. Enter the threshold and total cosigner count.
9. Paste the commitments from tabs B and C when prompted.
10. Let tab A push the account to GUARDIAN.
11. In tabs B and C, select `[2] Sync account` and pull the created account.
12. Allow sync to take time before treating slow account availability as a failure.

Expect:

- all three tabs point to the same GUARDIAN server and testnet
- account creation succeeds in tab A
- the other tabs can eventually pull and sync the same account
- all tabs now act as cosigners for the same multisig account

## `add-cosigner-canary`

Use when:

- the prompt asks for a default create/sign/execute canary
- proposal creation, signing, execution, or post-execution sync changed
- you need a smoke test that does not depend on notes or assets

Setup:

1. Start from the baseline harness with three demo tabs.
2. Record the commitments from tabs A, B, and C.
3. In tab A, create the initial multisig account using only tabs A and B as cosigners.
4. In tab B, sync and pull the created account.
5. Keep tab C unbound to the account for now; its commitment will be the `Add cosigner` target.

Steps:

1. In tab A, open `[4] Proposal management`.
2. Select `[1] Create proposal`.
3. Select `Add cosigner`.
4. Paste tab C's commitment.
5. Confirm the proposal is created and automatically signed in tab A.
6. In tab B, run `[2] Sync account` before signing if the account state may be stale.
7. In tab B, open `[4] -> [3] Sign a proposal` and sign the pending proposal.
8. If the signature threshold is satisfied, execute from tab A or tab B with `[4] -> [4] Execute a proposal`.
9. Wait for execution to finish; proof generation can take time.
10. After execution, sync tabs A and B because the account state changed.
11. In tab C, use `[2] Sync account` to pull the updated account as the newly-added cosigner.

Expect:

- proposal creation succeeds in tab A
- the proposal uses tab C's commitment as the new signer
- tab B can sync, see the proposal, and sign it
- execution succeeds once signatures are sufficient
- post-execution sync updates the account state for the existing cosigners
- tab C can pull the updated account after execution

Canary checks:

- treat any error in create, sync, sign, execute, or post-execution sync as reportable
- if execution stalls or takes a long time, note that it is in proof generation rather than silently treating it as hung
- if a retry is needed, report the original error and the recovery path
- if tab C cannot pull the updated account after execute, report that as a canary failure even if tabs A and B look healthy
- record elapsed time for proposal create, sign, execute, existing-cosigner resync, and new-cosigner pull after canonicalization

## `payment-roundtrip-canary`

Use when:

- the prompt asks to send a payment
- note ingestion or note consumption changed
- vault updates or P2ID transfer behavior changed
- you need a canary that exercises faucet receipt through outbound transfer

Setup:

1. Start two demo tabs, A and B, plus one GUARDIAN server tab.
2. Point both demo tabs at the local GUARDIAN server and Miden testnet.
3. Choose the requested signature scheme or default to Falcon.
4. Create a 2-of-2 multisig using tabs A and B.
5. In tab B, sync and pull the created account.
6. In either tab, press `s` and copy the full `Account ID`.

Steps:

1. Open [Miden Faucet](https://faucet.testnet.miden.io/).
2. Paste the multisig `Account ID` into `Recipient address`.
3. Send a public note with `Send Public Note`.
4. Wait for the faucet confirmation before continuing.
5. In tabs A and B, run `[2] Sync account`.
6. Optionally run `[3] List consumable notes` until the new note appears and becomes consumable.
7. In tab A, open `[4] Proposal management` and create a `Consume notes` proposal.
8. Select the note that was just received from the faucet.
9. In tab B, sync if needed, then sign the consume-notes proposal.
10. Execute once signatures are sufficient.
11. Sync both tabs again.
12. Verify the vault is no longer empty or that the account balance reflects the consumed assets.
13. In tab A, create a `Transfer assets (P2ID)` proposal.
14. Use the same multisig `Account ID` as the recipient.
15. Choose the faucet asset and amount from the vault.
16. In tab B, sign the P2ID proposal.
17. Execute once signatures are sufficient.
18. Sync both tabs again.
19. Verify the account received the note from the self-addressed P2ID transfer by checking `[3] List consumable notes` or the note/status output.

Expect:

- faucet mint succeeds and confirms publicly before demo-side sync
- the new public note becomes visible after sync
- the consume-notes proposal can target the received note
- after consume execution, the vault contains the received asset
- the P2ID proposal executes successfully
- after the final sync, a new note addressed to the same account is visible

Canary checks:

- if the faucet page is unavailable, its fields differ materially, or mint confirmation never arrives, report that as a canary failure
- if sync succeeds but the note never appears, report the sync attempt count and elapsed wait
- if `Consume notes` shows no consumable notes, report whether the note was visible but not yet consumable
- if the vault remains empty after consume execution and sync, report that as a failure even if execution returned success
- if the self-P2ID executes but no received note appears after final sync, report that as a failure
- record elapsed time for faucet PoW, faucet mint response, first note visibility, consume execute, P2ID execute, and final note visibility

## `switch-guardian-offline-canary`

Use when:

- the prompt asks to switch GUARDIAN providers
- `SwitchGuardian` transaction behavior changed
- offline proposal creation, import, signing, or execution changed
- post-switch account registration on the new GUARDIAN changed

Important:

- The server binary hardcodes ports in `crates/server/src/main.rs`.
- If running both servers from one checkout, start the default-port GUARDIAN first, then edit `main.rs` for the replacement GUARDIAN ports before launching the second server.
- Give the replacement GUARDIAN its own `GUARDIAN_KEYSTORE_PATH`, `GUARDIAN_STORAGE_PATH`, and `GUARDIAN_METADATA_PATH` so it behaves like a distinct provider.
- Pick one host literal and keep it consistent. Prefer `127.0.0.1` or prefer `localhost`, but do not mix them inside the same canary.

Suggested replacement ports:

- HTTP: `3001`
- gRPC: `50052`

Setup:

1. Start GUARDIAN A on the default ports from the current checkout:
   ```bash
   cargo run -p guardian-server --bin server
   ```
2. Without stopping GUARDIAN A, temporarily edit `crates/server/src/main.rs` so GUARDIAN B uses alternate ports such as HTTP `3001` and gRPC `50052`.
3. Start GUARDIAN B in another tab with distinct directories:
   ```bash
   GUARDIAN_KEYSTORE_PATH=/tmp/guardian-b/keystore \
   GUARDIAN_STORAGE_PATH=/tmp/guardian-b/storage \
   GUARDIAN_METADATA_PATH=/tmp/guardian-b/metadata \
   cargo run -p guardian-server --bin server
   ```
4. Fetch GUARDIAN B's commitment from its HTTP `/pubkey` endpoint:
   - Falcon:
     ```bash
     curl http://127.0.0.1:3001/pubkey
     ```
   - ECDSA:
     ```bash
     curl 'http://127.0.0.1:3001/pubkey?scheme=ecdsa'
     ```
5. Start two demo tabs against GUARDIAN A on Miden testnet.
6. Create a 2-of-2 multisig and sync both tabs.
7. Record the account ID from `s`.
8. Kill GUARDIAN A.

Steps:

1. In one existing demo tab, open `[4] Proposal management`.
2. Select `[1] Create proposal`.
3. Select `Switch GUARDIAN provider`.
4. Enter GUARDIAN B's gRPC endpoint, for example `http://127.0.0.1:50052`.
5. Enter GUARDIAN B's commitment from `/pubkey`.
6. Because GUARDIAN A is down, expect online proposal creation to fail and the demo to offer offline fallback.
7. Accept offline creation and save the exported JSON proposal file.
8. In the other demo tab, open `[4] -> [6] Import & work with proposal file`.
9. Import the saved proposal, sign it, save if needed, and execute it once ready.
10. If proposal import or offline signing fails with a recency-condition error indicating the client is behind the chain tip, perform a network-only refresh before retrying.
11. In the current SDK, proposal binding verification retries once automatically after a network-only refresh for that specific error.
12. Wait for execution to finish; proof generation can take time.
13. In the executing tab, press `s` and verify `GUARDIAN Endpoint` now shows GUARDIAN B's endpoint.
14. If the demo later supports retargeting an existing cosigner session to GUARDIAN B, verify that path as a second check.

Expect:

- GUARDIAN B exposes a distinct, fetchable commitment
- offline `Switch GUARDIAN` proposal creation succeeds after GUARDIAN A is killed
- the second tab can import, sign, and execute the offline proposal
- the executing tab updates its in-memory `GUARDIAN Endpoint` to GUARDIAN B
- the updated account is registered on GUARDIAN B
- if available, a retargeted existing cosigner session can sync from GUARDIAN B

Canary checks:

- if GUARDIAN B reuses the old provider identity unintentionally, report the keystore/storage setup because the canary is not valid
- if `/pubkey` does not match the commitment entered into the proposal, report the exact mismatch
- if localhost and `127.0.0.1` are mixed and the endpoint verification becomes ambiguous, report the exact strings used
- if execution succeeds on-chain but registration on GUARDIAN B fails, report that as a canary failure
- if import repeatedly hits a recency-condition error even after the retry path, report that as a canary failure
- if an additional existing-cosigner retarget check is available and it cannot sync from GUARDIAN B after the switch, report that as a failure even if the executing tab shows the new endpoint
- record elapsed time for replacement GUARDIAN startup, offline fallback prompt, offline proposal creation, import, sign, execute, and restarted-tab sync

## `account-bootstrap`

Use when:

- builder or startup configuration changes
- account creation logic changes
- threshold or per-procedure threshold handling changes
- GUARDIAN account registration changes

Steps:

1. Complete `three-cosigner-baseline` through account creation.
2. Select `s` to show account details in the creator tab.

Expect:

- `Client initialized!`
- `Account created`
- `Account configured in GUARDIAN`
- account details render without error

Covers:

- builder initialization
- key generation
- commitment parsing
- `create_account_with_proc_thresholds`
- `push_account`

## `cosigner-sync`

Use when:

- account import changes
- `pull_account`, `get_deltas`, or `sync` changes
- retry, reinitialize, or store-recovery logic changes

Steps:

1. Complete `three-cosigner-baseline`.
2. Copy the account ID from account details or creation output.
3. In tabs B and C, select `[2] Sync account` and paste the account ID if needed.
4. Confirm the sync completes and the current nonce is shown.
5. Optionally select `v` after sync.

Expect:

- account fetch succeeds
- sync completes with a visible nonce
- retry or reinitialize paths print clear messages if they trigger

## `online-proposal-lifecycle`

Use when:

- proposal parsing or metadata changes
- threshold counting changes
- signing or execution changes
- post-execution sync changes

Choose the proposal type that matches the changed code:

- `Add cosigner`
- `Remove cosigner`
- `Transfer assets (P2ID)`
- `Consume notes`
- `Switch GUARDIAN provider`
- `Update procedure threshold override`

Prefer `Switch GUARDIAN provider`, `Add cosigner`, or threshold updates when the account has no assets or notes yet.

Use `add-cosigner-canary` as the first online workflow unless the prompt explicitly asks for another proposal type.

Use `payment-roundtrip-canary` when the prompt specifically asks for public note receipt, consume-notes, or P2ID payment validation.

Use `switch-guardian-offline-canary` when the prompt specifically asks for provider migration, offline `Switch GUARDIAN`, or verifying failover from a dead GUARDIAN instance.

Steps:

1. In session A, open `[4] Proposal management`.
2. Select `[1] Create proposal`.
3. Choose the matching proposal type and complete the prompts.
4. Verify the proposal is created and automatically signed by the proposer.
5. In session B, select `[4] -> [3] Sign a proposal`.
6. In either session, select `[4] -> [4] Execute a proposal`.

Expect:

- the proposal appears in the pending list
- signature counts increase after cosigner approval
- the ready marker or all-signatures-collected message appears at threshold
- execution succeeds and the account nonce increments
- post-execution sync succeeds or fails with an explicit, actionable message

## `offline-switch-guardian`

Use when:

- export or import code changes
- offline signing or execution changes
- `SwitchGuardian` behavior changes
- fallback behavior changes

Important:

- Fully offline create, sign, and execute only supports `SwitchGuardian`.
- Exporting an existing GUARDIAN proposal to file is broader, but executing an imported proposal offline is still limited to `SwitchGuardian`.

Path A: offline fallback flow

1. Start proposal creation for `Switch GUARDIAN provider`.
2. If GUARDIAN proposal creation fails and the demo offers offline fallback, accept it.
3. Save the generated JSON file.
4. In another session, use `[4] -> [6] Import & work with proposal file`.
5. Sign the imported proposal, save it, re-import it as needed, and execute it once ready.

Path B: export/import inspection flow

1. Create an online proposal.
2. Use `[4] -> [5] Export proposal to file`.
3. Use `[4] -> [6] Import & work with proposal file`.
4. If the imported proposal is `SwitchGuardian`, continue through offline signing and execution.
5. If the imported proposal is not `SwitchGuardian`, expect sign or execute to reject the unsupported offline path explicitly.

Expect:

- proposal ID, account ID, and signature counts survive serialization
- malformed or mismatched proposals fail clearly during import or execute
- offline execute only proceeds when signatures are complete and the proposal type supports it

## `state-verification`

Use when:

- `verify_state_commitment` changes
- commitment comparison changes
- sync-after-execute behavior changes

Steps:

1. Load or sync an account.
2. Select `v`.
3. Compare the local and on-chain commitments.
4. If a proposal was just executed, run this after sync.

Expect:

- `State commitment verified`
- account ID, local commitment, and on-chain commitment are printed
- mismatches fail loudly

## `ecdsa-pass`

Use when:

- builder key-manager logic changes
- commitment hex handling changes
- signature encoding changes
- signature-scheme selection changes

Steps:

1. Restart the demo.
2. Choose signature scheme `[2] ECDSA`.
3. Run `account-bootstrap`.
4. Run the smallest additional workflow that matches the change:
   - `cosigner-sync`
   - `online-proposal-lifecycle`
   - `offline-switch-guardian`

Expect:

- startup prints `ECDSA` as the active scheme
- the commitment display remains usable for account setup
- no Falcon-only assumptions break the flow
- signing and execution semantics match the tested Falcon workflow

## Result Template

- Commands:
- Endpoints:
- Signature scheme:
- Workflows:
- Outcomes:
- Errors:
- Skips:
