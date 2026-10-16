# Custom modules

Write your own module against `guardian-module-sdk`, then either serve it over
gRPC or compile it into your own server build. The worked example,
[`note_policy/mod.rs`](../../../crates/guardian-module-sdk/examples/note_policy/mod.rs),
is a module `basic-policy` cannot express: it limits how many payment notes a
transaction creates and can refuse private notes, whose recipients the
Guardian cannot see. The same code runs both ways. Do the
[setup](./README.md#setup) first.

| | Over gRPC | In process |
|---|---|---|
| Runs | As its own service ([`note-policy-serve.rs`](../../../crates/guardian-module-sdk/examples/note-policy-serve.rs)) | Inside your server build ([`custom_module_server.rs`](../../../crates/server/examples/custom_module_server.rs)) |
| Catalog | [`custom.toml`](./custom.toml): `kind = "grpc"` and an `endpoint` | [`custom-in-process.toml`](./custom-in-process.toml): `kind = "note-policy"` |
| Cost per call | A network round trip | A function call on its own task |
| Isolation | Separate process; can only refuse | Shares the process that holds the signing keys |
| Upgrades | Deploy the module alone | Rebuild the server for every Guardian release |
| Use it for | Third-party code, modules other teams run | Code you review and ship as part of your Guardian build |

## The `Module` trait

A stateless module implements three methods:

| Method | Returns | Notes |
|---|---|---|
| `describe(guardian)` | `ModuleInfo`: name, version, `PROTOCOL_VERSION`, supported hooks, required capabilities | `guardian` carries the Guardian's protocol range and capabilities. The Guardian refuses an instance subscribed to a hook the module does not list, a protocol version outside its range, or a capability it lacks. |
| `validate_params` | `ParamsVerdict::ok()`, `ParamsVerdict::invalid(message)` or `ParamsVerdict::normalized(params)` | Called at startup and on every params write. Reject unknown fields (`#[serde(deny_unknown_fields)]`) so a typo fails here, not silently at evaluation. Return `normalized` to have the Guardian store the params in your current shape. |
| `evaluate` | `Decision::allow()` or `Decision::deny(reason_code, message)` | Must be free of side effects: the same transaction is evaluated at proposal creation and again at approval, and clients retry. |

A reason code is lowercase `snake_case`; anything else counts as a module
failure. Return `ModuleError` for a request you cannot judge; the instance's
`on_error` then decides. `Decision::allow` and `Decision::deny` set the
decision's `effect`; a Guardian treats an effect it does not know as a
failure, never as an allow.

## Changing your module across releases

- **Params.** When you rename or restructure a param, keep accepting the
  previous shape for at least one release (serde aliases and defaults) and
  return `ParamsVerdict::normalized` with the new shape. At startup the
  Guardian stores the migrated global params, with a history entry by
  `module:<name> <version>`; account layers move to the new shape when they
  are next written. Never reuse a param name with a different meaning.
- **Protocol.** Within a protocol version the Guardian only adds fields,
  hooks and capabilities, so a module keeps working across Guardian releases.
  When a new protocol version ships, the Guardian keeps accepting the previous
  one for one release.
- **Rust API.** Build protocol types with `..Default::default()` or the SDK
  helpers, and SDK-owned types with their constructors
  (`ModuleInstanceConfig::new(..)`), so new fields do not break your build.

The rules are in [RFC 0002 section 3.5](../../rfcs/0002-guardian-modules.md#35-protocol-evolution-and-compatibility).

## What a request carries

| Field | Use it for |
|---|---|
| `params` | The params to apply, as JSON bytes: the global ones, or an account's layer on its second pass |
| `subject.miden()` (the Miden variant of `subject.transaction.projection`) | The decoded transaction: vault changes, output notes (amount, faucet, `note_type`, `p2id_target`, `tx_fee`), input notes, storage changes |
| `before`, `after` | Account views: signer commitments, `default_threshold` (absent for single-key accounts), procedure thresholds, auth scheme |
| `submitter`, `subject.transaction.proposer` | Verified signer commitments |
| `subject.transaction.unverified_metadata` | What the proposer claimed; never base a decision on it alone |
| `subject.transaction.raw` | The transaction in its network's own encoding (`encoding = "miden.transaction_summary"`), for modules that decode it themselves; tied to the Guardian's Miden version |

Skip the fee note (`tx_fee = true`) when counting payments: every transaction
pays its fee through an output note.

Projections are tagged by network (`TransactionProjection`, `oneof network`).
Miden is the only network today; read it with `subject.miden()` and return a
`ModuleError` when it is absent, so a module never judges a transaction it
does not understand.

## Run it over gRPC

The [remote](./remote.md) page explains the transport.

```sh
openssl rand -hex 32 > .guardian/note-policy.token
cargo run -p guardian-module-sdk --example note-policy-serve -- \
  --listen 127.0.0.1:7444 --instance note-policy .guardian/note-policy.token
cp docs/guides/policy-modules/custom.toml .guardian/
```

Set `GUARDIAN_MODULES_CONFIG=.guardian/custom.toml` in the guide's `.env` and
start the server as in the setup. The log line lists
`note-policy=note-policy <version>`.

## Run it in process

Register the kind on the server builder; that is the only difference from the
stock `server` binary:

```rust
ServerBuilder::new()
    // ...the same configuration as crates/server/src/main.rs...
    .with_module_kind("note-policy", Arc::new(NotePolicyFactory))
```

The factory gets each catalog instance's configuration and returns the module.
`build` fails if the kind name is already taken, including by `basic-policy`
or `grpc`. Run the example build:

```sh
cp docs/guides/policy-modules/custom-in-process.toml .guardian/
# set GUARDIAN_MODULES_CONFIG=.guardian/custom-in-process.toml in the guide's .env
set -a && . docs/guides/policy-modules/.env && set +a
cargo run -p guardian-server --example custom_module_server
```

No module process, token or endpoint is involved. To run it next to another
Guardian, set `GUARDIAN_EXAMPLE_HTTP_PORT` and `GUARDIAN_EXAMPLE_GRPC_PORT`
(default 3000 and 50051). The stock binary refuses
this catalog at startup, since it does not know the `note-policy` kind.

Each call still runs on its own task with the instance's `timeout_ms`, so a
panic in the module, or a timeout, is a module failure and `on_error` applies,
as for a remote module. Keep `evaluate` async and non-blocking all the same: a
module that blocks holds a runtime worker thread until it returns, even after
the Guardian has answered the request. The example server copies the setup
from `main.rs`, so keep it in step with it when you upgrade.

## Try it

| Test | Expected |
|---|---|
| A transaction with one payment note | Allowed |
| A transaction with two payment notes, if your client can build one | Refused with `too_many_payment_notes` |
| A payment sent as a private note, if your client supports it | Refused with `private_note_not_allowed` |
| `admin PUT /modules/note-policy/params '{"params":{"max_payment_notes":3},"reason":"test"}'` | Two notes allowed; private notes allowed too, since `deny_private_notes` now defaults to `false` |
| `admin PUT /modules/note-policy/params '{"params":{"max_notes":3},"reason":"typo"}'` | `400`, nothing stored: `validate_params` rejected the unknown field |

The tests work the same in either mode. The module's own tests run with
`cargo test -p guardian-module-sdk --example note-policy-serve`.

## Shipping your module

- Use `serve(YourModule).instance(id, token)` from the SDK, or implement the
  `GuardianModule` service from the proto in any language with gRPC support.
- Keep the module stateless for now. Limits that count across transactions
  (daily caps) need the commit notifications of the RFC's M2b slice.
- The SDK's conformance tests
  ([`tests/conformance.rs`](../../../crates/guardian-module-sdk/tests/conformance.rs))
  check that `basic-policy` decides the same in process and over gRPC; copy the
  pattern to test your module over the real transport.
- Prefer the remote path for code you do not own. In process, the rule that a
  module can only refuse, never sign, holds by code review rather than by
  isolation.
- A custom server build leaves the published image, so your release process has
  to rebuild it for every Guardian release.
