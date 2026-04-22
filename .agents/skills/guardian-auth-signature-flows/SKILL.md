---
name: guardian-auth-signature-flows
description: Implement and debug Guardian authentication and signature flows across server, clients, multisig SDKs, and examples. Use when Codex changes request signing, auth metadata, ack signing, Falcon or ECDSA behavior, key managers, public key handling, or signature encoding and verification.
---

# Guardian Auth Signature Flows

## Overview

Use this skill for the repo's most fragile crypto-adjacent flows. Keep scheme handling explicit, validate boundary conversions, and verify both Falcon and ECDSA whenever the touched path supports both.

## Read First

Read these files before editing:

- `AGENTS.md`
- `crates/server/src/ack/*`
- `crates/server/src/metadata/auth/*`
- `crates/client/src/auth/*`
- `crates/client/src/keystore/*`
- `packages/guardian-client/src/auth-request.ts`
- `packages/guardian-client/src/conversion.ts`
- `packages/miden-multisig-client/src/utils/signature.ts`
- `packages/miden-multisig-client/src/utils/encoding.ts`
- [`references/auth-surfaces.md`](references/auth-surfaces.md)

Inspect example surfaces too if the changed auth or signature behavior reaches execution or browser signers.

## Workflow

1. Classify the signature path.
   Determine whether the task affects:
   - request auth metadata
   - proposal signatures
   - guardian ack signing
   - keystore or signer abstractions
   - signature encoding or decoding
   - scheme selection or scheme-specific public key handling
2. Edit the lowest boundary first.
   - server verification or signing logic
   - Rust client auth or signer logic
   - TS HTTP auth or signature utilities
3. Verify conversion boundaries explicitly.
   Normalize external hex, bytes, base64, public keys, and commitments at the boundary module instead of scattering ad hoc conversion logic.
4. Re-check both schemes when the path supports both.
   Falcon-only or ECDSA-only paths can stay narrow, but mixed flows must be validated under each scheme.
5. Expand upward to multisig execution or examples if the changed signature shape is consumed there.

## Guardrails

- Do not branch on free-form error strings in core logic.
- Do not introduce implicit crypto conversions in feature code.
- Do not drop public key information for ECDSA flows that require it downstream.
- Preserve explicit `x-pubkey`, `x-signature`, and `x-timestamp` behavior when touching request signing.
- Preserve `ack_sig`, `ack_pubkey`, and `ack_scheme` behavior when touching execution or proposal flows.
- If a path uses both Falcon and ECDSA, test both. Do not infer parity from one scheme.

## Validation

Default targeted checks:

```bash
cargo test -p guardian-client
cargo test -p guardian-server
cd packages/guardian-client && npm test
```

Expand when the auth or signature change crosses into multisig execution:

- `cargo test -p miden-multisig-client`
- `cd packages/miden-multisig-client && npm test`
- `smoke-test-rust-multisig-sdk`
- `smoke-test-ts-multisig-sdk`

## Output Shape

Report:

- signature path touched
- schemes exercised
- boundary modules updated
- downstream consumers checked
- tests and smoke coverage
- unresolved assumptions
