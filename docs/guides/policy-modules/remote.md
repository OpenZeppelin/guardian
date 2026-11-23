# Remote modules

A remote module is its own service that implements the `GuardianModule` gRPC
API from
[`guardian_module_v1.proto`](../../../crates/guardian-module-sdk/proto/guardian_module_v1.proto).
The Guardian finds it through the catalog's `endpoint`, and the module
describes itself when the Guardian asks. This page runs the built-in
`spending-limit` policy as a remote service, so the only difference from the
[built-in](./built-in.md) page is the transport. Do the [setup](./README.md#setup) first; the catalog is
[`remote.toml`](./remote.toml).

## How the Guardian talks to it

| Call | When | Purpose |
|---|---|---|
| `Describe` | Startup, before enabling, and every 5 s as a health probe | Name, version, protocol version and supported hooks. A mismatch with the catalog stops startup. |
| `ValidateParams` | Startup, and on every params write | The module rejects params it cannot use; nothing is stored. |
| `Evaluate` | Every gated request | One request in, one decision out, within `timeout_ms`. |

Each request carries the instance id, the params to apply (global or the
account's layer), the before and after account views, the decoded transaction
projection, the raw transaction summary, the verified submitter and proposer,
and the proposal's unverified metadata. The module keeps no params of its own,
so params changed through the API apply on the next request without
restarting it.

To write your own remote module, in Rust or any language with gRPC, read
[custom modules](./custom.md): the protocol, every request field, the rules for
decisions, dry runs and the conformance suite. This page is about running one.

## 1. Token and module

Each instance authenticates the Guardian with a bearer token, sent on every
call:

```sh
(umask 077; openssl rand -hex 32 > .guardian/caps.token)
cargo run -p guardian-policy-module -- --kind spending-limit \
  --listen 127.0.0.1:7443 --instance caps-remote .guardian/caps.token
```

The module is [`examples/policy-module`](../../../examples/policy-module/README.md),
built on the SDK's serve harness; `--kind` picks which of the four built-in
policies it serves. It accepts a call only for an instance id it
knows, with that instance's token. One process can host several instances, each
with its own token: repeat `--instance`.

## 2. Catalog and server

```sh
cp docs/guides/policy-modules/remote.toml .guardian/
```

Replace the faucet placeholder, set `GUARDIAN_MODULES_CONFIG=.guardian/remote.toml`
in the guide's `.env`, and start the server as in the setup. The module must
already be running: an enabled instance that is new to this Guardian and
cannot be reached stops startup. Once the instance has started here, a later
start during a module outage succeeds with the instance unavailable (its
accounts get `GUARDIAN_MODULE_UNAVAILABLE`, or skip it with
`on_error = "allow"`) until a health probe passes its startup checks.
The log line lists `caps-remote=spending-limit <version>`.

`auth.token_secret` names a secret in the Guardian's secrets backend (see
[Credentials](#credentials)); here the `file` provider reads it from
`.guardian/caps.token`.

## 3. Try it

| Test | How | Expected |
|---|---|---|
| Remote refusal | Pay 4 | Refused by `caps-remote` (cap 3); the in-process `caps` (cap 5) allows it |
| Remote params | `admin PUT /modules/caps-remote/params '{"params":{"max_per_transaction":{"<faucet id in 0x hex>":"1"}},"reason":"test"}'`, then pay 2 | Refused; the module was not restarted |
| Module down, fail closed | Stop the module, pay 1 | `503 GUARDIAN_MODULE_UNAVAILABLE`, module `caps-remote` |
| Switch it off while down | `admin POST /modules/caps-remote/disable '{"reason":"module down"}'`, pay 1 | Allowed; start the module again and `/enable` (which re-runs `Describe` and `ValidateParams`) |
| Fail open | Set `on_error = "allow"`, restart the Guardian, stop the module, pay 1 | Allowed; the delta's `modules.failed_open` on the dashboard delta detail names `caps-remote` |
| Wrong token | Start the module with a different token file, restart the Guardian | Startup fails: the module rejects `Describe` |

## Transport security

- `grpc://host:port` is always TLS, verified against the system roots or a
  private CA. Mutual TLS is optional:

  ```toml
  endpoint = "grpc://risk.internal:7443"
  tls = { ca_file = "risk-ca.pem", client_cert_secret = "guardian/modules/risk-cert", client_key_secret = "guardian/modules/risk-key" }
  ```

  `ca_file` is a path (relative to the catalog's directory); the client
  certificate and key are secret names, like the token.

- Plain text needs the explicit `grpc+insecure://` scheme and a loopback host
  (`127.0.0.1`, `localhost`, `::1`), so a missing letter cannot turn TLS off.
- In the prod stage (`GUARDIAN_ENV=prod`) plain text is refused and `auth` is
  mandatory for every remote instance.
- The example module serves TLS and mutual TLS itself (`--tls-cert`,
  `--tls-key`, `--client-ca`); see its
  [README](../../../examples/policy-module/README.md#tls). A module built on
  the SDK harness does the same by adding the harness service to a tonic
  server configured for TLS.

## Credentials

The token and the mTLS client certificate and key are secrets, named in the
catalog and read once at startup through the secrets backend that
`GUARDIAN_MODULE_SECRET_PROVIDER` selects:

| Provider | Default for | A secret name is |
|---|---|---|
| `file` | every stage but prod | A path below `GUARDIAN_MODULE_SECRET_DIR` (the catalog's directory when unset). The file must be readable by its owner only (`0600`), or startup refuses it |
| `aws` | the prod stage | An AWS Secrets Manager secret id, read with the task's credentials (`AWS_REGION` required) |

An enabled instance whose secret cannot be read stops startup; a disabled one
stays disabled. Changing a secret takes effect at the next restart.

## What a remote module cannot do

It cannot sign, change its hooks, or see secrets: catalog params with secret
references are refused for remote kinds. It can rewrite the params it is given
into its own normalized form when they are validated (at startup, on enable
and on every operator write), within the params size cap; the Guardian stores
that form, and with `disclosure = "params"` cosigners see it. The worst a compromised
or failing remote module can do is refuse (or, with `on_error = "allow"`, be
skipped), and the operator can switch it off if the catalog allows it.
