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

Add `guardian-module-sdk` with the `conformance` feature as a dev-dependency to
test it (see [Test it](#test-it)). The crate re-exports `async_trait`, so
`#[guardian_module_sdk::async_trait]` goes on the `impl`.

## The protocol

Modules speak `guardian.module.v1`, defined in
[`guardian_module_v1.proto`](../../../crates/guardian-module-sdk/proto/guardian_module_v1.proto).
Protocol version 1 is frozen: later Guardian releases add fields, hooks and
named capabilities, never a change that breaks a version 1 module. A module in
another language implements the `GuardianModule` service from that file with
any gRPC stack; the Rust trait above is a thin layer over it.

| RPC | When the Guardian calls it |
|---|---|
| `Describe` | At startup, before enabling an instance, and every `GUARDIAN_MODULE_PROBE_INTERVAL_MS` (5 s) as a health probe |
| `ValidateParams` | At startup for the stored global params, and on every params write through the operator API |
| `Evaluate` | Once per gate hook invocation the instance applies to, and again for an account's layer |
| `Notify`, `NotifyGap` | Not called yet; commit notifications arrive with the M2b slice. The SDK answers them with success |

Every call carries `authorization: Bearer <token>` in its gRPC metadata, with
the token of the instance it is for, and `instance_id` in the message
(`Describe` names no instance). Any gRPC error status is a module failure, as
is a call that outlives the instance's `timeout_ms`, and the instance's
`on_error` decides; repeated failures open a circuit breaker that skips the
module until a health probe succeeds. Requests can be up to 2 MiB encoded
(`GUARDIAN_MODULE_REQUEST_MAX_BYTES`), so keep your server's receive limit at
least that high (tonic's default is 4 MiB).

## Decisions

| Field | Rule |
|---|---|
| `effect` | `ALLOW` or `DENY`, set by `Decision::allow()` and `Decision::deny(..)`. A value the Guardian does not know is a module failure, never an allow. A module that leaves it unspecified is read through `allow` |
| `reason_code` | Required on a deny: 1 to 64 characters of `a-z`, `0-9` and `_`. Anything else turns the deny into a module failure. Stable codes let operators and clients act on them |
| `message` | For the operator: logged with the decision, never sent to clients |
| `client_message` | Optional, for the person who sent the request, via `.with_client_message(..)`. Plain text: the Guardian drops control characters, collapses whitespace and keeps at most 256 characters. Shown only for instances with `client_reason = "full"` (the default); with `"generic"` the client sees `reason_code = "policy_denied"` and nothing else |

Return `ModuleError` for a request you cannot judge (a missing projection,
params you cannot read). It is a module failure and `on_error` decides, so a
module never allows by accident.

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

## Hooks and requests

| Hook | Called | `subject` |
|---|---|---|
| `before_proposal_created` | Before a multisig proposal is stored, and in dry runs | `transaction` |
| `before_transaction_approved` | Before the Guardian signs a pushed delta or a Guardian execution: the one that enforces, since transactions signed offline skip proposals | `transaction` |
| `before_account_registered` | When an account is first configured on this Guardian | `registration` |

A module lists the hooks it supports in `describe`; the catalog subscribes each
instance to some of them, and the Guardian refuses an instance subscribed to a
hook its module does not list.

| `HookRequest` field | Contents |
|---|---|
| `request_id` | Unique per hook invocation and shared by every module called in it; log it to join your logs with the Guardian's |
| `hook` | Which hook is calling |
| `dry_run` | `true` for a policy check: nothing is stored or signed whatever you decide (see [Dry runs](#dry-runs)) |
| `network`, `network_id` | `"miden"`, and `"mainnet"`, `"testnet"`, `"devnet"`, `"cedar"` or `"local"` |
| `account_id`, `instance_id` | The account, and the catalog id of the instance being called |
| `params` | UTF-8 JSON: the instance's global params, the account's layer on its second pass, or the account's own params for a `default` or `assigned` instance. Read them on every call; they change without restarting the module |
| `accounting_time` | The time recorded with the transaction if it is signed; at `before_proposal_created` and in a dry run, the evaluation time |
| `submitter`, `submitter_session` | The verified signer commitment that authenticated the request, and whether it did so through a session key it delegated |
| `before` | The account as the Guardian holds it; absent at `before_account_registered` |
| `after` | The account as it will be if every module allows |
| `subject.transaction.projection` | The decoded transaction; read the Miden variant with `subject.miden()` |
| `subject.transaction.proposer` | The verified signer commitment that created the matching proposal; empty when there is none |
| `subject.transaction.unverified_metadata` | UTF-8 JSON the proposer asserted; never base a decision on it alone |
| `subject.transaction.raw` | The transaction in its network's own encoding (`encoding = "miden.transaction_summary"`), for modules that decode it themselves; tied to the Guardian's Miden version |
| `subject.transaction.pending` | Empty until commit notifications ship (M2b) |
| `subject.registration.initial_state` | The account about to be accepted: vault and storage slots |

An account view (`before`, `after`) carries the account `commitment`, its
`signer_commitments` with each signer's scheme in `signer_schemes` (`auth_scheme`
is `"falcon"`, `"ecdsa"` or `"mixed"`), `default_threshold` (absent for
single-key accounts), `procedure_thresholds`, `code_commitment` and the
`procedure_roots` the code exports.

The Miden projection has the `nonce_delta`, the `reference_block` and optional
`expiration_block`, `vault_changes` (asset and direction), `output_notes`
(`note_type`, assets, `p2id_target` for known P2ID and P2IDE scripts,
`tx_fee`), `input_notes`, `storage_changes` by slot name, and
`new_code_commitment` when the transaction replaces the account's code.
Amounts are decimal strings. Skip the fee note (`tx_fee = true`) when counting
payments: every transaction pays its fee through an output note. The Guardian
sets `tx_fee` only on the one note the fee payment creates, never on another
note that merely uses the fee script, so treat every other note as a payment.

Projections are tagged by network (`TransactionProjection`, `oneof network`).
Miden is the only network today; read it with `subject.miden()` and return a
`ModuleError` when it is absent, so a module never judges a transaction it
does not understand.

## Personal data

Requests can carry personal data: the proposer's free-text `description` in
`unverified_metadata`, and recipients and amounts that identify people. For a
remote module the operator treats you as a data processor, so document what
your module stores, for how long and where it runs. A module that does not
need a field should not log it, and `message` (logged by the Guardian for
every denial) should not repeat it.

## Dry runs

Cosigners can check a transaction before proposing it
(`POST /delta/proposal/check`). The Guardian then calls the account's gate
instances with `dry_run = true`: the creation instances, then those subscribed
only to `before_transaction_approved`. Decide exactly as you would otherwise;
`evaluate` has no side effects either way. An operator who does not want an
instance to see transactions that may never be proposed sets `dry_run = false`
in its catalog entry, and the check then reports `complete: false`.

## Run it over gRPC

The [remote](./remote.md) page explains the transport.

```sh
(umask 077; openssl rand -hex 32 > .guardian/note-policy.token)
cargo run -p guardian-module-sdk --example note-policy-serve -- \
  --listen 127.0.0.1:7444 --instance note-policy .guardian/note-policy.token
cp docs/guides/policy-modules/custom.toml .guardian/
```

Set `GUARDIAN_MODULES_CONFIG=.guardian/custom.toml` in the guide's `.env` and
start the server as in the setup. The log line lists
`note-policy=note-policy <version>`.

The catalog names the token with `auth = { token_secret = "note-policy.token" }`;
the Guardian reads it through its secrets backend, and the module is started
with the same value. For TLS, mutual TLS and the AWS secrets provider, see
[remote modules](./remote.md#credentials) and
[`examples/policy-module`](../../../examples/policy-module/README.md), a
standalone service you can copy as a starting point.

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

No module process, token or endpoint is involved. An in-process module that
needs a credential of its own, such as a vendor API key, names it in its
catalog params as `{ "$secret" = "<name>" }`. The Guardian reads it at startup
and the factory gets the value from `ModuleInstanceConfig::secret("<name>")`;
`HookRequest.params` keeps the reference, never the value. To run it next to another
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

## Test it

The SDK's conformance suite runs your module in process and over loopback gRPC,
with two instances on one service:

```rust
use guardian_module_sdk::conformance::Suite;

#[tokio::test]
async fn note_policy_conforms() {
    Suite::new(NotePolicy)
        .request("one payment", one_payment())
        .request("two payments", two_payments())
        .valid_params(serde_json::json!({ "max_payment_notes": 1 }))
        .invalid_params(serde_json::json!({ "max_notes": 1 }))
        .run()
        .await
        .assert_conformant();
}
```

It checks that `describe` reports protocol version 1 and known hooks, that
params validate the same over both transports and that normalized params
validate to themselves, that every request gets the same answer in process and
over gRPC, repeated, in reverse order and concurrently (so `evaluate` keeps no
state), that every deny has a valid `reason_code` and a plain `client_message`
of at most 256 characters, that one instance's token is refused for another,
and that a request at the 2 MiB size cap goes through. A module written in
another language can port these cases to its own tests. `note-policy` and `basic-policy` run it in
[`tests/conformance.rs`](../../../crates/guardian-module-sdk/tests/conformance.rs).

## Shipping your module

- Use `serve(YourModule).instance(id, token)` from the SDK, or implement the
  `GuardianModule` service from the proto in any language with gRPC support.
- Keep the module stateless for now. Limits that count across transactions
  (daily caps) need the commit notifications of the RFC's M2b slice.
- Run the conformance suite in your CI, with requests that cover every rule.
- Prefer the remote path for code you do not own. In process, the rule that a
  module can only refuse, never sign, holds by code review rather than by
  isolation.
- A custom server build leaves the published image, so your release process has
  to rebuild it for every Guardian release.
