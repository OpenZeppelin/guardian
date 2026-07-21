# Quickstart: Executing the Miden v0.16 Migration

**Feature**: `329-upgrade-miden-016` | **Date**: 2026-07-20

Condensed execution walkthrough for the Rust-first slice. Order matters —
it follows bottom-up propagation (constitution Principle I).

## 0. Preconditions

- Re-check upstream before pinning (alphas move every few days):
  ```bash
  curl -s https://crates.io/api/v1/crates/miden-node-proto/versions | jq -r '.versions[].num' | head
  curl -s https://crates.io/api/v1/crates/miden-client/versions | jq -r '.versions[].num' | head
  ```
  If a `miden-node-proto` alpha aligned with protocol `alpha.4` (or newer
  matched set) exists, use it; otherwise apply research R2's fallback
  (`miden-node-proto-build` bindings in `crates/miden-rpc-client`).
- Local 0.16 Miden node available for e2e (devnet works but alphas skew;
  localhost proves locally and avoids remote-prover version skew).

## 1. Pins and toolchain

```bash
# rust-toolchain.toml: channel = "1.96.1"
# Cargo.toml L68-74: exact pins per data-model.md Entity 1
cargo update
cargo tree -i miden-protocol   # must resolve to exactly one 0.16 version
cargo tree | grep -E '^.*p3-' | sort -u   # consistent 0.6 set, no 0.5.x stragglers
```

Expect the workspace to not compile yet — that's the migration surface.

## 2. Bottom-up source migration

1. `crates/contracts`: re-port 8 MASM files (`@account_procedure`,
   `@transaction_script`, `basic_wallet::create_note`, relocated asset
   procedures), adapt `masm_builder.rs` to assembler 0.25 / project model.
2. `crates/shared`, `crates/miden-keystore`, `crates/client`: type renames,
   `PublicKeyCommitment`, auth helpers.
3. `crates/miden-rpc-client`: node-proto resolution (R2); `validator_keys`,
   `TransactionHeader.fee` removal.
4. `crates/miden-multisig-client`: delta→patch
   (`TransactionResult::account_patch`), `Approver`/`ApproverSet`, wallet
   factory split, sync proving call sites, `AssetAmount`, P2ID ≥1 asset.
5. `crates/server`: `delta_summary/{projection,build}.rs` and
   canonicalization verify path onto `AccountPatch`; auth metadata handlers
   (`AuthRequest` reshape); check projection for dropped fee field.
6. `examples/demo`, `examples/rust`, benches: follow SDK; delete debug-mode
   usage.

## 3. Atomic artifact regeneration (research R4)

```bash
cargo run --example procedure_roots -p miden-multisig-client -- --json
# → update crates/miden-multisig-client/src/procedures.rs
# → mirror into packages/miden-multisig-client/src/procedures.ts
rsync -a crates/contracts/masm/ packages/miden-multisig-client/masm/
# → update kernel commitment constant in src/transaction/mod.rs
# → regenerate fixtures/miden-multisig-client/p2id-serial-vectors.json
# → regenerate crates/server/src/testing/fixtures/*.json
```

## 4. Validation gates

```bash
cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace                      # includes both guard tests + contracts suite
cargo test -p guardian-server --features e2e   # lifecycle/canonicalization e2e
cargo run --features evm --bin gen-openapi -- --check docs   # contract-shape invariance
```

Manual smoke (skills: `smoke-test-rust-multisig-sdk`): run `examples/demo`
against the local 0.16 node — create account, register, cosigner sync,
propose/sign/execute, offline export/import, verify state commitment,
switch-guardian. Then repeat the happy path against devnet (watch for
remote-prover skew; issue #3113 class).

**Reset note**: wipe stale local state first (`~/.guardian` metadata and
miden-client SQLite stores are 0.15-format and will fail confusingly).

## 5. Docs

Update per CONTRIBUTING.md docs table: QUICKSTART, LOCAL_DEV, CONFIGURATION
(if env surface moved), TROUBLESHOOTING (version-mismatch symptom, store
reset), MULTISIG_SDK, guides' compose/.env artifacts, README divergence note
(TS package remains on 0.15 until upstream ships 0.16 — no releases until
parity).

## 6. Deferred TS slice — re-entry trigger

Watch `0xMiden/web-sdk` PR #225 / npm:
```bash
npm view @miden-sdk/miden-sdk versions --json | tail -5
```
When any 0.16.x appears: bump `packages/miden-multisig-client` +
`examples/{web,smoke-web,_shared/multisig-browser,operator-smoke-web}`,
absorb `accountPatch()` / component-naming / non-empty-P2ID changes, rebuild
dist, re-run vitest + Playwright cross-SDK determinism gate + browser smokes
(`smoke-test-ts-multisig-sdk`), then release both SDKs together.
