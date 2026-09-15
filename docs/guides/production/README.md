# Production deployment

The end-to-end walkthrough for running Guardian in production. It sequences the
pieces documented elsewhere into one run that lands you on a deployment
satisfying every item of the [production checklist](../../PRODUCTION.md#production-checklist):
Postgres-only storage, a stable Guardian identity, verified database TLS,
storage encryption at rest, protected metrics, explicit CORS, and (where you
want it) multi-replica HA.

The committed Compose stacks (tracks B and C) run standalone against a bundled
Postgres so you can evaluate and smoke-test them; that bundled database has no
TLS and no backups, so production readiness on those tracks also requires
steps B3 (your own TLS-verified database) and B5 (your ingress).

This guide **assembles and orders**; it does not restate what each variable
means or how each procedure works. For the meaning of any variable see
[`CONFIGURATION.md`](../../CONFIGURATION.md); for the AWS deploy mechanics see
[`SERVER_AWS_DEPLOY.md`](../../SERVER_AWS_DEPLOY.md); for operational
procedures see the [runbooks](../../runbooks/).

> **Living document.** Read it from `main` (or your deployed version's tag)
> rather than a cached copy. If anything here disagrees with
> [`CONFIGURATION.md`](../../CONFIGURATION.md) or [`PRODUCTION.md`](../../PRODUCTION.md),
> those win.

## Pick your track

| Track | You run | Guardian secrets live in | Database | HA |
|---|---|---|---|---|
| [**A. AWS ECS/Fargate**](#track-a-aws-ecsfargate-reference-deployment) (reference, **recommended**) | `scripts/aws-deploy.sh` + Terraform in `infra/` | AWS Secrets Manager + KMS (task role, nothing on disk) | Amazon RDS via RDS Proxy, Terraform-managed backups | 2 to 6 tasks with autoscaling |
| [**B. Docker image, self-managed**](#track-b-self-managed-docker-image-no-aws) | The published image on your own host, VM, or Kubernetes | Files and an env file you protect | Your own Postgres (managed or self-hosted) | Yours to build; step 7 says how |
| [**C. Docker image + AWS secret custody**](#track-c-self-managed-docker-image-with-aws-secret-custody) (only when ECS is not possible for you) | The published image on your own host, AWS only for secrets | AWS Secrets Manager + KMS | Your own Postgres | Same as B |

Files in this directory, by track:

| Track | Files |
|---|---|
| A | [`.env.aws-ecs.example`](./.env.aws-ecs.example) |
| B | [`docker-compose.yml`](./docker-compose.yml), [`.env.example`](./.env.example), [`operators.example.json`](./operators.example.json), [`smoke.sh`](./smoke.sh) (plus the `ack-keys/` and `storage-encryption-keys.json` you generate in B1 and B2) |
| C | [`docker-compose.aws-no-ecs.yml`](./docker-compose.aws-no-ecs.yml), [`.env.aws-no-ecs.example`](./.env.aws-no-ecs.example) |

**Recommendation: track A**, the reference deployment. It is what OpenZeppelin
runs, what the Terraform, deploy script, runbooks, and CloudWatch dashboards
are written against, and the only track where the database, HA, backups, and
alarms are built for you. Use **track C** only when running the reference
stack is not possible for you (no ECS in your organization, a platform
mandate, an existing Docker or Kubernetes estate you must deploy into). It
keeps the part that matters most, secret custody in AWS Secrets Manager and
KMS (the hosted ECDSA signer never exposes its key to the process), on any
Docker host, and leaves the database, ingress, and backups to you. It is not
a lighter alternative to track A: everything Terraform builds for you there
becomes your job here. Track B is the
fallback for operators with no AWS account at all: it works and reaches the
same shape, but it hands you more to protect (the ACK private keys and the
encryption key document live as files on your host) and is the
least-travelled configuration.

## Decisions every track shares

| Decision | Production choice |
|---|---|
| **Miden network** | Set `GUARDIAN_NETWORK_TYPE` explicitly: `MidenTestnet`, `MidenDevnet`, or `MidenLocal`. The server refuses to start when it is unset or unrecognized; there is no fallback network. |
| **Image version** | Pin an explicit release tag, **later than `v0.17.0`**. This guide depends on server features that v0.17.0 does not have: `ack-keygen` in the image, `GUARDIAN_STORAGE_ENCRYPTION_KEY_FILE`, `GUARDIAN_ALLOWED_ACCOUNT_SCHEMES`, and the `GUARDIAN_ENV=prod` runtime defaults. An older server does not fail on the unknown variables: it boots, stores payloads in **plaintext**, accepts every scheme, and runs the development rate limits. The tell is the startup banner: the `ack signers` line carries `account_schemes` only on a new enough image, and `smoke.sh` fails at its first step on an old one. Guardian 0.17.x runs on Miden 0.16; check [`MIDEN_COMPATIBILITY.md`](../../MIDEN_COMPATIBILITY.md) before choosing, and read [Upgrading to Miden 0.16](../../PRODUCTION.md#upgrading-to-miden-016) if you are moving an existing deployment. Never run `latest` in production. |
| **Server features** | The published `ghcr.io/openzeppelin/guardian` image is built with the `postgres` feature, which is the production storage backend; that is the image every track in this guide runs. |
| **Storage backend** | Postgres, always. The filesystem backend is dev-only and refused at startup in the prod stage. |
| **Account signature scheme** | Set `GUARDIAN_ALLOWED_ACCOUNT_SCHEMES=ecdsa` on a new deployment, and treat it as required on the AWS tracks (A and C): only ECDSA has a hosted signer (KMS), so an ECDSA-only fleet is the only one whose account-facing ACK key never enters the process. Falcon has no remote signer, so every Falcon account is acked by a key that has to be loaded into memory from Secrets Manager; Falcon is second-class ([`PRODUCTION.md`](../../PRODUCTION.md#account-signature-scheme)). The gate applies to **new** registrations only, so it is safe on a fleet that already has Falcon accounts, and it has no effect on which ACK keys the server needs: the Falcon ACK key is still required today, so bootstrap and protect it exactly as below. Rejected registrations get `signature_scheme_not_allowed`; `GET /dashboard/info` shows `accounts_by_auth_method` if you want to check what exists before tightening. All three templates carry the variable. |
| **ECDSA ACK signer backend** | AWS KMS where AWS is available (tracks A and C): the private key never enters the process. Track B keeps it in a `0600` file. Whichever you choose, the ACK keys **are Guardian's identity**: changing them later is a `SwitchGuardian` migration for every existing account, not a routine rotation ([`runbooks/secrets.md`](../../runbooks/secrets.md#ack-signing-keys)). |
| **Storage encryption** | Recommended. It is opt-in by key-source presence and must be enabled against an **empty** store; the server refuses to mix plaintext and ciphertext once a marker is written ([`PRODUCTION.md`](../../PRODUCTION.md#storage-encryption)). The same `{active, keys}` key document works from Secrets Manager (tracks A and C) or from a mounted file (track B), with multi-key rotation on both. |

## Track A: AWS ECS/Fargate (reference deployment)

What this lands you on: the Guardian image on ECS/Fargate behind an ALB, Amazon
RDS reached through RDS Proxy, Secrets Manager + KMS for the ACK identity and
deploy-time secrets, two or more tasks with Postgres-backed coordination, and
an ADOT sidecar shipping metrics to CloudWatch dashboards and alarms. Topology
and Terraform ownership: [`architecture/infra.md`](../../architecture/infra.md).

### Prerequisites

- The repo checked out (for `scripts/aws-deploy.sh` and `infra/`), Docker for
  `build`, Terraform, `jq`.
- An authenticated AWS session. The script and Terraform call AWS on every
  command and never prompt for login. Confirm with `aws sts get-caller-identity`.
  Required permissions: [`SERVER_AWS_DEPLOY.md` → Prerequisites](../../SERVER_AWS_DEPLOY.md#prerequisites).
- A `STACK_NAME` (for example `guardian-prod`). Secret names and every resource
  derive from it, so never reuse a staging stack's name.

Copy the template and source it before **every** command in this track. It
carries the stack identity, region, network, and every deploy-time choice:

```bash
cp docs/guides/production/.env.aws-ecs.example .env.aws-ecs   # gitignored; fill it in
set -a && source .env.aws-ecs && set +a
aws sts get-caller-identity                            # right account, right role
```

`AWS_REGION` must be set before step 1: the bootstrap commands write the
secrets and the KMS key into it.

### A1. Bootstrap secrets (once per stack)

`plan` and `deploy` **require** these secrets to exist and never create or
rotate them. Each bootstrap command generates its key material itself, writes
it to Secrets Manager (or creates the key inside KMS), and refuses to overwrite
an existing secret, so re-running is safe.

**Dashboard cursor secret (required in prod).** Terraform injects the same
value into every task so pagination cursors validate on any replica; `plan`
and `deploy` abort when the secret is missing:

```bash
./scripts/aws-deploy.sh bootstrap-dashboard-cursor-secret   # <stack>/server/dashboard-cursor-secret
```

**ACK signing identity: KMS ECDSA + Secrets Manager Falcon.** Create the KMS
key first and export its ARN before the Falcon bootstrap and before deploy, so
the script skips the ECDSA Secrets Manager secret:

```bash
./scripts/aws-deploy.sh bootstrap-kms-ecdsa-key             # creates the key, prints the ARN
export TF_VAR_guardian_ack_ecdsa_kms_key_arn="arn:aws:kms:...:key/<key-id>"   # also put it in .env.aws-ecs
./scripts/aws-deploy.sh bootstrap-ack-keys                  # Falcon only; skips ECDSA
```

Terraform grants the task role `kms:Sign` + `kms:GetPublicKey` and injects
`GUARDIAN_ACK_ECDSA_BACKEND=aws-kms`. The key must be `ECC_SECG_P256K1` /
`SIGN_VERIFY`. Details and trade-offs:
[`runbooks/secrets.md` → Hosted ECDSA backend](../../runbooks/secrets.md#hosted-ecdsa-backend-aws-kms).
Staying on Secrets Manager for ECDSA instead? Skip the KMS step and run
`bootstrap-ack-keys` without the ARN exported; it creates both secrets.

**Accept only ECDSA accounts.** The KMS key above protects ECDSA acks only;
Falcon has no hosted signer, so any Falcon account this stack registers is
acked by the Secrets Manager key loaded into the task. Set
`GUARDIAN_ALLOWED_ACCOUNT_SCHEMES=ecdsa` in `.env.aws-ecs` (the template does)
so new accounts cannot bypass KMS. The Falcon secret is still bootstrapped and
still required at startup; with the gate on it stays dormant unless a Falcon
account already exists ([`PRODUCTION.md`](../../PRODUCTION.md#account-signature-scheme)).

**Storage encryption key (recommended).**

```bash
./scripts/aws-deploy.sh bootstrap-storage-encryption-key    # <stack>/server/storage-encryption-key
```

Bootstrapping creates the key but does **not** turn encryption on. The
on-switch is `GUARDIAN_STORAGE_ENCRYPTION_SECRET_NAME` in `.env.aws-ecs`, set to
the secret **name** the command printed (Terraform resolves the ARN, wires the
task-role `secretsmanager:GetSecretValue` grant, and injects the runtime
`GUARDIAN_STORAGE_ENCRYPTION_KEY_SECRET_ID`). Enable it on a stack whose store
is still empty. Rotation and compromise response:
[`runbooks/secrets.md` → Storage encryption key](../../runbooks/secrets.md#storage-encryption-key).

**Verified database TLS (recommended).** By default `DATABASE_URL` uses
`sslmode=require` (encrypted, certificate not verified). For `verify-full`,
build the combined CA bundle (regional RDS roots plus Amazon Trust Services
roots, because RDS Proxy presents an ACM certificate), store it as a secret,
and set `TF_VAR_rds_ca_bundle_secret_arn` in `.env.aws-ecs`. Procedure:
[`SERVER_AWS_DEPLOY.md` → Database TLS verification](../../SERVER_AWS_DEPLOY.md#database-tls-verification).
For a stack that is already live, follow
[`runbooks/enable-db-tls.md`](../../runbooks/enable-db-tls.md) instead.

**Dashboard operator allowlist (if the dashboard is used).** There is no
bootstrap command: the server never holds an operator private key. Each
operator generates their own Falcon keypair on a trusted device and hands you
the `0x…` public key ([`DASHBOARD.md` → Enrolling an operator](../../DASHBOARD.md#enrolling-an-operator)).
Then either set `GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON` (Terraform creates the
secret; grants `dashboard:read` only) or manage the secret yourself with object
entries and set `GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ARN` (the only path that
can grant `accounts:pause`). The allowlist is re-read on every challenge and
authenticated request, so later changes need no restart.

### A2. Review the environment

The `prod` profile sets the HA and throughput knobs for you. **Verify** these
rather than overriding them:

| Set by the prod profile | Effect |
|---|---|
| `GUARDIAN_ENV=prod` | Prod-stage startup guards (filesystem backend refused, ephemeral ACK identity refused, 0-req/replica rate limit refused); ACK keys load from Secrets Manager or KMS. |
| `GUARDIAN_DASHBOARD_CURSOR_SECRET` | Injected from the secret bootstrapped in A1 into every task. |
| `GUARDIAN_MAX_REPLICAS` | The greater of desired count and autoscaling max (6 by default). Each replica enforces `global / GUARDIAN_MAX_REPLICAS`; an override is clamped up to that steady-state capacity. |
| `GUARDIAN_RATE_BURST_PER_SEC` / `GUARDIAN_RATE_PER_MIN` | `200` / `5000`. `PER_MIN` is per IP across HTTP **and** gRPC, so size it for both transports ([checklist](../../PRODUCTION.md#production-checklist)). |
| `GUARDIAN_DB_POOL_MAX_SIZE`, canonicalization concurrency | `32` and `50`. |
| RDS | `db.r6g.large`, 50 GiB with autoscaling to 200 GiB, 7-day backup retention, deletion protection on, final snapshot on destroy, storage encrypted. Multi-AZ is **off**: set `TF_VAR_rds_multi_az=true` if you want standby failover ([`PRODUCTION.md` → Durability](../../PRODUCTION.md#durability-and-recovery)). |
| RDS Proxy, autoscaling 2 to 6 tasks | Connection pooling and HA. |
| Metrics | `GUARDIAN_METRICS_ENABLED=true` bound to loopback inside the task, scraped by the ADOT sidecar into CloudWatch (namespace `<Stack>/Server`) with a dashboard and alarms. No bearer token is involved because nothing outside the task can reach the port. Set `TF_VAR_alarm_actions` to an SNS topic ARN or the alarms notify nobody. |
| `GUARDIAN_LOG_FORMAT=json` | For CloudWatch Logs Insights. |

What **you** provide in `.env.aws-ecs`:

| Variable | Notes |
|---|---|
| `DEPLOY_STAGE=prod`, `STACK_NAME`, `AWS_REGION`, `GUARDIAN_NETWORK_TYPE` | Stack identity and network. |
| `GUARDIAN_SERVER_FEATURES` | `postgres`. |
| `GUARDIAN_CORS_ALLOWED_ORIGINS` | Exact browser origins, comma-separated; wildcards are rejected. Unset means permissive `Any` with credentials disabled, which is not for production. |
| `TF_VAR_guardian_ack_ecdsa_kms_key_arn` | From A1. |
| `GUARDIAN_ALLOWED_ACCOUNT_SCHEMES=ecdsa` | From A1: new accounts must use the KMS-backed scheme. Terraform injects it only when set; unset keeps both schemes. |
| `GUARDIAN_STORAGE_ENCRYPTION_SECRET_NAME` | From A1; turns encryption on. |
| `TF_VAR_rds_ca_bundle_secret_arn` | From A1; turns verified DB TLS on. |
| `GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON` or `..._SECRET_ARN` | From A1, if the dashboard is used. |
| `DOMAIN_NAME`, `SUBDOMAIN`, `ACM_CERTIFICATE_ARN`, plus `ROUTE53_ZONE_ID` or `CLOUDFLARE_*` | The public hostname and what is built for it. Terraform defaults the hostname to `guardian.openzeppelin.com` when these are unset **or empty** in the shell, so set your own. DNS records are created only when a zone id is set; HTTPS, and gRPC through the ALB on `:443`, only when the certificate ARN is set ([HTTPS and gRPC](../../SERVER_AWS_DEPLOY.md#https-and-grpc)). Without a certificate the ALB serves plain HTTP on its raw DNS name and does not route gRPC. |
| `TF_VAR_alarm_actions`, `TF_VAR_rds_multi_az` | Alerting destination and standby failover, as above. |

Any Terraform variable can be overridden through `TF_VAR_*`; the full list and
the stage defaults are in
[`SERVER_AWS_DEPLOY.md` → Terraform Variables](../../SERVER_AWS_DEPLOY.md#terraform-variables)
and [Stage Profiles](../../SERVER_AWS_DEPLOY.md#stage-profiles). Override only
what you need.

> **Single task?** `TF_VAR_server_autoscaling_enabled=false` with
> `TF_VAR_server_desired_count=1` is a valid cost choice for a test network, but
> it is not HA. Coordination stays correct (Postgres-backed) and
> `GUARDIAN_MAX_REPLICAS` follows the steady-state capacity you configured.

### A3. Deploy

```bash
./scripts/aws-deploy.sh build                 # build + push the image to ECR
./scripts/aws-deploy.sh plan                  # validates secrets exist, then terraform plan against the pushed digest
./scripts/aws-deploy.sh deploy --skip-build   # apply the reviewed plan
./scripts/aws-deploy.sh status                # Terraform outputs: ALB DNS, hostname, metrics namespace
```

`deploy` without `--skip-build` builds and applies in one step. Do not rebuild
between `plan` and `deploy --skip-build`. Reference:
[`SERVER_AWS_DEPLOY.md` → Deploy](../../SERVER_AWS_DEPLOY.md#deploy).

### A4. Validate

```bash
./scripts/aws-deploy.sh status          # raw Terraform outputs: alb_url (raw ALB) and, with a domain, the custom hostname
url=https://guardian.example.com        # your hostname: the certificate is issued for it, not for the ALB name
curl -sf --max-time 10 "$url/"                                # liveness
curl -sf --max-time 10 "$url/pubkey" | jq .                   # Falcon commitment
curl -sf --max-time 10 "$url/pubkey?scheme=ecdsa" | jq .      # ECDSA commitment + pubkey (derived from the KMS key)
grpcurl -import-path crates/server/proto -proto guardian.proto -d '{}' guardian.example.com:443 guardian.Guardian/GetPubkey
```

A stack without a certificate is reachable only over plain HTTP at the
`alb_url` output (`http://<alb-dns>`), and the ALB does not route gRPC at all in that
mode, so the `grpcurl` check applies to HTTPS stacks only. With a certificate,
use the hostname it was issued for: the raw ALB name fails TLS verification.

Record both commitments. Then read the startup banner in the logs
(`./scripts/aws-deploy.sh logs`) and confirm:

- `coordination mode="shared" backend="postgres" stage="prod" max_replicas=<N> cursor_secret="configured"`.
  Anything else is not safe to run with more than one task.
- `ack signers … ecdsa_backend="aws-kms"`: the ECDSA key is the KMS one. The
  KMS sign probe runs before this banner, so reaching it means the probe passed.
- `storage backend storage=Postgres`, and no encryption or TLS error before the
  listeners bind (both fail fast).

Then, outside the server:

- Metrics are arriving in CloudWatch and the dashboard populates
  ([Verify metrics after a deploy](../../SERVER_AWS_DEPLOY.md#verify-metrics-after-a-deploy)).
- The RDS instance shows 7-day backups, deletion protection, and encryption;
  do a restore drill before you need one ([`runbooks/backup-restore.md`](../../runbooks/backup-restore.md#verify-backups-do-this-now-not-during-an-incident)).
- On a staging stack, verify rate-limit keying on the gRPC path as the
  [checklist](../../PRODUCTION.md#production-checklist) describes.
- Run the relevant SDK or dashboard smoke path against the public hostname
  (`smoke-test-rust-multisig-sdk`, `smoke-test-ts-multisig-sdk`, or
  `smoke-test-operator-dashboard`). The SDK smoke paths default to Falcon:
  the demo's scheme prompt (`[1]`), `examples/smoke-web` (`falcon`), and
  `examples/rust`, which has no ECDSA option. Against the `ecdsa`-only gate
  this template sets, select ECDSA (`[2]` in the demo, `ecdsa` in smoke-web)
  or registration fails with `signature_scheme_not_allowed`; `examples/rust`
  cannot register on this stack until it gains an ECDSA path.

### A5. Day two

- **Routine version upgrades** ship through the **AWS Deploy** GitHub Actions
  workflow, which verifies a published GHCR release against its provenance
  attestation and rolls only the image
  ([Deploying a published image from GitHub Actions](../../SERVER_AWS_DEPLOY.md#deploying-a-published-image-from-github-actions)).
  When a release changes the task definition (new env vars, secrets, IAM),
  apply that release's Terraform with `scripts/aws-deploy.sh` first.
- **Secrets**: replacement, rotation, and compromise response per category in
  [`runbooks/secrets.md`](../../runbooks/secrets.md). ACK key replacement is an
  identity change.
- **Backups and restore**: [`runbooks/backup-restore.md`](../../runbooks/backup-restore.md),
  including the Guardian-level reconciliation after a point-in-time restore.
- **Scaling**: [`runbooks/horizontal-scaling.md`](../../runbooks/horizontal-scaling.md).

## Track B: Self-managed Docker image (no AWS)

You run `ghcr.io/openzeppelin/guardian` yourself (a VM, a Compose host,
Kubernetes) with no AWS account involved. Everything the AWS stack does for you
becomes yours: the database and its backups, TLS termination and client-IP
forwarding at your ingress, protecting the key files and the env file, and
scraping metrics. The Guardian configuration itself is small; this track is
mostly about doing those surrounding jobs correctly.

The committed [`docker-compose.yml`](./docker-compose.yml) plus
[`.env.example`](./.env.example) is the runnable form. The env file is the
container's env file, so it is also the basis for `docker run --env-file` or a
Kubernetes Secret, together with the handful of values the compose file fixes
(see the end of this track).

### B1. Generate the Guardian identity

The server needs a stable ACK keypair; the non-prod default of minting a fresh
one per boot would freeze every account that pinned the previous commitment,
and `GUARDIAN_ENV=prod` refuses it. Without Secrets Manager the keys live in
two owner-only files loaded through `GUARDIAN_ACK_SECRET_PROVIDER=file`. The
image ships `ack-keygen`, so generate them once with nothing but Docker:

```bash
cd docs/guides/production
mkdir -p ack-keys
docker run --rm --user "$(id -u):$(id -g)" -v "$PWD/ack-keys:/out" \
  ghcr.io/openzeppelin/guardian:<version> /app/ack-keygen --out-dir /out
```

Use the same tag you will put in `.env` (later than `v0.17.0`, see
[Decisions](#decisions-every-track-shares)). It writes
`ack-keys/ack-falcon-secret-key` and `ack-keys/ack-ecdsa-secret-key` as `0600`
files owned by you (the `--user` flag), and refuses to overwrite files that
already exist, so re-running it can never silently replace an identity. If the
second file fails to write, it removes the first, so a rerun starts clean. On
rootless Docker or Podman drop `--user` (your uid is already root inside the
container, and the files come out owned by you); on an SELinux-enforcing host
add `:Z` to the volume flag. `ack-keys/` is gitignored. Back the two files up out of band and
treat them like any private key: the server refuses a file readable by group
or others, and losing or regenerating them is a Guardian identity change
(`SwitchGuardian` for every account). The compose file declares the two files
(and `operators.json`) as Compose secrets and configs, so `up` aborts if any of
them is missing rather than mounting an empty directory in its place. Details:
[`runbooks/secrets.md` → Self-hosted stable identity](../../runbooks/secrets.md#self-hosted-stable-identity-without-aws).

### B2. Configure

```bash
cp .env.example .env
cp operators.example.json operators.json
( umask 077; set -C; printf '{"active":"k1","keys":{"k1":"%s"}}\n' "$(openssl rand -base64 32)" > storage-encryption-keys.json )
```

The third line creates the storage-encryption key document as a new `0600`
file (gitignored). `set -C` makes the redirection refuse to overwrite an
existing file: once a store has been encrypted with this document, replacing
it with a fresh random key makes every record unreadable, so this line must
never clobber. Rotation is a different procedure ([`runbooks/secrets.md` → Rotation](../../runbooks/secrets.md#rotation)).
Fill in `.env`. Each value maps to a checklist item:

| Set | Why |
|---|---|
| `GUARDIAN_VERSION` | An explicit release later than `v0.17.0` (see [Decisions](#decisions-every-track-shares)), never `latest`. The template leaves it blank so Compose refuses to start until you choose one. |
| `GUARDIAN_NETWORK_TYPE` | Required; the server refuses to start without it. |
| `DATABASE_URL` | Your production Postgres with `sslmode=verify-full&sslrootcert=/etc/guardian/tls/ca.pem` (B3). The template's `POSTGRES_PASSWORD` is for the bundled smoke-only database. |
| `storage-encryption-keys.json` | The `{ "active": kid, "keys": { kid: base64-32-bytes } }` key document, mounted as a Compose secret and read through `GUARDIAN_STORAGE_ENCRYPTION_KEY_FILE`. Its presence turns encryption on. It is the same document Secrets Manager holds on tracks A and C, so rotation works the same way: add a key, repoint `active`, keep the old key ([`runbooks/secrets.md` → Rotation](../../runbooks/secrets.md#rotation)). Keep it `0600` (the server refuses anything wider) and keep a copy with your database backups; ciphertext is unrecoverable without it ([nonce budget](../../runbooks/secrets.md#nonce-budget) if you write at very high volume). |
| `GUARDIAN_DASHBOARD_CURSOR_SECRET` | `openssl rand -hex 32`, pinned; identical on every replica. |
| `GUARDIAN_CORS_ALLOWED_ORIGINS` | Exact browser origins. |
| `GUARDIAN_ALLOWED_ACCOUNT_SCHEMES=ecdsa` | Recommended: new accounts use the ECDSA scheme Guardian is standardizing on. On this track both ACK keys are files either way, so the gain is alignment rather than custody; drop it if your wallets need Falcon. |
| `GUARDIAN_METRICS_ENABLED=true`, `GUARDIAN_METRICS_BEARER_TOKEN` | Metrics on `:9464`, published on loopback only, gated by the token (B6). |
| `operators.json` | Operator allowlist, if the dashboard is used. The example file is an empty allowlist (`[]`), which the server accepts; the dashboard is unreachable until you add entries. Use object entries with explicit permissions (`accounts:pause` is only grantable this way); the file is re-read on every request, so edits need no restart. Keep it read-only to the container and restricted on the host: it is the dashboard's trust root ([`DASHBOARD.md` → Allowlist payload](../../DASHBOARD.md#allowlist-payload)). |

An allowlist entry looks like this (the public key comes from the operator,
see [Enrolling an operator](../../DASHBOARD.md#enrolling-an-operator)):

```json
[{ "public_key": "0x<falcon-operator-public-key>", "permissions": ["dashboard:read", "accounts:pause"] }]
```

The compose file fixes what the topology dictates: `GUARDIAN_ENV=prod`, the
file ACK provider and its two paths, the key-document path, the operators file
path, the keystore volume, and `GUARDIAN_METRICS_ADDR=0.0.0.0:9464`
(container-internal; the host publishes it on `127.0.0.1`). The keystore volume
holds the imported ACK keys and per-account material; keep it persistent.

**`GUARDIAN_ENV=prod` is the stage profile.** Besides the fail-fast guards, it
switches the server's own defaults from local-development values to the
production ones, the same values the AWS Terraform profile injects, so a
self-managed deployment does not list them. Set any of these only to change
it:

| Variable | Default outside prod | Default with `GUARDIAN_ENV=prod` |
|---|---|---|
| `GUARDIAN_RATE_BURST_PER_SEC` | `10` | `200` (per IP and endpoint; then size for your traffic) |
| `GUARDIAN_RATE_PER_MIN` | `60` | `5000` (per IP across HTTP **and** gRPC) |
| `GUARDIAN_DB_POOL_MAX_SIZE` / `GUARDIAN_METADATA_DB_POOL_MAX_SIZE` | `16` each | `32` each. Two independent pools, so budget `(storage + metadata) × replicas` (64 per replica) plus headroom for migrations against your Postgres `max_connections`. |
| `GUARDIAN_CANONICALIZATION_MAX_CONCURRENT_ACCOUNTS` | `10` | `50` |
| `GUARDIAN_LOG_FORMAT` | `text` | `json` |

What stays yours because it depends on your topology, not on the stage:
`GUARDIAN_MAX_REPLICAS` (your replica count; `1` for one server), the pinned
`GUARDIAN_DASHBOARD_CURSOR_SECRET`, `GUARDIAN_METRICS_ENABLED` with its bearer
token, and `GUARDIAN_CORS_ALLOWED_ORIGINS`. The compose file and `.env.example`
carry all four. What the AWS profile provides at the infrastructure layer has
no environment variable at all and is yours to build: RDS Proxy connection
pooling, 2 to 6 tasks with autoscaling behind an ALB with TLS, backups with
deletion protection, and CloudWatch alarms. Steps 3, 5, 6, and 7 cover each.

### B3. Database

The bundled Postgres in the compose file exists so the stack runs and
smoke-tests standalone. It has no TLS and no backups, so it is not a production
database. For production:

- Use a managed Postgres (or one you operate with backups and point-in-time
  recovery) and set `DATABASE_URL` in `.env`.
- Verify its certificate: mount the provider's CA bundle (uncomment the
  `./certs/ca.pem` volume in the compose file) and use
  `sslmode=verify-full&sslrootcert=/etc/guardian/tls/ca.pem`. The server fails
  closed on a bad chain or hostname instead of connecting unverified
  ([`CONFIGURATION.md` → Database TLS](../../CONFIGURATION.md#database-tls);
  a self-contained rehearsal is the [postgres-tls guide](../postgres-tls/README.md)).
- Start only the server: `docker compose up -d --no-deps server`.
- Own the backup story: Guardian is stateless, so durability is your database's
  durability. What a restore means for Guardian accounts, and why the
  encryption key is part of the recovery set, is in
  [`PRODUCTION.md` → Durability and recovery](../../PRODUCTION.md#durability-and-recovery)
  and the reconciliation section of
  [`runbooks/backup-restore.md`](../../runbooks/backup-restore.md#after-the-restore-guardian-level-reconciliation).

### B4. Run

```bash
docker compose up -d                     # evaluation: bundled Postgres, no TLS, no backups
docker compose up -d --no-deps server    # production: your own Postgres via DATABASE_URL (B3)
docker compose logs -f server
```

`.env` is the only source of operator values: the compose file passes it as
the container's `env_file` and never interpolates a server variable, so an
exported `DATABASE_URL` or `GUARDIAN_NETWORK_TYPE` in your shell cannot
retarget the stack. Compose reads the shell only for `GUARDIAN_VERSION` and
the host port and bind settings, where an exported name does win over `.env`.
A missing `.env` aborts `up`.

The three secret files and `operators.json` are bind-mounted from the host
(the compose file deliberately sets no `uid`, `gid`, or `mode` on them: with
those, Compose copies the file in at creation and a restart keeps the stale
copy). Their host permissions carry through, which is why they must be `0600`.
After rotating the storage-encryption key document, `docker compose restart
server` re-reads it; `operators.json` is re-read on every request with no
restart at all.

Tracks B and C use distinct Compose project names, so running both from this
directory never shares a database or keystore volume between them. A reused
volume matters: the store remembers the encryption key id it was initialized
with, and pointing it at a different key fails at read time. In evaluation,
`docker compose down -v` resets it.

### B5. Put an ingress in front

The container speaks plaintext HTTP on `3000` and plaintext gRPC (h2c) on
`50051`, published on `127.0.0.1` by default. Production traffic must come
through a TLS-terminating reverse proxy or load balancer that:

- forwards to both ports, with HTTP/2 to the gRPC upstream;
- appends `X-Forwarded-For` on **both** listeners (gRPC proxying often needs
  separate header configuration), or strips client-sent `X-Forwarded-For` if
  it identifies callers with `X-Real-IP`;
- is the only thing that can reach `3000`/`50051`. Forwarding headers are
  trusted whenever present, so a client that connects directly picks its own
  rate-limit identity.

The reasoning and the two probes that confirm your ingress is keying rate
limits on real client addresses are in
[`PRODUCTION.md` → Running behind your own ingress](../../PRODUCTION.md#running-behind-your-own-ingress-non-aws).
A working Caddy configuration for HTTP + h2c gRPC with health checks is the
[`Caddyfile`](../horizontal-scaling/Caddyfile) in the horizontal-scaling guide.
If the load balancer runs on another machine, set `GUARDIAN_BIND_ADDR` to the
private interface it reaches and firewall the ports to it.

### B6. Validate

The committed smoke test does all of this against a throwaway identity and
tears down afterwards. It needs Docker, `jq`, `curl`, `openssl`, and outbound
HTTPS to the Miden network's RPC endpoint: the server connects to it at
startup, before the listeners bind, on every track. No Rust toolchain: the
identity comes from the image's `ack-keygen`.

```bash
./smoke.sh                              # image tag from ./.env (B2); or GUARDIAN_VERSION=<tag> ./smoke.sh
SMOKE_PULL_POLICY=missing GUARDIAN_VERSION=<tag> ./smoke.sh   # image you built locally under that tag
```

The committed compose file pins `pull_policy: always`, so without the
override Compose refuses any tag that is not on GHCR. `SMOKE_PULL_POLICY`
(`always`, `missing`, `never`) is rewritten into the script's scratch copy
only; the guide's artifact is unchanged.

By hand, against your real stack:

```bash
set -a; source .env; set +a                                    # for the token below
curl -sf 127.0.0.1:3000/pubkey | jq .                          # Falcon commitment
curl -sf "127.0.0.1:3000/pubkey?scheme=ecdsa" | jq .           # ECDSA commitment + pubkey
docker compose logs server | grep -E 'coordination|ack signers|storage backend|listeners'
curl -s -o /dev/null -w '%{http_code}\n' 127.0.0.1:9464/metrics                          # 401
curl -sf -H "Authorization: Bearer $GUARDIAN_METRICS_BEARER_TOKEN" 127.0.0.1:9464/metrics | head
docker compose restart server && curl -sf "127.0.0.1:3000/pubkey?scheme=ecdsa"          # same commitment
```

The ports are published on `127.0.0.1`, so address them as such (`localhost`
can resolve to `::1`).

The banner is JSON by default in prod (one object per line, the message name
in `"message"`). If the `ack signers` line has no `account_schemes` key, or
the logs are plain text although you set nothing, the image predates this
guide: stop, and pin a newer release before trusting anything below. Expect `"message":"coordination"` with `"mode":"shared"`,
`"backend":"postgres"`, `"stage":"prod"`, `"max_replicas":1`,
`"cursor_secret":"configured"`; `"message":"ack signers"` with
`"ecdsa_backend":"in-memory"` (the file-provided key) and
`"account_schemes":"ecdsa"` if you set the gate; `"message":"storage
backend"` with `"storage":"Postgres"`; `"message":"canonicalization"` with
`"max_concurrent_accounts":50` (the prod default applied, not the code default
of 10); `"message":"listeners"` with `"metrics":"0.0.0.0:9464"` rather than
`"disabled"`; and an unchanged `/pubkey` after the restart. (With
`GUARDIAN_LOG_FORMAT=text` the same fields read `coordination mode="shared"
backend="postgres" stage="prod" …`.) Record both commitments. Storage encryption and database TLS have no success log line:
they are validated at startup and a bad key or certificate prevents the
listeners from binding. Then run an SDK or dashboard smoke path through your
ingress, and the rate-limit probes from B5. The SDK smoke paths default to
Falcon (demo prompt `[1]`, `examples/smoke-web`, `examples/rust`); if you kept
`GUARDIAN_ALLOWED_ACCOUNT_SCHEMES=ecdsa`, select ECDSA (`[2]` in the demo,
`ecdsa` in smoke-web) or registration fails with
`signature_scheme_not_allowed`. `examples/rust` has no ECDSA option yet.

### B7. Scaling out

Two or more replicas need exactly what the AWS prod profile provides:

- the same `ack-keys/` files and the same `storage-encryption-keys.json` on
  every replica (one Guardian identity, one key set);
- the same `GUARDIAN_DASHBOARD_CURSOR_SECRET`;
- `GUARDIAN_MAX_REPLICAS` equal to the replica count on every replica, so the
  global rate limit partitions correctly (the prod stage refuses a limit that
  partitions to zero per replica);
- one shared Postgres (sessions, challenges, the canonicalization lease and
  replay state are shared through it, with nothing to enable);
- a load balancer with health checks on both ports.

The [horizontal-scaling guide](../horizontal-scaling/README.md) is a runnable
two-replica version of this with Caddy, and
[`runbooks/horizontal-scaling.md`](../../runbooks/horizontal-scaling.md) is the
operational contract, including rolling-upgrade behavior across schema
migrations.

### Kubernetes and plain `docker run`

`.env` plus the values the compose file fixes is the whole runtime contract.
`.env.example` sets `DATABASE_URL` explicitly (Compose would otherwise fall
back to the bundled Postgres; plain Docker has no such fallback), so make sure
it points at your production database before running this. `GUARDIAN_ENV=prod`
brings the production defaults with it, so the tuning knobs need no flags.
Plain Docker has no equivalent of Compose secrets: a bind mount whose host
file is missing silently creates an empty **directory** at the target, and the
server then fails with a misleading read error. Check the three files exist
before running this, and add the CA bind mount only once `certs/ca.pem` exists
and `DATABASE_URL` uses `verify-full`:

```bash
for f in ack-keys/ack-falcon-secret-key ack-keys/ack-ecdsa-secret-key storage-encryption-keys.json operators.json; do
  [ -f "$f" ] || { echo "missing $f"; exit 1; }
done
docker run -d --restart unless-stopped --env-file .env \
  -e GUARDIAN_ENV=prod -e GUARDIAN_KEYSTORE_PATH=/var/guardian/keystore \
  -e GUARDIAN_ACK_SECRET_PROVIDER=file \
  -e GUARDIAN_ACK_FALCON_SECRET_PATH=/etc/guardian/ack/ack-falcon-secret-key \
  -e GUARDIAN_ACK_ECDSA_SECRET_PATH=/etc/guardian/ack/ack-ecdsa-secret-key \
  -e GUARDIAN_STORAGE_ENCRYPTION_KEY_FILE=/etc/guardian/storage-encryption-keys.json \
  -e GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE=/etc/guardian/operators.json \
  -e GUARDIAN_METRICS_ADDR=0.0.0.0:9464 \
  -v "$PWD/ack-keys:/etc/guardian/ack:ro" \
  -v "$PWD/storage-encryption-keys.json:/etc/guardian/storage-encryption-keys.json:ro" \
  -v "$PWD/operators.json:/etc/guardian/operators.json:ro" \
  -v guardian-keystore:/var/guardian/keystore \
  -p 127.0.0.1:3000:3000 -p 127.0.0.1:50051:50051 -p 127.0.0.1:9464:9464 \
  ghcr.io/openzeppelin/guardian:<version>
# verified DB TLS (B3): add  -v "$PWD/certs/ca.pem:/etc/guardian/tls/ca.pem:ro"
```

On Kubernetes, the env file becomes a `Secret` consumed with `envFrom`, the
ACK keys and the key document `Secret`s mounted with `defaultMode: 0400` (the
`0644` default is refused by the owner-only check on both), `GUARDIAN_MAX_REPLICAS`
equals the Deployment's replica count, and the Service for `3000`/`50051` must
preserve client IPs to the ingress (see the SNAT note in the PRODUCTION ingress
section).

## Track C: Self-managed Docker image with AWS secret custody

Take this track only when running the ECS reference stack (track A) is not
possible for you. It is track B with the secrets moved off the host: the Falcon ACK key comes from
Secrets Manager, the ECDSA ACK key lives in KMS and never enters the process,
the storage-encryption key document (optional, with multi-key rotation) and
the operator allowlist can be Secrets Manager secrets. Because only ECDSA has
a hosted signer, keep `GUARDIAN_ALLOWED_ACCOUNT_SCHEMES=ecdsa` (the template
sets it) so every account this Guardian registers is acked from KMS. The
database, ingress, backups, metrics scraping, and scaling are exactly as in
track B, steps 3 to 7.
This is the full-hardening sibling of the focused
[aws-signers guide](../aws-signers/README.md).

1. Bootstrap the secrets with the track A commands (cursor secret excluded; on
   this track it is a value in your env file):
   `bootstrap-kms-ecdsa-key`, then `bootstrap-ack-keys` with the ARN exported,
   then `bootstrap-storage-encryption-key`. Note the secret names and the KMS
   ARN they print.
2. Configure and run:

   ```bash
   cd docs/guides/production
   cp .env.aws-no-ecs.example .env.aws-no-ecs      # fill in region, secret ids, KMS key, cursor secret, origins, version; keep GUARDIAN_ALLOWED_ACCOUNT_SCHEMES=ecdsa
   docker compose --env-file .env.aws-no-ecs -f docker-compose.aws-no-ecs.yml up -d
   ```

   The container needs AWS credentials because `GUARDIAN_ENV=prod` makes it
   call Secrets Manager and KMS. AWS credentials are not read from your shell:
   put static keys in `.env.aws-no-ecs` (it is the container's env file), or leave
   them out and let the SDK's default chain use the host's instance or
   container role, which is the right choice for a long-lived host. The IAM
   principal needs `secretsmanager:GetSecretValue` on the secrets and
   `kms:GetPublicKey` + `kms:Sign` on the key. As on track B, `.env.aws-no-ecs` is
   the only source of server values: the compose file never interpolates
   them, so an exported `DATABASE_URL` in your shell cannot retarget the
   stack. Only `GUARDIAN_VERSION` and the host port and bind settings are
   read through interpolation, where the shell wins over the file.
3. Validate as in B6. Expect `ecdsa_backend="aws-kms"` in the banner; the KMS
   sign probe runs before the banner is printed, so a banner means it passed.

## Production checklist coverage

Where each [`PRODUCTION.md` checklist](../../PRODUCTION.md#production-checklist)
item is satisfied, per track:

| Checklist item | A (ECS) | B (Docker) | C (Docker + AWS secrets) |
|---|---|---|---|
| `DEPLOY_STAGE=prod` / prod-stage guards and runtime defaults | `.env.aws-ecs`, profile sets `GUARDIAN_ENV=prod` and injects the values | compose sets `GUARDIAN_ENV=prod`; the server applies the same defaults | same as B |
| `postgres` feature, no filesystem backend | A2 | Decisions (published image is `postgres`) | same |
| Stable ACK identity; ECDSA backend decision | A1 (KMS + Secrets Manager) | B1 (`file` provider) | C1 (KMS + Secrets Manager) |
| New accounts restricted to ECDSA | `.env.aws-ecs` (`GUARDIAN_ALLOWED_ACCOUNT_SCHEMES`, Terraform-injected) | `.env` | `.env.aws-no-ecs` |
| `DATABASE_URL` from a managed secret; verified DB TLS | A1 CA bundle, Terraform-managed RDS secret | B3 (`verify-full` + mounted CA) | B3 |
| Storage encryption against an empty store | A1 + `GUARDIAN_STORAGE_ENCRYPTION_SECRET_NAME` | B2 (`storage-encryption-keys.json` via `GUARDIAN_STORAGE_ENCRYPTION_KEY_FILE`, rotatable) | C1 (the same document in Secrets Manager) |
| RDS durability: backups, deletion protection, final snapshot, Multi-AZ decision | A2, A4 | B3 (your database) | B3 |
| `GUARDIAN_CORS_ALLOWED_ORIGINS` | A2 | B2 | B2 |
| Operator allowlist, object entries for `accounts:pause` | A1 (`_SECRET_ARN` path) | B2 (`operators.json`) | C (Secrets Manager secret id) |
| Pinned `GUARDIAN_DASHBOARD_CURSOR_SECRET` | A1 bootstrap, injected by Terraform | B2 | `.env.aws-no-ecs` |
| `GUARDIAN_MAX_REPLICAS` for HA rate partitioning | A2 (profile) | B7 | B7 |
| Rate limits sized for HTTP + gRPC; keying verified on staging | A2, A4 | B2 tuning, B5 probes | same |
| Metrics protected | A2 (loopback + ADOT, CloudWatch alarms with `alarm_actions`) | B2 + B6 (loopback publish + bearer token) | same |
| Validate `/`, `/pubkey`, smoke path | A4 | B6 / `smoke.sh` | C3 |

## Troubleshooting

| Symptom | Likely cause / where to look |
|---|---|
| `plan`/`deploy` abort: missing dashboard cursor secret | Run `bootstrap-dashboard-cursor-secret` (A1); prod deploys require it and never create it. |
| `plan`/`deploy` abort: missing ACK or storage-encryption secret | The matching `bootstrap-*` command was not run in this region/stack, or `GUARDIAN_STORAGE_ENCRYPTION_SECRET_NAME` names a secret that does not exist. |
| Startup fails: `GUARDIAN_NETWORK_TYPE` unset or unrecognized | Set it explicitly; there is no fallback network. |
| Startup fails: `Failed to create network client` | The server connects to the Miden RPC endpoint of `GUARDIAN_NETWORK_TYPE` (or `GUARDIAN_MIDEN_RPC_ENDPOINT`) before binding its listeners. Allow outbound HTTPS to it, or point at a reachable node. |
| Startup fails: filesystem backend in prod | Prod refuses filesystem. Use the `postgres` image with `DATABASE_URL`. |
| Startup fails: `AWS_REGION is required when GUARDIAN_ENV=prod` (track B) | `GUARDIAN_ACK_SECRET_PROVIDER=file` is missing, so prod defaulted to Secrets Manager. |
| Startup fails: `GUARDIAN_ACK_SECRET_PROVIDER=none is not allowed` | Prod refuses an ephemeral identity. Provide keys via `file` or `aws`. |
| Startup fails: ACK secret file, or storage encryption key file, must not be accessible by group or others | `chmod 600` the key files (Kubernetes: `defaultMode: 0400`). `ack-keygen --out-dir` and the B2 `umask 077` one-liner create them that way. |
| `ack-keygen` exits with "already exists; refusing to overwrite" | Deliberate: it never replaces an identity. Point `--out-dir` at an empty directory only if you mean to create a *new* Guardian identity (a `SwitchGuardian` migration for every account). |
| `mode="single-process" backend="filesystem"` in the banner | Not multi-replica safe; the backend must be Postgres ([runbook](../../runbooks/horizontal-scaling.md)). |
| `cursor_secret="ephemeral"` in the banner | `GUARDIAN_DASHBOARD_CURSOR_SECRET` is unset; the server boots but pagination cursors break across replicas and restarts. |
| Sign probe / `configuration_error` at startup (KMS) | Wrong key spec (`ECC_SECG_P256K1` / `SIGN_VERIFY`) or missing `kms:Sign` ([runbook](../../runbooks/secrets.md#hosted-ecdsa-backend-aws-kms)). |
| Startup fails with a storage-encryption marker or key error | A key against a store that already holds plaintext, or a malformed / wrong-length key. Enable only against an empty store. |
| Decryption errors after changing the key, or a marker error naming a key id | The store remembers the key id it was initialized with and a different key under that id fails at read time. Restore the original key document; in evaluation, `docker compose down -v` resets the bundled database. |
| Compose refuses to start: secret or config file not found | `ack-keys/`, `storage-encryption-keys.json`, or `operators.json` is missing. Run B1 and the B2 setup lines. |
| Certificate errors with `verify-full` | Missing or wrong CA in the bundle (RDS Proxy chains to Amazon Trust Services), or the hostname does not match the certificate SAN ([runbook](../../runbooks/enable-db-tls.md)). |
| Every client shares one rate-limit budget, or a forged `X-Forwarded-For` is honored | Ingress misconfiguration; see [Running behind your own ingress](../../PRODUCTION.md#running-behind-your-own-ingress-non-aws). |
| Rate limit rejects all traffic in prod | The global limit partitions to 0 per replica; raise `GUARDIAN_RATE_*` or lower `GUARDIAN_MAX_REPLICAS`. |
| `/configure` returns 403 `signature_scheme_not_allowed` | `GUARDIAN_ALLOWED_ACCOUNT_SCHEMES` excludes the scheme the wallet used to create the account. Intended for new Falcon accounts on an `ecdsa`-only Guardian; the wallet must create the account with an allowed scheme. "Existing" means present in **this** Guardian's metadata: a Falcon account re-onboarding after a metadata restore, or arriving via `SwitchGuardian` from another Guardian, counts as new and is rejected too. Allow `falcon,ecdsa` temporarily for that migration, then tighten again. |
| `invalid GUARDIAN public key binding` after a restart or redeploy | The ACK identity changed (regenerated files, new secret, new KMS key). Restore the original keys, or migrate accounts with `SwitchGuardian`. |

See [`TROUBLESHOOTING.md`](../../TROUBLESHOOTING.md) for the full error-code
playbook.
