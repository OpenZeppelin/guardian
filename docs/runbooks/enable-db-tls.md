# Enabling verified database TLS Runbook

Operational guide for migrating an already-deployed Guardian stack from
encrypted-only (`sslmode=require`) to **verified** database TLS
(`sslmode=verify-full`). Companion to
[`SERVER_AWS_DEPLOY.md`](../SERVER_AWS_DEPLOY.md#database-tls-verification) (which
explains the mechanism) and [Database TLS](../CONFIGURATION.md#database-tls) (the
authoritative meaning of `sslmode`/`sslrootcert`).

> **Audience:** operators with AWS Secrets Manager and ECS/Terraform write access
> for the target Guardian stack.

## Why this is safe

- **Opt-in.** Until `rds_ca_bundle_secret_arn` is set, nothing changes —
  `DATABASE_URL` stays `sslmode=require`. Setting it is the only trigger.
- **Fail-closed, not service-down.** The service runs
  `deployment_minimum_healthy_percent = 100` behind an ALB health check. If a
  verifying task can't validate the certificate it exits and fails its health
  check, so ECS **keeps the existing encrypt-only tasks serving** and the rollout
  stalls. You fix the bundle and re-apply — no outage.
- **One-variable rollback.** Blank `rds_ca_bundle_secret_arn` and `terraform
  apply` to revert to `sslmode=require`.

## The proxy-vs-direct gotcha (read before prod)

`prod`/`testnet` route `DATABASE_URL` through the **RDS Proxy**, whose certificate
is issued by **AWS Certificate Manager → Amazon Trust Services roots**. A
**direct** RDS instance (the default in non-prod, where the proxy is
`is_prod`-gated) instead presents the **Amazon RDS CA roots**.

- The **combined bundle (region RDS roots + ATS roots)** covers both — which is
  why it's mandatory.
- But validating on a *direct* staging DB does **not** exercise the proxy's
  cert/SAN path. Either temporarily enable the proxy on staging to mirror prod,
  or treat the prod cutover as the first real proxy validation (with rollback
  ready).

## Pre-flight checklist

- [ ] The deployed server image **includes this feature** (older images don't
      parse `verify-full`/`sslrootcert` and will fail to connect). Roll the new
      image out first with verification still **off** (Step 0).
- [ ] Combined CA bundle built for the stack's region and **< 64 KiB** (Secrets
      Manager rejects larger; do **not** use `global-bundle.pem`).
- [ ] You know the stack's topology (proxy vs direct) and have matched the test
      plan to it.
- [ ] Rollback understood: blank the variable and re-apply.

## How these commands run

This stack is driven by [`scripts/aws-deploy.sh`](../../scripts/aws-deploy.sh)
(the same tool used in [`SERVER_AWS_DEPLOY.md`](../SERVER_AWS_DEPLOY.md)), not raw
`terraform`. Target a stack with `STACK_NAME`/`DEPLOY_STAGE`, and pass the new
variable through Terraform's standard `TF_VAR_` mechanism — the script forwards
the environment to Terraform, which honors `TF_VAR_rds_ca_bundle_secret_arn`
because that variable is declared in [`infra/variables.tf`](../../infra/variables.tf).

```bash
export STACK_NAME=guardian-staging     # your stack
export DEPLOY_STAGE=staging            # dev | staging | testnet | prod
```

`deploy` builds + pushes a new image then applies; `deploy --skip-build` applies
Terraform against the **already-deployed** image (use this for the verification
flip, which needs no new image); `plan` previews; `status` / `logs` inspect.

## Step 0 — Roll out the feature image with verification OFF

Deploy the image containing verified DB TLS **without** setting
`rds_ca_bundle_secret_arn`. The stack stays `sslmode=require` but becomes
*capable* of verifying. This separates "new image" from "new verification" so a
regression is attributable to one change, not both.

```bash
# from a checkout that includes this feature
./scripts/aws-deploy.sh deploy
./scripts/aws-deploy.sh status        # confirm the new tasks are healthy
```

## Step 1 — Build the combined CA secret (per region)

There is no `aws-deploy.sh` command for this (it's not an ACK secret), so create
it directly. The name is stack-scoped, matching the ACK convention:

```bash
REGION=us-east-1          # the stack's region

curl -sS "https://truststore.pki.rds.amazonaws.com/${REGION}/${REGION}-bundle.pem" -o rds.pem
: > ats.pem
for ca in AmazonRootCA1 AmazonRootCA2 AmazonRootCA3 AmazonRootCA4 SFSRootCAG2; do
  curl -sS "https://www.amazontrust.com/repository/${ca}.pem" >> ats.pem; echo >> ats.pem
done
{ cat rds.pem; echo; cat ats.pem; } > rds-combined-ca.pem
test "$(wc -c < rds-combined-ca.pem)" -lt 65536 || { echo "bundle exceeds 64 KiB"; exit 1; }
grep -c "BEGIN CERTIFICATE" rds-combined-ca.pem   # sanity: total root count

aws secretsmanager create-secret \
  --name "${STACK_NAME}/server/rds-ca-bundle" \
  --secret-string file://rds-combined-ca.pem \
  --query ARN --output text          # note the ARN for Step 2
```

Create the secret **before** the apply that references its ARN.

## Step 2 — Flip the stack to verify-full

Point the variable at the secret ARN from Step 1 and preview, then apply against
the existing image (no rebuild needed — this is a config-only change):

```bash
export TF_VAR_rds_ca_bundle_secret_arn="arn:aws:secretsmanager:<REGION>:<ACCT>:secret:<STACK_NAME>/server/rds-ca-bundle-XXXXXX"

./scripts/aws-deploy.sh plan                 # review
./scripts/aws-deploy.sh deploy --skip-build  # apply against the deployed image
```

The plan should show: an `rds-ca-initializer` init container added; a shared
volume + read-only mount + `dependsOn { condition = SUCCESS }` on the server
container; an execution-role `GetSecretValue` grant for the secret ARN; and
`DATABASE_URL` changing to `…&sslmode=verify-full&sslrootcert=<mounted path>`.

> Keep `TF_VAR_rds_ca_bundle_secret_arn` exported for every subsequent
> `aws-deploy.sh` invocation on this stack (or set it in the stack's tfvars), so
> later deploys don't silently revert the stack to `sslmode=require`.

## Step 3 — Validate

```bash
./scripts/aws-deploy.sh status   # the new deployment should reach a healthy/steady state
./scripts/aws-deploy.sh logs     # init container exited 0; no cert error; migrations ran; server listening
```

Expected: `db-cert-init` exits 0 → Postgres reachable → migrations applied → HTTP
listening, with **no** `certificate verify failed`. A crash-looping new task with
a cert error means the bundle or endpoint is wrong — the old encrypt-only tasks
keep serving, so fix the secret and re-apply.

## Step 4 — Promote to prod/testnet

Repeat Steps 1–3 with `STACK_NAME`/`DEPLOY_STAGE` pointed at prod/testnet and a
prod-scoped secret, in a low-traffic window. Because traffic flows through the
**RDS Proxy**, this is where the ACM/ATS chain and the **proxy-endpoint SAN** are
verified for the first time — watch the rollout (`aws-deploy.sh status`/`logs`)
and keep the rollback ready.

## Rollback

```bash
unset TF_VAR_rds_ca_bundle_secret_arn        # or remove it from the stack tfvars
./scripts/aws-deploy.sh deploy --skip-build
```

`DATABASE_URL` reverts to `sslmode=require`, the init container and mount are
removed, and tasks roll back to encrypt-only. (A *failed* verifying deploy never
displaces the healthy old tasks in the first place.)

## Rotation (after cutover)

CA roots rotate rarely, but when they do: update the secret value (it may hold
old and new roots together for overlap), then force a redeploy so the init
container rewrites the file. A value-only secret change doesn't alter the task
definition, so `aws-deploy.sh deploy` won't roll tasks on its own — force it:

```bash
aws secretsmanager put-secret-value \
  --secret-id "${STACK_NAME}/server/rds-ca-bundle" \
  --secret-string file://rds-combined-ca.pem

# cluster/service names follow the stack name; confirm via `aws-deploy.sh status`
aws ecs update-service --cluster "${STACK_NAME}-cluster" \
  --service "${STACK_NAME}-server" --force-new-deployment
```

Ensure the new roots are present before they become the only trusted ones.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| New task crash-loops with `certificate verify failed` | bundle doesn't cover the presented chain — proxy stack needs the **ATS roots**, direct needs the **RDS CA roots** (use the combined bundle) |
| `create-secret` fails on size | used `global-bundle.pem`; switch to the **region** bundle + ATS roots (< 64 KiB) |
| Startup error naming `sslrootcert` | init container didn't write the file — check its logs and the execution-role secret grant |
| Server won't parse `verify-full` | image predates the feature — complete Step 0 first |

See [`TROUBLESHOOTING.md`](../TROUBLESHOOTING.md#server-fails-to-start) for the
full database-TLS failure-to-cause mapping.
