# Production Guide

This is the production entry point for Guardian operators. It summarizes the
supported production shape and points to the detailed deploy, architecture,
configuration, and runbook docs.

## Supported shape

The reference production deployment is AWS ECS/Fargate running the Guardian
server with the Postgres backend, RDS for durable state, and AWS Secrets
Manager for deployment secrets.

Production deployments should use:

- `DEPLOY_STAGE=prod` for the Terraform stage profile.
- `GUARDIAN_SERVER_FEATURES=postgres` for Miden-only deployments.
- `GUARDIAN_SERVER_FEATURES=postgres,evm` when EVM proposal support is
  required.
- Amazon RDS for state, deltas, proposals, account metadata, and audit rows.
- AWS Secrets Manager for ACK signing keys and deploy-time secrets.
- Explicit `GUARDIAN_CORS_ALLOWED_ORIGINS` for browser clients.

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

Filesystem mode is a local development backend only. It has no durable admin
audit table, no schema migrations, and cannot safely back multiple ECS tasks.

### Running behind your own ingress (non-AWS)

The rate limiter keys clients by IP, and that identity comes from the
ingress in front of the server. The reference ALB deployment handles
this automatically; if you run Guardian behind your own proxy or load
balancer instead, three things must hold:

- Your ingress must **append or overwrite** `X-Forwarded-For` (or set
  `X-Real-IP` from the socket) on **both** listeners: the HTTP port and
  the gRPC port. Guardian keys on the rightmost `X-Forwarded-For` entry,
  the one your ingress appended. Note that proxies often need separate
  configuration for gRPC (for example nginx `grpc_pass` does not add
  forwarding headers unless `grpc_set_header` is configured explicitly).
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
value you forged in a test request. The
[deployed-path verification runbook](./runbooks/verify-grpc-rate-limit.md)
is written against the ALB, but its two-machine and forged-prefix probes
apply to any ingress.

## Production checklist

Before treating a deployment as production-ready:

- Set `DEPLOY_STAGE=prod`.
- Build with `postgres`, plus `evm` if the EVM API must be served.
- Bootstrap ACK secrets once with
  `DEPLOY_STAGE=prod ./scripts/aws-deploy.sh bootstrap-ack-keys`.
- For the ECDSA signer, decide between Secrets Manager (default) and KMS
  (preferred for new deployments); if using KMS, create the key and set
  `guardian_ack_ecdsa_kms_key_arn` per
  [`runbooks/secrets.md`](./runbooks/secrets.md#hosted-ecdsa-backend-aws-kms).
- Confirm `DATABASE_URL` is supplied through the Terraform-managed RDS secret.
- Optionally enable storage encryption at rest: run
  `./scripts/aws-deploy.sh bootstrap-storage-encryption-key`, then deploy with
  `GUARDIAN_STORAGE_ENCRYPTION_SECRET_NAME` set, against an empty store (the 0.15
  cutover is the natural window). See "Storage encryption" below.
- Review RDS backup retention, deletion protection, and final snapshot
  settings for the stack.
- Set `GUARDIAN_CORS_ALLOWED_ORIGINS` to the exact browser origins that need
  access.
- If the operator dashboard is enabled, configure the operator allowlist
  secret and use object entries when permissions beyond `dashboard:read` are
  needed.
- Before the first managed prod deploy, run
  `./scripts/aws-deploy.sh bootstrap-dashboard-cursor-secret`; Terraform then injects the same
  `GUARDIAN_DASHBOARD_CURSOR_SECRET` into every ECS task.
- Validate `/`, `/pubkey`, and the relevant SDK or dashboard smoke path after
  deploy.
- Size `GUARDIAN_RATE_BURST_PER_SEC` / `GUARDIAN_RATE_PER_MIN` for the
  combined HTTP **and** gRPC volume: both transports draw from one shared
  budget, so gRPC traffic (the Rust SDK's and benchmark harness's default
  transport) is throttled too. Budgets sized for HTTP-only traffic
  under-provision after the transport-bypass fix.
- On the first deploy that ships gRPC rate limiting, run the deployed-path
  keying verification: see
  [`runbooks/verify-grpc-rate-limit.md`](./runbooks/verify-grpc-rate-limit.md),
  owned by whoever runs the deploy.
- If Prometheus scraping is wanted, set `GUARDIAN_METRICS_ENABLED=true`,
  bind `GUARDIAN_METRICS_ADDR=0.0.0.0:9464` (containers), keep the port
  reachable only from the scraper's network, and set
  `GUARDIAN_METRICS_BEARER_TOKEN`. See the
  [Observability guide](./guides/observability/README.md) for scraping and a
  Grafana dashboard stack, and
  [`CONFIGURATION.md`](./CONFIGURATION.md#runtime--metrics-prometheus) for
  the env vars.

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

- Production key source: AWS Secrets Manager, holding
  `{ "active": "k1", "keys": { "k1": "<base64 32 bytes>" } }`. On the standard
  stack, `./scripts/aws-deploy.sh bootstrap-storage-encryption-key` creates it and
  a deploy with `GUARDIAN_STORAGE_ENCRYPTION_SECRET_NAME` set wires
  `GUARDIAN_STORAGE_ENCRYPTION_KEY_SECRET_ID` plus the task-role
  `secretsmanager:GetSecretValue` grant (same pattern as the ACK keys). `AWS_REGION`
  is reused.
- Enable against an **empty** store. The server writes a one-time marker on the
  first encrypted write and then refuses to mix plaintext and ciphertext, so it
  fails fast if a key is configured against a store that already holds plaintext
  records. The Miden 0.15 cutover (which truncates account data) is the natural
  enablement window.
- Startup is fail-fast: a missing/malformed/wrong-length key, or more than one
  key source, prevents startup rather than degrading to plaintext.
- Key rotation: add a new entry to `keys` and move `active`; keep the old key so
  existing records still decrypt. Bulk re-encryption tooling is not yet provided.

Full configuration and a dev walkthrough are in
[`CONFIGURATION.md`](./CONFIGURATION.md#storage-encryption-at-rest).

## Where details live

| Need | Read |
|---|---|
| Step-by-step setup for a specific run mode | [`guides/`](./guides/README.md) |
| Deploy or update the AWS stack | [`SERVER_AWS_DEPLOY.md`](./SERVER_AWS_DEPLOY.md) |
| Understand the AWS topology and Terraform ownership | [`architecture/infra.md`](./architecture/infra.md) |
| Understand server storage modes and why prod uses Postgres | [`architecture/services.md`](./architecture/services.md#storage-modes) |
| Check runtime and deploy-time env vars | [`CONFIGURATION.md`](./CONFIGURATION.md) |
| Bootstrap, replace, or respond to ACK/operator/EVM secret issues | [`runbooks/secrets.md`](./runbooks/secrets.md) |
| Migrate a deployed stack to verified database TLS | [`runbooks/enable-db-tls.md`](./runbooks/enable-db-tls.md) |
| Verify rate-limit keying on the deployed gRPC path | [`runbooks/verify-grpc-rate-limit.md`](./runbooks/verify-grpc-rate-limit.md) |
| Configure dashboard operators and permissions | [`DASHBOARD.md`](./DASHBOARD.md) |
| Scrape Prometheus metrics and visualize them | [`guides/observability/`](./guides/observability/README.md) |
| Diagnose deploy/runtime failures | [`TROUBLESHOOTING.md`](./TROUBLESHOOTING.md) |

## Non-goals

This page does not replace the AWS deploy guide or the runbooks. Keep
procedural steps in those docs so deployment behavior has one source of truth.
