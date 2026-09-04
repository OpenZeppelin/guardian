# Miden compatibility

Which Miden protocol line each Guardian release targets, what changed between
lines, and what each upgrade does to stored data.

> Guardian's own version and Miden's are **not** aligned. Guardian 0.16.x runs on
> Miden 0.15; Miden 0.16 arrives in Guardian 0.17.x. Read the matrix rather than
> matching the numbers.

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
| 0.17.0-rc.3 | 0.16 (rc) | `=0.16.0-rc.9` | `=0.16.0-rc.4` | `0.16.0-rc.7` (exact) |
| 0.17.0-rc.2 | 0.16 (rc) | `=0.16.0-rc.6` | `=0.16.0-rc.3` | `0.16.0-rc.5` (exact) |
| 0.17.0-rc.1 | 0.16 (rc) | `=0.16.0-rc.6` | `=0.16.0-rc.2` | `0.16.0-rc.3` (exact) |
| 0.16.x | 0.15 | `0.15.3` | `0.15.0` | `^0.15.8` |
| 0.15.x | 0.15 | `0.15.x` | `0.15.0` | `^0.15.0` |
| 0.14.x | 0.14 | n/a | `0.14.x` | `^0.14.0` |
| 0.13.x | 0.13 | n/a | `0.13.0` | `^0.13.0` |
| 0.12.x | 0.12 | n/a | `0.12.5` | `^0.12.5` |

The 0.17 line is a release candidate while `miden-standards` itself is still an
rc, and it is published to npm under the `rc` dist-tag, so `npm install` without
an explicit version still resolves the 0.16.x line.

Pins are exact on the 0.16 rc line because the rc protocol is still moving. The
Rust and npm pins must move together: nothing at build time verifies that the npm
SDK's embedded `miden-standards` matches the Rust pin, so the CI parity gates are
what catch drift. See
[`MULTISIG_SDK.md`](./MULTISIG_SDK.md#contract-version-pinning).

**0.17.0-rc.3 is a breaking contract bump.** The guarded-multisig auth component now pays
the transaction fee, so it calls `miden::standards::fee` procedures that first ship in
`miden-standards` 0.16.0-rc.9, and it is built from that release's standard
`AuthGuardedMultisig` component rather than from a copy compiled here. Both move the
`auth_tx` procedure root, so an account created by 0.17.0-rc.2 does not carry the root
0.17.0-rc.3 pins: `assertPinnedContractVersion` rejects it, and reads through
`AccountInspector` fail with `UnsupportedContractVersion`. There is no in-place migration —
accounts must be recreated on the new contract version. See
[`MULTISIG_SDK.md`](./MULTISIG_SDK.md#contract-version-pinning).

**Drain pending proposals before upgrading.** The rebuild now commits fee conversion info
unconditionally, and every proposal's auth arg is the commitment `hash(CONVERSION_INFO || SALT)`
rather than the bare salt. A proposal still pending from 0.17.0-rc.1 or rc.2 therefore cannot be
reproduced: the Rust client fails `verify_proposal_summary_binding` with "metadata does not match
tx_summary", and the TypeScript client reports that the metadata does not match the transaction
summary. The blast radius is wider than the one proposal — strict `list_proposals` /
`syncProposals` fail for the whole account
while GUARDIAN keeps serving it, and the isolating recovery sync skips it silently, so notes
embedded in it are never imported.

Execute or cancel every pending proposal on the old version, and have GUARDIAN drop any that
cannot be executed, before upgrading. Recreating the account (above) does not clear proposals
served for the old one.

A Guardian server or SDK built on one protocol line rejects a node from another.
Run a node matching the **Miden protocol** column.

## Data resets

Guardian has twice been unable to migrate stored account data across a Miden
line. Both resets are embedded migrations that run automatically at server
startup, both are irreversible, and both scope the purge to Miden rows using
`account_metadata.network_config->>'kind'` so EVM accounts survive.

| Migration | Introduced in | Deletes | Preserves |
|---|---|---|---|
| `2026-08-24-000001_miden_016_irreversible_reset` | Guardian 0.17.x | Miden rows in `delta_proposals`, `deltas`, `states`, `account_metadata`; `account_auth_state` by cascade | EVM rows, `admin_actions`, `auth_sessions`, `auth_challenges`, `storage_encryption_marker`, `worker_leases`, keystore |
| `2026-06-14-000001_v015_account_id_cutover` | Guardian 0.15.x | pre-0.15 (v0 account ID) Miden rows in the same four tables | EVM rows, `admin_actions` |

Both are Postgres-only. Filesystem-backed deployments reset by starting from
empty storage and metadata directories, preserving the keystore directory.

A deployment upgrading across more than one line runs both migrations in the same
startup; the newer reset subsumes the older one.

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
