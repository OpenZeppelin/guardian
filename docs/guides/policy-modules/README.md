# Policy modules

Run a local Guardian with policy modules: spending caps, recipient and signer
rules, and your own checks, consulted before the Guardian signs. A module only
answers allow or deny. A deny refuses the Guardian's acknowledgement, so the
transaction cannot execute; a module can never produce a signature.

> **Not released yet.** Modules are implemented on the
> [`251-modules-m2a`](https://github.com/OpenZeppelin/guardian/tree/251-modules-m2a)
> branch and are not in a release. The design is
> [RFC 0002](../../rfcs/0002-guardian-modules.md). No published image includes
> modules yet, so this guide runs the server from source instead of Docker
> Compose.

This guide assembles the pieces; the reference lives elsewhere:
[`CONFIGURATION.md`](../../CONFIGURATION.md#runtime-policy-modules) for the
variables and [`DASHBOARD.md`](../../DASHBOARD.md#policy-modules) for the
operator endpoints and permissions.

## Three ways to run a module

| Page | Module runs | Use it for |
|---|---|---|
| [Built-in](./built-in.md) | Inside the Guardian process (`spending-limit`, `recipient-policy`, `signer-policy`, `code-policy`) | Spending caps and allowed assets, recipient lists, signer lists, code pinning, with no extra service |
| [Remote](./remote.md) | As its own service over gRPC | A module operated, scaled or upgraded apart from the Guardian, or by another team |
| [Custom](./custom.md) | Your code, written with `guardian-module-sdk`, served over gRPC or compiled into your own server build | Rules the built-in policies cannot express |

For a complete worked example of a custom remote module, see
[KYC and sanctions screening](./compliance.md): signers must be KYC-verified
and payments are screened against a blocklist or an allowlist read from your
own data.

## How evaluation works

- **Catalog.** A TOML file (`GUARDIAN_MODULES_CONFIG`) declares every module
  instance: its kind, the hooks it runs at, its scope, its timeout and what
  happens when it fails. It is read once at startup. Instances cannot be added
  or reconfigured at runtime.
- **Hooks.** `before_proposal_created` filters a proposal before it is stored;
  `before_transaction_approved` runs before the Guardian signs, for a pushed
  delta and for a Guardian execution alike, and is the one that enforces,
  because transactions signed offline skip proposals;
  `before_account_registered` runs when an account is configured.
- **Scope.** A `scope = "all"` instance applies to every account. A
  `scope = "assigned"` instance applies only to accounts an operator assigns it
  to.
- **Params.** The catalog's `params` seed an instance's global params on first
  start. After that, operators change them through the API, and can add a
  per-account layer on an `all` instance. The layer is evaluated as a second
  pass and both must allow, so it can only add restrictions.
- **Failure.** If a module times out or errors, `on_error = "deny"` (the
  default) refuses with `503 GUARDIAN_MODULE_UNAVAILABLE`; `"allow"` continues
  and records the failure on the signed delta.
- **Refusal.** A deny returns `403 GUARDIAN_MODULE_DENIED` with `module`,
  `reason_code` and `layer` (`global` or `account`) in `meta`, plus the
  module's `client_message` when it sets one. The Rust and TypeScript clients
  surface it as a typed refusal (`ClientError::module_refusal`,
  `GuardianHttpError.moduleRefusal()`). A Guardian execution refused this way ends `failed` with
  `GUARDIAN_EXECUTION_MODULE_DENIED` (same `meta`) or
  `GUARDIAN_EXECUTION_MODULE_UNAVAILABLE` (`meta.module`, omitted like the
  denial's for a generic instance), and its proposal is
  kept.

## What users see

Each instance decides how much the account's cosigners learn
([RFC 0002 section 2.7](../../rfcs/0002-guardian-modules.md#27-policy-discovery-and-disclosure)):

| Catalog field | Values | Effect |
|---|---|---|
| `client_reason` | `full` (default), `generic` | `generic` turns a denial into `meta = { reason_code: "policy_denied" }`: no module, layer or message. Use it for screening, where telling a user why could itself be prohibited. The server log still has the full decision. |
| `disclosure` | `none` (default), `summary`, `params` | What a policy summary for cosigners will show. `summary` and `params` need a `description`. |
| `description` | Text, at most 500 characters | Shown to cosigners, for example "At most 5 per transaction". |
| `dry_run` | `true` (default), `false` | Whether a dry-run check may call the instance. |

Cosigners use two client endpoints:

- `GET /policies?account_id=…` lists the disclosed policies (Rust
  `get_policies`, TypeScript `getPolicies`).
- `POST /delta/proposal/check` runs proposal creation without storing
  anything: it answers `{ "allowed": true, "complete": … }` or the error
  creation would return. At most `GUARDIAN_MODULE_CHECK_RATE_PER_MIN` (30)
  checks per account and signer per minute.

The multisig SDKs wrap both: Rust `get_policies`, `check_transaction` and
`check_custom_transaction`, TypeScript `getPolicies` and `checkTransaction`,
which build the transaction exactly as proposing it would.

An instance whose catalog params contain secret references cannot use
`disclosure = "params"`: the server refuses to start, since the reference
would show cosigners the secret's name.

Such an instance's global params stay in the catalog file: the operator API
refuses to change them, and the database keeps only an empty placeholder. If a
later catalog drops the secret references, the instance becomes
operator-managed: on that start the new catalog params apply and are stored, a
warning is logged, and from then on operators change them through the API like
any other instance's. The other way round, adding secret references to an
instance operators managed, makes the catalog params apply and logs that the
stored operator params no longer do. (An instance first stored before this
release keeps the old behaviour on that first switch: it starts from the empty
placeholder, so set its params through the API right after that deploy.)

In
[`built-in.toml`](./built-in.toml), `caps` discloses its params and
`blocklist` denies generically.

## Setup

Run everything from the repository root, on the `251-modules-m2a` branch.

### 1. Operator key

The module endpoints need an operator session. Generate a Falcon key with the
CLI that ships with the server:

```sh
mkdir -p .guardian
cargo run -p guardian-server --example modules_admin -- keygen .guardian/operator.key
```

It prints the key's commitment and an allowlist entry. Copy
[`operators.example.json`](./operators.example.json) to
`.guardian/operators.json` and paste the printed `public_key` into it. The
example grants all four permissions the module endpoints use. `.guardian/` is
gitignored, so keys and tokens stay out of commits.

### 2. Catalog and environment

```sh
cp docs/guides/policy-modules/built-in.toml .guardian/
cp docs/guides/policy-modules/.env.example docs/guides/policy-modules/.env
cargo run -p guardian-server --bin ack-keygen -- --out-dir .guardian/modules-guide/ack
```

`ack-keygen` writes the Guardian's signing keys, which the guide's `.env` loads.
Accounts are bound to these keys when they register, so they must survive the
restarts the guide asks for; without them each start makes new keys.

Edit `.guardian/built-in.toml` for your network: replace `0xREPLACE_WITH_FAUCET_ID`
with the faucet id of an asset you hold, in `0x` hex form or as its bech32
address (stored as hex). The server does not start until you do: the built-in
policies refuse a placeholder, since it would never match a transaction. To
find the id, open the demo (step 5) with your account and choose `s` (Show
account details): the vault lists each asset with its full faucet id. The cap
applies only to the faucets it names; a payment in any other asset is not
capped, unless `allowed_assets` limits which assets may be sent (see
[built-in policies](./built-in.md#spending-limit)). The other pages swap in
`remote.toml` or `custom.toml` by changing `GUARDIAN_MODULES_CONFIG` in the
guide's `.env`.

### 3. Start the server

```sh
set -a && . docs/guides/policy-modules/.env && set +a
cargo run -p guardian-server --bin server
```

Exported variables take precedence over the repository's own `.env`, but the
server still reads that file for anything the guide's `.env` leaves unset. If
it sets `GUARDIAN_MODULES_CONFIG`, that catalog loads whenever the guide's
`.env` is not loaded in the shell, and to run without modules you must
`unset GUARDIAN_MODULES_CONFIG` and remove the line from the repository's
`.env` too (an empty value is refused at startup). The server uses the
filesystem backend unless built with `--features postgres`; module state then
goes to `GUARDIAN_METADATA_PATH/modules/module_state.json`. A Postgres build
uses `DATABASE_URL` instead, from either file, together with whatever module
state that database already holds.
Look for this line in the log:

```
Policy modules loaded catalog_hash=0x… params_version=… builds=caps=spending-limit <version>, …
```

The ports are fixed at 3000 (HTTP) and 50051 (gRPC); stop any other Guardian
first.

### 4. The operator CLI

Define a shell function for the CLI, then list the modules:

```sh
admin() { ./target/debug/examples/modules_admin --key .guardian/operator.key "$@"; }
admin GET /modules
```

`admin METHOD PATH [JSON]` logs in with the key and calls `/dashboard/PATH` on
`http://localhost:3000` (`--url` changes it). Each instance in the listing
shows its `enabled` flag, current params, `build`, and whether the stored
values have drifted from the catalog.

### 5. A client

Send transactions with the Rust demo, which surfaces module refusals with the
module and reason code:

```sh
cargo run -p guardian-demo
```

At its prompts, pick the network the guide's `.env` names
(`GUARDIAN_NETWORK_TYPE`, devnet by default; the demo's own default is local),
and use an account funded with the asset whose faucet id you put in the
catalog, or the cap has nothing to apply to.

Every transaction pays its fee from the account's own vault in the network's
native asset, the account's first transaction included, so fund a new account
with that asset before you test with it. An account without it fails while the
client builds the transaction, before the Guardian or any module sees it, with
`assertion failed with error code: 644413868907058392` (the vault holds less
than the fee). The fee note is not a payment: the cap does not count it.

Any client works; refusals are plain Guardian errors.

## Catalog fields

One `[[module]]` table per instance. A field the table does not list, or a
value outside these rules, stops startup with `invalid module catalog`.

| Field | Required | Values and default |
|---|---|---|
| `id` | yes | 1 to 64 characters of `a-z`, `0-9`, `_` and `-`, unique in the catalog. It names the instance in the operator API, logs, metrics and full denials |
| `kind` | yes | A built-in policy (`spending-limit`, `recipient-policy`, `signer-policy`, `code-policy`), `grpc` for a remote module, or a kind a custom build registers |
| `endpoint` | `grpc` only | `grpc://host:port` (TLS) or `grpc+insecure://127.0.0.1:port` (loopback only) |
| `scope` | yes | `all` (every account; an account layer can only add restrictions), `default` (copied to each account when it registers, then replaceable per account) or `assigned` (only where an operator assigns it) |
| `hooks` | yes | Non-empty list of `before_account_registered`, `before_proposal_created`, `before_transaction_approved`; each must be one the module supports |
| `timeout_ms` | yes | 1 to 1000 |
| `on_error` | no | `deny` (default) or `allow`: what happens when the module fails or does not answer |
| `params` | no | The instance's global params, seeded on first start and changed through the operator API afterwards. Params with `$secret` references stay in this file; they are refused for `grpc` instances and for `scope = "default"` (whose params are copied per account) |
| `enabled` | no | `true` (default). Read only the first time the instance is seen; the runtime flag applies afterwards |
| `operator_can_disable` | no | `false` (default). Whether the operator API may switch the instance off |
| `disclosure` | no | `none` (default), `summary` or `params`: what `get_policies` shows cosigners. `params` is refused when the params reference secrets |
| `description` | unless `disclosure = "none"` | Up to 500 characters, shown with `disclosure = summary` or `params` |
| `client_reason` | no | `full` (default: module, reason code and message) or `generic` (`policy_denied` only) |
| `dry_run` | no | `true` (default). Whether cosigners can check a transaction against the instance without submitting it |
| `auth` | `grpc` only | `{ token_secret = "<secret name>" }`; required in the prod stage |
| `tls` | `grpc` only | `ca_file` (relative to the catalog), and `client_cert_secret` with `client_key_secret` for mTLS |

`after_transaction_committed` and `require_fresh_state_ms` belong to stateful
modules, which are not in this release; a catalog naming them does not start.

## Operator API at a glance

| Task | Call |
|---|---|
| Change a global rule | `admin PUT /modules/{id}/params '{"params":{...},"reason":"..."}'` |
| Add a stricter rule for one account | `admin PUT /accounts/{account_id}/modules/{id}/params '{"params":{...},"reason":"..."}'` |
| Remove that account rule | `admin DELETE /accounts/{account_id}/modules/{id}/params '{"reason":"..."}'` |
| Assign opt-in modules to an account | `admin PUT /accounts/{account_id}/modules '{"modules":[{"module_id":"...","params":{...}}],"reason":"..."}'` |
| See what applies to an account | `admin GET /accounts/{account_id}/modules` |
| Switch an instance off or on | `admin POST /modules/{id}/disable '{"reason":"..."}'`, then `/enable` |
| See every change | `admin GET /modules/{id}/params/history` |
| See which configuration approved a delta | `admin GET /accounts/{account_id}/deltas/{nonce}`, field `modules` |

Every write is validated by the module before it is stored; invalid params
return `400` and change nothing. A module that is down cannot validate, so a
change to its params returns `503 GUARDIAN_MODULE_UNAVAILABLE` (not `400`),
while an assignment that keeps its entry unchanged is still accepted: entries
stored as they are sent are not validated again.

## Troubleshooting

For outages, a module that refuses too much, and break-glass steps, see the
[policy modules runbook](../../runbooks/policy-modules.md).

| Symptom | Cause |
|---|---|
| `cannot read module catalog` | `GUARDIAN_MODULES_CONFIG` is relative to the directory you started the server in; start from the repository root. |
| `module caps: invalid params: …` at startup | The catalog's seed params fail the module's validation on a first start: fix the catalog; nothing is stored until every instance passes. On a later start it is the stored params that fail (the catalog's are no longer read): see the [runbook](../../runbooks/policy-modules.md). |
| `GUARDIAN_MODULES_CONFIG is set but empty` or `declares no module` at startup | An empty value or an empty catalog would silently run without any policy. Point the variable at a catalog, or unset it to run without modules. |
| `module caps-remote: …` at startup | An enabled remote module rejected the token or answered incompatibly, or it is new to this Guardian and unreachable. Start the module first, or set `enabled = false` until it runs. A module that has started here before and is merely down does not stop startup: the instance starts unavailable and recovers on a health probe. |
| `relation "module_state_version" does not exist` | A Postgres database that ran an earlier prototype migration. Drop the `module_*` tables and the functions `guardian_module_snapshot` and `guardian_module_seed_assignment`, delete the module rows in `__diesel_schema_migrations` (versions `20261015000001`, `20261109000001`, `20261110000001` and `20261110000002`, whichever exist), then restart so every module migration runs again. |
| `GUARDIAN public key commitment 0x… does not match account commitment 0x…` | The server started with different signing keys from the ones the account registered with, usually because `GUARDIAN_ACK_SECRET_PROVIDER=file` was not set (Setup step 2). Keys made without it are gone after a restart: register a new account. |
| `login failed: no session cookie` | The server did not load the key: check `GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE` and the `public_key` in it. |
| The client shows a generic error | The server log names the module, reason code and layer for every refusal. |
