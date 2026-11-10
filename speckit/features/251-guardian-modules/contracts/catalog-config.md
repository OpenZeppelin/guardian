# Contract: Module Catalog Configuration

**Source**: RFC 0002 section 2.2. Loaded from the path in `GUARDIAN_MODULES_CONFIG` at startup; unset means no pipeline.

## Environment variables

| Variable | Default | Slice | Meaning |
|---|---|---|---|
| `GUARDIAN_MODULES_CONFIG` | unset | M2a | Path to the catalog TOML; unset disables the feature |
| `GUARDIAN_MODULES_STATE` | `<GUARDIAN_METADATA_PATH>/modules` | M2a | Filesystem backend only: directory of the module state file; Postgres keeps module state in its tables |
| `GUARDIAN_MODULE_PARAMS_MAX_BYTES` | `65536` | M2a | Largest params value (JCS-encoded) accepted at startup or through the operator API |
| `GUARDIAN_MODULE_CHECK_RATE_PER_MIN` | `30` | M2a | Dry-run checks per minute per account and signer (RFC 0002 section 2.7) |
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
tls = { ca_file = "risk-ca.pem" }     # optional; client_cert_secret and client_key_secret add mTLS
```

## Disclosure (RFC 0002 section 2.7)

| Field | Values | Effect |
|---|---|---|
| `disclosure` | `none` (default), `summary`, `params` | What the policy summary shows the account's cosigners; `params` is refused for an instance whose params contain secret references |
| `description` | Text, at most 500 characters | Required unless `disclosure = "none"` |
| `client_reason` | `full` (default), `generic` | `generic`: denials carry only `reason_code = "policy_denied"` |
| `dry_run` | `true` (default), `false` | Whether the dry-run check may call the instance |

## Remote transport

| Endpoint | Transport | Allowed |
|---|---|---|
| `grpc://host:port` | TLS, verified against the system roots or `tls.ca_file` | Always |
| `grpc+insecure://host:port` | Plain text | Loopback hosts only (`localhost`, `127.0.0.1`, `::1`), never in the prod stage |

Plain text needs its own scheme so a missing letter cannot turn TLS off. In the prod stage every remote instance must use `grpc://` and set `auth`, or startup stops. `auth` and `tls` are allowed only on remote kinds.

## Secrets

`auth.token_secret`, `tls.client_cert_secret`, `tls.client_key_secret` and `{ "$secret" = "<name>" }` in the params of an in-process instance name secrets; `tls.ca_file` is a path, relative to the catalog's directory. A name is 1 to 512 characters of letters, digits and `/_+=.@-`, and a `$secret` object has no other key. Secrets are read once, when the instances are created, through `GUARDIAN_MODULE_SECRET_PROVIDER`:

| Provider | Default | Name |
|---|---|---|
| `file` | Outside the prod stage | Path below `GUARDIAN_MODULE_SECRET_DIR` (default: the catalog's directory), no `.` or `..` segments, file mode `0600` |
| `aws` | Prod stage | AWS Secrets Manager secret id; needs `AWS_REGION` |

A secret that cannot be read fails the instance's creation: startup stops for an enabled instance, and a disabled one stays disabled. The token is trimmed of surrounding whitespace. `$secret` values reach only the in-process factory (`ModuleInstanceConfig::secret`); `HookRequest.params` keeps the references, and they are refused in remote params, account layers and the operator API.

## Kinds

`basic-policy` (in process) and `grpc` (remote) ship with the Guardian. A custom build registers more in-process kinds with `ServerBuilder::with_module_kind(name, factory)` (M2c, prototyped in #554); a catalog entry names one in `kind`, with no `endpoint`. A kind name is taken once, and the first-party names cannot be replaced. The stock binary refuses a catalog that names a kind it does not know.

## `basic-policy` params (M2a)

Every id is written in `0x` hex form (case-insensitive); `validate_params` refuses any other form, such as a bech32 address or a placeholder that is not hex digits, which would never match a request. A modifier without the list it modifies (`deny_unknown_recipients`, `signer_check`, `code_check`) and a cap naming one faucet twice are refused too.

| Param | Type | Effect |
|---|---|---|
| `max_amount_per_tx` | map faucet id to decimal string | For each listed faucet, deny if the gross amount sent exceeds it: the sum of that faucet's fungible amounts over every output note, full or partial. Input notes consumed in the same transaction do not offset it, the fee note (`tx_fee`) is not counted, faucets not listed are uncapped, non-fungible assets are not counted (use `asset_allowlist`) |
| `recipient_allowlist` | list of account ids | Deny a payment note whose `p2id_target` is not listed; partial or non-P2ID notes deny |
| `recipient_denylist` | list of account ids | Deny a payment note whose `p2id_target` is listed |
| `deny_unknown_recipients` | bool, default `false` | With `recipient_denylist`, also deny partial and non-P2ID payment notes, whose recipient cannot be checked |
| `asset_allowlist` | list of faucet ids | Deny an unlisted faucet among assets sent in payment notes or received in input notes; the fee note (`tx_fee`) is not checked |
| `signer_allowlist` / `signer_denylist` | list of signer commitments | Check signers per `signer_check`; at most one of the two |
| `signer_check` | `added` (default) or `all` | `added`: signers in `after` not in `before`; `all`: every signer in `after` |
| `allowed_code_commitments` | list of code commitments | Code allowlist, checked per `code_check` |
| `code_check` | `changed` (default) or `all` | `changed`: `after.code_commitment` at registration and `new_code_commitment` when a transaction replaces code; `all`: `after.code_commitment` at every gate hook |

`params` in the file seed the global params once (section "Layered parameters" of RFC 0002 section 2.2); afterwards they are changed through the operator API, and a changed file value is reported as drift. Per-account settings never live in this file.

Unknown params fail `validate_params`. Hooks supported: `before_transaction_approved`, `before_proposal_created`, `before_account_registered`; transaction-only params are ignored at registration. A transaction request without a Miden projection is a module error (`on_error` applies), not a deny.
