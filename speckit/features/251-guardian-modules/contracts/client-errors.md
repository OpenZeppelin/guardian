# Contract: Client-Visible Module Errors

**Source**: RFC 0002 section 4.1. Same envelope `{ code, message, meta }` on HTTP and gRPC (constitution principle II).

| Code | HTTP | gRPC | Retryable | `meta` | Returned by |
|---|---|---|---|---|---|
| `GUARDIAN_MODULE_DENIED` | 403 | `PERMISSION_DENIED` | no | `{ module, reason_code }` | Proposal creation, `push_delta`, `/configure` |
| `GUARDIAN_MODULE_UNAVAILABLE` | 503 | `UNAVAILABLE` | yes | `{ module }` | Same |

- `module` is the operator-chosen instance `id`; the module kind never reaches clients.
- `reason_code` matches `[a-z0-9_]{1,64}`; the module's free-text message is logged, never returned.
- Execution path (after PR #510): typed abort reasons `module_denied { module, reason_code }` and `module_unavailable { module }` in the execution status.

## Consumers that must expose them as typed errors with identical retry classification

| Layer | Package |
|---|---|
| Base client, Rust | `crates/client` |
| Base client, TypeScript | `packages/guardian-client` |
| Multisig SDK, Rust | `crates/miden-multisig-client` |
| Multisig SDK, TypeScript | `packages/miden-multisig-client` |
| Validation surfaces | `examples/demo`, `examples/smoke-web` |

Each layer gets a test for both codes on proposal creation and on push; the multisig SDKs also on registration.
