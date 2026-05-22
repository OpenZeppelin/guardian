# CLAUDE.md — Guardian

**Read [AGENTS.md](./AGENTS.md) first.** It is the canonical agent guide for
this repo: system shape, change rules, contract-change workflow, validation
matrix, high-risk areas, known gotchas, security hygiene, repo-specific
skills, and coding style.

This file is a Claude-Code-specific stub. Cross-tool guidance belongs in
`AGENTS.md`; for any rule that conflicts, `AGENTS.md` wins.

## Reading order

1. [`AGENTS.md`](./AGENTS.md) — §1 (System Shape) and §2 (Repo Map) first;
   then §3 (Core Change Rules) and §4 (Contract-Change Workflow) before
   touching any wire contract.
2. The deeper guides — [`docs/MULTISIG_SDK.md`](./docs/MULTISIG_SDK.md),
   [`docs/SERVER_AWS_DEPLOY.md`](./docs/SERVER_AWS_DEPLOY.md),
   [`spec/`](./spec/index.md) — for SDK usage, deployment, and protocol
   definitions respectively. A richer documentation hub is on `main`
   (PR #246); if this branch trails it, prefer the corresponding files on
   `main`.

## Where to look for X

This branch ships a minimal `docs/`. The richer set
(`docs/CONCEPTS.md`, `docs/QUICKSTART.md`, `docs/LOCAL_DEV.md`,
`docs/CONFIGURATION.md`, `docs/TROUBLESHOOTING.md`, `docs/dashboard.md`,
`docs/architecture/`, `docs/runbooks/`) lives on `main` after PR #246; rebase
or check `main` if you need them.

| Need | File on this branch |
|---|---|
| Multisig SDK usage | [`docs/MULTISIG_SDK.md`](./docs/MULTISIG_SDK.md) |
| AWS deployment | [`docs/SERVER_AWS_DEPLOY.md`](./docs/SERVER_AWS_DEPLOY.md), [`infra/README.md`](./infra/README.md) |
| Protocol spec | [`spec/`](./spec/index.md) |
| Security disclosure | [`SECURITY.md`](./SECURITY.md) |
| Repo overview | [`README.md`](./README.md) |

## Layered context

These directories ship their own `AGENTS.md` (canonical) plus a thin
`CLAUDE.md` pointer. Claude Code loads them additively as you navigate.

- Root: `AGENTS.md`
- `crates/server/AGENTS.md`
- `crates/miden-multisig-client/AGENTS.md`
- `packages/guardian-operator-client/AGENTS.md`
- `packages/miden-multisig-client/AGENTS.md`
- `examples/demo/AGENTS.md`
- `examples/web/AGENTS.md`
- `infra/AGENTS.md`

**Gaps (no layered guide yet, fall back to root AGENTS.md + the code):**
`crates/client`, `crates/contracts`, `crates/miden-keystore`,
`crates/miden-rpc-client`, `crates/shared`, `packages/guardian-client`,
`packages/guardian-evm-client`, `examples/rust`, `examples/smoke-web`,
`examples/operator-smoke-web`, `examples/evm-smoke-web`, `examples/_shared`.

## Claude-Code-specific tips

- Before claiming a task complete, follow `AGENTS.md` §8 (Definition of Done):
  run the actual command and confirm output, do not assume.
- For an isolated package, start Claude in that directory rather than repo
  root — context loads more cleanly and test/build commands scope correctly:
  ```bash
  cd crates/server && claude
  cd packages/guardian-operator-client && claude
  ```
