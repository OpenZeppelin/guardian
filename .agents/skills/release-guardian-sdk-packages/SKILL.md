---
name: release-guardian-sdk-packages
description: Version, validate, dry-run, and publish the repository's Rust and TypeScript Guardian SDK packages to crates.io and npm. Use when Codex needs to choose the next coordinated release version, update release manifests and lockfiles, run targeted checks, prepare or execute publish commands in dependency order, and minimize the user's work to registry login or final irreversible publish confirmation.
---

# Release Guardian SDK Packages

Read the current source of truth at the start of every release task:

- `docs/MULTISIG_SDK.md`
- `Cargo.toml`
- `crates/shared/Cargo.toml`
- `crates/client/Cargo.toml`
- `crates/contracts/Cargo.toml`
- `crates/miden-multisig-client/Cargo.toml`
- `packages/guardian-client/package.json`
- `packages/guardian-client/package-lock.json`
- `packages/guardian-evm-client/package.json`
- `packages/guardian-evm-client/package-lock.json`
- `packages/miden-multisig-client/package.json`
- `packages/miden-multisig-client/package-lock.json`
- `references/release-surface.md`

Trust these sources in this order:

1. crate manifests, package manifests, and lockfiles
2. `docs/MULTISIG_SDK.md`
3. `references/release-surface.md`

## Default Behavior

Do as much of the release prep as possible without user intervention:

- check the current git branch before mutating release files
- inspect the current publishable versions
- use the target version provided by the user
- update manifests and lockfiles
- run targeted tests, builds, and dry-runs
- give the user the exact remaining auth or publish commands

Before changing versions or preparing publishes:

- if the current branch is not a dedicated release branch, tell the user to move to one first
- prefer a branch name like `release/v<version>` when the target version is known
- do not create, rename, or push the branch unless the user explicitly asks Codex to do it

If the user does not provide a version:

- inspect the current coordinated release version
- propose the next valid version on the active line
- stop for confirmation unless the user explicitly asked Codex to choose the next version automatically

Unless the user explicitly asks Codex to perform the real publish and credentials are already valid, stop before irreversible `cargo publish` or `npm publish` steps.

## Version Policy

- Keep the publishable SDK surface on one coordinated version
- Stay on the active Miden dependency line unless the task is an explicit migration
- Treat the user-provided target version as the source of truth for the release
- If the user has not decided yet, present the current version and the likely next patch version, but do not bump files until the version is confirmed
- If the user asks for "current +1" on the active line, choose the next patch above the highest committed publishable version on that line
- Do not change `crates/server`, `crates/miden-rpc-client`, `crates/miden-keystore`, or example crate versions as part of the SDK release

## Publishable Surface

Rust crates:

- `guardian-shared`
- `guardian-client`
- `miden-confidential-contracts`
- `miden-multisig-client`

TypeScript packages:

- `@openzeppelin/guardian-client`
- `@openzeppelin/guardian-evm-client`
- `@openzeppelin/miden-multisig-client`

## Version Bump Rules

For a coordinated release, update all of these:

- `Cargo.toml` `[workspace.package] version`
- `crates/client/Cargo.toml` internal `guardian-shared` dependency version
- `crates/contracts/Cargo.toml` internal `guardian-shared` dependency version
- `crates/miden-multisig-client/Cargo.toml` internal `guardian-client`, `guardian-shared`, and `miden-confidential-contracts` dependency versions
- `packages/guardian-client/package.json` `version`
- `packages/guardian-evm-client/package.json` `version`
- `packages/miden-multisig-client/package.json` `version`
- `packages/miden-multisig-client/package.json` `@openzeppelin/guardian-client` dependency range

After editing TypeScript versions, refresh lockfiles from the package directories:

```bash
cd packages/guardian-client && npm install --package-lock-only
cd packages/guardian-evm-client && npm install --package-lock-only
cd packages/miden-multisig-client && npm install --package-lock-only
```

Inspect the resulting lockfile diff. Keep the refresh focused on version and dependency metadata.

## Validation

Run the smallest release-relevant checks first:

```bash
cargo test -p guardian-shared
cargo test -p guardian-client
cargo test -p miden-confidential-contracts
cargo test -p miden-multisig-client
```

```bash
cd packages/guardian-client && npm test
cd packages/guardian-client && npm run build
cd packages/guardian-evm-client && npm test
cd packages/guardian-evm-client && npm run build
cd packages/miden-multisig-client && npm test
cd packages/miden-multisig-client && npm run build
```

Then run publish dry-runs:

```bash
cargo publish -p guardian-shared --dry-run
cargo publish -p guardian-client --dry-run
cargo publish -p miden-confidential-contracts --dry-run
cargo publish -p miden-multisig-client --dry-run
```

```bash
cd packages/guardian-client && npm publish --access public --dry-run
cd packages/guardian-evm-client && npm publish --access public --dry-run
cd packages/miden-multisig-client && npm publish --access public --dry-run
```

If a dry-run or test fails, stop there and report the failing step, package, and minimal next action.

## Git Workflow

Before any release edits:

- inspect `git branch --show-current`
- inspect `git status --short`
- if the user is still on a feature or work branch, ask them to switch to a release branch before proceeding

Suggested branch commands:

```bash
git checkout -b release/v<version>
```

If the branch already exists:

```bash
git checkout release/v<version>
```

## Publish Order

Rust crates must be published in dependency order:

1. `guardian-shared`
2. `guardian-client`
3. `miden-confidential-contracts`
4. `miden-multisig-client`

Wait for crates.io indexing between dependent publishes.

TypeScript packages must be published in dependency order:

1. `@openzeppelin/guardian-client`
2. `@openzeppelin/guardian-evm-client`
3. `@openzeppelin/miden-multisig-client`

## Manual Boundary

The user should usually only need to handle:

- moving to or confirming the release branch
- `cargo login <CRATES_IO_TOKEN>` or equivalent registry auth check
- `npm whoami` or `npm login`
- any final publish confirmation the user wants to own
- git tag creation and push

When auth is missing or unverified, give a short ordered command sequence. Prefer:

```bash
cargo login <CRATES_IO_TOKEN>
npm whoami || npm login
```

If the user has already authenticated, continue with the automated prep and only surface the exact real publish commands that remain.

## Post-Release

After publishing:

- ask the user whether to create and push the release tag
- verify the published versions if the task requires it

Use:

```bash
git tag v<version>
git push origin v<version>
```

## Output Shape

Default to a short release handoff:

- target version
- expected release branch
- files updated
- checks and dry-runs completed
- exact remaining commands for branch, auth, publish, and tagging

If the user asks to publish, separate dry-run commands from real publish commands and keep the final sequence copy-pasteable.
