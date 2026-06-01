# Contributing to Guardian

Thanks for considering a contribution. This document is the human-oriented
companion to [`AGENTS.md`](./AGENTS.md) (which is written for coding
agents, but the operational guidance applies equally to people).

Security issues do **not** go through this process — see
[`SECURITY.md`](./SECURITY.md) for private disclosure via GitHub Security
Advisories.

## Before you start

Read these in order; each is short and answers a different question:

1. [`docs/CONCEPTS.md`](./docs/CONCEPTS.md) — what Guardian is, the
   custody model, state/delta lifecycle, trust boundaries.
2. [`docs/architecture/services.md`](./docs/architecture/services.md) —
   the module-level map of the codebase.
3. [`AGENTS.md`](./AGENTS.md) §1–§4 — the system shape, repo map, change
   rules, and the **mandatory contract-change workflow** for anything
   touching the wire contract.

If you skip all three, the most likely failure mode is to change one
layer (server / Rust client / TS client) and miss the others.

## Picking something to work on

- Browse [open issues](https://github.com/OpenZeppelin/guardian/issues).
- `good first issue` labels mark scoped, well-defined starting points.
- Larger features usually have a spec under [`spec/`](./spec/) before
  code lands — if your idea is large, open an issue first to align on
  the approach.
- The audit reports under [`audits/`](./audits/) sometimes reference
  known follow-ups worth picking up.

## Dev environment

Local setup, feature flags, and example harnesses are covered in
[`docs/LOCAL_DEV.md`](./docs/LOCAL_DEV.md). Once a server is running,
[`docs/QUICKSTART.md`](./docs/QUICKSTART.md) shows the 60-second sanity
check, and [`docs/CONFIGURATION.md`](./docs/CONFIGURATION.md) is the
canonical env-var reference.

Pin to the toolchain in [`rust-toolchain.toml`](./rust-toolchain.toml).
Node 18+ is required for the TS packages.

## Branching and PRs

- Branch off `main`.
- One logical change per PR. If you find yourself describing a PR with
  "and also…", split it.
- Open the PR against `main` and reference the issue it resolves.
- CI must pass (`.github/workflows/ci.yml`).
- A maintainer from [`CODEOWNERS`](./CODEOWNERS) reviews and merges.

Commit message style follows
[Conventional Commits](https://www.conventionalcommits.org/):

```
feat: short imperative summary
fix(server): handle the case where …
chore(release): bump TS SDK packages to 0.14.6
docs: clarify ACK rotation runbook
```

Scopes are optional but encouraged when the change is package-local.
Look at `git log --oneline -20` for live examples.

## Cross-layer changes are the norm

Guardian is a layered system. A change to the server contract (proto
shapes, HTTP payloads, status enums, auth headers) is **not done** until
both the Rust client (`crates/client`) and the TS client
(`packages/guardian-client`) are aligned, and at least one example or
SDK consumer demonstrates the new behavior end to end.

The full contract-change workflow is the canonical reference; see
[`AGENTS.md`](./AGENTS.md#4-contract-change-workflow-mandatory) §4. The
short version:

1. Update the contract source first (`crates/server/proto/guardian.proto`
   or the HTTP shapes in `crates/server/src/api/`).
2. Update Rust client (`crates/client`).
3. Update TS client (`packages/guardian-client`).
4. Propagate to the multisig SDKs (`crates/miden-multisig-client`,
   `packages/miden-multisig-client`).
5. Update or add an example that exercises the new path.
6. Update specs / docs that describe the surface you changed.

Skipping any layer creates silent drift between Rust and TypeScript
clients.

## Testing

Run the validation matched to the layer you changed. If you are using
agent tooling, the `guardian-validation-matrix` skill can pick the
smallest meaningful set for a given change. Otherwise use the matrix
below and the package READMEs.

Common invocations:

```bash
# Rust
cargo test --workspace
cargo test -p guardian-server --features integration
cargo test -p guardian-server --features e2e

# TypeScript (per package)
cd packages/guardian-client && npm install && npm test
cd packages/miden-multisig-client && npm install && npm test
cd packages/guardian-evm-client && npm install && npm test
cd packages/guardian-operator-client && npm install && npm test

# Examples (manual)
cd examples/demo && cargo run --release   # Rust TUI multisig flow
# smoke-web / operator-smoke-web / evm-smoke-web have their own READMEs
```

CI currently enforces the Rust workspace, formatting, and clippy jobs.
Run the TypeScript package checks locally when you touch TS packages or
browser examples.

For UI / SDK changes you cannot fully verify with `cargo test` alone,
run the matching smoke example end to end. The
`smoke-test-*` skills (`smoke-test-rust-multisig-sdk`,
`smoke-test-ts-multisig-sdk`, `smoke-test-operator-dashboard`,
`smoke-test-evm-proposal-support`) automate this when you have agent
tooling; otherwise follow the example's README.

## Code style

- **Rust:** `cargo fmt --all -- --check` and
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
  Match existing patterns; favour explicit error types over `anyhow` in
  library code (`crates/server`, `crates/client`).
- **TypeScript:** each package carries its own scripts; run
  `npm run typecheck`, `npm test`, and `npm run build` (where defined)
  before pushing.
- **Comments:** prefer no comment unless the *why* is non-obvious. Don't
  describe what well-named code already says.
- **Backwards compatibility:** don't add compatibility shims for behavior
  the task didn't ask for. See `AGENTS.md` §3, rule 6.

## Docs

If your change is user- or operator-visible, update the matching doc:

| Change | Update |
|---|---|
| New env var or config knob | [`docs/CONFIGURATION.md`](./docs/CONFIGURATION.md) |
| New error code | [`docs/TROUBLESHOOTING.md`](./docs/TROUBLESHOOTING.md) error reference |
| New runtime behavior | [`docs/CONCEPTS.md`](./docs/CONCEPTS.md) or the relevant architecture doc |
| Deploy or infra changes | [`docs/SERVER_AWS_DEPLOY.md`](./docs/SERVER_AWS_DEPLOY.md) and [`docs/architecture/infra.md`](./docs/architecture/infra.md) |
| Published Docker image / publish workflow | [`docs/SERVER_AWS_DEPLOY.md`](./docs/SERVER_AWS_DEPLOY.md) ("Published Docker images"); image at `ghcr.io/openzeppelin/guardian` |
| Wire contract changes | [`spec/api.md`](./spec/api.md), [`spec/processes.md`](./spec/processes.md) |
| New SDK feature | [`docs/MULTISIG_SDK.md`](./docs/MULTISIG_SDK.md) where relevant |

Doc-only PRs are welcome and reviewed under the same process.

## Releases

The Rust crates and TypeScript packages publish in lockstep at
coordinated versions. See the `release-guardian-sdk-packages` skill for
the canonical procedure. In short: maintainers run the release; you
don't need to bump versions in feature PRs.

## CLA

Contributions are gated by a Contributor License Agreement enforced by
[`.github/workflows/cla.yml`](./.github/workflows/cla.yml). Your first
PR will prompt you to sign it.

## Where to ask

- General questions: open a GitHub Discussion or issue.
- Security: [`SECURITY.md`](./SECURITY.md) (private disclosure).
- Code review: a `CODEOWNERS` reviewer is auto-assigned on every PR.

Thanks for helping make Guardian better.
