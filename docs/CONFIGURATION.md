# Configuration Reference

Every environment variable Guardian honours, in one place.

Use this as a lookup table — pair it with [`LOCAL_DEV.md`](./LOCAL_DEV.md)
for which combinations make sense locally and
[`SERVER_AWS_DEPLOY.md`](./SERVER_AWS_DEPLOY.md) for how the deploy script
sets them in production.

For Terraform variables (everything under `infra/`), see
[`infra/README.md`](../infra/README.md#variables-reference) — those are a
separate surface; the deploy script translates between Terraform vars and
the runtime env vars in this document.

## Conventions

- **Required** means the server will refuse to start if the variable is
  missing in the relevant build/feature combo.
- **Default** is what the server picks when the variable is unset.
- **Build mode** indicates which Cargo feature gate consumes the value.
  Defaults builds are the no-feature filesystem build unless stated.

## Runtime — server identity and storage

| Variable | Default | Build mode | Notes |
|---|---|---|---|
| `DATABASE_URL` | _required_ | `postgres` | Postgres connection string. Server panics at startup if unset under `--features postgres`. |
| `GUARDIAN_STORAGE_PATH` | `/var/guardian/storage` | filesystem | Path for state + delta blobs. |
| `GUARDIAN_METADATA_PATH` | `/var/guardian/metadata` | filesystem | Path for accounts, auth credentials, network config. |
| `GUARDIAN_KEYSTORE_PATH` | `/var/guardian/keystore` | any | Local Falcon/ECDSA key files (ACK signers and per-account creds). |
| `GUARDIAN_DB_POOL_MAX_SIZE` | `16` (code default); `32` set by the prod Terraform profile | `postgres` | Storage backend pool size. |
| `GUARDIAN_METADATA_DB_POOL_MAX_SIZE` | matches storage | `postgres` | Metadata backend pool size; usually leave equal. |
| `GUARDIAN_SERVER_FEATURES` | _build-time_ | deploy script | Comma list (`postgres`, `evm`) the deploy script compiles in. Not read at runtime — controls how the image is built. |

## Runtime — ACK signing and network

| Variable | Default | Notes |
|---|---|---|
| `GUARDIAN_ENV` | _unset_ | Set to `prod` to load ACK keys from AWS Secrets Manager. Anything else (or unset) uses filesystem keystore and auto-generates if absent. |
| `AWS_REGION` | _unset_ | **Required** when `GUARDIAN_ENV=prod`. Region for Secrets Manager calls. |
| `GUARDIAN_NETWORK_TYPE` | `MidenDevnet` | Miden network identifier (`MidenDevnet`, `MidenTestnet`, etc.). Required only when you need a non-default network. Pins which Miden RPC and on-chain consensus the server speaks to. |

ACK secret IDs (`GUARDIAN_ACK_FALCON_SECRET_ID`,
`GUARDIAN_ACK_ECDSA_SECRET_ID`) are passed in by Terraform/ECS env in
prod; defaults derive from `${stack_name}-prod/server/ack-*-secret-key`.
See [Secrets runbook](./runbooks/secrets.md#ack-signing-keys).

## Runtime — request safety

| Variable | Default | Notes |
|---|---|---|
| `GUARDIAN_RATE_LIMIT_ENABLED` | `true` | Master kill-switch for HTTP rate limiting. Set `false` only in test environments. |
| `GUARDIAN_RATE_BURST_PER_SEC` | `10` (code default); `200` set by the prod Terraform profile | Token-bucket burst. |
| `GUARDIAN_RATE_PER_MIN` | `60` (code default); `5000` set by the prod Terraform profile | Sustained rate. |
| `GUARDIAN_MAX_REQUEST_BYTES` | `1048576` (1 MB) | Reject request bodies larger than this. |
| `GUARDIAN_MAX_PENDING_PROPOSALS_PER_ACCOUNT` | `20` | Account-level cap; hitting it returns `pending_proposals_limit`. |
| `GUARDIAN_CORS_ALLOWED_ORIGINS` | _unset_ | Comma-separated explicit origins. **Unset → permissive `Any` origin / `Any` methods / `Any` headers, credentials disabled** (suitable for local dev). **Set → strict allowlist with `allow_credentials(true)`** (required for production browser clients). |

## Runtime — dashboard

| Variable | Default | Notes |
|---|---|---|
| `GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ID` | _unset_ | AWS Secrets Manager secret name/ARN holding the operator allowlist JSON. Hot-reloaded on every challenge and authenticated `/dashboard/*` request. |
| `GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE` | _unset_ | Local JSON path for the same payload. Local dev only. |
| `GUARDIAN_ENVIRONMENT` | `testnet` | Cosmetic string surfaced on `GET /dashboard/info` (`mainnet` / `testnet` / `staging`). |
| `GUARDIAN_DASHBOARD_CURSOR_SECRET` | random per process | 32-byte hex HMAC key for dashboard pagination cursors. Pin a shared value when running ≥2 ECS tasks so cursors validate across replicas. |

Allowlist payload shapes and enrollment flow:
[`docs/dashboard.md`](./dashboard.md).

## Runtime — EVM (feature-gated)

These take effect only when the server is built with `--features evm`.
The server reads only the two variables in this table; the allowed chain
set is **derived from the keys of `GUARDIAN_EVM_RPC_URLS`** rather than a
separate variable.

| Variable | Default | Notes |
|---|---|---|
| `GUARDIAN_EVM_RPC_URLS` | _unset_ (treated as an empty registry) | Comma list `chain_id=rpc_url`. E.g. `1=https://…,11155111=https://…`. Allowed chain IDs are the keys of this map. **Required for usable EVM chains** — when unset, the server starts but the EVM registry is empty and every chain ID will be rejected. |
| `GUARDIAN_EVM_ENTRYPOINT_ADDRESS` | `0x433709009b8330fda32311df1c2afa402ed8d009` (EntryPoint v0.9) | Shared EntryPoint address used for finality checks across chains. |

## Logging

| Variable | Default | Notes |
|---|---|---|
| `RUST_LOG` | `info` | Standard `tracing-subscriber` filter. Module-scoped filters work: `RUST_LOG=server::jobs::canonicalization=debug`. |

Useful filters during debugging — see
[`TROUBLESHOOTING.md`](./TROUBLESHOOTING.md#logging-and-observability).

## Deploy script (`scripts/aws-deploy.sh`)

These are read by the deploy script, not by the server itself. The script
turns them into Terraform variables or build-time choices.

| Variable | Default | Notes |
|---|---|---|
| `STACK_NAME` | `guardian` | Base name for all AWS resources and Terraform state file. |
| `DEPLOY_STAGE` | `dev` | `dev` or `prod`; selects stage profile (autoscaling, RDS Proxy, etc.). |
| `CPU_ARCHITECTURE` | `X86_64` | `X86_64` or `ARM64`. Picks the Docker buildx platform and the ECS task arch. |
| `AWS_REGION` | _required_ | All AWS API calls. |
| `SUBDOMAIN` | `guardian` | Host portion of the public hostname. |
| `ROUTE53_ZONE_ID` | _unset_ | Optional Route 53 hosted zone for an alias record. |
| `CLOUDFLARE_ZONE_ID` | _unset_ | Optional Cloudflare zone for CNAME management. |
| `CLOUDFLARE_API_TOKEN` | _unset_ | Required iff `CLOUDFLARE_ZONE_ID` is set. |
| `CLOUDFLARE_PROXIED` | `false` | Whether the Cloudflare CNAME should be proxied. |
| `GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON` | _unset_ | Inline JSON array of operator pubkeys; Terraform creates the secret from this. Mutually exclusive with `GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ARN`. |
| `GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ARN` | _unset_ | ARN of an externally-managed operator pubkeys secret. When set, Terraform does not create one and the task reads from this ARN instead. |
| `GUARDIAN_EVM_CHAIN_CONFIG_FILE` | _unset_ | Path to a JSON file the deploy script reads to derive `GUARDIAN_EVM_RPC_URLS` (and the bookkeeping `GUARDIAN_EVM_ALLOWED_CHAIN_IDS` Terraform variable). Not read by the server. |
| `GUARDIAN_EVM_ALLOWED_CHAIN_IDS` | _unset_ | Comma list of chain IDs used **only by Terraform** for bookkeeping / secret naming. The server itself derives allowed chains from `GUARDIAN_EVM_RPC_URLS` keys. |
| `GUARDIAN_EVM_RPC_URLS_SECRET_ARN` | _unset_ | ECS-injection only: when set, the ECS task reads `GUARDIAN_EVM_RPC_URLS` from this Secrets Manager ARN at task start (the server still sees a plain env var). |
| `GUARDIAN_EVM_ALLOWED_CHAIN_IDS_SECRET_ARN` | _unset_ | Same, for the bookkeeping chain-ID list. |
| `TF_VAR_*` | _unset_ | Any standard Terraform var override; the script passes through. |

## What's _not_ env-configurable

A few things are deliberately compile-time or builder-API only — knowing
this saves you from grepping:

- **HTTP / gRPC ports.** `3000` and `50051` are builder defaults
  ([`builder/mod.rs:68`](../crates/server/src/builder/mod.rs#L68));
  configurable through the Rust builder but not via env. ECS pins these
  in the task definition.
- **Storage backend choice.** Cargo feature `postgres` (or its absence),
  not an env var. See
  [Storage modes](./architecture/services.md#storage-modes).
- **EVM support.** Cargo feature `evm`. If the binary wasn't built with
  it, no env var will turn it on.
- **Canonicalization knobs** (`check_interval_seconds`, `max_retries`,
  `submission_grace_period_seconds`). Currently hard-coded in the
  canonicalization worker; require a code change to alter.
- **Auth timestamp window.** `MAX_TIMESTAMP_SKEW_MS = 300_000` (5 min) is
  hard-coded in
  [`metadata/auth/credentials.rs:6`](../crates/server/src/metadata/auth/credentials.rs#L6).

## Quick combos

| I want… | Set |
|---|---|
| Minimum local dev | _nothing_ — `docker compose up` works |
| Postgres backend locally | `DATABASE_URL=…` + build with `--features postgres` |
| EVM support locally | `GUARDIAN_EVM_RPC_URLS` (allowed chain set derives from its keys) + build with `--features evm` |
| Use Secrets Manager for ACK keys | `GUARDIAN_ENV=prod` + `AWS_REGION=<region>` + secrets pre-created |
| Run the dashboard locally | `GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE=/path/to/allowlist.json` |
| Multi-replica dashboard | `GUARDIAN_DASHBOARD_CURSOR_SECRET=<32-byte hex>` pinned across tasks |
| Higher throughput in prod | `GUARDIAN_RATE_BURST_PER_SEC`, `GUARDIAN_RATE_PER_MIN`, `GUARDIAN_DB_POOL_MAX_SIZE` |
