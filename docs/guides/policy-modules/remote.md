# Remote modules

A remote module is its own service that implements the `GuardianModule` gRPC
API from
[`guardian_module_v1.proto`](../../../crates/guardian-module-sdk/proto/guardian_module_v1.proto).
The Guardian finds it through the catalog's `endpoint`, and the module
describes itself when the Guardian asks. This page runs `basic-policy` as a
remote service, so the only difference from the [built-in](./built-in.md) page
is the transport. Do the [setup](./README.md#setup) first; the catalog is
[`remote.toml`](./remote.toml).

## How the Guardian talks to it

| Call | When | Purpose |
|---|---|---|
| `Describe` | Startup, and before enabling | Name, version, protocol version and supported hooks. A mismatch with the catalog stops startup. |
| `ValidateParams` | Startup, and on every params write | The module rejects params it cannot use; nothing is stored. |
| `Evaluate` | Every gated request | One request in, one decision out, within `timeout_ms`. |

Each request carries the instance id, the params to apply (global or the
account's layer), the before and after account views, the decoded transaction
projection, the raw transaction summary, the verified submitter and proposer,
and the proposal's unverified metadata. The module keeps no params of its own,
so params changed through the API apply on the next request without
restarting it.

## 1. Token and module

Each instance authenticates the Guardian with a bearer token, sent on every
call:

```sh
openssl rand -hex 32 > .guardian/caps.token
cargo run -p guardian-module-sdk --example basic-policy-serve -- \
  --listen 127.0.0.1:7443 --instance caps-remote .guardian/caps.token
```

The serve harness accepts a call only for an instance id it knows, with that
instance's token. One process can host several instances, each with its own
token: repeat `--instance`.

## 2. Catalog and server

```sh
cp docs/guides/policy-modules/remote.toml .guardian/
```

Replace the faucet placeholder, set `GUARDIAN_MODULES_CONFIG=.guardian/remote.toml`
in the guide's `.env`, and start the server as in the setup. The module must
already be running: an enabled instance that cannot be reached stops startup.
The log line lists `caps-remote=basic-policy 0.18.1`. The token path resolves
against the catalog's directory.

## 3. Try it

| Test | How | Expected |
|---|---|---|
| Remote refusal | Pay 4 | Refused by `caps-remote` (cap 3); the in-process `caps` (cap 5) allows it |
| Remote params | `admin PUT /modules/caps-remote/params '{"params":{"max_amount_per_tx":{"0xFAUCET":"1"}},"reason":"test"}'`, then pay 2 | Refused; the module was not restarted |
| Module down, fail closed | Stop the module, pay 1 | `503 GUARDIAN_MODULE_UNAVAILABLE`, module `caps-remote` |
| Switch it off while down | `admin POST /modules/caps-remote/disable '{"reason":"module down"}'`, pay 1 | Allowed; start the module again and `/enable` (which re-runs `Describe` and `ValidateParams`) |
| Fail open | Set `on_error = "allow"`, restart the Guardian, stop the module, pay 1 | Allowed; the delta's `metadata.modules.failed_open` names `caps-remote` |
| Wrong token | Start the module with a different token file, restart the Guardian | Startup fails: the module rejects `Describe` |

## Transport security

- `grpc://host:port` is always TLS, verified against the system roots or a
  private CA. Mutual TLS is optional:

  ```toml
  endpoint = "grpc://risk.internal:7443"
  tls = { ca_file = "risk-ca.pem", client_cert_file = "guardian.pem", client_key_file = "guardian.key" }
  ```

- Plain text needs the explicit `grpc+insecure://` scheme and a loopback host
  (`127.0.0.1`, `localhost`, `::1`), so a missing letter cannot turn TLS off.
- In the prod stage (`GUARDIAN_ENV=prod`) plain text is refused and `auth` is
  mandatory for every remote instance.
- The SDK's serve harness speaks plain text only, so for TLS put it behind a
  TLS-terminating proxy on the module's host, or serve the module with your own
  tonic server configured for TLS.

## What a remote module cannot do

It cannot sign, change its own params or hooks, or see secrets: catalog params
with secret references are refused for remote kinds. The worst a compromised
or failing remote module can do is refuse (or, with `on_error = "allow"`, be
skipped), and the operator can switch it off if the catalog allows it.
