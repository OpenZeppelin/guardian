# Miden compatibility

Which Miden protocol line each Guardian release targets, what changed between
lines, and what each upgrade does to stored data.

> Guardian's own version and Miden's are **not** aligned. Guardian 0.16.x runs on
> Miden 0.15; Miden 0.16 arrives in Guardian 0.17.x; Miden 0.17 arrives in
> Guardian 0.18.x. Read the matrix rather than matching the numbers.

This page is the single source of truth for those facts. Procedures live
elsewhere and link here:

| For | Read |
|---|---|
| Operator upgrade steps | [`PRODUCTION.md`](./PRODUCTION.md) |
| Diagnosing a version-mismatch symptom | [`TROUBLESHOOTING.md`](./TROUBLESHOOTING.md) |
| SDK contract pinning and release policy | [`MULTISIG_SDK.md`](./MULTISIG_SDK.md#contract-version-pinning) |

## Support matrix

| Guardian | Miden protocol | `miden-protocol` / `miden-standards` | `miden-client` (Rust) | `@miden-sdk/miden-sdk` (npm) |
|---|---|---|---|---|
| 0.18.x (pre-release) | 0.17 | `=0.17.0-rc.7` | `=0.17.0-rc.3` | `0.17.0-rc.3` (exact) |
| 0.17.0 | 0.16 | `=0.16.1` | `=0.16.0` | `0.16.0` (exact) |
| 0.16.x | 0.15 | `0.15.3` | `0.15.0` | `^0.15.8` |
| 0.15.x | 0.15 | `0.15.x` | `0.15.0` | `^0.15.0` |
| 0.14.x | 0.14 | n/a | `0.14.x` | `^0.14.0` |
| 0.13.x | 0.13 | n/a | `0.13.0` | `^0.13.0` |
| 0.12.x | 0.12 | n/a | `0.12.5` | `^0.12.5` |

0.18.x tracks the Miden 0.17 release candidates. `@miden-sdk/miden-sdk` 0.17.0-rc.3
embeds `miden-client` 0.17.0-rc.3 and `miden-protocol` / `miden-standards` 0.17.0-rc.7,
which is why the Rust pins are rc.7 for the protocol crates and rc.3 for the client
crates. It is not a production target until Miden 0.17.0 is stable and devnet and
testnet run it.

0.17.0 builds on the stable Miden 0.16 release. `@miden-sdk/miden-sdk` 0.16.0 embeds
`miden-client` 0.16.0 and `miden-protocol` / `miden-standards` 0.16.1, which is why the
Rust pins are 0.16.1 for the protocol crates and 0.16.0 for the client crates.

Pins are exact on both lines, and the Rust and npm pins must move together: nothing
at build time verifies that the npm SDK's embedded `miden-standards` matches the Rust
pin, so the CI parity gates are what catch drift. See
[`MULTISIG_SDK.md`](./MULTISIG_SDK.md#contract-version-pinning).

**Upgrading from 0.17.0 (Miden 0.16) to 0.18.x (Miden 0.17)** is a protocol-line change.
Stored Miden account data is reset by the embedded migration listed below, accounts
must be recreated, and both the Rust SQLite store and the browser IndexedDB store
must be recreated: a store created under 0.16 does not open under 0.17.

**Upgrading from 0.16.x (Miden 0.15) to 0.17.0 (Miden 0.16)** is a protocol-line change:
the guarded-multisig auth component now pays the transaction fee and transaction summaries
bind the reference block, so nothing signed or stored on 0.15 verifies on 0.16. Stored Miden account data is
reset by the embedded migration listed below, accounts must be recreated, and the Rust
SDK's local `miden-client` SQLite store must be recreated (the browser IndexedDB store
migrates in place). `miden-client` 0.16.0 also raises the MSRV to 1.98.1.

**0.17.0-rc.1 to rc.3 were pre-releases on the Miden 0.16 release candidates**, published
to npm under the `rc` dist-tag. They are not supported. Every rc pinned a different
`auth_tx` procedure root than 0.17.0 (0.16.1 factored the fee payment into
`miden::standards::auth::multisig::pay_bounded_fee`), so an account created on an rc is
rejected with `UnsupportedContractVersion`, and a proposal still pending from an rc cannot
be reproduced: `TransactionRequest` serialization changed and the auth arg commitment moved.
Execute or cancel every pending proposal on the rc version, have GUARDIAN drop any that
cannot be executed, then recreate the account on 0.17.0. Recreating the account does not
clear proposals served for the old one.

A Guardian server or SDK built on one protocol line rejects a node from another.
Run a node matching the **Miden protocol** column.

## Data resets

Guardian has three times been unable to migrate stored account data across a Miden
line. Each reset is an embedded migration that runs automatically at server
startup, each is irreversible, and each scopes the purge to Miden rows using
`account_metadata.network_config->>'kind'` so EVM accounts survive.

| Migration | Introduced in | Deletes | Preserves |
|---|---|---|---|
| `2026-09-22-000001_miden_017_irreversible_reset` | Guardian 0.18.x | Miden rows in `delta_proposals`, `deltas`, `states`, `account_metadata`; `account_auth_state` by cascade | EVM rows, `admin_actions`, `auth_sessions`, `auth_challenges`, `storage_encryption_marker`, `worker_leases`, dashboard stats snapshot, keystore |
| `2026-08-24-000001_miden_016_irreversible_reset` | Guardian 0.17.x | Miden rows in `delta_proposals`, `deltas`, `states`, `account_metadata`; `account_auth_state` by cascade | EVM rows, `admin_actions`, `auth_sessions`, `auth_challenges`, `storage_encryption_marker`, `worker_leases`, keystore |
| `2026-06-14-000001_v015_account_id_cutover` | Guardian 0.15.x | pre-0.15 (v0 account ID) Miden rows in the same four tables | EVM rows, `admin_actions` |

All three are Postgres-only. Filesystem-backed deployments reset by starting from
empty storage and metadata directories, preserving the keystore directory.

A deployment upgrading across more than one line runs every pending reset in the
same startup; the newer reset subsumes the older ones.

## Guardian 0.18.x on Miden 0.17

Nothing stored under Miden 0.16 survives:

- **Serialized accounts, headers, and asset ids carry a version.** `Account`
  decoding rejects the 0.16 encoding (`account version is 241 but only version 1
  is supported`).
- **Account code procedures are ordered with the authentication procedure
  first**, so the code commitment of an account built from the same components
  changes.
- **Delta and storage-patch commitments are versioned**, and their domain
  separators moved into the hasher capacity word, so a stored delta no longer
  recomputes to the commitment it was signed under.
- **The transaction summary is versioned.** It binds a caller-chosen block and
  carries six user params instead of seven: the approval-expiration block (zero
  when the approval never expires), a zero, then the four salt felts. Stored
  summaries cannot be deserialized or re-verified.
- **The multisig auth argument is the commitment to a three-word preimage**:
  `[bound_block, approval_expiration, 0, 0]`, the salt, and the native 1/1 fee
  conversion info. Both SDKs set that preimage on the request. Rust builds
  `MultisigAuthArgs`; TypeScript starts from `feeAwareTransactionRequestBuilder`.
  A 0.16 request that only declared `fee_conversion_salt` is one word short and
  aborts in the auth procedure.
- **The guarded-multisig auth procedure pays the transaction fee** (since
  `miden-standards` 0.17.0-rc.7). It creates the `TX_FEE` note from the
  account's vault before the transaction summary is built, so the fee note is
  covered by the approver and GUARDIAN signatures. Every transaction therefore
  needs a balance in the chain's native fee asset, including the first one
  that deploys the account. A node rejects a transaction without a canonical
  `TX_FEE` note, and an account created by a build pinned to an earlier release
  candidate never creates one, so it cannot transact and must be recreated.
- **The fee asset left the block header.** It lives in the chain's protocol
  configuration, which the client receives from the node with each sync and
  stores per header. The auth args name the fee faucet of the configuration the
  bound block commits to, read from that store.
- **P2ID note storage is four felts** (target account, then a two-felt salt that
  defaults to zero) and **P2IDE storage is six** (reclaimer, target, reclaim
  height, timelock height). The script roots moved with the layouts.
- **Execution proofs use format 2** (VM 0.33). A 0.16 proof is rejected.

### Open upstream items

The facts below change independently of this repository. This list is the one
place that tracks them; other documents point here rather than restating them.
Last checked 2026-09-25.

- **Public networks.** Devnet runs node 0.17.0-rc.2, and this build's protocol
  configuration for devnet's fee asset hashes to the commitment in devnet's
  block headers, so the transaction kernels match. A guarded-multisig
  transaction from this build has not yet been executed there end to end.
  Devnet has no faucet front end: an account is funded by calling the node's
  `RegisterAccount` RPC, which pays it a small public P2ID note in the fee asset
  (`scripts/devnet-register-account.sh`). `miden-client`'s `register_account`
  does not send the request on devnet, because devnet enforces no allowlist and
  reports every account as already allowed. Testnet runs Miden 0.16, so
  on testnet the examples need a local `miden-node` from the pinned line until
  it upgrades.
- **miden-client fee path.** miden-client (still in 0.17.0-rc.3) commits the
  two-word 0.16 auth arg when a request declares `fee_conversion_salt`, so both
  SDKs set the three-word auth arg themselves (rationale in the multisig
  client's `transaction/auth_args.rs`). When the client builds
  `MultisigAuthArgs` itself: the helper can delegate to it; nothing stored or
  signed changes.
- **Pins.** The workspace pins protocol 0.17.0-rc.7 and client 0.17.0-rc.3 (see
  the matrix). The protocol pin follows the client and web SDK releases, not the
  protocol tags, because both SDKs must embed the same kernel. Moving to stable
  re-pins, regenerates roots, fixtures and the cross-SDK determinism vectors,
  and is the point at which 0.18.0 is released.

Data effect: full reset, see above. Client stores are recreated, not migrated.
Operator steps: [`PRODUCTION.md`](./PRODUCTION.md#upgrading-to-miden-017).

### Before bumping the Miden pin

A deployed Miden account is immutable and its procedure roots fix at creation, so
moving the contract pin strands every account created under the previous one,
including each network's qualification treasury. Nothing catches this
automatically: the qualification suite deliberately carries no long-lived
account, because GUARDIAN holds the only full copy of a private account and the
suite's stack is torn down with every run (see "Current limits" in
[QUALIFICATION.md](./QUALIFICATION.md)).

So a pin bump owes these by hand, in this order:

1. Qualify an account created **before** the bump against a server built
   **after** it, and confirm it fails loudly rather than silently misbehaving.
   A run whose accounts are all created after the bump proves nothing about it.
2. Recreate each network's treasury with `treasury-new`, fund it, and record the
   new key. The old treasury is stranded like any other account.
3. Note the bump in the support matrix above, with whether existing accounts
   survive.

Step 1 is the one most easily skipped, because every other signal stays green.

## Guardian 0.17.x on Miden 0.16

Nothing stored under Miden 0.15 survives, because the account's on-chain surface
moved in several independent ways:

- **Procedure roots changed**, so stored proposals no longer address the
  procedures they were signed against, and root-keyed storage reads
  (`procedure_thresholds`) miss.
- **ECDSA-k256 public-key commitments changed** in `miden-crypto` 0.28 to hash
  native affine-coordinate limbs (`qx || qy` as little-endian `u32` limbs)
  instead of the compressed SEC1 bytes, so stored approver commitments no longer
  match their keys. Compressed SEC1 *serialization* is unchanged, which is why
  this fails as a commitment mismatch rather than a decode error.
- **The signature advice ABI changed** in `miden-vm` 0.29 to
  `QX[8] || QY[8] || SIG_R[8] || SIG_S[8]`, and the recovery byte is no longer
  part of it, so stored signatures cannot be replayed into a transaction.
- **Storage slot names moved** from `openzeppelin::*` to `miden::standards::*`,
  so stored state cannot be read back by name.
- **The transaction summary layout changed** and now binds a chain anchor, so
  stored summaries cannot be recomputed or re-verified. Proposals carry a
  serialized `ChainAnchor` (wire field `chain_anchor`) and verification and
  execution pin to it.
- **The custody account is now the upstream `miden-standards`
  `AuthGuardedMultisig` component** rather than Guardian's local MASM, and
  `guardianEnabled` is gone: the guardian is always present.
- **Transaction fees became the auth component's responsibility**, and
  `AuthGuardedMultisig` now pays them. Its auth procedure calls
  `miden::standards::fee::pay_fee` *before* building the transaction summary, so
  the fee note and the vault withdrawal funding it fall inside what the cosigners
  sign rather than being appended afterwards.

  That makes the auth arg carry double duty. `fee::load_conversion_info` reads it
  as the commitment `hash(CONVERSION_INFO || SALT)` and looks the preimage up in
  the advice map; the same word then serves as the transaction summary salt. A
  bare salt still satisfies the salt role but not the fee role: the lookup
  misses, conversion info comes back empty, and `pay_fee` aborts with
  `ERR_FEE_CONVERSION_INFO_MISSING` — though only once the computed fee is
  non-zero, so a zero-`verification_base_fee` chain never notices.

  **Every typed `create*Proposal` path in both SDKs therefore commits native
  conversion info**, at rate 1/1 under the chain's own fee faucet. This is the
  invariant change: a proposal's auth arg is no longer its salt.

  Cross-SDK reconstruction survives because the committed value is *derived*, not
  chosen. The faucet is read from the block the proposal is anchored at — the
  anchor travels with the proposal and is checked against the summary's block
  commitment before use — and the rate is fixed at 1/1. Both SDKs declare the
  stored proposal salt on their transaction request builders:
  `fee_conversion_salt(salt)` in Rust and `withFeeConversionSalt(salt)` in
  TypeScript. The pinned Miden clients then derive and commit the same native
  conversion info from the reference header used for execution.

  Two consequences worth knowing:

  - The pinned Miden clients classify `AuthGuardedMultisig` as
    `CallerChosenSalt`. A request declares a salt, and the client commits the
    chain-native conversion info under that salt. Components that do not read
    fee conversion info still fail with
    `TransactionRequestError::FeeConversionInfoUnsupported`.
  - `pay_fee` spends the faucet and rate the committed conversion info names, so
    what a guarded account must hold follows from what it commits. The built-in
    typed proposal paths always commit the chain-native asset at rate 1/1, so on
    a fee-charging chain an account driving them needs that native asset in its
    vault or `pay_fee` aborts before the summary exists — and guardian-assisted
    recovery cannot route around it, since it takes the same path. A custom
    request that commits a different fee asset must instead fund *that* asset:
    holding only it is enough to execute through fee payment, provided the
    request needs no other assets. Whether the resulting transaction is then
    *included* is a separate question — the batch builder decides what fee
    asset and rate it accepts.

  The exported builders always declare the fee conversion salt. A caller
  assembling a raw custom request can omit it, but the resulting request works
  only on a zero-fee chain. Typed proposal reconstruction always declares the
  stored `salt_hex`.

Data effect: full reset, see above. Operator steps:
[`PRODUCTION.md`](./PRODUCTION.md#upgrading-to-miden-016).

## Guardian 0.15.x and 0.16.x on Miden 0.15

Miden 0.15 invalidated account ID version 0: encoded version `0` is rejected, and
every serialized `AccountDelta` or `TransactionSummary` embedding a v0 ID fails to
deserialize. A v0 ID is a proof-of-work-derived commitment with no v1 equivalent,
so there is no in-place migration. Addresses also moved to bech32m.

Guardian 0.16.x stayed on Miden 0.15 and required no reset; the changes in that
release were Guardian-side only.

Data effect: the 0.15 cutover above, on the first 0.15 deploy.

## Adding a line

When Guardian adopts a new Miden line:

1. Add a matrix row with the exact pins.
2. Add a per-line section stating what broke and what it does to stored data.
3. If data cannot be migrated, add the migration to the reset table and write the
   operator steps in [`PRODUCTION.md`](./PRODUCTION.md).
4. Leave the procedural and symptom docs pointing here rather than restating the
   version facts, so there is one place to update.
