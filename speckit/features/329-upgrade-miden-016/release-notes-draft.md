# Release Notes Draft: Guardian v0.16.0 — Miden v0.16 Line Upgrade

**Status**: DRAFT — do not publish until the TypeScript SDK reaches the same
Miden line (Contract 6) and upstream 0.16 stabilizes. Prepared 2026-07-21.

## Breaking: Guardian v0.16.0 targets the Miden v0.16 line

All Guardian crates and npm packages version to **0.16.0**, aligned with the
active Miden dependency line per the repository versioning policy.

The Rust workspace (server, `guardian-client`, `miden-multisig-client`,
`miden-confidential-contracts`, examples) moved from Miden 0.15.x to the
0.16 pre-release line (exact pins: `miden-protocol`/`-standards`/`-tx`
`=0.16.0-alpha.4`, `miden-client` `=0.16.0-alpha.1`, `miden-testing`
`=0.16.0-alpha.2`). Pins move to stable 0.16.0 when it publishes.

### No interoperability across the 0.15 ↔ 0.16 boundary

- **Networks**: the 0.16 devnet is a fresh chain. Accounts and notes created
  on the 0.15 devnet no longer exist; recreate them.
- **Local client stores**: miden-client changed its store format (account IDs
  as BLOB, new note layout). Delete `store.sqlite3`, `~/.guardian` metadata,
  and browser IndexedDB state; there is no migration.
- **Pending proposals**: proposals created under 0.15 cannot be executed
  under 0.16 (the account-update commitment layout changed).
- **Guardian server records** for 0.15 accounts remain readable as
  append-only history but do not verify against 0.16 networks.
- **Version mismatch behavior**: a 0.16 client against a 0.15 node (or vice
  versa) is rejected at the RPC boundary via the genesis-commitment check.

### Required consumer actions

1. Update Rust deps to the matching Guardian release; MSRV is now 1.96.1.
2. Reset local Miden state (stores, `~/.guardian`) and recreate accounts.
3. Point at a network running the 0.16 node (devnet today).
4. Regenerated protocol artifacts ship in this release: all multisig
   procedure roots changed (accounts gained a `create_note` procedure from
   BasicWallet), and the tx-kernel commitment is
   `0x60e15da40818dc87d8a04daee51e98ff4d6af6b2a24819a56abacefc09adb730`.

### API changes in Guardian SDKs

- `build_transfer_asset(faucet_id, amount)` no longer takes the account —
  the asset-callback flag now derives from the faucet account ID.
- `ProcedureName` gained a `CreateNote` variant.
- Transaction scripts authored against the SDK's libraries must use the
  0.16 form (`@transaction_script` + `pub proc main`) instead of
  `begin … end`.
- Guardian's wire contract (gRPC/HTTP shapes, status enums, error semantics)
  is unchanged; only the opaque Miden payload encoding moved to 0.16.

### TypeScript SDK

`@openzeppelin/miden-multisig-client` targets `@miden-sdk/miden-sdk
0.16.0-alpha.1` (exact pin), matching the Rust workspace — no source changes
were needed beyond the regenerated procedure roots and synced MASM. Browser
examples pin the matched `@miden-sdk/{miden-sdk,react}` 0.16.0-alpha.1 set;
the Para wallet packages (`@miden-sdk/miden-para`,
`@miden-sdk/use-miden-para-react`) still ship against 0.15 and are held via
npm overrides — Para-signer flows are unverified until upstream updates them.
