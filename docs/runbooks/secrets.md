# Secrets and Key Management Runbook

Operational guide for the secrets Guardian relies on in production.
Companion to [`docs/architecture/infra.md`](../architecture/infra.md), which
explains *which* AWS resources hold each secret;
this doc covers *how* to bootstrap, rotate, and respond to compromise.

> **Audience:** operators with AWS Secrets Manager and ECS write access for
> the target Guardian stack.

## Categories at a glance

| Category | Stored in | Lifecycle | Who reads it |
|---|---|---|---|
| `DATABASE_URL` | Secrets Manager (`<stack>/server/database-url`) | Managed by Terraform | ECS task **execution** role, at task start |
| RDS Proxy credentials (prod) | Secrets Manager (`<stack>/server/database-credentials`) | Managed by Terraform | RDS Proxy IAM role |
| ACK signing keys (prod) | Secrets Manager (`<stack>-prod/server/ack-falcon-secret-key`, `…/ack-ecdsa-secret-key`) | Bootstrapped once via `aws-deploy.sh bootstrap-ack-keys`; never rotated by deploys | ECS task **runtime** role, at server startup |
| Operator public keys | Secrets Manager (Terraform-managed or pre-existing ARN) | Updated by editing Terraform var or rotating the secret value | ECS task runtime role, on each dashboard challenge **and each authenticated `/dashboard/*` request** (hot-reloaded — no restart needed) |
| EVM allowed chains + RPC URLs | Secrets Manager (Terraform-managed) | Updated by editing `config/evm/chains.json` and redeploying | ECS task execution role; surfaced as env to the task |

Naming defaults derive from `stack_name`; the trailing `-prod` on ACK secrets
is historical. Override via `TF_VAR_ack_falcon_secret_name` /
`TF_VAR_ack_ecdsa_secret_name` if needed.

## ACK signing keys

ACK keys (one Falcon, one ECDSA) are Guardian's own response signers.
Clients pin Guardian's pubkey via `GetPubkey` on first contact and verify
every response thereafter — **stable identity matters**. Treat ACK key
rotation the same way you would treat rotating an upstream service's TLS
identity.

### Bootstrap (first prod deploy)

```bash
DEPLOY_STAGE=prod STACK_NAME=<stack> ./scripts/aws-deploy.sh bootstrap-ack-keys
```

What that command does
([`scripts/aws-deploy.sh:352`](../../scripts/aws-deploy.sh#L352)):

1. Refuses to run if either secret already exists.
2. Generates key material locally via
   `cargo run --bin ack-keygen` (no key ever leaves the operator's host
   except via the `aws secretsmanager create-secret` call).
3. Creates both secrets in Secrets Manager with the generated values.

Verify:
```bash
aws secretsmanager describe-secret --secret-id <stack>-prod/server/ack-falcon-secret-key
aws secretsmanager describe-secret --secret-id <stack>-prod/server/ack-ecdsa-secret-key
```

Subsequent `aws-deploy.sh deploy` runs assert these secrets exist
([`aws-deploy.sh:331`](../../scripts/aws-deploy.sh#L331)) and fail fast
otherwise.

### Rotation

ACK rotation is **not** part of the regular deploy cycle. Rotating breaks
clients that pinned the previous pubkey until they refetch via `GetPubkey`.

Procedure (planned rotation, e.g. annual):

1. Announce the rotation window to all consumers that pin a pubkey.
2. Generate new key material on a trusted host:
   ```bash
   cargo run --quiet --package guardian-server --bin ack-keygen > /tmp/ack-keys.json
   ```
3. Put new values into Secrets Manager — `update-secret` creates a new
   version without disturbing the secret ID:
   ```bash
   FALCON=$(jq -r .falcon_secret_key /tmp/ack-keys.json)
   ECDSA=$(jq -r .ecdsa_secret_key /tmp/ack-keys.json)

   aws secretsmanager update-secret \
     --secret-id <stack>-prod/server/ack-falcon-secret-key \
     --secret-string "$FALCON"
   aws secretsmanager update-secret \
     --secret-id <stack>-prod/server/ack-ecdsa-secret-key \
     --secret-string "$ECDSA"
   ```
4. Force a new ECS deployment so tasks restart and import the new keys:
   ```bash
   aws ecs update-service --cluster <stack>-cluster \
     --service <stack>-server --force-new-deployment
   ```
5. Confirm rotation:
   ```bash
   curl https://guardian.openzeppelin.com/pubkey
   ```
   Should return the new key material.
6. Securely shred `/tmp/ack-keys.json`.

### Compromise response

If you believe an ACK secret leaked:

1. **Immediately** rotate using the procedure above — bypass any change
   window.
2. Revoke any operator AWS credentials that could have read the secret
   (CloudTrail `GetSecretValue` events scoped to those secret ARNs are the
   audit trail).
3. Force-cycle all tasks (`update-service --force-new-deployment`) so the
   old keys are no longer resident in any task's filesystem keystore.
4. Inform downstream clients to refetch the pubkey and invalidate cached
   verifiers.
5. File an incident referencing the secret ARN, the rotation timestamp,
   and the CloudTrail evidence.

## Operator public keys

Operators authenticate to the dashboard via Falcon-signed challenges
against an **allowlist** of public keys. Two ways to manage the list:

- **Terraform-managed** — set `guardian_operator_public_keys` in
  Terraform (or `GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON` in the deploy env);
  Terraform creates and maintains the secret.
- **Pre-existing ARN** — set
  `guardian_operator_public_keys_secret_arn` to an ARN you manage
  externally. Terraform won't touch the contents.

The secret payload is the JSON shape consumed by
[`dashboard/allowlist.rs`](../../crates/server/src/dashboard/allowlist.rs).
See [`docs/DASHBOARD.md`](../DASHBOARD.md#enrolling-an-operator) for the
payload schema and an enrollment walkthrough.

### Adding or removing an operator

The server **rereads the operator secret on every dashboard challenge and
every authenticated `/dashboard/*` request** — no ECS restart needed for
allowlist changes.

Terraform-managed path:
1. Edit `guardian_operator_public_keys` (or the env var the deploy script
   reads from).
2. `./scripts/aws-deploy.sh deploy` — Terraform updates the secret in
   place. Effect is immediate.

Externally-managed path:
1. Update the secret with `aws secretsmanager update-secret`. That's it.

### Revoking a compromised operator

The hot-reload path makes this fast — no deploy window required:

```bash
aws secretsmanager update-secret \
  --secret-id <operator-secret-id> \
  --secret-string "$(cat new-operator-list.json)"
```

The next challenge issuance or authenticated request from any task picks
up the new list and rejects the revoked key. Active sessions belonging
to the revoked operator are rejected at their next call: the per-request
reload re-validates the operator against the current allowlist on every
authenticated `/dashboard/*` hit
([`dashboard/state.rs:284-324`](../../crates/server/src/dashboard/state.rs#L284)).

Operator sessions are in-memory per task. There is no ALB session
stickiness, so on multi-task deployments operators may be routed to a
task that did not mint their session and prompted to re-authenticate
— this is the normal failure mode, not a revocation signal. Use the
audit / CloudTrail trail (below) to confirm a revocation took effect.

## `DATABASE_URL` and RDS Proxy credentials

Both are **created and owned by Terraform** ([`infra/rds.tf:43`](../../infra/rds.tf#L43),
[`infra/rds.tf:48`](../../infra/rds.tf#L48)). Do not edit them by hand —
the next `terraform apply` will overwrite your change.

To rotate the database password:
1. Set `postgres_password` to a new value in `terraform.tfvars` (or unset
   it to let Terraform regenerate via `random_password`).
2. `terraform apply` — Terraform updates the RDS master password,
   `DATABASE_URL`, and the proxy credentials secret atomically.
3. ECS rolls the service automatically on the next deploy; force it
   sooner with `update-service --force-new-deployment`.

There is no separate read-only credential; the server connects with the
master user. This is a known production-hardening gap.

## EVM allowed chains and RPC URLs

Populated by the deploy script from
[`config/evm/chains.json`](../../config/evm/chains.json) when
`GUARDIAN_SERVER_FEATURES=postgres,evm`.

To add a chain:
1. Edit `config/evm/chains.json` — add the `chain_id`, RPC URL, and
   entrypoint address.
2. `./scripts/aws-deploy.sh deploy` — the script rebuilds the Secrets
   Manager values from the JSON and Terraform updates the secret
   versions.
3. ECS rolls and the new task reads the updated lists.

To rotate an RPC URL (e.g. switch provider):
1. Edit `chains.json`, redeploy. No special handling — the server treats
   chain config as a startup-time read.

## Audit trail

CloudTrail `GetSecretValue` events are scoped per-secret ARN. The
relevant principals you should see hitting each secret:

| Secret | Expected principals |
|---|---|
| `DATABASE_URL` | ECS task execution role only |
| `database-credentials` (proxy) | RDS Proxy IAM role only |
| ACK Falcon / ECDSA | ECS task runtime role (on cold start) + operators running `bootstrap-ack-keys` or rotation |
| Operator pubkeys | ECS task runtime role + operators updating the list |
| EVM chains / RPCs | ECS task execution role only |

Any other principal touching these secrets is suspicious.

## What is deliberately not here

- **No KMS CMK** — Secrets Manager uses the default AWS-owned key. Move
  to a CMK before enabling cross-account access.
- **No automated rotation lambdas** — all rotations are operator-driven.
- **No envelope encryption of ACK secret values** — Secrets Manager
  protects the secret at rest; the value itself is the raw key material
  that the server imports into its filesystem keystore on startup.
