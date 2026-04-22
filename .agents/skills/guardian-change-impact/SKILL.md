---
name: guardian-change-impact
description: Identify the impact radius of a proposed or in-progress change in the Guardian repository and turn it into a concrete edit and validation plan. Use when Codex needs to map a request, bug, or diff across server, Rust client, TypeScript client, multisig SDKs, examples, docs, and tests before implementation or review.
---

# Guardian Change Impact

## Overview

Use this skill before making non-trivial changes or when reviewing a diff that may cross layers. Start from the lowest affected layer, decide what must propagate upward, and produce a short impact report that names the files, tests, and manual smoke surfaces that matter.

## Read First

Read these sources before deciding scope:

- `AGENTS.md`
- `docs/MULTISIG_SDK.md`
- `spec/api.md`
- `spec/components.md`
- `spec/processes.md`
- [`references/impact-matrix.md`](references/impact-matrix.md)

Prefer current source files over prose docs when they disagree.

## Workflow

1. Classify the change.
   Use the matrix in [`references/impact-matrix.md`](references/impact-matrix.md) to decide whether the request is primarily:
   - server contract
   - server lifecycle or canonicalization
   - auth or signature flow
   - base client parity
   - multisig proposal workflow
   - browser signer or example integration
   - deploy, release, or benchmark infrastructure
2. Find the lowest affected layer.
   Start from:
   - `crates/server`
   - `crates/client` and `packages/guardian-client`
   - `crates/miden-multisig-client` and `packages/miden-multisig-client`
   - `examples/`
3. Enumerate required propagation upward.
   If a lower layer changes, name every upstream layer that must be inspected or updated in the same task.
4. Choose the smallest sufficient validation set.
   Map the impact classification to the minimum cargo, npm, and manual smoke coverage. Use `guardian-validation-matrix` if the change is already understood and only the verification set needs to be chosen.
5. Hand off to a narrower skill when appropriate.
   - Use `guardian-contract-change` for endpoint, payload, or enum changes.
   - Use `guardian-multisig-proposal-lifecycle` for proposal or offline flow changes.
   - Use `guardian-auth-signature-flows` for auth, keystore, Falcon, ECDSA, or ack changes.
   - Use `smoke-test-rust-multisig-sdk` or `smoke-test-ts-multisig-sdk` for manual canaries.
   - Use `deploy-guardian-aws`, `release-guardian-sdk-packages`, or `run-guardian-prod-benchmarks` for those specialized flows.

## Output Shape

Produce a compact report with:

- lowest affected layer
- directly impacted files or modules
- required propagation layers
- mandatory tests and smoke checks
- docs that must be checked or updated
- open questions or risky assumptions

Do not stop at vague statements like "probably affects clients". Name the concrete surfaces.

## Guardrails

- Preserve bottom-up reasoning. Do not start from examples or docs and infer the server contract from them.
- Treat auth, signature handling, canonicalization, proposal status transitions, and offline import or export as high-risk.
- Treat server contract edits as multi-package edits by default.
- Do not assume Rust and TypeScript behavior match. Verify both surfaces explicitly when the workflow exists in both stacks.
- Prefer a minimal edit plan over a large refactor plan.
