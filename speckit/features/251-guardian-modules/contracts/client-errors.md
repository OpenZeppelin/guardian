# Contract: Client-Visible Module Errors

**Source**: RFC 0002 section 4.1. Same envelope `{ code, message, meta }` on HTTP and gRPC (constitution principle II).

| Code | HTTP | gRPC | Retryable | `meta` | Returned by |
|---|---|---|---|---|---|
| `GUARDIAN_MODULE_DENIED` | 403 | `PERMISSION_DENIED` | no | `{ module, reason_code, layer, client_message? }` (`layer`: `global` or `account`); for a generic denial only `{ reason_code: "policy_denied" }` | Proposal creation, `push_delta`, `/configure`, the dry-run check |
| `GUARDIAN_MODULE_UNAVAILABLE` | 503 | `UNAVAILABLE` | yes | `{ module }` | Same |

- `module` is the operator-chosen instance `id`; the module kind never reaches clients.
- `reason_code` matches `[a-z0-9_]{1,64}`; the module's free-text `message` is logged, never returned.
- `client_message` is the module's optional text for the requester (plain text, at most 256 characters), forwarded only for instances with `client_reason = "full"` (RFC 0002 section 2.7).
- A generic denial (an instance with `client_reason = "generic"`) carries no `module`, `layer` or `client_message`; clients treat `module` and `layer` as optional.
- Execution path (after PR #510): typed abort reasons in the execution status follow the same disclosure rules: `module_denied { module, reason_code, layer }` for an instance with full reasons, `module_denied { reason_code: "policy_denied" }` for a generic denial (no module or layer), and `module_unavailable { module }`.

## Consumers that must expose them as typed errors with identical retry classification

| Layer | Package |
|---|---|
| Base client, Rust | `crates/client` |
| Base client, TypeScript | `packages/guardian-client` |
| Multisig SDK, Rust | `crates/miden-multisig-client` |
| Multisig SDK, TypeScript | `packages/miden-multisig-client` |
| Validation surfaces | `examples/demo`, `examples/smoke-web` |

Each layer gets a test for both codes on proposal creation and on push; the multisig SDKs also on registration.
