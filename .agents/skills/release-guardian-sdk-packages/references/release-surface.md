# Guardian SDK Release Surface

Current coordinated SDK release line: `0.14.x`

## Publishable Rust Crates

1. `guardian-shared`
   - manifest: `crates/shared/Cargo.toml`
   - version source: `Cargo.toml` `[workspace.package] version`
2. `guardian-client`
   - manifest: `crates/client/Cargo.toml`
   - internal release dependency: `guardian-shared`
3. `miden-confidential-contracts`
   - manifest: `crates/contracts/Cargo.toml`
   - internal release dependency: `guardian-shared`
4. `miden-multisig-client`
   - manifest: `crates/miden-multisig-client/Cargo.toml`
   - internal release dependencies: `guardian-client`, `guardian-shared`, `miden-confidential-contracts`

## Publishable TypeScript Packages

1. `@openzeppelin/guardian-client`
   - manifest: `packages/guardian-client/package.json`
   - lockfile: `packages/guardian-client/package-lock.json`
2. `@openzeppelin/guardian-evm-client`
   - manifest: `packages/guardian-evm-client/package.json`
   - lockfile: `packages/guardian-evm-client/package-lock.json`
   - no internal release dependencies
3. `@openzeppelin/miden-multisig-client`
   - manifest: `packages/miden-multisig-client/package.json`
   - lockfile: `packages/miden-multisig-client/package-lock.json`
   - internal release dependency: `@openzeppelin/guardian-client`

## Files Usually Touched In A Coordinated Release

- `Cargo.toml`
- `crates/client/Cargo.toml`
- `crates/contracts/Cargo.toml`
- `crates/miden-multisig-client/Cargo.toml`
- `packages/guardian-client/package.json`
- `packages/guardian-client/package-lock.json`
- `packages/guardian-evm-client/package.json`
- `packages/guardian-evm-client/package-lock.json`
- `packages/miden-multisig-client/package.json`
- `packages/miden-multisig-client/package-lock.json`
- `docs/MULTISIG_SDK.md` if release examples or tag snippets need updating

## Default Publish Sequence

```bash
cargo publish -p guardian-shared
cargo publish -p guardian-client
cargo publish -p miden-confidential-contracts
cargo publish -p miden-multisig-client
```

```bash
cd packages/guardian-client && npm publish --access public
cd packages/guardian-evm-client && npm publish --access public
cd packages/miden-multisig-client && npm publish --access public
```
