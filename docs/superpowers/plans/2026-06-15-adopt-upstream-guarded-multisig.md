# Adopt Upstream `miden-standards` Guarded-Multisig — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace Guardian's forked multisig/guardian MASM contracts with the upstream `miden-standards` `AuthGuardedMultisig` component, adopting upstream's guardian-rotation model (cold-key/multisig rotates the guardian — matching `docs/CONCEPTS.md`), while preserving rogue-signer protection via a per-procedure threshold on the rotation procedure.

**Architecture:** Rust consumes the compiled `miden_standards::account::auth::AuthGuardedMultisig` component directly. The web SDK 0.15.0 exposes **no** guarded-multisig component, so the TypeScript SDK vendors the **upstream standards MASM source** (replacing Guardian's forked MASM strings) and compiles it via `AccountComponent.compile`, pinned to the **same exact `miden-standards` version** as Rust. Account identity (code commitment + storage layout) changes — acceptable because this lands in the Miden 0.15 v1 cutover window where all pre-0.15 accounts are already being recreated.

**Tech Stack:** Rust (`miden-standards` 0.15.x, `miden-protocol` 0.15.3, `miden-client` 0.15.0), TypeScript (`@miden-sdk/miden-sdk` ^0.15.0), MASM, Diesel/Postgres, vitest, cargo test.

---

## ⚠️ Pre-flight: this is a custody-logic change
The multisig/guardian contract defines account identity **and** authorization. This plan changes both. Two non-negotiables:
1. **Security re-audit** of the resulting custody path is mandatory before any production use (Task 18).
2. **Version-lock discipline**: `miden-standards` is exact-pinned, and the deterministic-account + procedure-root parity tests must gate every future bump (Task 14, Task 17).

## Phase 0 — Decisions to lock before coding

- [ ] **D1 — Rotation model:** upstream (cold-key/multisig rotates guardian; no current-guardian co-signature). **Confirmed by product owner.** Matches `CONCEPTS.md`.
- [ ] **D2 — Guardian-rotation threshold: DECIDED → default threshold (no override).** Guardian rotation (`update_guardian_public_key`, sole op) uses the account's **default multisig threshold** and no guardian signature — i.e. a 2-of-3 needs 2 user sigs to rotate. **Implication (accepted):** any normal user quorum can replace the guardian; the guardian protects only against *sub-threshold* attackers, not a compromised/colluding threshold quorum. This matches `CONCEPTS.md` (user keys sovereign, "rotate away at any time"). Task 4 therefore adds **no** per-proc override on the rotation procedure.
- [ ] **D3 — Selector:** drop `guardian_enabled`/the selector entirely (not a surfaced feature; always-on guardian). Confirm no roadmap need for "multisig-only" accounts.
- [ ] **D4 — Sequencing:** land on a dedicated branch off `miden-v0-15-upgrade`, after 0.15.0 alignment (done), and **before any v1 production accounts exist**.

---

## File Structure

**Rust — remove:**
- `crates/contracts/masm/auth/{multisig,multisig_ecdsa,guardian,guardian_ecdsa}.masm`
- `crates/contracts/masm/account_components/auth/{multisig,multisig_ecdsa,multisig_guardian,multisig_guardian_ecdsa}.masm`
- The auth-component build paths in `crates/contracts/src/masm_builder.rs`

**Rust — modify:**
- `crates/contracts/src/multisig_guardian.rs` — `MultisigGuardianConfig`/`MultisigGuardianBuilder` wrap upstream `AuthGuardedMultisigConfig`
- `crates/miden-multisig-client/src/procedures.rs` — regenerate procedure roots
- `crates/miden-multisig-client/examples/procedure_roots.rs` — point at upstream proc roots
- `crates/server/src/network/miden/account_inspector.rs`, `crates/miden-multisig-client/src/account.rs`, `crates/server/src/network/miden/mod.rs` — storage slot-name constants
- root `Cargo.toml` — exact-pin `miden-standards`

**TS — modify:**
- `packages/miden-multisig-client/src/account/masm/auth.ts` + `account-components/auth.ts` — replace forked MASM with upstream standards MASM
- `packages/miden-multisig-client/src/account/builder.ts`, `account/storage.ts` — upstream storage layout, drop `guardianEnabled`
- `packages/miden-multisig-client/src/procedures.ts` — regenerate roots
- `packages/miden-multisig-client/src/inspector.ts`, `src/types.ts` — slot names, drop `guardianEnabled`

**Fixtures:**
- `fixtures/miden-multisig-client/*.json` — regenerate deterministic-account, procedure-roots, p2id-serial from new contract

---

## Phase 1 — Rust contracts + SDK

### Task 1: Pin miden-standards exactly + spike the upstream API

**Files:**
- Modify: `Cargo.toml` (workspace `[workspace.dependencies]`)

- [ ] **Step 1: Exact-pin miden-standards.** Change `miden-standards = "0.15.3"` to `miden-standards = "=0.15.3"` (exact pin — any future bump becomes a deliberate, audited event).

- [ ] **Step 2: Verify the upstream API surface compiles.** Add a throwaway test in `crates/contracts/src/multisig_guardian.rs` under `#[cfg(test)]`:

```rust
#[test]
fn upstream_guarded_multisig_api_spike() {
    use miden_protocol::account::auth::{AuthScheme, PublicKeyCommitment};
    use miden_standards::account::auth::{
        AuthGuardedMultisig, AuthGuardedMultisigConfig, GuardianConfig,
    };
    use miden_protocol::account::AccountComponent;

    let signer = PublicKeyCommitment::from(miden_protocol::Word::from([1u32, 2, 3, 4]));
    let guardian = PublicKeyCommitment::from(miden_protocol::Word::from([5u32, 6, 7, 8]));
    let cfg = AuthGuardedMultisigConfig::new(
        vec![(signer, AuthScheme::Falcon512Poseidon2)],
        1u32,
        GuardianConfig::new(guardian, AuthScheme::Falcon512Poseidon2),
    )
    .unwrap();
    let component = AuthGuardedMultisig::new(cfg).unwrap();
    let _: AccountComponent = component.into();
}
```

- [ ] **Step 3: Run the spike.** Run: `cargo test -p miden-confidential-contracts upstream_guarded_multisig_api_spike -- --nocapture`. Expected: PASS. If signatures differ, read `~/.cargo/registry/src/*/miden-standards-0.15.3/src/account/auth/guarded_multisig.rs` and correct the call before proceeding.

- [ ] **Step 4: Delete the spike test, commit.**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: exact-pin miden-standards; verify upstream guarded-multisig API"
```

### Task 2: Rewrite `MultisigGuardianBuilder`/`Config` over upstream

**Files:**
- Modify: `crates/contracts/src/multisig_guardian.rs`

- [ ] **Step 1: Update `MultisigGuardianConfig`.** Drop `guardian_enabled`. Keep `threshold: u32`, `signer_commitments: Vec<Word>`, `guardian_commitment: Word`, `signature_scheme: SignatureScheme`, `account_type: AccountType`, `proc_threshold_overrides: Vec<(Word, u32)>`. Remove `with_guardian_enabled`.

- [ ] **Step 2: Implement `build()` over upstream.** Map `signature_scheme` → `AuthScheme` (Falcon→`Falcon512Poseidon2`, Ecdsa→`EcdsaK256Keccak`); build `approvers: Vec<(PublicKeyCommitment, AuthScheme)>` from `signer_commitments`; build `GuardianConfig::new(guardian_commitment.into(), scheme)`; assemble `AuthGuardedMultisigConfig::new(approvers, threshold, guardian_config)?`; apply `.with_proc_thresholds(...)?` **only** for caller-supplied `proc_threshold_overrides` — per **D2, add NO override for `update_guardian_public_key`** (rotation uses the default threshold); `AuthGuardedMultisig::new(cfg)?` → `AccountComponent::from(...)`; then `AccountBuilder::new(seed).with_auth_component(...).with_component(BasicWallet).account_type(...).build()`.

- [ ] **Step 3: Update the deterministic-account test.** The pinned `id`/`commitment` in `test_browser_deterministic_account_matches_rust_builder` WILL change. Run the test to capture the new values:

Run: `cargo test -p miden-confidential-contracts test_browser_deterministic_account_matches_rust_builder -- --nocapture`
Expected: FAIL showing the new id/commitment. Record both; update the test's expected constants to the new values (these become the cross-SDK parity anchor — Task 14 confirms TS matches).

- [ ] **Step 4: Run contracts tests.** Run: `cargo test -p miden-confidential-contracts`. Expected: PASS.

- [ ] **Step 5: Commit.** `git commit -am "feat(contracts): build guarded-multisig from upstream miden-standards"`

### Task 3: Delete the forked MASM + masm_builder auth paths

**Files:**
- Delete: the 8 forked auth `.masm` files (see File Structure)
- Modify: `crates/contracts/src/masm_builder.rs` (remove `build_multisig_*`/`build_guardian_*` component fns and the auth-library assembly; keep any tx-script library helpers still used by the client, e.g. `get_multisig_library`/`get_guardian_library` — see Task 4)

- [ ] **Step 1: Identify live consumers of the tx-script libraries.** Run: `grep -rn "get_multisig_library\|get_guardian_library\|build_multisig\|build_guardian" crates --include=*.rs`. Anything under `crates/miden-multisig-client/src/transaction/` that builds `call.multisig::*`/`call.guardian::*` scripts must be repointed at the upstream library path (`miden::standards::auth::*`) in Task 4 before deletion.

- [ ] **Step 2: Remove the auth-component builders + forked MASM** once Task 4 has repointed the tx-scripts. Delete the 8 `.masm` files and the corresponding `build_*_component` functions.

- [ ] **Step 3: Build.** Run: `cargo check -p miden-confidential-contracts -p miden-multisig-client`. Expected: PASS (after Task 4). If it fails on the tx-script libraries, complete Task 4 first.

- [ ] **Step 4: Commit.** `git commit -am "refactor(contracts): remove forked auth MASM; use upstream components"`

### Task 4: Repoint tx-scripts (update_signers / update_guardian / threshold) at upstream

**Files:**
- Modify: `crates/miden-multisig-client/src/transaction/configuration/config.rs`, `crates/miden-multisig-client/src/transaction/guardian.rs`

- [ ] **Step 1: Map the script-call paths.** Guardian's tx-scripts `call` into `oz_multisig::multisig::update_signers_and_threshold` / `oz_guardian::guardian::update_guardian_public_key` / `multisig::update_procedure_threshold`. Upstream exposes these as `update_signers_and_threshold`, `update_guardian_public_key`, `set_procedure_threshold` in `miden::standards::auth::{multisig,guardian}` (verify exact proc names against `~/.cargo/registry/src/*/miden-standards-0.15.3/asm/standards/auth/{multisig,guardian}.masm`).

- [ ] **Step 2: Link the upstream library in the CodeBuilder.** Replace the forked-library linking with the upstream standards library so `compile_tx_script` resolves `miden::standards::auth::*`. (Mirror the existing `CodeBuilder::with_dynamically_linked_library` pattern; the standards lib is already a dep.)

- [ ] **Step 3: Confirm advice-map encoding unchanged.** The `build_multisig_config_advice` config-hash/key encoding (`config.rs`) must match what upstream `update_signers_and_threshold` expects (`adv.push_mapval(config_hash)` over `[CONFIG, PUB_KEYS...]`). Diff upstream `multisig.masm:update_signers_and_threshold` inputs against the current encoding; adjust if the layout differs.

- [ ] **Step 4: Build + run client lib tests.** Run: `cargo test -p miden-multisig-client --lib`. Expected: PASS.

- [ ] **Step 5: Commit.** `git commit -am "refactor(client): repoint multisig/guardian tx-scripts at upstream standards"`

### Task 5: Regenerate Rust procedure roots

**Files:**
- Modify: `crates/miden-multisig-client/src/procedures.rs`

- [ ] **Step 1: Regenerate.** Run: `cargo run --example procedure_roots -p miden-multisig-client -- --json`. Capture the new roots (they change because the contract changed). Note upstream uses `update_guardian_public_key` (vs the fork's `update_guardian`/`verify_guardian`); update `ProcedureName` variants/roots accordingly.

- [ ] **Step 2: Update `procedures.rs`** with the new roots (both `rust_hex` and `typescript_hex` encodings).

- [ ] **Step 3: Run the procedures test.** Run: `cargo test -p miden-multisig-client procedures`. Expected: PASS.

- [ ] **Step 4: Commit.** `git commit -am "chore(client): regenerate procedure roots for upstream contract"`

---

## Phase 2 — Server

### Task 6: Update server storage slot-name constants

**Files:**
- Modify: `crates/server/src/network/miden/account_inspector.rs`, `crates/server/src/network/miden/mod.rs`, `crates/miden-multisig-client/src/account.rs`

- [ ] **Step 1: Replace slot-name constants.** `openzeppelin::multisig::*` → `miden::standards::auth::multisig::*`; `openzeppelin::guardian::public_key` → `miden::standards::auth::guardian::pub_key`. Remove `openzeppelin::guardian::selector` and all selector reads (D3). Map old→new from upstream `multisig.rs`/`guardian.rs` slot consts.

- [ ] **Step 2: Fix the replay-protection write** in `network/miden/mod.rs:~205` — `EXECUTED_TXS_SLOT_NAME` → `miden::standards::auth::multisig::executed_transactions`.

- [ ] **Step 3: Update `guardian_enabled()` accessor** (`account.rs`): upstream has no selector → either remove the accessor or hardcode `true` (the guardian is always present). Update the TS `inspector.ts` equivalently in Task 11.

- [ ] **Step 4: Build server (all features).** Run: `cargo check -p guardian-server --all-targets --features postgres,evm`. Expected: PASS.

- [ ] **Step 5: Commit.** `git commit -am "refactor(server): use upstream standards storage slot names; drop guardian selector"`

### Task 7: Server test suite

- [ ] **Step 1: Run.** Run: `cargo test -p guardian-server`. Expected: PASS. Fix any fixture/snapshot that referenced the old slot names or the selector.
- [ ] **Step 2: Commit** any fixture updates. `git commit -am "test(server): align fixtures with upstream slot names"`

---

## Phase 3 — TypeScript SDK (vendors upstream MASM)

### Task 8: Vendor the upstream guarded-multisig MASM source

**Files:**
- Modify: `packages/miden-multisig-client/src/account/masm/auth.ts`, `packages/miden-multisig-client/src/account/masm/account-components/auth.ts`

- [ ] **Step 1: Extract the upstream MASM source.** From `~/.cargo/registry/src/*/miden-standards-0.15.3/asm/standards/auth/{multisig,guardian,signature,mod}.masm` and `asm/account_components/auth/guarded_multisig.masm`. Add a generator script (mirror the Rust `procedure_roots` example pattern) that emits these as TS string constants, so the embed is reproducible from the pinned version rather than hand-copied. Document the exact source commit/version in a header comment.

- [ ] **Step 2: Replace the forked constants** (`MULTISIG_MASM`, `GUARDIAN_MASM`, the `*_ECDSA` and account-component variants) with the upstream sources + the library paths (`miden::standards::auth::*`).

- [ ] **Step 3: Typecheck.** Run: `npm run typecheck --prefix packages/miden-multisig-client`. Expected: PASS (string constants only — no semantic check yet; Task 14 validates derivation).

- [ ] **Step 4: Commit.** `git commit -am "feat(ts): vendor upstream standards guarded-multisig MASM"`

### Task 9: Update TS account builder for upstream layout

**Files:**
- Modify: `packages/miden-multisig-client/src/account/builder.ts`, `packages/miden-multisig-client/src/account/storage.ts`

- [ ] **Step 1: Update storage-slot construction** (`storage.ts`) to the upstream layout/order (multisig: threshold_config, approver_public_keys, approver_schemes, executed_transactions, procedure_thresholds; guardian: pub_key, scheme). Drop the selector slot. Per **D2**, add **no** rotation override to the procedure_thresholds map (rotation uses the default threshold); include only caller-supplied overrides.

- [ ] **Step 2: Update `builder.ts`** to compile/link the upstream MASM (Task 8 constants) via `createCodeBuilder`/`compileAccountComponentCode`/`AccountComponent.compile`, with the upstream library paths. Drop `guardianEnabled` handling.

- [ ] **Step 3: Update the SDK mock in `builder.test.ts`** to match.

- [ ] **Step 4: Typecheck + unit tests.** Run: `npm test --prefix packages/miden-multisig-client`. Expected: PASS except the parity tests (Task 14).

- [ ] **Step 5: Commit.** `git commit -am "feat(ts): build guarded-multisig from upstream MASM + layout"`

### Task 10: Regenerate TS procedure roots

**Files:**
- Modify: `packages/miden-multisig-client/src/procedures.ts`

- [ ] **Step 1: Sync roots** to the Rust-regenerated values (Task 5) — they must be byte-identical (the parity guard).
- [ ] **Step 2: Run the procedures parity test.** Run: `npm test --prefix packages/miden-multisig-client -- tests/procedure-roots.test.ts`. Expected: PASS.
- [ ] **Step 3: Commit.** `git commit -am "chore(ts): regenerate procedure roots for upstream contract"`

### Task 11: Update TS inspector + types

**Files:**
- Modify: `packages/miden-multisig-client/src/inspector.ts`, `src/types.ts`, `src/client.ts`

- [ ] **Step 1:** Replace slot-name constants with `miden::standards::auth::*`. Remove `guardianEnabled` from types/inspector/client (or hardcode `true`), consistent with Task 6.
- [ ] **Step 2: Typecheck + tests.** Run: `npm test --prefix packages/miden-multisig-client`. Expected: PASS (minus parity, Task 14).
- [ ] **Step 3: Commit.** `git commit -am "refactor(ts): upstream slot names; drop guardianEnabled"`

---

## Phase 4 — Cross-SDK parity + fixtures

### Task 12: Regenerate deterministic-account fixture

**Files:**
- Modify: `fixtures/miden-multisig-client/*.json` (whichever holds the deterministic-account vectors), Rust + TS parity tests

- [ ] **Step 1:** Capture the new deterministic account id + commitment from Rust (Task 2 Step 3). Update the shared fixture.
- [ ] **Step 2: Run Rust + TS deterministic-account tests** against the regenerated fixture. Expected: PASS on both.
- [ ] **Step 3: Commit.** `git commit -am "test: regenerate deterministic-account parity fixture"`

### Task 13: Regenerate remaining parity fixtures (p2id-serial, etc.)

- [ ] **Step 1:** p2id-serial is protocol-derived (unchanged by the contract swap) — confirm it still passes both sides. If the account-derivation-dependent fixtures changed, regenerate from Rust and confirm TS matches.
- [ ] **Step 2: Commit** any regenerated fixtures.

### Task 14: Cross-SDK parity gate (the critical validation)

- [ ] **Step 1: Run the full parity suite.** Rust: `cargo test -p miden-confidential-contracts -p miden-multisig-client`. TS: `npm test --prefix packages/miden-multisig-client`.
- [ ] **Step 2: Confirm the deterministic-account test produces IDENTICAL id+commitment on both sides.** This proves the Rust compiled component ≡ TS embedded MASM at the pinned standards version. Expected: PASS on both. If they differ, the TS MASM embed (Task 8) is out of sync with the Rust standards version — fix before proceeding.
- [ ] **Step 3: Commit** (no-op if green) and tag this as the parity baseline.

---

## Phase 5 — Validation

### Task 15: Full workspace + SDK test run

- [ ] **Step 1:** Run: `cargo test --workspace`. Expected: PASS.
- [ ] **Step 2:** Run: `npm test --prefix packages/miden-multisig-client`. Expected: 308+/308 PASS.
- [ ] **Step 3:** Run: `cargo clippy --workspace --all-targets`. Expected: clean.

### Task 16: Runtime validation on devnet

- [ ] **Step 1:** Build the demo: `cargo build -p guardian-demo`.
- [ ] **Step 2:** Create a v1 guarded-multisig on devnet; exercise: add cosigner, remove cosigner, p2id, consume notes.
- [ ] **Step 3: Critical — validate the restored recovery:** rotate the guardian via `switch_guardian` using **only the user multisig threshold (no current-guardian signature)**. Confirm it succeeds (this is the whole point of adopting upstream). Confirm it is *blocked* below the D2 rotation threshold.
- [ ] **Step 4: Bonus — re-test the proposal-creation abort:** create an undeployed v1 guarded-multisig and attempt an `UpdateSigners`/`RemoveCosigner` proposal. Record whether the canonical upstream account construction sidesteps the nonce-0 advice-map abort (tracked separately).

### Task 17: Version-lock guardrail

- [ ] **Step 1:** Confirm `miden-standards` is exact-pinned (Task 1) and `Cargo.lock` resolves to the audited version.
- [ ] **Step 2:** Add a one-line note in `crates/contracts/src/multisig_guardian.rs` (and the TS MASM header) stating: the auth contract is sourced from `miden-standards =X.Y.Z`; bumping it changes account derivation and **requires** re-running the deterministic-account + procedure-root parity tests, re-audit, and an account migration.

### Task 18: Security re-audit (gate to production)

- [ ] **Step 1:** Commission/perform a re-audit of the guarded-multisig custody path, focused on: the rotation model (cold-key rotates guardian, gated by D2 threshold), the per-proc threshold enforcement, and the everyday auth (user-threshold + mandatory guardian). **No production use before sign-off.**

---

## Phase 6 — Docs + cutover

### Task 19: Docs

- [ ] **Step 1: Verify `docs/CONCEPTS.md` is now accurate** — the upstream rotation model matches its claims; confirm no edits needed (it should require none).
- [ ] **Step 2: Update `docs/MULTISIG_SDK.md`** for any SDK API changes (config fields dropped, e.g. `guardian_enabled`).
- [ ] **Step 3: Commit.** `git commit -am "docs: align guarded-multisig docs with upstream adoption"`

### Task 20: Cutover migration (already present)

- [ ] **Step 1:** Confirm `crates/server/migrations/2026-06-14-000001_v015_account_id_cutover/` is in place — it clears pre-0.15 accounts so the new (different-derivation) v1 accounts start clean. No new migration needed; account-ID change is absorbed by the cutover.

---

## Guardrails (carry through every task)
- **Exact-pin `miden-standards`**; deterministic-account + procedure-root parity tests gate any bump.
- **TS MASM is vendored** from the pinned standards version via a generator (Task 8) — never hand-edited.
- **Cross-SDK parity (Task 14) is a hard gate** — Rust component ≡ TS MASM.
- **Re-audit (Task 18) gates production.**

## Open items / external blockers
- **D2 (rotation threshold)** must be decided before Task 4.
- **Browser examples** (`smoke-web`, `web`, `_shared/multisig-browser`) remain blocked on `@miden-sdk/miden-para` 0.15 — out of scope here.
- **Proposal-creation abort** (nonce-0 advice-map) is tracked separately; Task 16 Step 4 tests whether this adoption incidentally fixes it.
