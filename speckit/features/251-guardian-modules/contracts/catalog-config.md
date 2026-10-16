# Contract: Module Catalog Configuration

**Source**: RFC 0002 section 2.2. Loaded from the path in `GUARDIAN_MODULES_CONFIG` at startup; unset means no pipeline.

## Environment variables

| Variable | Default | Slice | Meaning |
|---|---|---|---|
| `GUARDIAN_MODULES_CONFIG` | unset | M2a | Path to the catalog TOML; unset disables the feature |
| `GUARDIAN_MODULES_STATE` | `<GUARDIAN_METADATA_PATH>/modules` | M2a | Filesystem backend only: directory of the module state file; Postgres keeps module state in its tables |
| `GUARDIAN_MODULE_TIMEOUT_MAX_MS` | `1000` | M2a | Upper bound for any instance `timeout_ms` |
| `GUARDIAN_MODULE_EVALUATION_BUDGET_MS` | `1000` | M2a | Total module time per hook invocation |
| `GUARDIAN_MODULE_REQUEST_MAX_BYTES` | `2097152` | M2a | Encoded request size cap |
| `GUARDIAN_MODULE_BREAKER_FAILURES` | `5` | M2a | Consecutive failures that open the circuit |
| `GUARDIAN_MODULE_PROBE_INTERVAL_MS` | `5000` | M2a | Health probe interval |
| `GUARDIAN_MODULE_PENDING_MAX` | `256` | M2b | Backlog cap per account and module |
| `GUARDIAN_MODULE_EVENT_RETENTION` | `14d` | M2b | Undelivered event retention |

## File format

```toml
[[module]]
id = "signer-policy"                  # required, unique
kind = "basic-policy"                 # required, registered kind
scope = "all"                         # all | default | assigned
hooks = ["before_proposal_created", "before_transaction_approved", "before_account_registered"]
timeout_ms = 20                       # required
on_error = "deny"                     # deny (default) | allow
enabled = true                        # seed only
operator_can_disable = false
params = { signer_check = "all", signer_allowlist = ["0x…"] }  # seed for the global params

[[module]]
id = "risk-vendor"
kind = "grpc"
endpoint = "grpc://risk:7443"
scope = "assigned"
hooks = ["before_transaction_approved"]
timeout_ms = 300
on_error = "allow"
auth = { token_secret = "guardian/modules/risk-vendor-token" }
tls = { ca_file = "risk-ca.pem" }     # optional; client_cert / client_key secret references add mTLS
```

## Disclosure (RFC 0002 section 2.7)

| Field | Values | Effect |
|---|---|---|
| `disclosure` | `none` (default), `summary`, `params` | What the policy summary shows the account's cosigners |
| `description` | Text, at most 500 characters | Required unless `disclosure = "none"` |
| `client_reason` | `full` (default), `generic` | `generic`: denials carry only `reason_code = "policy_denied"` |
| `dry_run` | `true` (default), `false` | Whether the dry-run check may call the instance |

## Remote transport

| Endpoint | Transport | Allowed |
|---|---|---|
| `grpc://host:port` | TLS, verified against the system roots or `tls.ca_file` | Always |
| `grpc+insecure://host:port` | Plain text | Loopback hosts only (`localhost`, `127.0.0.1`, `::1`), never in the prod stage |

Plain text needs its own scheme so a missing letter cannot turn TLS off. In the prod stage every remote instance must use `grpc://` and set `auth`, or startup stops. `auth` and `tls` are allowed only on remote kinds.

The prototype (#554) reads secrets from files instead of the secrets backend: `auth = { token_file = "..." }` and `tls = { ca_file, client_cert_file, client_key_file }`, paths relative to the catalog file.

## Kinds

`basic-policy` (in process) and `grpc` (remote) ship with the Guardian. A custom build registers more in-process kinds with `ServerBuilder::with_module_kind(name, factory)` (M2c, prototyped in #554); a catalog entry names one in `kind`, with no `endpoint`. A kind name is taken once, and the first-party names cannot be replaced. The stock binary refuses a catalog that names a kind it does not know.

## `basic-policy` params (M2a)

| Param | Type | Effect |
|---|---|---|
| `max_amount_per_tx` | map faucet id to decimal string | For each listed faucet, deny if the gross amount sent exceeds it: the sum of that faucet's fungible amounts over every output note, full or partial. Input notes consumed in the same transaction do not offset it, the fee note (`tx_fee`) is not counted, faucets not listed are uncapped, non-fungible assets are not counted (use `asset_allowlist`) |
| `recipient_allowlist` | list of account ids | Deny a payment note whose `p2id_target` is not listed; partial or non-P2ID notes deny |
| `recipient_denylist` | list of account ids | Deny a payment note whose `p2id_target` is listed |
| `deny_unknown_recipients` | bool, default `false` | With `recipient_denylist`, also deny partial and non-P2ID payment notes, whose recipient cannot be checked |
| `asset_allowlist` | list of faucet ids | Deny an unlisted faucet among assets sent in payment notes or received in input notes; the fee note (`tx_fee`) is not checked |
| `signer_allowlist` / `signer_denylist` | list of signer commitments | Check signers per `signer_check`; at most one of the two |
| `signer_check` | `added` (default) or `all` | `added`: signers in `after` not in `before`; `all`: every signer in `after` |

`params` in the file seed the global params once (section "Layered parameters" of RFC 0002 section 2.2); afterwards they are changed through the operator API, and a changed file value is reported as drift. Per-account settings never live in this file.

Unknown params fail `validate_params`. Hooks supported: `before_transaction_approved`, `before_proposal_created`, `before_account_registered`; transaction-only params are ignored at registration.
