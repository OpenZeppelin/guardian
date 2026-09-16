# Contract: scenario manifest

**Feature**: `001-system-e2e-qualification` · Satisfies FR-001 to FR-004, FR-017, FR-019a, FR-026e.

The manifest is data, read by both drivers. Neither driver defines scenarios;
that is what keeps the Rust and TypeScript sets describing the same thing and
makes FR-023a ("TypeScript at least as complete as Rust") checkable rather than
aspirational.

## `scenarios.toml`

```toml
[[scenario]]
id            = "proposal-execute-2of3-falcon"
title         = "2-of-3 Falcon proposal reaches threshold and executes"
profile       = "live"
sdk           = "both"
runtime       = "server-side"
scheme        = "falcon"
shape         = "2-of-3"
mode          = "online"
actions       = ["account-create", "account-register", "proposal-create",
                 "proposal-sign", "proposal-execute", "commitment-verify"]
step_budget   = "90s"
required      = true

[[scenario]]
id            = "guardian-migrate-offline-1of1-ecdsa"
title         = "Guardian migration created offline, signed, imported, executed"
profile       = "live"
sdk           = "both"
runtime       = "server-side"
scheme        = "ecdsa"
shape         = "1-of-1"
mode          = "offline"
actions       = ["proposal-create-offline", "proposal-export",
                 "proposal-sign-external", "proposal-import",
                 "proposal-execute", "guardian-migrate"]
step_budget   = "240s"
required      = true
```

## Action vocabulary (FR-019a)

`account-create`, `account-register`, `account-recover-by-cosigner`,
`proposal-create`, `proposal-create-offline`, `proposal-export`,
`proposal-sign`, `proposal-sign-external`, `proposal-import`,
`proposal-execute`, `proposal-reject-below-threshold`,
`proposal-reject-duplicate-signature`, `asset-transfer`, `note-consume`,
`balance-assert`, `commitment-verify`, `guardian-migrate`, `handoff-rust-to-ts`,
`handoff-ts-to-rust`.

A scenario claims only the actions it lists. Passing a generic lifecycle never
implies an action it did not name (FR-019b).

## `matrix.toml`

```toml
[[pair]]
network      = "testnet"
sdk          = "rust"
availability = "available"

[[pair]]
network      = "devnet"
sdk          = "typescript"
availability = "available"
```

Both networks are declared available for both SDKs. `unavailable` is reserved
for a limitation believed to be structural. A network that is merely down, or
temporarily refusing a transport, stays `available` and its scenarios report
environment-blocked at run time (FR-026f). That way a recovered network needs
no manifest edit, and a persistent outage is visible as a run of blocked
results rather than as silence.

## Load-time validation

Rejected before any container starts, because each of these would otherwise
surface as a confusing mid-run failure:

1. Duplicate or missing `id`.
2. `mode = "offline"` on a scenario whose actions are not guardian migration
   (FR-021a).
3. `runtime = "browser"` combined with `sdk = "rust"`.
4. `step_budget` exceeding the historical window of a network where the
   scenario is `required` for that pair (FR-018d). The fix is to mark the
   scenario not-required for that network, not to shrink a budget the flow
   cannot actually meet. Networks with a short retention window will therefore
   carry a smaller required set than networks without one, which the matrix
   states rather than leaves implicit.
5. A required scenario referencing a pair marked `unavailable`.
6. `scheme` naming a mixed set (FR-017a).
7. An action outside the vocabulary.
