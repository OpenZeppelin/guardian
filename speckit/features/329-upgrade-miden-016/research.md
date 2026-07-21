# Research: Upgrade Guardian to the Miden v0.16 Package Line

**Feature**: `329-upgrade-miden-016` | **Date**: 2026-07-20
**Sources**: upstream changelogs at tags (`0xMiden/miden-base` v0.16.0-alpha.4, `0xMiden/miden-client` v0.16.0-alpha.1, `0xMiden/miden-node` v0.16.0-alpha.2), crates.io version/dependency API, `0xMiden/web-sdk` (PR #225), Guardian repo survey (Cargo pins, Cargo.lock, git history of PR #287), status.devnet.miden.io.

## Headline finding

**This is a redesign-sized migration, not a dependency bump.** Miden 0.16 removes
`AccountDelta` — the relative account-update model Guardian's entire
state/delta/commitment pipeline is built on — and replaces it with absolute
`AccountPatch` types with a different commitment layout. Everything else
(MASM attribute system, auth type reshaping, sync proving, store format resets)
is the familiar 0.15-style churn; the delta→patch change is the item that makes
this migration architecturally larger than 0.14→0.15 (PR #287: 131 files).

## R1: Target version pin set

**Decision**: Pin the Rust workspace to the version set that `miden-client
0.16.0-alpha.1` itself pins, with exact `=` pins for every pre-release:

| Workspace dep | Target pin |
|---|---|
| miden-protocol / miden-standards / miden-tx | `=0.16.0-alpha.4` |
| miden-testing | `=0.16.0-alpha.2` (latest published; matches client's pin) |
| miden-client / miden-client-sqlite-store | `=0.16.0-alpha.1` |
| miden-node-proto | see R2 — cannot be pinned to any 0.16 alpha today |
| Rust toolchain | `1.96.1` (upstream MSRV; Guardian currently pins 1.93.0 in rust-toolchain.toml) |

**Rationale**: miden-client alpha.1's own Cargo.toml pins protocol-line alpha.4
and testing alpha.2 — mirroring it is the only combination guaranteed to
resolve to a single 0.16 protocol version. Exact pins implement spec FR-005
(no caret drift during alpha churn). Toolchain bump is forced by upstream
(MSRV 1.96.1, edition 2024 — Guardian is already on edition 2024).

**Alternatives considered**: pinning protocol alpha.2 across the board (aligns
with node-proto but abandons the published miden-client, which requires
alpha.4 — rejected); tracking upstream `next` branches by git pin (how the
0.15 migration started; rejected while published alphas exist, keeps
lockfile reproducible).

## R2: The miden-node-proto version knot

**Finding**: `miden-node-proto 0.16.0-alpha.2` exact-pins
`miden-protocol =0.16.0-alpha.2`, while `miden-client 0.16.0-alpha.1` requires
protocol `0.16.0-alpha.4`. Cargo can resolve only one 0.16.x `miden-protocol`,
so **the two crates cannot coexist in one lockfile today**. Guardian is
directly exposed: `crates/miden-rpc-client/src/lib.rs:8` re-exports typed
`miden_node_proto::generated::{account, blockchain, note, primitives, rpc, transaction}`.

**Decision**: At implementation start, check for a newer `miden-node-proto`
alpha aligned with protocol alpha.4 (node lags protocol by ~2 alphas; alphas
ship every few days). If still misaligned, switch `crates/miden-rpc-client`
from the typed `miden-node-proto` crate to `miden-node-proto-build`-generated
bindings — the exact strategy miden-client uses to avoid this same conflict
(`miden-node-proto-build` has no miden-protocol dependency).

**T001 outcome (2026-07-20)**: re-checked crates.io — `miden-node-proto`
latest is still 0.16.0-alpha.2; the knot stands. Taking the fallback:
`crates/miden-rpc-client` generates its own bindings from
`miden-node-proto-build =0.16.0-alpha.2` via build.rs, and the workspace
`miden-node-proto` dependency is removed.

**Rationale**: waiting-with-fallback avoids committing to a bindings
restructure that a routine upstream alpha may make unnecessary, while the
fallback is proven upstream. **Alternatives considered**: forking node-proto
with relaxed pins (maintenance burden, rejected); holding the whole migration
until node alphas realign (blocks all other work on the critical path,
rejected).

## R3: AccountDelta → AccountPatch migration strategy

**Finding** (miden-base #3010/#3071/#3089/#3109/#3110/#3038/#3123/#3142/#3144):
`Account::apply_delta`, `AccountDelta::merge`, and `AccountStorageDelta` are
gone. Replacements: absolute `AccountPatch` / `AccountStoragePatch` /
`AccountVaultPatch`, `AccountPatch::merge`, `Account::apply_patch`,
`Account::try_from(&AccountPatch)`. Storage patches carry per-slot
Create/Update/Remove ops **included in the patch commitment**; fungible and
non-fungible vault deltas are unified, changing the on-chain account-delta
commitment layout. `miden-client`'s `TransactionResult::account_delta` becomes
`account_patch`.

**Decision**: Migrate Guardian's internal pipeline (server delta-summary
projection, canonicalization verify path, multisig SDK delta extraction and
apply) to the patch model while **preserving Guardian's own concepts
unchanged**: Guardian's "delta" records stay append-only with explicit
pending→candidate→canonical/discarded lifecycle and `prev_commitment`/nonce
lineage; what changes is the opaque Miden-encoded payload inside those records
and the commitment values Guardian computes/verifies. Guardian's wire contract
shape (proto/HTTP) is expected to be unchanged; this is verified during
implementation and any shape change triggers the full contract-change workflow
(AGENTS.md §4).

**Rationale**: Guardian's append-only lifecycle is a constitution invariant
(Principle III) and is conceptually independent of whether the embedded
account-update payload is relative (delta) or absolute (patch).
**Alternatives considered**: translating patches back into a Guardian-local
relative-delta representation (re-implements deleted upstream code against a
commitment layout that no longer matches — rejected as drift-prone).

## R4: MASM contract re-port

**Finding**: 0.16 introduces mandatory `@account_procedure` /
`@transaction_script` attributes, `miden-project.toml` library packaging,
note creation restricted to `basic_wallet::create_note`, relocated asset
procedures (`protocol::asset::*` → `miden::standards::assets::*`), and
upstream optimizations to the multisig MASM (`signature.masm`,
`update_signers_and_threshold`, `get_signer_at`) — all of which shift
procedure MAST roots.

**Decision**: Re-port all 8 MASM files in `crates/contracts/masm/` to the 0.16
attribute/procedure model, then regenerate in one atomic step: procedure roots
(`crates/miden-multisig-client/src/procedures.rs:26-45` via the
`procedure_roots` example, mirrored into
`packages/miden-multisig-client/src/procedures.ts:14-20`), the vendored MASM
copy in `packages/miden-multisig-client/masm/`, the kernel commitment constant
(`crates/miden-multisig-client/src/transaction/mod.rs:110`), P2ID serial
vectors (`fixtures/miden-multisig-client/p2id-serial-vectors.json`), and server
testing fixtures. Existing guard tests (`procedure_roots_match_compiled_account`,
`transaction_kernel_commitment_matches_network`, contracts behavior suite)
validate the regeneration.

**Rationale**: partial regeneration was the dominant failure mode of the 0.15
cycle (stale roots in procedures.rs, stale TS dist MASM). One atomic
regeneration step, gated by the existing tests, prevents recurrence.

## R5: Auth and signer surface changes

**Finding**: `AuthMethod`/`AccountAuthComponent`/`AccountAuthScheme` removed;
new `Approver`/`ApproverSet` types wrap `(PublicKeyCommitment, AuthScheme)`
and `(threshold, approvers)`; `AuthRequest.pub_key_hash: Word` renamed to
`pub_key_commitment: PublicKeyCommitment` and the event now carries signature
*or* tx summary (not both); wallet factory split
(`create_basic_wallet`/`create_multisig_wallet`/`create_guarded_wallet`);
proving changed from async to sync (#3281). Falcon/Word commitment encoding
itself is not reported changed.

**Decision**: Treat this as a high-risk auth-layer change under Constitution
Principle IV: adapt Guardian's Falcon/ECDSA metadata handlers
(`crates/server/src/metadata/auth/*`), the multisig builders, and signer
plumbing to the new types; update tests in each changed layer plus at least
one upstream consumer (SDK → demo). The `guardian-auth-signature-flows` skill
drives this slice during implementation.

## R6: Persisted state and environment resets

**Finding**: 0.16 devnet is structurally a fresh chain (new BlockHeader
multi-validator format, new account-update commitment layout, node RocksDB
layout change) — 0.15 accounts/notes are gone. miden-client 0.16 also breaks
its own store format (account IDs as BLOB #2309, `ConsumedExternal`-only note
layout #2313): **existing local client stores must be recreated**.

**Decision** (implements spec FR-006): no compatibility shims. Document a
clean-slate migration: local miden-client stores (SQLite and IndexedDB) are
recreated; demo/test accounts recreated; Guardian server-side records for
0.15-era accounts remain readable as historical data but are not
interoperable with 0.16 networks — documented in release notes and
TROUBLESHOOTING. This matches project policy (AGENTS.md §3 rule 6) and the
0.15 precedent.

## R7: TypeScript SDK sequencing (implements spec FR-008)

**Finding**: the TS/WASM SDK moved to `0xMiden/web-sdk`; npm has no 0.16
(latest 0.15.7). The 0.16 migration is in flight as web-sdk PR #225 (targets
`next`): `accountDelta()` → `accountPatch()`, MASM attribute requirements,
exact component naming, non-empty P2ID assets, `debugMode` no-op. First
`0.16.0-alpha` npm release expected after it merges.

**Decision**: Rust-first (user-decided 2026-07-20). This plan's implementation
scope is the Rust workspace, MASM, docs, and Rust-side examples. The TS package
(`packages/miden-multisig-client`) and browser examples migrate in a follow-up
slice when `@miden-sdk/miden-sdk` publishes any 0.16 version; web-sdk PR #225
is the watchlist item and preview of the TS-facing breaks. Until then the
Rust/TS divergence is explicit and documented, and no Guardian packages are
released. Cross-SDK determinism gates re-run when the TS side lands.

## R8: Transitive proving-stack hygiene

**Finding**: the 0.16 line uses miden-crypto 0.28 with **Plonky3 `p3-*` 0.6**
(0.15 used 0.5.3) and zero winterfell deps; miden-vm is 0.25. The known
failure mode — stale `p3-*` lockfile pins producing a wrong tx-kernel
commitment ("value for key … not present in advice map") — now applies to the
0.6 line. `miden-remote-prover-client` no longer exists as a standalone crate
(miden-client vendors its own remote-prover bindings), so Guardian's
transitive 0.15.0 copy disappears naturally.

**Decision**: After the pin bump, verify Cargo.lock resolves a single
consistent `p3-*` 0.6.x set and rely on
`transaction_kernel_commitment_matches_network` as the regression gate
(regenerated per R4). Remote-prover version skew against devnet (issue #3113
class) is re-checked during devnet validation; local-node validation is the
primary path while alphas churn.

## R9: Miden-client behavioral deltas absorbed in passing

- Debug-mode API removed (`DebugMode`, `in_debug_mode`, `MIDEN_DEBUG`) — any Guardian/demo usage is deleted.
- Fungible amounts move `u64` → typed `AssetAmount` in balances and builders.
- P2ID/P2IDE notes must carry ≥1 asset (typestate builders).
- RPC-response hardening (client rejects unrequested notes/nullifiers, binds `get_account` to sync target block) — may change error surfaces Guardian's SDK relies on; watch the known private-account `get_account_details` behavior.
- Client/node genesis-commitment header check remains the version-mismatch guard (satisfies spec FR-009: mismatches fail at the RPC boundary with a recognizable error; Guardian must surface it actionably).
- Fees: tx kernel no longer auto-computes fees; `TransactionHeader.fee` dropped from the node proto — check delta-summary projection for any fee field usage.

## As-built notes (implementation, 2026-07-20/21)

- **R2 fallback taken**: `crates/miden-rpc-client` generates bindings from
  `miden-node-proto-build =0.16.0-alpha.2` via build.rs (same generated module
  names; zero call-site changes needed).
- **rand line forced to 0.10** by miden-crypto 0.28 (`RngCore` root export
  gone → `Rng`); three member crates had non-workspace rand pins.
- **AccountDelta survives in 0.16** as the relative format inside
  `TransactionSummary`; what was removed is `Account::apply_delta` and
  `AccountDelta::merge`. Replacements built:
  `guardian_shared::account_delta::apply_account_delta` (vault assets +
  public storage mutators + nonce increment; used by server, SDK, contracts
  test, fixture generator) and a server-local `merge_account_deltas`.
- **All `begin…end` transaction scripts** (tests, SDK builders, examples,
  e2e) required conversion to `@transaction_script` + `pub proc main`.
- **Accounts gained an 8th procedure**: BasicWallet 0.16 exports
  `create_note` (and renames `send_asset` → `move_asset_to_note`; Guardian
  keeps its `SendAsset` vocabulary). `ProcedureName::CreateNote` added.
- **Regenerated values**: kernel commitment
  `0x60e15da40818dc87d8a04daee51e98ff4d6af6b2a24819a56abacefc09adb730`;
  determinism vector account `0x249e2f760a7ace015f223ed697269e` /
  commitment `0xc774f5…10a3b5`; procedure roots in procedures.rs use the
  canonical (TS-format) hex that `Word::parse` expects. The
  p2id-serial-vectors fixture was verified UNCHANGED (RandomCoin stable).
- **Devnet validated live**: full lifecycle (create → configure → pull →
  summarize → push → signed execute) green against public devnet;
  0.15-testnet probe fails recognizably at the RPC boundary (FR-009).

## Effort model (from the 0.14→0.15 migration, PR #287)

131 files changed. Concentrations: `crates/miden-multisig-client` (builders,
helpers, execution, proposals), `crates/server` (network bridge, auth
metadata, canonicalization, delta summary), `crates/contracts`
(tests/auth/multisig.rs alone was 525 lines), MASM sources, then a mechanical
regeneration wave (roots, vectors, fixtures, lockfiles). 0.16 adds the
delta→patch pipeline rework and MASM attribute migration on top, and defers
the TS wave (~50k lines of package-lock churn in #287) to the follow-up slice.
Expect **larger than #287** on the Rust side.
