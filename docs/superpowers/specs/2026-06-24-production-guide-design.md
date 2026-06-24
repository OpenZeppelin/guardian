# Design: `docs/guides/production/` — end-to-end production walkthrough

Issue: [#299](https://github.com/OpenZeppelin/guardian/issues/299). Date: 2026-06-24.

## Intent

A single assembly-layer guide under `docs/guides/production/README.md` that
sequences the supported production path — **AWS ECS/Fargate via
`scripts/aws-deploy.sh` + Terraform (`infra/`)** — into one copy-pasteable run
that lands an operator on a deployment satisfying **every** `docs/PRODUCTION.md`
checklist item. It assembles and orders; it does **not** duplicate meanings or
procedures, which stay in `CONFIGURATION.md`, `SERVER_AWS_DEPLOY.md`, and the
runbooks.

## Decisions (settled in brainstorming)

1. **Scope: AWS-only.** No committed Docker Compose stack and no second
   "non-AWS" track. The supported production shape (`PRODUCTION.md`) is AWS
   ECS/Fargate; the checklist items (RDS backup retention, deletion protection,
   KMS, Secrets Manager, RDS Proxy, autoscaling) are inherently AWS. A Compose
   track cannot satisfy them, can never set `GUARDIAN_ENV=prod` (that switch
   forces the AWS-managed secret backends), would overlap the existing
   `aws-signers/` and `miden-dashboard/` Compose guides, and adds bit-rot risk.
   The issue's "*If* a Compose stack is committed…" clause is conditional;
   declining it is the cleaner choice.
2. **Smoke test = post-deploy validation** against the real stack: `curl /`,
   `curl /pubkey`, and the `coordination mode=shared backend=postgres …`
   startup-log line. No committed runnable artifact to smoke. For a local
   pre-flight the guide links the existing `aws-signers/` Compose guide rather
   than shipping a competing stack.
3. **Write as if #293 and #301 are merged.** Cover storage encryption at rest
   (#293) and horizontal scaling (#301) as first-class steps. This guide lands
   with or after those PRs. Concretely this means referencing:
   - `./scripts/aws-deploy.sh bootstrap-storage-encryption-key` and the deploy
     var `GUARDIAN_STORAGE_ENCRYPTION_SECRET_NAME` (which wires the runtime
     `GUARDIAN_STORAGE_ENCRYPTION_KEY_SECRET_ID` + the task-role grant), enabled
     against an **empty** store (the Miden 0.15 cutover is the window);
   - Postgres-backed shared coordination (sessions, login challenges,
     canonicalization lease), the **required-in-prod** pinned
     `GUARDIAN_DASHBOARD_CURSOR_SECRET` (32-byte hex / 64 hex chars), and
     `GUARDIAN_MAX_REPLICAS` (rate-limit divisor, defaults to autoscaling max).
4. **Do not link `speckit/.../quickstart.md`.** PR #293 references it, but
   process files (quickstart/plan/research/tasks) are removed under the
   canonical-spec convention (spec.md + data-model.md + contracts/ only). Point
   at `CONFIGURATION.md#storage-encryption-at-rest` instead.

## Deliverable

- `docs/guides/production/README.md` — the walkthrough (only file).
- Wiring:
  - add a row to `docs/guides/README.md` "Available guides" table;
  - add a "Step-by-step setup" link from `docs/PRODUCTION.md` (link from the
    checklist intro / "Where details live" table → this guide).

## README outline

1. **What this gives you / supported shape** → links `PRODUCTION.md`,
   `architecture/infra.md`. States: AWS ECS/Fargate, Postgres backend, RDS,
   Secrets Manager + KMS.
2. **Prerequisites** — repo checkout; AWS CLI creds able to create a KMS key,
   Secrets Manager secrets, and run Terraform; Docker (for `build`); a
   Route 53 zone / ACM as covered in `SERVER_AWS_DEPLOY.md`.
3. **Decisions before you deploy** — `DEPLOY_STAGE=prod`;
   `GUARDIAN_SERVER_FEATURES=postgres` (`postgres,evm` if EVM);
   `GUARDIAN_NETWORK_TYPE` set explicitly; ECDSA backend **KMS (recommended)**
   vs Secrets Manager (`runbooks/secrets.md#hosted-ecdsa-backend-aws-kms`).
4. **Step 1 — Bootstrap secrets once** (idempotent, refuse-overwrite):
   - KMS ECDSA key: `bootstrap-kms-ecdsa-key` → export
     `TF_VAR_guardian_ack_ecdsa_kms_key_arn`.
   - Falcon ACK secret: `bootstrap-ack-keys` (skips ECDSA when the KMS ARN is
     exported). Links `aws-signers/` + `runbooks/secrets.md`.
   - Storage-encryption key: `bootstrap-storage-encryption-key` (enable against
     an empty store). Links `CONFIGURATION.md#storage-encryption-at-rest`.
5. **Step 2 — The production environment set** — one grouped table, every value
   an operator must set, each row linking `CONFIGURATION.md` for meaning:
   - stage/identity: `DEPLOY_STAGE`, `GUARDIAN_ENV=prod`,
     `GUARDIAN_SERVER_FEATURES`, `GUARDIAN_NETWORK_TYPE`, `AWS_REGION`;
   - database + verified TLS: Terraform-managed `DATABASE_URL`, `verify-full` +
     `sslrootcert` → `runbooks/enable-db-tls.md`,
     `SERVER_AWS_DEPLOY.md#database-tls-verification`;
   - browser access: `GUARDIAN_CORS_ALLOWED_ORIGINS` (explicit);
   - dashboard + HA: operator allowlist secret (object entries for
     `accounts:pause`), **pinned** `GUARDIAN_DASHBOARD_CURSOR_SECRET`,
     `GUARDIAN_MAX_REPLICAS` → `runbooks/horizontal-scaling.md`;
   - metrics: `GUARDIAN_METRICS_ENABLED`, `GUARDIAN_METRICS_ADDR`,
     `GUARDIAN_METRICS_BEARER_TOKEN` → `guides/observability/`;
   - storage encryption: `GUARDIAN_STORAGE_ENCRYPTION_SECRET_NAME`;
   - throughput: prod rate-limit overrides (note Terraform prod defaults).
6. **Step 3 — Deploy** — `aws-deploy.sh build → plan → deploy → status`, with
   the env exports shown. Links `SERVER_AWS_DEPLOY.md#deploy`.
7. **Step 4 — Validate** — `curl /` + `curl /pubkey` (record the commitment);
   the `coordination mode=shared backend=postgres stage=prod …` log line; the
   `ECDSA ACK signer ready` line; confirm RDS backup retention / deletion
   protection / storage encryption in the console. Links
   `SERVER_AWS_DEPLOY.md#validate`.
8. **Production checklist coverage** — restate the `PRODUCTION.md` checklist as
   a table mapping each item → the step above that satisfies it (so the guide is
   provably complete against the issue).
9. **Troubleshooting** — short table + link to `TROUBLESHOOTING.md` and the
   relevant runbooks (DB TLS, secrets, horizontal scaling).

## Checklist-coverage matrix (acceptance criteria)

Every `docs/PRODUCTION.md` checklist item must map to a step:

| PRODUCTION.md item | Guide step |
|---|---|
| `DEPLOY_STAGE=prod` | 3, 5 |
| `postgres` (+`evm`) features | 3, 5 |
| Bootstrap ACK secrets | 4 |
| ECDSA: Secrets Manager vs KMS | 3, 4 |
| `DATABASE_URL` via Terraform RDS secret | 5 |
| Storage encryption (opt-in, empty store) | 4, 5 |
| RDS backup retention / deletion protection / final snapshot | 7 |
| `GUARDIAN_CORS_ALLOWED_ORIGINS` | 5 |
| Operator allowlist (object entries) | 5 |
| Pinned `GUARDIAN_DASHBOARD_CURSOR_SECRET` (multi-task) | 5 |
| `GUARDIAN_MAX_REPLICAS` (HA rate partitioning) | 5 |
| Verified DB TLS (`verify-full` + `sslrootcert`) | 5 |
| Prometheus metrics + bearer token | 5 |
| Validate `/`, `/pubkey`, smoke path | 7 |

## Non-goals / rules

- No duplication of variable meanings or step-by-step procedure — link out.
- No committed Compose stack, no smoke harness artifact, no second track.
- No backwards-compat shims; no `speckit/.../quickstart.md` link.
- Keep prose tight; this is assembly + sequencing, not a reference.
