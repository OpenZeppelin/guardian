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
- Review RDS backup retention, deletion protection, and final snapshot
  settings for the stack.
- Set `GUARDIAN_CORS_ALLOWED_ORIGINS` to the exact browser origins that need
  access.
- If the operator dashboard is enabled, configure the operator allowlist
  secret and use object entries when permissions beyond `dashboard:read` are
  needed.
- If running two or more ECS tasks, pin
  `GUARDIAN_DASHBOARD_CURSOR_SECRET` so dashboard cursors validate across
  tasks.
- Validate `/`, `/pubkey`, and the relevant SDK or dashboard smoke path after
  deploy.

## Upgrading to Miden 0.15

> **One-time, irreversible: the first 0.15 deploy wipes all pre-0.15 account
> data.** Miden 0.15 changed account-ID derivation (v0 → v1) and Guardian's
> custody account now uses the upstream `miden-standards` guarded-multisig
> component, so stored 0.14 account states, deltas, proposals, and metadata
> can no longer be deserialized or recomputed. They cannot be migrated.

What happens on the first 0.15 startup (Postgres backend):

- The embedded cutover migration
  `2026-06-14-000001_v015_account_id_cutover` runs automatically via
  `run_pending_migrations` and `TRUNCATE`s `states`, `deltas`,
  `delta_proposals`, and `account_metadata`.
- The append-only `admin_actions` audit table is **preserved** (it is
  DB-trigger protected and carries no un-deserializable account state).
- The migration is irreversible — its `down.sql` is a no-op. There is no
  partial-salvage path.

Operator actions:

- **Back up the database before deploying 0.15** if any pre-0.15 record must
  be retained for audit outside `admin_actions`.
- After the upgrade, existing accounts must be **recreated** on 0.15; there is
  no in-place account migration. Users re-establish custody accounts (new v1
  IDs) and re-register them on the Guardian.
- Filesystem-backed deployments have no migration step — remove the data
  directory instead.

## Where details live

| Need | Read |
|---|---|
| Step-by-step setup for a specific run mode | [`guides/`](./guides/README.md) |
| Deploy or update the AWS stack | [`SERVER_AWS_DEPLOY.md`](./SERVER_AWS_DEPLOY.md) |
| Understand the AWS topology and Terraform ownership | [`architecture/infra.md`](./architecture/infra.md) |
| Understand server storage modes and why prod uses Postgres | [`architecture/services.md`](./architecture/services.md#storage-modes) |
| Check runtime and deploy-time env vars | [`CONFIGURATION.md`](./CONFIGURATION.md) |
| Bootstrap, replace, or respond to ACK/operator/EVM secret issues | [`runbooks/secrets.md`](./runbooks/secrets.md) |
| Configure dashboard operators and permissions | [`DASHBOARD.md`](./DASHBOARD.md) |
| Diagnose deploy/runtime failures | [`TROUBLESHOOTING.md`](./TROUBLESHOOTING.md) |

## Non-goals

This page does not replace the AWS deploy guide or the runbooks. Keep
procedural steps in those docs so deployment behavior has one source of truth.
