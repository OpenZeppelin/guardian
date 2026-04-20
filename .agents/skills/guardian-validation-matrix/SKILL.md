---
name: guardian-validation-matrix
description: Select the smallest meaningful verification set for changes in the Guardian repository. Use when Codex needs to decide which cargo tests, npm tests, builds, example smokes, or specialized validation skills are required for server, client, multisig, browser, deploy, release, or benchmark changes.
---

# Guardian Validation Matrix

## Overview

Use this skill after the change scope is understood. Choose the minimum validation that still covers the changed behavior, then expand only when the touched path is high-risk or crosses more layers.

## Read First

Read:

- `AGENTS.md`
- [`references/command-matrix.md`](references/command-matrix.md)
- the changed files or summarized impact report

## Workflow

1. Classify the affected layer and risk.
   Decide whether the change is:
   - server-only internal
   - server contract
   - Rust client
   - TS client
   - multisig Rust
   - multisig TS
   - browser example or signer integration
   - deploy, release, or benchmark
2. Start with the package-local checks from [`references/command-matrix.md`](references/command-matrix.md).
3. Expand when any of these are true:
   - the change affects a public contract
   - the change affects auth or signature flows
   - the change affects proposal lifecycle or canonicalization
   - the change affects browser signers or user-visible examples
4. Add manual smoke when a lower-layer change is observable in `examples/demo` or `examples/smoke-web`.
5. Prefer existing specialized skills over duplicating their workflow.

## Guardrails

- Do not jump directly to `cargo test --workspace` unless the impact is broad or targeted checks already failed.
- Do not treat unit tests as a replacement for example smoke when the behavior is user-visible.
- When a server or client contract changes, validate at least one upstream consumer.
- Document skipped coverage with a reason instead of implying it was unnecessary.

## Specialized Skills

Use these instead of expanding this skill:

- `smoke-test-rust-multisig-sdk`
- `smoke-test-ts-multisig-sdk`
- `deploy-guardian-aws`
- `release-guardian-sdk-packages`
- `run-guardian-prod-benchmarks`

## Output Shape

Produce:

- exact ordered commands to run
- manual smoke required
- why each command is included
- what was intentionally skipped
- next expansion step if the first layer of checks fails
