# Production Guide

This is the production entry point for Guardian operators. It summarizes the
supported production shape and points to the detailed deploy, architecture,
configuration, and runbook docs.

## Supported shape

Every production Guardian, wherever it runs, has the same shape:

- The **Postgres** storage backend: `GUARDIAN_SERVER_FEATURES=postgres`, plus
  `evm` when EVM proposal support is required. Filesystem mode is a local
  development backend only: it has no durable admin audit table, no schema
  migrations, and cannot safely back more than one replica. The prod stage
  refuses it at startup.
- `GUARDIAN_ENV=prod`, which turns on the prod-stage startup guards.
- A **stable ACK signing identity** loaded from a secret store or from
  owner-only key files, never minted per boot.
- Explicit `GUARDIAN_CORS_ALLOWED_ORIGINS` for browser clients.
- Verified TLS to the database, storage encryption where the threat model
  warrants it, and a metrics endpoint that is never publicly reachable.

The **reference deployment**, and the recommended one, is AWS ECS/Fargate
built by `scripts/aws-deploy.sh` and the Terraform in `infra/`:
`DEPLOY_STAGE=prod` selects the stage profile, Amazon RDS holds state, deltas,
proposals, account metadata, and audit rows, and AWS Secrets Manager (or KMS
for the ECDSA signer) holds the ACK keys and deploy-time secrets.

Running the published Docker image on your own host, VM, or Kubernetes is
supported and reaches the same shape. When ECS is not an option, the
recommended way to do it keeps secret custody in AWS: the ACK keys in Secrets Manager and KMS, the
storage-encryption key document in Secrets Manager, and everything else
(Postgres, ingress, backups, metrics) yours
([production guide, track C](./guides/production/README.md#track-c-self-managed-docker-image-with-aws-secret-custody)).
Running with no AWS account at all is also supported, with file-based ACK keys
(written by the image's `ack-keygen`) and the same rotatable `{active, keys}`
encryption key document in an owner-only file
([track B](./guides/production/README.md#track-b-self-managed-docker-image-no-aws)).
On both, `GUARDIAN_ENV=prod` applies the production runtime defaults inside
the server, so only the topology-dependent values (`GUARDIAN_MAX_REPLICAS`, the
cursor secret, metrics, CORS) are set by hand; the guide walks through it,
including what becomes your responsibility (database backups, TLS termination,
client-IP forwarding, metrics scraping).

### ECDSA ACK signer: Secrets Manager or KMS

The Falcon and ECDSA ACK keys default to AWS Secrets Manager, which is the
path existing deployments use and remains fully supported. For the ECDSA signer
specifically, new production deployments should prefer **AWS KMS**: the private
key is generated in and never leaves KMS, so it is never resident in the
Guardian process. Set `guardian_ack_ecdsa_kms_key_arn` and the server uses the
KMS backend instead of the Secrets Manager secret (Falcon is unaffected).

This is opt-in, not the default, because the KMS key is a distinct keypair:
switching an existing deployment changes Guardian's ECDSA identity and requires
the `SwitchGuardian` migration for existing accounts. Create the key and read
the trade-offs in [`runbooks/secrets.md`](./runbooks/secrets.md#hosted-ecdsa-backend-aws-kms).

KMS covers ECDSA acks only. Falcon has no hosted signer, so a Falcon account is
always acked by a key resident in the process. A KMS deployment therefore only
realizes its custody benefit if new accounts are ECDSA, which is what the next
section enforces.

### Account signature scheme

Each account picks its signature scheme (Falcon or ECDSA) once, at creation;
the choice selects the account's on-chain auth code and cannot change later.
ECDSA is the scheme with hosted-signer support (the KMS backend above) and the
default direction for Guardian; Falcon has no remote signer and is second-class.
New production deployments should therefore accept only ECDSA registrations:

```bash
GUARDIAN_ALLOWED_ACCOUNT_SCHEMES=ecdsa      # Terraform: guardian_allowed_account_schemes
```

The gate acts on `/configure` for accounts that do not exist yet, returning
`signature_scheme_not_allowed` (HTTP 403) with the rejected scheme and the
accepted set in `meta`. Accounts already in this Guardian's metadata are never
affected, because every later request is verified with the scheme stored for
that account, so the variable is safe to set on a fleet that already has Falcon
accounts. An on-chain Falcon account that this Guardian does not know yet (a
re-onboard after a metadata restore, or a `SwitchGuardian` from another
Guardian) counts as new: widen the list to `falcon,ecdsa` for that migration
and tighten it again afterwards.
`GET /dashboard/info` reports `accounts_by_auth_method` if you want to see what
exists first. The gate does not change which ACK keys the server needs: the
Falcon ACK key is still loaded and still required today, so bootstrap and
protect it as described above. Reference:
[`CONFIGURATION.md`](./CONFIGURATION.md#runtime--ack-signing-and-network).

### Running behind your own ingress (non-AWS)

The rate limiter keys clients by IP, and that identity comes from the
ingress in front of the server. The reference ALB deployment handles
this automatically; if you run Guardian behind your own proxy or load
balancer instead, these must hold:

- Your ingress must **append or overwrite** `X-Forwarded-For` on **both**
  listeners: the HTTP port and the gRPC port. Guardian keys on the
  rightmost `X-Forwarded-For` entry, the one your ingress appended. Note
  that proxies often need separate configuration for gRPC (for example
  nginx `grpc_pass` does not add forwarding headers unless
  `grpc_set_header` is configured explicitly).
- If your ingress identifies callers with `X-Real-IP` instead, it must
  **strip** any client-supplied `X-Forwarded-For`. `X-Forwarded-For` is
  read first, so a caller that sends one wins over the `X-Real-IP` your
  proxy set and picks its own rate-limit identity.
- Only the ingress may reach the server ports (3000/50051). Forwarding
  headers are trusted whenever present, so a client that can connect
  directly can choose its own rate-limit identity.
- If the ingress does not forward the client address at all (an
  unconfigured proxy, Kubernetes `externalTrafficPolicy: Cluster` SNAT,
  an L4 balancer without client-IP preservation), every client collapses
  into one shared budget keyed on the proxy's address: one noisy client
  then throttles everyone, and the sustained limit caps the whole
  deployment's throughput.

To check which case you are in, restart with
`RUST_LOG=info,server::middleware::rate_limit=debug` (the per-rejection
lines are `debug`, since refusals are expected traffic), trip the limiter
and read the `Request rate limited` lines: `client_ip` must show real
client addresses: not your proxy's address, not `unknown`, and not a
value you forged in a test request. Two probes settle it: exhaust the
budget from one machine and confirm a second machine on a different
address still succeeds, then retry from the exhausted machine with a
forged `X-Forwarded-For` prefix and confirm it stays throttled.

## Production checklist

For a copy-pasteable, end-to-end walkthrough that satisfies every item below,
follow the [Production deployment guide](./guides/production/README.md). This
checklist is the summary; the guide is the step-by-step.

Before treating a deployment as production-ready:

- Set `DEPLOY_STAGE=prod` (AWS) or `GUARDIAN_ENV=prod` (anywhere else). The
  stage turns on the fail-fast guards and applies the production runtime
  defaults (rate limits, pool sizes, canonicalization concurrency, JSON logs)
  inside the server, so a self-managed deployment lists only what depends on
  its topology: `GUARDIAN_MAX_REPLICAS`, the cursor secret, metrics, and CORS.
  The [production guide](./guides/production/README.md#b2-configure) has the
  table.
- Build with `postgres`, plus `evm` if the EVM API must be served.
- Establish a stable ACK identity once. AWS: `DEPLOY_STAGE=prod
  ./scripts/aws-deploy.sh bootstrap-ack-keys`. Self-managed:
  `GUARDIAN_ACK_SECRET_PROVIDER=file` with two owner-only key files written by
  the image's `ack-keygen --out-dir`
  ([`runbooks/secrets.md`](./runbooks/secrets.md#self-hosted-stable-identity-without-aws)).
- For the ECDSA signer, decide between Secrets Manager (default) and KMS
  (preferred for new deployments where AWS is available); if using KMS, create
  the key and set `guardian_ack_ecdsa_kms_key_arn` per
  [`runbooks/secrets.md`](./runbooks/secrets.md#hosted-ecdsa-backend-aws-kms).
- Restrict new accounts to ECDSA with `GUARDIAN_ALLOWED_ACCOUNT_SCHEMES=ecdsa`
  (see [Account signature scheme](#account-signature-scheme)). Safe to set
  even when Falcon accounts already exist; the Falcon ACK key stays required.
- Confirm `DATABASE_URL`. AWS: supplied through the Terraform-managed RDS
  secret. Self-managed: points at your own Postgres with
  `sslmode=verify-full&sslrootcert=…` (see "Database TLS" in
  [`CONFIGURATION.md`](./CONFIGURATION.md#database-tls)).
- Optionally enable storage encryption at rest against an empty store (the
  Miden 0.16 reset is the natural window). AWS: run
  `./scripts/aws-deploy.sh bootstrap-storage-encryption-key`, then deploy with
  `GUARDIAN_STORAGE_ENCRYPTION_SECRET_NAME` set. Self-managed: the same
  `{active, keys}` document in an owner-only file named by
  `GUARDIAN_STORAGE_ENCRYPTION_KEY_FILE`, rotatable the same way. See "Storage
  encryption" below.
- Review database durability. AWS: the prod stage defaults to 7-day backup
  retention (point-in-time recovery), deletion protection on, and a final
  snapshot on destroy; Multi-AZ is opt-in via `rds_multi_az`. Self-managed:
  your database's backups and point-in-time recovery, with the encryption key
  kept alongside them. See "Durability and recovery" below.
- Set `GUARDIAN_CORS_ALLOWED_ORIGINS` to the exact browser origins that need
  access.
- If the operator dashboard is enabled, configure the operator allowlist
  secret and use object entries when permissions beyond `dashboard:read` are
  needed.
- Pin `GUARDIAN_DASHBOARD_CURSOR_SECRET` identically on every replica. AWS:
  run `./scripts/aws-deploy.sh bootstrap-dashboard-cursor-secret` before the
  first prod deploy; Terraform then injects the same value into every ECS
  task. Self-managed: one `openssl rand -hex 32` value in every replica's
  environment.
- Validate `/`, `/pubkey`, and the relevant SDK or dashboard smoke path after
  deploy.
- Size `GUARDIAN_RATE_PER_MIN` for the combined HTTP **and** gRPC volume:
  the sustained limit is keyed per IP alone, so gRPC traffic (the Rust
  SDK's and benchmark harness's default transport) draws on the same
  allowance as HTTP. Budgets sized for HTTP-only traffic under-provision
  after the transport-bypass fix. `GUARDIAN_RATE_BURST_PER_SEC` is keyed
  per IP and endpoint, so it applies to each HTTP path and gRPC method
  separately and can be sized per-endpoint rather than in aggregate.
- On the first deploy that ships gRPC rate limiting, verify keying on the
  deployed gRPC path: against staging with a deliberately low
  `guardian_rate_burst_per_sec`, exhaust the budget from one machine,
  confirm a second machine on a different address is unaffected, then
  confirm a forged `X-Forwarded-For` prefix from the exhausted machine
  stays throttled. Owned by whoever runs the deploy.
- On the AWS reference deployment, metrics are on by default: the endpoint
  binds loopback inside the ECS task and an ADOT sidecar exports selected
  metrics to CloudWatch dashboards and alarms — no external exposure, no
  bearer token needed. ERROR-level server log lines additionally feed a
  metric-filter alarm that does not depend on the metrics pipeline. See
  [`SERVER_AWS_DEPLOY.md`](./SERVER_AWS_DEPLOY.md#metrics-dashboard-and-alarms)
  and [its log-level alarms section](./SERVER_AWS_DEPLOY.md#log-level-alarms).
- If you scrape Prometheus yourself in a **self-managed deployment**, set
  `GUARDIAN_METRICS_ENABLED=true`, bind an explicitly routable
  `GUARDIAN_METRICS_ADDR` only if the scraper lives outside the host or task,
  keep the port reachable only from the scraper's network, and set
  `GUARDIAN_METRICS_BEARER_TOKEN`. Note the AWS reference Terraform hard-binds
  the listener to `127.0.0.1` and exposes no bind-address, security-group, or
  token knobs — `cloudwatch_metrics_enabled = false` there leaves a
  loopback-only endpoint that only an in-task collector (added by customizing
  the module) can reach, not an external scraper. See the
  [Observability guide](./guides/observability/README.md) for scraping and a
  Grafana dashboard stack, and
  [`CONFIGURATION.md`](./CONFIGURATION.md#runtime--metrics-prometheus) for
  the env vars.

## Durability and recovery

On the reference stack, all Guardian state — account state, deltas,
proposals, account metadata, and audit rows — lives in RDS Postgres. The
Guardian server itself is stateless, so crash resistance reduces to the
database's guarantees:

- **Write durability.** Postgres WAL: a write acknowledged by the database
  survives a crash of the instance. Guardian acknowledges a delta to the
  client only after the database write succeeds.
- **Point-in-time recovery.** RDS automated backups (daily snapshot plus
  continuous WAL archiving) allow restore to any point within
  `rds_backup_retention_days` (default 7). WAL is shipped roughly every
  five minutes, which bounds worst-case data loss for a total instance
  loss to about that window.
- **Operator-error protection.** In the prod stage, deletion protection is
  on and destroying the stack takes a final snapshot
  (`<stack>-postgres-final`) by default.
- **Standby failover.** Multi-AZ is **off by default** in every stage. Set
  `rds_multi_az = true` if the deployment needs automatic failover to a
  standby replica; this is an availability trade-off (roughly double the
  instance cost), not a backup mechanism.

What is deliberately **not** provided: cross-region replicas, automated
disaster-recovery drills, or backup-failure alarms (see
[`architecture/infra.md`](./architecture/infra.md#things-that-are-deliberately-not-here)).

Two Guardian-specific caveats when restoring from a backup:

- Restoring to an earlier point rewinds stored account state. Any delta
  that canonicalized on-chain after the restore point cannot be
  regenerated by the guardian — guarded accounts are private on Miden, so
  only client devices hold the full state. Affected accounts fail
  state-commitment verification until a device holding the newer state
  re-syncs it (see the failure table in
  [`CONCEPTS.md`](./CONCEPTS.md#failure-and-recovery)).
- If storage encryption is enabled, database backups contain ciphertext.
  The storage-encryption key document is part of the recovery set whether it
  comes from Secrets Manager or from `GUARDIAN_STORAGE_ENCRYPTION_KEY_FILE`:
  losing it makes every restored payload unrecoverable. Keep a protected
  out-of-band copy alongside the database backups.

The verification and restore procedure is in
[`runbooks/backup-restore.md`](./runbooks/backup-restore.md).

## Storage encryption

Guardian can encrypt the sensitive stored payloads (account state, delta and
proposal payloads) at rest, above whatever disk-level encryption the database
already provides. It is opt-in: set a key source and the server encrypts; set
none and behavior is unchanged.

- Confidentiality boundary: only the payload JSON is encrypted — the account
  `state_json`, delta `delta_payload`, and proposal `delta_payload`. Routing and
  index columns (`account_id`, `nonce`, `status`, proposal `commitment`) stay
  **plaintext** by design: they are needed for lookups and some are bound as AEAD
  additional authenticated data (authenticated, not hidden). "Encrypted at rest"
  here means payload confidentiality, not metadata confidentiality — anyone with
  database read access still sees which accounts exist, their nonce/commitment
  lineage, and proposal status. Use disk/database-level encryption if the index
  metadata itself is sensitive in your threat model.

- Production key source: the key document
  `{ "active": "k1", "keys": { "k1": "<base64 32 bytes>" } }`, held either in
  AWS Secrets Manager or in an owner-only file. On the standard stack,
  `./scripts/aws-deploy.sh bootstrap-storage-encryption-key` creates the secret
  and a deploy with `GUARDIAN_STORAGE_ENCRYPTION_SECRET_NAME` set wires
  `GUARDIAN_STORAGE_ENCRYPTION_KEY_SECRET_ID` plus the task-role
  `secretsmanager:GetSecretValue` grant (same pattern as the ACK keys); `AWS_REGION`
  is reused. Self-managed deployments mount the same document and set
  `GUARDIAN_STORAGE_ENCRYPTION_KEY_FILE`
  ([production guide, track B](./guides/production/README.md#b2-configure)).
- Enable against an **empty** store. The server writes a one-time marker on the
  first encrypted write and then refuses to mix plaintext and ciphertext, so it
  fails fast if a key is configured against a store that already holds plaintext
  records. For Miden-only deployments the Miden 0.16 reset (which purges Miden
  account data) is the natural enablement window; deployments that also retain EVM
  account rows — which the reset preserves — must clear or migrate those
  plaintext records before enabling encryption, or the first encrypted write will
  fail fast against the mixed store.
- Startup is fail-fast: a missing/malformed/wrong-length key, or more than one
  key source, prevents startup rather than degrading to plaintext.
- Key rotation, on either source: add a new entry to `keys` and move `active`;
  keep the old key so existing records still decrypt. Bulk re-encryption tooling
  is not yet provided.

Full configuration and a dev walkthrough are in
[`CONFIGURATION.md`](./CONFIGURATION.md#storage-encryption-at-rest).

## Upgrading to Miden 0.16

> **One-time, irreversible: the first 0.16 deploy wipes all pre-0.16 Miden
> account data. EVM accounts are unaffected.** Stored 0.15 Miden states, deltas,
> proposals, and metadata can no longer be deserialized or recomputed, and cannot
> be migrated. For what changed on the Miden side and why none of it survives, see
> [`MIDEN_COMPATIBILITY.md`](./MIDEN_COMPATIBILITY.md#guardian-017x-on-miden-016).
> Guardian 0.17.x is the release that adopts Miden 0.16; Guardian 0.16.x runs on
> Miden 0.15.

What happens on the first 0.16 startup (Postgres backend):

- The embedded reset migration
  `2026-08-24-000001_miden_016_irreversible_reset` runs automatically via
  `run_pending_migrations` and deletes Miden-network rows from
  `delta_proposals`, `deltas`, `states`, and `account_metadata`. Rows whose
  `account_metadata.network_config->>'kind'` is `evm` are preserved across all
  four tables. Every other row is purged, including Miden rows and any row
  orphaned from its metadata.
- `account_auth_state` is cleared by cascade from `account_metadata`, so
  per-signer replay floors for deleted accounts go with them.
- The migration takes a brief `ACCESS EXCLUSIVE` lock on `account_metadata` so a
  replica still running the old binary cannot write Miden rows mid-reset. It
  fails fast (5s `lock_timeout`) rather than stalling startup; if that happens,
  confirm the old server is stopped and restart.
- **Preserved:** `admin_actions` (append-only audit), `auth_sessions`,
  `auth_challenges`, `storage_encryption_marker`, `worker_leases`, and the
  Guardian ACK/operator keystore. Guardian identity and operational audit data
  survive the reset.
- The migration is irreversible: its `down.sql` is a no-op. There is no
  partial-salvage path and no legacy-account filtering: every incompatible
  surface is removed at once rather than left for per-instance repair.
- A deployment upgrading from before 0.15 also runs the older
  `2026-06-14-000001_v015_account_id_cutover` in the same startup. Its effect is
  subsumed by this reset (both purge Miden rows and preserve EVM rows), so the
  steps below are the only ones that apply.

Operator actions, in order:

1. **Back up the database.** This is the only way to retain pre-0.16 records,
   and the reset runs automatically on startup. Do this even if you believe the
   data is disposable: for private accounts, Guardian may hold state the client
   no longer has, and step 4 clears the client copy too.
2. **Stop the old server** before deploying. The lock above is a backstop, not a
   substitute.
3. **Deploy against a matching Miden 0.16 node.** A 0.16 server against a 0.15
   node fails on protocol mismatch, not on anything this reset controls.
4. **Clear client-side state**: SDK SQLite stores, browser IndexedDB, and local
   metadata under `~/.guardian`. Stale local metadata outlives the server reset
   and will not deserialize.
5. **Recreate and re-register accounts.** There is no in-place account
   migration; users re-establish custody accounts and register them on the
   Guardian. EVM accounts continue operating unchanged.
6. **Discard all pending and offline proposals.** Exported proposal files and
   unexecuted pending proposals from 0.15 are bound to the old summary layout
   and procedure roots, so they can never execute.

Filesystem-backed deployments have no migration step: start from empty storage
and metadata directories, and preserve the keystore directory.

## Where details live

| Need | Read |
|---|---|
| End-to-end production walkthrough (AWS ECS, self-managed Docker, Docker + AWS secrets) | [`guides/production/`](./guides/production/README.md) |
| Match a Guardian release to a Miden version | [`MIDEN_COMPATIBILITY.md`](./MIDEN_COMPATIBILITY.md) |
| Step-by-step setup for a specific run mode | [`guides/`](./guides/README.md) |
| Deploy or update the AWS stack | [`SERVER_AWS_DEPLOY.md`](./SERVER_AWS_DEPLOY.md) |
| Understand the AWS topology and Terraform ownership | [`architecture/infra.md`](./architecture/infra.md) |
| Understand server storage modes and why prod uses Postgres | [`architecture/services.md`](./architecture/services.md#storage-modes) |
| Check runtime and deploy-time env vars | [`CONFIGURATION.md`](./CONFIGURATION.md) |
| Bootstrap, replace, or respond to ACK/operator/EVM secret issues | [`runbooks/secrets.md`](./runbooks/secrets.md) |
| Verify database backups or restore from one | [`runbooks/backup-restore.md`](./runbooks/backup-restore.md) |
| Migrate a deployed stack to verified database TLS | [`runbooks/enable-db-tls.md`](./runbooks/enable-db-tls.md) |
| Configure dashboard operators and permissions | [`DASHBOARD.md`](./DASHBOARD.md) |
| Scrape Prometheus metrics and visualize them | [`guides/observability/`](./guides/observability/README.md) |
| Diagnose deploy/runtime failures | [`TROUBLESHOOTING.md`](./TROUBLESHOOTING.md) |

## Non-goals

This page does not replace the AWS deploy guide or the runbooks. Keep
procedural steps in those docs so deployment behavior has one source of truth.
