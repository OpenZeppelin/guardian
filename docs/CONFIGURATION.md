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
| `DATABASE_URL` | _required_ | `postgres` | Postgres connection string. Server panics at startup if unset under `--features postgres`. TLS verification is controlled by the standard `sslmode`/`sslrootcert` parameters — see [Database TLS](#database-tls). |
| `GUARDIAN_STORAGE_PATH` | `/var/guardian/storage` | filesystem | Path for state + delta blobs. |
| `GUARDIAN_METADATA_PATH` | `/var/guardian/metadata` | filesystem | Path for accounts, auth credentials, network config. |
| `GUARDIAN_KEYSTORE_PATH` | `/var/guardian/keystore` | any | Local Falcon/ECDSA key files (ACK signers and per-account creds). |
| `GUARDIAN_DB_POOL_MAX_SIZE` | `16` (code default); `32` set by the prod Terraform profile | `postgres` | Storage backend pool size. |
| `GUARDIAN_METADATA_DB_POOL_MAX_SIZE` | matches storage | `postgres` | Metadata backend pool size; usually leave equal. |
| `GUARDIAN_SERVER_FEATURES` | _build-time_ | deploy script | Comma list (`postgres`, `evm`) the deploy script compiles in. Not read at runtime — controls how the image is built. |

### Database TLS

TLS behavior is driven entirely by the standard libpq parameters in
`DATABASE_URL`; there is no Guardian-specific TLS env var. To migrate a deployed
AWS stack to verified TLS, see
[`runbooks/enable-db-tls.md`](./runbooks/enable-db-tls.md). The same parameters
govern both the synchronous startup-migration connection and the asynchronous
runtime pools, so they always behave identically.

| `sslmode` | `sslrootcert` | Behavior |
|---|---|---|
| _omitted_ / `disable` | _(any)_ | Plaintext, no TLS. |
| `require` | _(none)_ | Encrypted, certificate **not** verified. |
| `require` | `<path>` | Encrypted + certificate chain verified (promoted to `verify-ca`, matching libpq). |
| `verify-ca` | `<path>` | Encrypted + chain verified (hostname not checked). |
| `verify-full` | `<path>` | Encrypted + chain **and** hostname verified. Recommended for managed providers. |

- `sslrootcert=<path>` points at a PEM CA bundle file the server can read; the
  whole bundle must parse (a malformed entry fails startup). It may contain
  multiple roots.
- Verifying modes **fail closed**: a missing/unreadable/empty CA bundle, an
  unknown `sslmode`, `allow`/`prefer` (which permit plaintext fallback), or
  `sslrootcert=system` (unsupported — needs libpq ≥16) all abort startup with an
  actionable error rather than connecting insecurely.
- Hostname matching under `verify-full` is strict SAN-based; certificates without
  a matching Subject Alternative Name (including Common-Name-only certs) are
  rejected.

Per-provider examples:

```text
# AWS RDS (managed; recommended). Mount a combined bundle of the Amazon RDS CA
# roots AND the Amazon Trust Services roots — the RDS Proxy presents an ACM
# certificate chaining to Amazon Trust Services, a direct instance chains to the
# RDS CA roots.
DATABASE_URL=postgres://USER:PW@HOST:5432/guardian?sslmode=verify-full&sslrootcert=/etc/guardian/tls/rds-combined-ca.pem

# Another managed provider (GCP Cloud SQL / Azure / Supabase / Neon …)
DATABASE_URL=postgres://USER:PW@HOST:5432/guardian?sslmode=verify-full&sslrootcert=/etc/guardian/tls/provider-ca.pem

# Local docker compose (no TLS)
DATABASE_URL=postgres://guardian:guardian@localhost:5432/guardian
```

## Runtime — ACK signing and network

| Variable | Default | Notes |
|---|---|---|
| `GUARDIAN_ENV` | _unset_ | Set to `prod` to load ACK keys from AWS Secrets Manager. Anything else (or unset) uses filesystem keystore and auto-generates if absent. |
| `AWS_REGION` | _unset_ | **Required** when `GUARDIAN_ENV=prod`. Region for Secrets Manager calls. |
| `GUARDIAN_NETWORK_TYPE` | `MidenDevnet` | Miden network identifier (`MidenDevnet`, `MidenTestnet`, etc.). Required only when you need a non-default network. Pins which Miden RPC and on-chain consensus the server speaks to. |

ACK secret IDs are configurable. The server reads two env vars at startup
and falls back to fixed defaults when they're unset
([`crates/server/src/ack/secrets_manager.rs:10-13`](../crates/server/src/ack/secrets_manager.rs#L10)):

| Variable | Default | Notes |
|---|---|---|
| `GUARDIAN_ACK_FALCON_SECRET_ID` | `guardian-prod/server/ack-falcon-secret-key` | Secrets Manager name/ARN for the Falcon ACK secret key. |
| `GUARDIAN_ACK_ECDSA_SECRET_ID` | `guardian-prod/server/ack-ecdsa-secret-key` | Secrets Manager name/ARN for the ECDSA ACK secret key. Used only when the ECDSA backend is `in-memory`. |

### Storage encryption at rest

Application-layer encryption of the sensitive stored payloads (account state,
delta and proposal payloads). It is **opt-in by key-source presence** — configure
a key and the server encrypts; configure none and it stores plaintext exactly as
before. Routing/index fields (account id, nonce, commitments, status, timestamps)
always stay plaintext. See the [storage-encryption quickstart](../speckit/features/001-storage-encryption/quickstart.md).

**Which variable do I set?** Choose **one key source** — the dev key for local
work, or the Secrets Manager secret for production. You never set both (doing so
is a startup error). `GUARDIAN_STORAGE_ENCRYPTION_KEY_ID` is **not** a key — it is
an optional label, and most users leave it unset.

**Key sources** (set exactly one; presence is what turns encryption on):

| Variable | Default | Notes |
|---|---|---|
| `GUARDIAN_STORAGE_ENCRYPTION_KEY` | _unset_ | **Dev key source.** The actual 32-byte key, base64-encoded (`openssl rand -base64 32`). |
| `GUARDIAN_STORAGE_ENCRYPTION_KEY_SECRET_ID` | _unset_ | **Prod key source.** AWS Secrets Manager name/ARN of a secret holding a structured `{ "active": kid, "keys": { kid: base64-32-bytes } }` document (one or more keys). Reuses `AWS_REGION`. |

**Optional label:**

| Variable | Default | Notes |
|---|---|---|
| `GUARDIAN_STORAGE_ENCRYPTION_KEY_ID` | `k1` | Only used with the dev key source. Sets the key id (`kid`) recorded on each encrypted record so the key can be identified later. The default is fine for almost everyone — leave it unset unless you specifically need the dev key's id to match an id used elsewhere. (With Secrets Manager the ids come from the secret's `keys`/`active` fields instead, so this variable is ignored.) |

Every encrypted record stores the `kid` of the key that wrote it, and on read the
key is resolved by that id — this is what lets Secrets Manager hold several keys
and rotate them (add a new key, repoint `active`, keep the old key so existing
records still decrypt). The dev key source holds a single key, so it cannot do
multi-key rotation reads; that is a production capability.

Rules: configure **exactly one** key source (both set → startup error). When a key
source is set the server validates it at startup and fails fast on a
missing/malformed/wrong-length key — it never silently falls back to plaintext.
Encryption is fixed for a populated store: the server records a marker on the
first encrypted write and refuses to mix plaintext and ciphertext, so enable it
against an empty store (e.g. after the Miden 0.15 cutover). Switching an existing
store requires an explicit re-encryption migration (not yet provided).

### Hosted ECDSA signer backend

The ECDSA ACK signer can keep its private key outside the process in a hosted
backend. Falcon is unaffected and always uses the in-memory path.

| Variable | Default | Notes |
|---|---|---|
| `GUARDIAN_ACK_ECDSA_BACKEND` | `in-memory` | `in-memory` (filesystem keystore, or Secrets Manager when `GUARDIAN_ENV=prod`) or `aws-kms`. An unrecognized value fails startup listing the supported values. |
| `GUARDIAN_ACK_ECDSA_KMS_KEY_ID` | _unset_ | **Required** when `GUARDIAN_ACK_ECDSA_BACKEND=aws-kms`. KMS key id, ARN, or alias. The key must be `ECC_SECG_P256K1` with usage `SIGN_VERIFY`. |

With `aws-kms`, the server holds only the key handle; the private key never enters
the process. At startup it fetches the public key, validates the key spec, and
performs a sign probe to confirm `kms:Sign` permission — failing fast otherwise.
The ECDSA secret in Secrets Manager (`GUARDIAN_ACK_ECDSA_SECRET_ID`) is **not**
read on this path. Credentials resolve through the standard AWS chain (the ECS
task role in production); required IAM is `kms:GetPublicKey` and `kms:Sign` on the
key.

> Switching an existing deployment from `in-memory` to `aws-kms` means a new
> keypair, hence a new ECDSA `pubkey`/`commitment`. Re-establish downstream trust
> accordingly.

> **`_SECRET_ID` (runtime) vs `_SECRET_NAME` (deploy-side):** the server
> reads `GUARDIAN_ACK_*_SECRET_ID` at startup, but you typically don't
> set these by hand. The deploy script accepts
> `GUARDIAN_ACK_*_SECRET_NAME` (see the [Deploy script
> section](#deploy-script-scriptsaws-deploysh) below), passes it into
> Terraform as `guardian_ack_*_secret_name`, and Terraform sets the
> matching `_SECRET_ID` env var on the ECS task. Same value, three
> places — see the [Secrets runbook](./runbooks/secrets.md#ack-signing-keys)
> for the full override chain.

In the reference AWS deploy, Terraform sets both `_SECRET_ID` env vars on
the ECS task to `${stack_name}/server/ack-{falcon,ecdsa}-secret-key` so
multi-stack deployments get scoped IDs.

## Runtime — request safety

| Variable | Default | Notes |
|---|---|---|
| `GUARDIAN_RATE_LIMIT_ENABLED` | `true` | Master kill-switch for HTTP rate limiting. Set `false` only in test environments. |
| `GUARDIAN_RATE_BURST_PER_SEC` | `10` (code default); `200` set by the prod Terraform profile | Token-bucket burst. |
| `GUARDIAN_RATE_PER_MIN` | `60` (code default); `5000` set by the prod Terraform profile | Sustained rate. |
| `GUARDIAN_MAX_REPLICAS` | `1` (code default); autoscaling **max** capacity set by the prod Terraform profile | Per-replica rate-limit divisor: each replica enforces `global / GUARDIAN_MAX_REPLICAS` so the fleet aggregate stays at or below the global limit. Use the autoscaling **max**, not the running count. Drives rate-limiting only — coordination mode is backend-derived. Running below max over-throttles (accepted); an override is clamped up to the autoscaling max by Terraform. See [`runbooks/horizontal-scaling.md`](./runbooks/horizontal-scaling.md). |
| `GUARDIAN_MAX_REQUEST_BYTES` | `1048576` (1 MB) | Reject request bodies larger than this. |
| `GUARDIAN_MAX_PENDING_PROPOSALS_PER_ACCOUNT` | `20` | Account-level cap; hitting it returns `pending_proposals_limit`. |
| `GUARDIAN_CORS_ALLOWED_ORIGINS` | _unset_ | Comma-separated explicit origins. **Unset → permissive `Any` origin / `Any` methods / `Any` headers, credentials disabled** (suitable for local dev). **Set → strict allowlist with `allow_credentials(true)`** (required for production browser clients). |

## Runtime — metrics (Prometheus)

| Variable | Default | Notes |
|---|---|---|
| `GUARDIAN_METRICS_ENABLED` | `false` | Master switch for the Prometheus integration. When `false` (default) nothing runs: no metrics listener, no recorder, no storage instrumentation, no background refresher. |
| `GUARDIAN_METRICS_ADDR` | `127.0.0.1:9464` | Bind address of the **dedicated** metrics listener (separate from the API port). Loopback by default; set `0.0.0.0:9464` in containers so a Prometheus sidecar/agent can reach it. Invalid values fall back to the default with a warning. |
| `GUARDIAN_METRICS_PATH` | `/metrics` | Path serving the Prometheus text exposition on that listener. Must start with `/`. |
| `GUARDIAN_METRICS_REFRESH_INTERVAL_SECS` | `30` | Cadence of the background task that refreshes slow aggregate gauges (delta status counts, in-flight proposals, account count). Scrapes never query storage directly. |
| `GUARDIAN_METRICS_BEARER_TOKEN` | _unset_ | Optional shared-secret scrape token. When set, scrapes must send `Authorization: Bearer <token>` (Prometheus `authorization.credentials` in the scrape config); anything else gets `401`. Compared in constant time, held as a non-loggable secret wrapper in process. |

The metrics endpoint is intentionally **not** part of the main API
router: it bypasses rate limiting and CORS and is protected by network
isolation first (loopback default / private network / security group),
the bearer token second, and proxy-terminated TLS where transport
encryption is required. Never expose it to a public network. The
exposed metric taxonomy and cardinality rules are documented in
[`spec/api.md`](../spec/api.md); see the
[Observability guide](./guides/observability/README.md) for scraping and a
one-command Grafana dashboard stack.

## Runtime — dashboard

| Variable | Default | Notes |
|---|---|---|
| `GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ID` | _unset_ | AWS Secrets Manager secret name/ARN holding the operator allowlist JSON. Hot-reloaded on every challenge and authenticated `/dashboard/*` request. |
| `GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE` | _unset_ | Local JSON path for the same payload. Local dev only. |
| `GUARDIAN_DASHBOARD_CURSOR_SECRET` | random per process if unset | 32-byte hex HMAC key for dashboard pagination cursors. Pin a shared value across replicas so cursors validate everywhere. If unset the server **warns** and generates an ephemeral per-process key and still boots (in every stage); an ephemeral key only breaks dashboard pagination across replicas — nothing else, so it is not a startup guard. |

`GET /dashboard/info.environment` is derived from `GUARDIAN_NETWORK_TYPE`
(`testnet`, `devnet`, or `local`) rather than configured separately.

### Prod-stage startup guards & HA behavior

When `GUARDIAN_ENV=prod`, the server fails fast on misconfigurations that are
silently broken across replicas:

- the **filesystem** storage backend is refused (single-instance only — use the
  Postgres image with `DATABASE_URL`);
- a rate limit that partitions to **0 requests per replica** is refused — i.e.
  the global `GUARDIAN_RATE_BURST_PER_SEC`/`GUARDIAN_RATE_PER_MIN` is below
  `GUARDIAN_MAX_REPLICAS`, which would make every replica throttle all traffic.
  Raise the global limit or lower `GUARDIAN_MAX_REPLICAS`. (Non-prod only warns.)

On the Postgres backend, operator/EVM sessions, login challenges, and the
canonicalization lease are shared across replicas (backend-derived — no tunable
disables this). If the database is briefly unavailable, authentication **fails
closed** (rejected, never bypassed) and recovers automatically. See the
[horizontal-scaling runbook](./runbooks/horizontal-scaling.md).

Allowlist payload shapes and enrollment flow:
[`docs/DASHBOARD.md`](./DASHBOARD.md).

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
| `GUARDIAN_ACK_FALCON_SECRET_NAME` | _unset_ → `${STACK_NAME}/server/ack-falcon-secret-key` | Deploy-side override for the Falcon ACK secret. Passed into Terraform as `guardian_ack_falcon_secret_name` and set on the ECS task as the runtime `GUARDIAN_ACK_FALCON_SECRET_ID`. |
| `GUARDIAN_ACK_ECDSA_SECRET_NAME` | _unset_ → `${STACK_NAME}/server/ack-ecdsa-secret-key` | Deploy-side override for the ECDSA ACK secret. Same flow as the Falcon entry above. |
| `GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON` | _unset_ | Inline JSON array of operator pubkeys; Terraform creates the secret from this. Mutually exclusive with `GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ARN`. |
| `GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ARN` | _unset_ | ARN of an externally-managed operator pubkeys secret. When set, Terraform does not create one and the task reads from this ARN instead. |
| `GUARDIAN_EVM_CHAIN_CONFIG_FILE` | _unset_ | Path to a JSON file the deploy script reads to derive `GUARDIAN_EVM_RPC_URLS` (and the bookkeeping `GUARDIAN_EVM_ALLOWED_CHAIN_IDS` Terraform variable). Not read by the server. |
| `GUARDIAN_EVM_ALLOWED_CHAIN_IDS` | _unset_ | Comma list of chain IDs used **only by Terraform** for bookkeeping / secret naming. The server itself derives allowed chains from `GUARDIAN_EVM_RPC_URLS` keys. |
| `GUARDIAN_EVM_RPC_URLS_SECRET_ARN` | _unset_ | ECS-injection only: when set, the ECS task reads `GUARDIAN_EVM_RPC_URLS` from this Secrets Manager ARN at task start (the server still sees a plain env var). |
| `GUARDIAN_EVM_ALLOWED_CHAIN_IDS_SECRET_ARN` | _unset_ | Same, for the bookkeeping chain-ID list. |
| `TF_VAR_*` | _unset_ | Any standard Terraform var override; the script passes through. |

## Secrets in env vars — scope of in-process protection

Secret-bearing env vars (`DATABASE_URL`, `GUARDIAN_DASHBOARD_CURSOR_SECRET`,
`GUARDIAN_EVM_RPC_URLS`) are wrapped at the point of read into
zero-on-drop, no-`Display`/no-`Serialize` types inside the server
process, so they cannot accidentally appear in logs, panic messages, or
serialized responses. **This does not protect the OS process
environment block** — `/proc/<pid>/environ`, coredumps, fork-inherited
env, and ECS task-definition `environment` fields all remain visible at
the OS layer regardless. For the highest-sensitivity material (ACK
signing keys) prefer the AWS Secrets Manager runtime-fetch path
(`GUARDIAN_ACK_FALCON_SECRET_ID` / `GUARDIAN_ACK_ECDSA_SECRET_ID`),
which is already how production loads those keys.

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
| Multi-replica (HA) | Postgres backend + `GUARDIAN_DASHBOARD_CURSOR_SECRET=<64 hex>` pinned across tasks + `GUARDIAN_MAX_REPLICAS=<autoscaling max>` (all set by the prod Terraform profile) |
| Higher throughput in prod | `GUARDIAN_RATE_BURST_PER_SEC`, `GUARDIAN_RATE_PER_MIN`, `GUARDIAN_DB_POOL_MAX_SIZE` |
