# Policy modules

Run a local Guardian with policy modules: spending caps, recipient and signer
rules, and your own checks, consulted before the Guardian signs. A module only
answers allow or deny. A deny refuses the Guardian's acknowledgement, so the
transaction cannot execute; a module can never produce a signature.

> **Prototype.** Modules are implemented on the
> [`251-modules-prototype`](https://github.com/OpenZeppelin/guardian/tree/251-modules-prototype)
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
| [Built-in](./built-in.md) | Inside the Guardian process (`basic-policy`) | Caps, asset and recipient lists, signer rules, with no extra service |
| [Remote](./remote.md) | As its own service over gRPC | A module operated, scaled or upgraded apart from the Guardian, or by another team |
| [Custom](./custom.md) | Your code, written with `guardian-module-sdk`, served over gRPC or compiled into your own server build | Rules `basic-policy` cannot express |

## How evaluation works

- **Catalog.** A TOML file (`GUARDIAN_MODULES_CONFIG`) declares every module
  instance: its kind, the hooks it runs at, its scope, its timeout and what
  happens when it fails. It is read once at startup. Instances cannot be added
  or reconfigured at runtime.
- **Hooks.** `before_proposal_created` filters a proposal before it is stored;
  `before_transaction_approved` runs before the Guardian signs, and is the one
  that enforces, because transactions signed offline skip proposals;
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
  `reason_code` and `layer` (`global` or `account`) in `meta`. The Rust clients
  surface it as a typed error.

## Setup

Run everything from the repository root, on the prototype branch.

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
```

Edit `.guardian/built-in.toml` for your network: replace `0xREPLACE_WITH_FAUCET_ID`
with the faucet id of an asset you hold. The other pages swap in
`remote.toml` or `custom.toml` by changing `GUARDIAN_MODULES_CONFIG` in the
guide's `.env`.

### 3. Start the server

```sh
set -a && . docs/guides/policy-modules/.env && set +a
cargo run -p guardian-server --bin server
```

Exported variables take precedence over the repository's own `.env`. The
server uses the filesystem backend unless built with `--features postgres`;
module state then goes to `GUARDIAN_METADATA_PATH/modules/module_state.json`.
Look for this line in the log:

```
Policy modules loaded catalog_hash=0x… params_version=… builds=caps=basic-policy 0.18.1, …
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

Any client works; refusals are plain Guardian errors.

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
| See which configuration approved a delta | `admin GET /accounts/{account_id}/deltas/{nonce}`, field `metadata.modules` |

Every write is validated by the module before it is stored; invalid params
return `400` and change nothing.

## Troubleshooting

| Symptom | Cause |
|---|---|
| `cannot read module catalog` | `GUARDIAN_MODULES_CONFIG` is relative to the directory you started the server in; start from the repository root. |
| `module caps: invalid params: …` at startup | The catalog's seed params, or the stored params on a later start, fail the module's validation. Fix the catalog; nothing is stored until every instance passes. |
| `module caps-remote: …` at startup | An enabled remote module is unreachable or rejected the token. Start the module first, or set `enabled = false` until it runs. |
| `relation "module_state_version" does not exist` | A Postgres database that ran an earlier prototype migration. Drop the `module_*` tables and the `20261015000001` row in `__diesel_schema_migrations`, then restart. |
| `login failed: no session cookie` | The server did not load the key: check `GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE` and the `public_key` in it. |
| The client shows a generic error | The server log names the module, reason code and layer for every refusal. |
