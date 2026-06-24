# Production deployment (AWS ECS/Fargate)

The end-to-end walkthrough for running Guardian in production. It sequences the
supported production path — AWS ECS/Fargate via [`scripts/aws-deploy.sh`](../../../scripts/aws-deploy.sh)
and the Terraform in [`infra/`](../../../infra) — into one run that lands you on
a deployment satisfying every item in the [production checklist](../../PRODUCTION.md#production-checklist),
including the recommended hardening: KMS-backed ECDSA signing, verified database
TLS, storage encryption at rest, and a multi-replica (HA) profile.

This guide **assembles and orders**; it does not restate what each variable
means or how each procedure works. For the authoritative meaning of any
variable, see [`CONFIGURATION.md`](../../CONFIGURATION.md); for the deploy
mechanics, [`SERVER_AWS_DEPLOY.md`](../../SERVER_AWS_DEPLOY.md).

Two ways to run the same hardened configuration:

- **AWS ECS/Fargate (recommended)** — the reference deployment, with autoscaling
  and multi-replica HA. This is the spine of the guide (steps 1–4 below).
- **Self-hosted Docker Compose** — the same prod-stage server on your own host,
  still using AWS Secrets Manager + KMS for secret custody. Single replica; see
  [Self-hosted via Docker Compose](#self-hosted-via-docker-compose-aws-managed-secrets)
  at the end. (This is the full-hardening sibling of the focused
  [`aws-signers/`](../aws-signers/README.md) example.)

## What this lands you on

- **AWS ECS/Fargate** running the published Guardian image with the `postgres`
  backend (filesystem mode is dev-only and is refused at startup in the prod
  stage).
- **Amazon RDS** for state, deltas, proposals, account metadata, and the audit
  log, reached through **RDS Proxy** in the prod profile.
- **AWS Secrets Manager + KMS** for the ACK signing identity and deploy-time
  secrets.
- **Two or more tasks** behind the ALB, with Postgres-backed shared coordination
  (sessions, login challenges, the canonicalization lease) across replicas.

See [`architecture/infra.md`](../../architecture/infra.md) for the topology and
Terraform ownership.

> **Living document.** This guide tracks the current recommended production
> setup and is updated as Guardian gains new hardening options. Always read it
> from `main` (or your deployed version's tag) rather than a cached copy, and
> treat [`CONFIGURATION.md`](../../CONFIGURATION.md) and
> [`PRODUCTION.md`](../../PRODUCTION.md) as the source of truth if anything here
> drifts.

## Prerequisites

- The repo checked out (for `scripts/aws-deploy.sh`, the Terraform in `infra/`,
  and the bootstrap helpers).
- Docker, for `aws-deploy.sh build`.
- AWS CLI credentials that can create a KMS key, create Secrets Manager secrets,
  and run Terraform (ECS, RDS, ALB, IAM, Route 53/ACM). See
  [`SERVER_AWS_DEPLOY.md` → Prerequisites](../../SERVER_AWS_DEPLOY.md#prerequisites).
- A `STACK_NAME` (e.g. `guardian-prod`). Secret names and resources derive from
  it, so distinct stacks coexist in one account.

## Before you start: four decisions

| Decision | Production choice |
|---|---|
| Stage profile | `DEPLOY_STAGE=prod` — autoscaling, RDS Proxy, prod-stage startup guards. |
| Server features | `GUARDIAN_SERVER_FEATURES=postgres`, or `postgres,evm` if the EVM API is served. |
| Miden network | Set `GUARDIAN_NETWORK_TYPE` explicitly. Supported: `MidenTestnet`, `MidenDevnet` (use `MidenTestnet` for the public network). An unrecognized value silently falls back to `MidenDevnet`, so never ship the default. |
| ECDSA ACK backend | **KMS** (recommended for new deployments — the private key never enters the process). Secrets Manager remains supported. See [`runbooks/secrets.md`](../../runbooks/secrets.md#hosted-ecdsa-backend-aws-kms). |

Throughout, export the stack identity once:

```bash
export DEPLOY_STAGE=prod
export STACK_NAME=guardian-prod
export GUARDIAN_NETWORK_TYPE=MidenTestnet   # MidenTestnet | MidenDevnet | MidenLocal
```

## 1. Bootstrap secrets (once per stack)

The normal deploy path never creates or rotates secrets — it expects them to
already exist. Each bootstrap command **generates the key material itself** and
writes it to Secrets Manager (or, for the ECDSA key, creates it inside KMS) —
you never pre-generate or paste in a key value. The only value you pass back is
the KMS **ARN**, which `bootstrap-kms-ecdsa-key` prints for you. Bootstrap
commands refuse to overwrite, so they are safe to re-run.

### ACK signing identity — KMS ECDSA + Secrets Manager Falcon

Create the KMS key first and export its ARN **before** the Falcon bootstrap and
the deploy, so the script knows to skip the ECDSA Secrets Manager secret:

```bash
./scripts/aws-deploy.sh bootstrap-kms-ecdsa-key                  # creates the key, prints the ARN
export TF_VAR_guardian_ack_ecdsa_kms_key_arn="arn:aws:kms:...:key/<key-id>"
./scripts/aws-deploy.sh bootstrap-ack-keys                       # Falcon only; skips ECDSA
```

Terraform then grants the task role `kms:Sign` + `kms:GetPublicKey` and injects
`GUARDIAN_ACK_ECDSA_BACKEND=aws-kms` / `GUARDIAN_ACK_ECDSA_KMS_KEY_ID`. The KMS
key must be `ECC_SECG_P256K1` / `SIGN_VERIFY` (immutable after creation). The
ACK key **is Guardian's identity**: moving to a different KMS key later is a
`SwitchGuardian` migration for existing accounts, not a routine rotation. See
[`SERVER_AWS_DEPLOY.md` → KMS-backed ECDSA signer](../../SERVER_AWS_DEPLOY.md#prod-with-a-kms-backed-ecdsa-signer)
and [`runbooks/secrets.md`](../../runbooks/secrets.md#hosted-ecdsa-backend-aws-kms).

> Staying on the Secrets Manager ECDSA path instead? Skip the KMS step and run
> `bootstrap-ack-keys` without the ARN exported — it creates both secrets.

### Storage encryption key (recommended; enable against an empty store)

Application-layer encryption of the sensitive payloads (account state, delta and
proposal payloads), above the database's own at-rest encryption. It is **opt-in
by key-source presence** and must be enabled against an **empty** store — the
server writes a one-time marker on the first encrypted write and then refuses to
mix plaintext and ciphertext. The Miden 0.15 cutover (which truncates account
data) is the natural enablement window.

```bash
./scripts/aws-deploy.sh bootstrap-storage-encryption-key
```

This generates a random 32-byte key (`openssl rand -base64 32`) and stores it as
a Secrets Manager secret holding
`{ "active": "k1", "keys": { "k1": "<the generated base64 key>" } }`. Setting
`GUARDIAN_STORAGE_ENCRYPTION_SECRET_NAME` at deploy (step 2) wires the runtime
`GUARDIAN_STORAGE_ENCRYPTION_KEY_SECRET_ID` plus the task-role
`secretsmanager:GetSecretValue` grant — the same pattern as the ACK keys.
Startup is fail-fast: a missing/malformed/wrong-length key, or more than one key
source, prevents startup rather than degrading to plaintext. Rotate by adding a
key to `keys` and moving `active`; keep the old key so existing records still
decrypt. See [`CONFIGURATION.md` → Storage encryption at rest](../../CONFIGURATION.md#storage-encryption-at-rest).

### Verified database TLS — CA bundle secret

By default `DATABASE_URL` uses `sslmode=require` (encrypted, certificate **not**
verified). For `verify-full`, store a combined CA bundle (region RDS roots +
Amazon Trust Services roots — RDS Proxy presents an ACM cert) and set
`rds_ca_bundle_secret_arn`. Build the bundle and create the secret per
[`SERVER_AWS_DEPLOY.md` → Database TLS verification](../../SERVER_AWS_DEPLOY.md#database-tls-verification);
to migrate an already-deployed stack safely, follow
[`runbooks/enable-db-tls.md`](../../runbooks/enable-db-tls.md).

### Dashboard operator allowlist (if the dashboard is enabled)

There is no bootstrap command for operator keys — unlike the ACK keys, the
server never holds an operator private key. Each operator generates their **own**
Falcon keypair on a trusted device (the `examples/operator-smoke-web` harness has
a UI for this) and hands you only the `0x…` **public** key; see
[`DASHBOARD.md` → Enrolling an operator](../../DASHBOARD.md#enrolling-an-operator).

You then let Terraform create the stack-scoped allowlist secret from those public
keys, or point at an existing secret ARN:

```bash
export GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON='["0x<alice-falcon-public-key>","0x<bob-falcon-public-key>"]'
```

The bare-key (Terraform) form grants `dashboard:read` only. To grant
`accounts:pause`, manage the secret externally with object entries. The server
re-reads the allowlist on every challenge and authenticated request, so
add/revoke takes effect without a restart. See [`DASHBOARD.md`](../../DASHBOARD.md).

## 2. Set the production environment

The prod Terraform profile already sets the HA and throughput knobs for you —
**verify** these rather than fighting them:

| Set automatically by the prod profile | Effect |
|---|---|
| `GUARDIAN_ENV=prod` | Activates the prod-stage startup guards (filesystem refused, cursor secret required, 0-partition rate limit refused) and loads ACK keys from Secrets Manager. |
| `GUARDIAN_DASHBOARD_CURSOR_SECRET` (32-byte hex / 64 chars) | Pinned across tasks so dashboard pagination cursors validate on any replica. **Required** in prod — startup fails if unset. |
| `GUARDIAN_MAX_REPLICAS` | Defaults to the autoscaling **max** capacity; each replica enforces `global / GUARDIAN_MAX_REPLICAS` for rate limiting. Coordination mode is **not** affected by this — it is backend-derived. |
| `GUARDIAN_RATE_BURST_PER_SEC` / `GUARDIAN_RATE_PER_MIN` | Prod defaults `200` / `5000` (code defaults are `10` / `60`). |
| RDS Proxy + autoscaling (`server_autoscaling_*`) | Connection pooling and ≥2 tasks. |

What **you** must provide:

| Variable | Notes |
|---|---|
| `GUARDIAN_NETWORK_TYPE` | The Miden network — set explicitly. |
| `GUARDIAN_SERVER_FEATURES` | `postgres`, or `postgres,evm`. |
| `GUARDIAN_CORS_ALLOWED_ORIGINS` | Exact browser origins (comma-separated; wildcards rejected). Unset → permissive `Any` with credentials disabled — not for production. |
| `TF_VAR_guardian_ack_ecdsa_kms_key_arn` | Exported in step 1 for the KMS path. |
| `GUARDIAN_STORAGE_ENCRYPTION_SECRET_NAME` | Turns on storage encryption with the secret from step 1. |
| `rds_ca_bundle_secret_arn` | The CA bundle secret ARN for verified DB TLS. |
| `GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON` / `..._SECRET_ARN` | The dashboard allowlist, if enabled. |
| `GUARDIAN_METRICS_ENABLED` / `GUARDIAN_METRICS_ADDR` / `GUARDIAN_METRICS_BEARER_TOKEN` | If Prometheus scraping is wanted — bind `0.0.0.0:9464`, keep the port reachable only from the scraper, gate with the bearer token. See [Observability guide](../observability/README.md). |

A typical export block before deploy (placeholders are stack-specific):

```bash
export GUARDIAN_SERVER_FEATURES=postgres                         # or postgres,evm
export GUARDIAN_CORS_ALLOWED_ORIGINS=https://accounts.example.com
export GUARDIAN_STORAGE_ENCRYPTION_SECRET_NAME="$STACK_NAME/server/storage-encryption-key"
export TF_VAR_rds_ca_bundle_secret_arn="arn:aws:secretsmanager:...:secret:$STACK_NAME/server/rds-ca-bundle"
export GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON='["0x<alice-falcon-public-key>","0x<bob-falcon-public-key>"]'
export GUARDIAN_METRICS_ENABLED=true GUARDIAN_METRICS_ADDR=0.0.0.0:9464 GUARDIAN_METRICS_BEARER_TOKEN=<token>
# TF_VAR_guardian_ack_ecdsa_kms_key_arn and DEPLOY_STAGE/STACK_NAME/GUARDIAN_NETWORK_TYPE already exported above.
```

Every variable's meaning lives in [`CONFIGURATION.md`](../../CONFIGURATION.md);
the deploy-time variables in
[`CONFIGURATION.md` → Deploy script](../../CONFIGURATION.md#deploy-script-scriptsaws-deploysh).

### Keep it in a `.env.prod` and override the profile

Rather than exporting variables piecemeal, keep them in a `.env.prod` and source
it before each command — the same file feeds every `bootstrap-*`, `plan`, and
`deploy` run:

```bash
set -a && source .env.prod && set +a
./scripts/aws-deploy.sh deploy
```

Two override channels feed the deploy:

- **Deploy-shell variables** — read directly by `scripts/aws-deploy.sh`:
  `DEPLOY_STAGE`, `STACK_NAME`, `SUBDOMAIN`, `DOMAIN_NAME`, `ECR_REPO_NAME`,
  `CPU_ARCHITECTURE`, `ACM_CERTIFICATE_ARN`, `CLOUDFLARE_ZONE_ID` /
  `CLOUDFLARE_API_TOKEN` (DNS/TLS at the ALB — see
  [HTTPS and gRPC](../../SERVER_AWS_DEPLOY.md#https-and-grpc)), and the
  `GUARDIAN_*` runtime vars above.
- **`TF_VAR_*` variables** — passed straight through to Terraform, overriding a
  stage-profile default, e.g. `TF_VAR_server_desired_count`,
  `TF_VAR_server_autoscaling_*`, `TF_VAR_rds_instance_class`,
  `TF_VAR_rds_allocated_storage`, `TF_VAR_guardian_db_pool_max_size`,
  `TF_VAR_guardian_rate_limit_enabled`.

The `prod` profile already picks production-shaped defaults (higher desired
count + autoscaling, larger RDS class with storage autoscaling, RDS Proxy,
higher rate-limit/DB-pool ceilings); override only the specific values you need.
The authoritative defaults and the full variable list are in
[`SERVER_AWS_DEPLOY.md` → Stage Profiles](../../SERVER_AWS_DEPLOY.md#stage-profiles)
and [Terraform Variables](../../SERVER_AWS_DEPLOY.md#terraform-variables).

> **Running a single task?** `TF_VAR_server_autoscaling_enabled=false` with
> `TF_VAR_server_desired_count=1` is a valid cost choice (e.g. testnet), but it
> is **not** HA — one task means no failover. Coordination is still correct
> (Postgres-backed), but `GUARDIAN_MAX_REPLICAS` still defaults to the
> autoscaling **max**, so a single running task is rate-limited at
> `global / max` unless you lower the max or (as on benchmark stacks) disable
> rate limiting with `TF_VAR_guardian_rate_limit_enabled=false`.

## 3. Deploy

```bash
./scripts/aws-deploy.sh build       # build + push image to ECR
./scripts/aws-deploy.sh plan        # review the Terraform plan against the pushed digest
./scripts/aws-deploy.sh deploy --skip-build   # apply the reviewed plan
./scripts/aws-deploy.sh status      # print Terraform outputs (ALB DNS, etc.)
```

`deploy` (without `--skip-build`) builds and applies in one step. Don't rebuild
between `plan` and `deploy --skip-build`. See
[`SERVER_AWS_DEPLOY.md` → Deploy](../../SERVER_AWS_DEPLOY.md#deploy) and
[Stage Profiles](../../SERVER_AWS_DEPLOY.md#stage-profiles).

## 4. Validate

```bash
curl -s https://<alb-dns>/            # liveness
curl -s https://<alb-dns>/pubkey | jq .   # Falcon + ECDSA public keys and commitments
```

Record the ECDSA commitment — it is derived from the KMS key, and changing the
key later is a `SwitchGuardian` identity change. Then check the startup logs
(`./scripts/aws-deploy.sh logs`) for:

- `coordination mode=shared backend=postgres stage=prod max_replicas=<N> cursor_secret=configured`
  — confirms replica-safe coordination. If you see `mode=single-process
  backend=filesystem`, the deployment is **not** safe to run with more than one
  task.
- `ECDSA ACK signer ready` with the active backend — confirms the KMS sign probe
  passed.

Finally, in the AWS console confirm the RDS instance has **backup retention**,
**deletion protection**, and **storage encryption** enabled, and run the
relevant SDK or dashboard smoke path. See
[`SERVER_AWS_DEPLOY.md` → Validate](../../SERVER_AWS_DEPLOY.md#validate).

## Production checklist coverage

Each [`PRODUCTION.md`](../../PRODUCTION.md#production-checklist) item, and where
this guide satisfies it:

| Checklist item | Step |
|---|---|
| `DEPLOY_STAGE=prod` | Decisions, 2 |
| `postgres` (+ `evm`) features | Decisions, 2 |
| Bootstrap ACK secrets | 1 |
| ECDSA: Secrets Manager vs KMS | Decisions, 1 |
| `DATABASE_URL` via Terraform RDS secret | 2 (prod profile) |
| Storage encryption at rest | 1, 2 |
| RDS backup retention / deletion protection / final snapshot | 4 |
| `GUARDIAN_CORS_ALLOWED_ORIGINS` | 2 |
| Operator allowlist (object entries for `accounts:pause`) | 1 |
| Pinned `GUARDIAN_DASHBOARD_CURSOR_SECRET` (multi-task) | 2 (prod profile) |
| `GUARDIAN_MAX_REPLICAS` (HA rate partitioning) | 2 (prod profile) |
| Verified DB TLS (`verify-full` + `sslrootcert`) | 1, 2 |
| Prometheus metrics + bearer token | 2 |
| Validate `/`, `/pubkey`, smoke path | 4 |

## Self-hosted via Docker Compose (AWS-managed secrets)

To run the same prod-stage server on your own host instead of ECS — still using
AWS for secret custody — use the committed [`docker-compose.yml`](./docker-compose.yml)
and [`.env.example`](./.env.example) in this directory. It runs a **single**
replica with `GUARDIAN_ENV=prod` (so every prod-stage guard is active) backed by
a bundled Postgres, the Falcon ACK key + storage-encryption key from Secrets
Manager, and the ECDSA ACK key in KMS. Multi-replica HA stays the ECS track
above — a single Compose host has no failover.

Bootstrap the secrets once (the step 1 commands), then:

```bash
cd docs/guides/production
cp .env.example .env          # fill in: POSTGRES_PASSWORD, AWS_REGION, the secret
                              # ids/ARNs, GUARDIAN_DASHBOARD_CURSOR_SECRET, origins
export AWS_ACCESS_KEY_ID=... AWS_SECRET_ACCESS_KEY=... AWS_SESSION_TOKEN=...   # or use a host role
docker compose up
```

The container needs AWS credentials because `GUARDIAN_ENV=prod` makes it call
Secrets Manager and KMS; Compose passes them through from your shell (a
long-lived host should use an instance/container role instead). Storage
encryption is enabled against the bundled Postgres, which is empty on first run.

**Validate (smoke):**

```bash
curl -s localhost:3000/pubkey | jq .     # Falcon + ECDSA public keys / commitments
docker compose logs server | grep "ECDSA ACK signer ready"
```

`/pubkey` returning both keys confirms Secrets Manager + KMS resolved; the
`ECDSA ACK signer ready` line confirms the KMS sign probe passed. Storage
encryption is confirmed by the server starting cleanly — a missing/malformed key
fails fast at startup rather than running unencrypted.

## Troubleshooting

| Symptom | Likely cause / where to look |
|---|---|
| Startup fails on the filesystem backend in prod | Prod refuses filesystem — deploy the Postgres image with `DATABASE_URL`. |
| Startup fails: cursor secret unset | `GUARDIAN_DASHBOARD_CURSOR_SECRET` is required in the prod stage. The prod profile sets it; check it wasn't overridden empty. |
| `mode=single-process backend=filesystem` in logs | Not multi-replica safe — the backend must be Postgres. See [`runbooks/horizontal-scaling.md`](../../runbooks/horizontal-scaling.md). |
| Sign-probe / `configuration_error` at startup | KMS key wrong spec (`ECC_SECG_P256K1` / `SIGN_VERIFY`) or missing `kms:Sign`. See [`runbooks/secrets.md`](../../runbooks/secrets.md#hosted-ecdsa-backend-aws-kms). |
| Startup fails with a storage-encryption marker error | A key was configured against a store that already holds plaintext — enable only against an empty store. |
| `verify-full` certificate errors | CA bundle missing the RDS Proxy (Amazon Trust Services) roots. See [`runbooks/enable-db-tls.md`](../../runbooks/enable-db-tls.md). |
| Rate limit rejects all traffic in prod | Global limit partitions to 0 per replica (`GUARDIAN_RATE_*` below `GUARDIAN_MAX_REPLICAS`). Raise the global limit. |

See [`TROUBLESHOOTING.md`](../../TROUBLESHOOTING.md) for the full error-code
playbook.
