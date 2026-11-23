# Custom modules

Write your own module against `guardian-module-sdk`, then either serve it over
gRPC or compile it into your own server build. The worked example,
[`note_policy/mod.rs`](../../../crates/guardian-module-sdk/examples/note_policy/mod.rs),
is a module the built-in policies cannot express: it limits how many payment notes a
transaction creates and can refuse private notes, whose recipients the
Guardian cannot see. The same code runs both ways. Do the
[setup](./README.md#setup) first.

| | Over gRPC | In process |
|---|---|---|
| Runs | As its own service ([`note-policy-serve.rs`](../../../crates/guardian-module-sdk/examples/note-policy-serve.rs)) | Inside your server build ([`examples/custom-guardian`](../../../examples/custom-guardian/README.md)) |
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
Protocol version 1 freezes with the first published `guardian-module-sdk`
release; from then on Guardian releases add fields, hooks and named
capabilities, never a change that breaks a version 1 module. A module in
another language implements the `GuardianModule` service from that file with
any gRPC stack; the Rust trait above is a thin layer over it.

| RPC | When the Guardian calls it |
|---|---|
| `Describe` | At startup, before enabling an instance, and every `GUARDIAN_MODULE_PROBE_INTERVAL_MS` (5 s) as a health probe |
| `ValidateParams` | At startup for the stored global params, and on every params write through the operator API |
| `Evaluate` | Once per gate hook invocation the instance applies to, and again for an account's layer |

Every call carries `authorization: Bearer <token>` in its gRPC metadata, with
the token of the instance it is for, and `instance_id` in the message
(`Describe` names no instance). A gRPC error status other than
`INVALID_ARGUMENT` is a module failure, as is a call that outlives the
instance's `timeout_ms`, and the instance's `on_error` decides; repeated
failures open a circuit breaker that skips the module until a health probe
succeeds. `INVALID_ARGUMENT` says the request cannot be judged, and the
request is refused whatever `on_error` says (see status codes below). Requests can be up to 2 MiB encoded
(`GUARDIAN_MODULE_REQUEST_MAX_BYTES`), so keep your server's receive limit at
least that high (tonic's default is 4 MiB).

## Decisions

| Field | Rule |
|---|---|
| `effect` | `ALLOW` or `DENY`, set by `Decision::allow()` and `Decision::deny(..)`. A value the Guardian does not know is a module failure, never an allow. A module that leaves it unspecified has failed: its instance's `on_error` applies |
| `reason_code` | Required on a deny: 1 to 64 characters of `a-z`, `0-9` and `_`. A deny with any other code is still a deny, but clients get the generic `policy_denied` and the Guardian logs a warning. Stable codes let operators and clients act on them |
| `message` | For the operator: logged with the decision, never sent to clients |
| `client_message` | Optional, for the person who sent the request, via `.with_client_message(..)`. Plain text: the Guardian drops control characters, collapses whitespace and keeps at most 256 characters. Shown only for instances with `client_reason = "full"` (the default); with `"generic"` the client sees `reason_code = "policy_denied"` and nothing else |

Return `ModuleError::InvalidRequest` for a request you cannot judge (a
missing projection, params you cannot read). The request is refused with
`GUARDIAN_MODULE_UNAVAILABLE` whatever `on_error` says, since a requester can
shape a request on purpose, so a module never allows by accident. Return
`ModuleError::Unavailable` or `ModuleError::Internal` for your own failures (a
data source that is down): those are module failures and `on_error` decides.

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
| `before_account_registered` | On every `/configure` of an account: its first configuration on this Guardian and each later re-configuration | `registration` |

A module lists the hooks it supports in `describe`; the catalog subscribes each
instance to some of them, and the Guardian refuses an instance subscribed to a
hook its module does not list.

| `HookRequest` field | Contents |
|---|---|
| `request_id` | Unique per hook invocation and shared by every module called in it; log it to join your logs with the Guardian's |
| `hook` | Which hook is calling |
| `dry_run` | `true` for a policy check: nothing is stored or signed whatever you decide (see [Dry runs](#dry-runs)) |
| `network`, `network_id` | `"miden"`, and the network this Guardian serves: `"mainnet"`, `"testnet"`, `"devnet"`, `"cedar"` or `"local"`. It comes from the Guardian's own configuration, never from the account's registration |
| `account_id`, `instance_id` | The account, and the catalog id of the instance being called |
| `params` | UTF-8 JSON: the instance's global params, the account's layer on its second pass, or the account's own params for a `default` or `assigned` instance. Read them on every call; they change without restarting the module |
| `accounting_time` | The time recorded with the transaction if it is signed; at `before_proposal_created` and in a dry run, the evaluation time |
| `submitter`, `submitter_session` | The verified signer commitment that authenticated the request, and whether it did so through a session key it delegated |
| `before` | The account as the Guardian holds it. At `before_account_registered` it is absent for a first configuration and set to the stored state for a re-configuration, so "no `before`" means "new to this Guardian", not "registration" |
| `after` | The account as it will be if every module allows |
| `subject.transaction.projection` | The decoded transaction; read the Miden variant with `subject.miden()` |
| `subject.transaction.proposer` | The verified signer commitment that created the matching proposal; empty when there is none |
| `subject.transaction.unverified_metadata` | UTF-8 JSON the proposer asserted; never base a decision on it alone |
| `subject.transaction.raw` | The transaction in its network's own encoding (`encoding = "miden.transaction_summary"`), for modules that decode it themselves; tied to the Guardian's Miden version |
| `subject.transaction.pending` | Empty until commit notifications ship (M2b) |
| `subject.registration.initial_state` | The account about to be accepted: its whole vault and every storage slot, map entries included. Read the Miden variant with `subject.miden()` |

An account view (`before`, `after`) carries the account `commitment`, its
`signer_commitments` with each signer's scheme in `signer_schemes` (`auth_scheme`
is `"falcon"`, `"ecdsa"` or `"mixed"`), `default_threshold` (absent for
single-key accounts), `procedure_thresholds`, `code_commitment` and the
`procedure_roots` the code exports.

The Miden projection has the `nonce_delta`, the `bound_block` (the block the
summary binds; the transaction executes against it or a later one), the
optional `expiration_delta` (blocks after the block it executes against, set by
its script, so not an absolute height), the optional
`approval_expiration_block` (the block at which the cosigners' approval
lapses; use this one for "valid until"), `vault_changes` (asset and
direction), `output_notes`
(`note_type`, assets, `p2id_target` for known P2ID and P2IDE scripts,
`reclaimer` for a P2IDE note whose reclaim is enabled, `tx_fee`), `input_notes`, `storage_changes` by slot name, and
`new_code_commitment` when the transaction replaces the account's code.
Amounts are decimal strings. Skip the fee note (`tx_fee = true`) when counting
payments: every transaction pays its fee through an output note. The Guardian
sets `tx_fee` only on the one note the fee payment creates, and only while it
carries no more of the fee asset than a transaction can owe (64 times the
chain's base fee), never on another note that merely uses the fee script, so
treat every other note as a payment.

A rule about who gets paid must judge `reclaimer` as well as `p2id_target`: the
creator of a P2IDE note picks the reclaimer freely, and once the note's reclaim
height is reached that account can consume it. A note to an allowed target with a
blocked reclaimer otherwise pays the blocked account. A reclaimer equal to the
paying account is the sender taking its own note back.

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
instances with `dry_run = true`: the creation instances, then the
`before_transaction_approved` ones. An instance on both hooks is asked once with
each, so a module that decides differently at approval is checked as it would
be then. Decide exactly as you would otherwise;
`evaluate` has no side effects either way. An operator who does not want an
instance to see transactions that may never be proposed sets `dry_run = false`
in its catalog entry, and the check then reports `complete: false`.

## Run it over gRPC

For a complete example with data of its own (KYC on signers, sanctions or
allowlists on recipients), see [KYC and sanctions screening](./compliance.md).

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

### Without the Rust SDK

A module in another language implements the `GuardianModule` service from
[`guardian_module_v1.proto`](../../../crates/guardian-module-sdk/proto/guardian_module_v1.proto).
What the Guardian expects on the wire:

- **Authentication.** Each call carries `authorization: Bearer <token>`, the
  instance's token. Check it against the instance the request names
  (`HookRequest.instance_id`), and refuse an unknown instance.
- **Status codes.** `INVALID_ARGUMENT` means "I cannot judge this request": the
  request is refused with `GUARDIAN_MODULE_UNAVAILABLE` whatever the instance's
  `on_error` says, since a requester can shape a request on purpose, and it
  does not count toward the circuit breaker. `UNAVAILABLE`, `DEADLINE_EXCEEDED` and `UNKNOWN`
  mean the module is down: they count toward the breaker, and on `Describe` at
  startup an already-known instance starts unavailable instead of stopping the
  server. Any other status (`UNAUTHENTICATED`, `PERMISSION_DENIED`, `INTERNAL`,
  ...) means the Guardian cannot use the module: on `Describe` it stops startup
  for a new instance and holds the breaker open until a health probe passes.
- **Decisions.** Set `effect` to `ALLOW` or `DENY`; an unset or unknown effect
  is a module failure. A deny needs a `reason_code` of 1 to 64 characters of
  `a-z`, `0-9` and `_` (another code is still a deny, but clients only see
  `policy_denied`).
- **Time.** `timeout_ms` bounds the whole call, including the TCP connect and
  the TLS handshake after the Guardian reconnects, so leave room for them.
- **Keepalive.** The Guardian sends HTTP/2 PINGs every 10 seconds on an idle
  connection and drops it after 5 seconds without an answer. gRPC servers that
  reject frequent pings by default (for example `grpc-go` and `grpc-java`
  enforcement policies) must permit pings every 10 seconds without active
  streams, or they close the connection with `too_many_pings`.
- **Size.** Requests can be up to `GUARDIAN_MODULE_REQUEST_MAX_BYTES` (2 MiB by
  default): raise your server's receive limit if it is lower.
- **Stateful modules** (commit notifications, counting across transactions)
  are not part of this protocol version yet; they arrive as a named
  capability, so a module written now keeps working.

The Rust conformance suite runs in process only, so port its cases (listed in
[Test it](#test-it)) to your language's test framework.

## Run it in process

A custom build is the official server plus one `with_module_kind` call per
kind. [`ServerBuilder::from_env`](../../../crates/server/src/builder/env.rs)
configures everything else from the environment exactly as the stock `server`
binary does, and `start` runs the startup account recovery
`GUARDIAN_STARTUP_ACCOUNT_RECOVERY` asks for before serving, as the binary
does, so there is no setup to copy or keep in step:

```rust
ServerBuilder::from_env()
    .await
    .expect("Failed to configure server")
    .with_module_kind("note-policy", Arc::new(NotePolicyFactory))
    .build()
    .await
    .expect("Failed to build server")
    .start()
    .await
    .unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(1)
    });
```

The factory gets each catalog instance's configuration and returns the module.
`build` fails if the kind name is already taken, including by a built-in
policy (`spending-limit`, `recipient-policy`, `signer-policy`, `code-policy`) or
`grpc`, and the server refuses to start with a catalog that enables an instance
of a kind it does not register. Builder methods called after `from_env`
override what the environment set. `from_env` and `build` return `BuildError`, a
`std::error::Error`, so a `main` returning `anyhow::Result` can use `?`.

Run the example build, [`examples/custom-guardian`](../../../examples/custom-guardian/README.md):

```sh
cp docs/guides/policy-modules/custom-in-process.toml .guardian/
# set GUARDIAN_MODULES_CONFIG=.guardian/custom-in-process.toml in the guide's .env
set -a && . docs/guides/policy-modules/.env && set +a
cargo run -p custom-guardian
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
the Guardian has answered the request.

### Your own build

Start from the `custom-guardian-<tag>.tar.gz` attached to each Guardian
release, not from this repository: it is the example as a standalone project
that depends on `guardian-server` and `guardian-module-sdk` at the release tag
and carries the release's `Cargo.lock`. The Guardian needs the exact Miden and
Plonky3 versions it was released with, and a freshly resolved lockfile can
pick versions that build but break transaction execution, so always build with
`--locked`. Add your crate, register its kind, and build the image with the
template's `Dockerfile`, which uses the official image's toolchain and base.
Its [README](../../../examples/custom-guardian/README.md) has the steps. A
different copy of `guardian-module-sdk` is a different trait, so the server
and your module must share the release's: the template's `[patch.crates-io]`
points a crate that depends on the crates.io SDK at the release tag, and that
crate must accept the release's SDK version.

On every Guardian release, start again from that release's template and
rebuild, sign and redeploy your image; the official image's provenance
attestation does not cover your build.

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
        .allows("one payment", one_payment())
        .denies_with("two payments", two_payments(), "too_many_payment_notes")
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
of at most 256 characters, that each request is answered as the suite states
(`allows`, `denies`, `denies_with` a reason code, or `fails` for a request the
module cannot judge), that one instance's token is refused for another,
and that a request at the 2 MiB size cap goes through. A suite needs at least
one request the module must deny: a module that allows everything answers
consistently too, so consistency alone does not conform. A module written in
another language can port these cases to its own tests. `note-policy` and the four built-in policies run it in
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
  to rebuild it from each release's `custom-guardian` template.
