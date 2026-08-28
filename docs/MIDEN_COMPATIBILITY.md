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
