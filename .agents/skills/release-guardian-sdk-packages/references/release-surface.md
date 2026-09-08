# Guardian SDK Release Surface

Current coordinated SDK release line: `0.16.x`

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

TypeScript packages live in the `packages/` npm workspace with a single
lockfile at `packages/package-lock.json`. `@openzeppelin/miden-multisig-client` depends
on `@openzeppelin/guardian-client` via a version range; the workspace links the
in-repo package for install/test/publish. Do not use the `workspace:` protocol.

1. `@openzeppelin/guardian-client`
   - manifest: `packages/guardian-client/package.json`
2. `@openzeppelin/guardian-evm-client`
   - manifest: `packages/guardian-evm-client/package.json`
   - no internal release dependencies
3. `@openzeppelin/miden-multisig-client`
   - manifest: `packages/miden-multisig-client/package.json`
   - internal release dependency: `@openzeppelin/guardian-client`
4. `@openzeppelin/guardian-operator-client`
   - manifest: `packages/guardian-operator-client/package.json`
   - no internal release dependencies

## Files Usually Touched In A Coordinated Release

- `Cargo.toml`
- `crates/client/Cargo.toml`
- `crates/contracts/Cargo.toml`
- `crates/miden-multisig-client/Cargo.toml`
- `packages/guardian-client/package.json`
- `packages/guardian-evm-client/package.json`
- `packages/miden-multisig-client/package.json`
- `packages/guardian-operator-client/package.json`
- `packages/package-lock.json`
- `docs/MULTISIG_SDK.md` if release examples or tag snippets need updating

## Rust Publication Automation

Stable workflow:

```text
.github/workflows/publish-crates.yml
```

Published releases select all four Rust crates. Manual runs provide `dry-run`,
and one boolean per crate. Pull requests changing the workflow run an
all-crate dry run without credentials.

The fixed selection and summary order is:

1. `guardian-shared`
2. `guardian-client`
3. `miden-confidential-contracts`
4. `miden-multisig-client`

Cargo receives selected crates in one multi-package command and owns dependency
ordering. Actual runs skip exact versions already visible on crates.io.

Each crate's crates.io trusted publisher is bound to:

```text
GitHub owner: OpenZeppelin
Repository: guardian
Workflow: publish-crates.yml
Environment: release
```

OIDC trusted publishing is the only publication authentication path.

## TypeScript Publish Sequence

```bash
cd packages
npm ci
npm publish -w @openzeppelin/guardian-client --access public
npm publish -w @openzeppelin/guardian-evm-client --access public
npm publish -w @openzeppelin/miden-multisig-client --access public
npm publish -w @openzeppelin/guardian-operator-client --access public
```
