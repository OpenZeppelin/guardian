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
| 0.17.0-rc.2 | 0.16 (rc) | `=0.16.0-rc.9` | `=0.16.0-rc.3` | `0.16.0-rc.5` (exact) |
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

**The 0.17.0-rc.2 row is mid-move.** The guarded-multisig auth component now pays the
transaction fee, which needs `miden::standards::fee` procedures that first ship in
`miden-standards` 0.16.0-rc.9 — so the Rust pin is there. The newest published
`@miden-sdk/miden-sdk` is still 0.16.0-rc.5, which embeds an older `miden-standards`: it
cannot assemble the current auth MASM, and its `AccountInterface` does not recognise an
account built from it. Until an rc.9-embedding web SDK is published, the browser builder
and anything reading a current account through that SDK are held back — the
`account-roundtrip` case that calls straight into the SDK
(`Account.getPublicKeyCommitments`) and the Playwright browser determinism gate both
fail on the version gap, not on a Guardian defect.

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
  commitment before use — and the rate is fixed at 1/1. So a rebuilder holding
  `salt_hex` and the anchor reproduces the auth arg without being told it, and
  both SDKs derive it identically: `fee_conversion_info_at()` in Rust,
  `proposalFeeFaucetIdHex()` in TypeScript. Reading the *chain tip's* faucet
  instead would break this, since fee parameters are a per-block header field;
  both SDKs read the anchor, and the TypeScript create path additionally asserts
  the committed faucet against the anchor it sealed.

  Two consequences worth knowing:

  - `miden-client` rejects *declared* conversion info for any auth component
    other than `AuthSingleSig`/`AuthMultisig`
    (`TransactionRequestError::FeeConversionInfoUnsupported`). That refusal keys
    on Rust's `TransactionRequestBuilder::fee_conversion_info`, not on the auth
    arg, so the Rust SDK deliberately avoids that method and sets the auth arg
    and advice entry directly.

    That refusal rests on a rationale protocol#3765 falsifies. `miden-client`'s
    `validate_fee_conversion_info_support` allowlists only
    `AuthSingleSig`/`AuthMultisig`, documenting that `AuthGuardedMultisig` "does
    not reach `miden::standards::fee` at all". Since #3765 it does — that is the
    whole point of this change. **The allowlist has to gain
    `AuthGuardedMultisig` in the same upstream train as #3765.** Until it does,
    the commitment is implemented three times: once upstream in
    `TransactionRequestBuilder::fee_conversion_info`, once in the Rust SDK's
    `MaybeFeeConversionInfo`, and once from scratch in TypeScript's
    `feeAuthArg`. A future change to the commitment scheme updates one and
    silently diverges the other two, which is what the pinned cross-SDK digest
    vectors exist to catch. Once the allowlist lands, the Rust SDK should switch
    to the upstream builder method and keep TypeScript pinned to it by vector.

    On the TypeScript side this is a *version* fact, not a structural guarantee.
    The bindings this repo currently installs expose no declaring method — only
    `withAuthArg` and `extendAdviceMap` — so both SDKs make the same two calls
    and neither can trip the refusal. Newer websdk builds do add a
    conversion-info builder method and a block-header accessor for the
    verification base fee. An integrator on such a build must not use the
    declaring method on a guarded account until the allowlist above changes;
    it yields `FeeConversionInfoUnsupported`.
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

  A caller driving the exported builders directly still chooses, in either SDK:
  `SignatureOptions.feeFaucetId` in TypeScript, the `Option<FeeConversionInfo>`
  parameter in Rust. Omitting it produces the pre-0.16 bare auth arg, which
  executes only on a zero-fee chain. The difference between the SDKs is on the
  *rebuild* side, not the build side: TypeScript's `detectAuthArgConvention`
  reproduces either convention, while Rust assumes committed and has no
  convention detector, so a bare typed proposal fails Rust's binding check.

  Note what that costs during a rolling upgrade, when pre-0.16 proposals are
  still in flight. Every one of them carries a bare auth arg, so every one fails
  that check for a Rust cosigner — and `MultisigClient::list_proposals`
  propagates the first such failure, hiding the account's whole pending list,
  whereas `sync_proposals` collects it into `skipped` and continues. Prefer
  `sync_proposals` until the pre-upgrade proposals have been dropped
  server-side. TypeScript does not fail at all here: it detects `bare` and
  proceeds, so the proposal verifies and only aborts later at proving.

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
