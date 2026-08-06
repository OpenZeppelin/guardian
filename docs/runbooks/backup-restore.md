# Database Backup and Restore Runbook

Operational guide for verifying that the Guardian RDS database is backed up
and for restoring it after data loss. Companion to the durability overview in
[`PRODUCTION.md`](../PRODUCTION.md#durability-and-recovery) (what is
guaranteed) and [`architecture/infra.md`](../architecture/infra.md) (how the
stack fits together).

> **Audience:** operators with AWS (RDS, ECS, Secrets Manager) and Terraform
> access for the target Guardian stack.

All Guardian state lives in the stack's RDS instance
(`<stack>-postgres`) — account state, deltas, proposals, account metadata,
and audit rows. The server tasks are stateless; nothing else needs to be
restored.

## What the stack backs up for you

- **Automated backups.** A daily snapshot plus continuous WAL archiving,
  retained for `rds_backup_retention_days` (default 7). This enables restore
  to any second within the window, up to `LatestRestorableTime` (typically
  within ~5 minutes of now).
- **Final snapshot on destroy.** In the prod stage,
  `./scripts/aws-deploy.sh cleanup` takes a final snapshot named
  `<stack>-postgres-final` before deleting the instance.
- **Deletion protection.** In the prod stage the instance refuses deletion
  until `rds_deletion_protection` is explicitly turned off.

What it does **not** back up:

- **Terraform state.** State files are local to the deploy host (see
  [`architecture/infra.md`](../architecture/infra.md#things-that-are-deliberately-not-here)).
- **Secrets Manager values.** The stack-managed `DATABASE_URL` secret is
  recreated by Terraform, but the ACK signing keys and — critically — the
  **storage encryption key** are not derivable from anything. If storage
  encryption is enabled, a database backup without the key is ciphertext:
  keep an out-of-band copy of the key secret per
  [`secrets.md`](./secrets.md).

## Verify backups (do this now, not during an incident)

```bash
aws rds describe-db-instances \
  --db-instance-identifier <stack>-postgres \
  --query 'DBInstances[0].{Retention:BackupRetentionPeriod,DeletionProtection:DeletionProtection,MultiAZ:MultiAZ,LatestRestorableTime:LatestRestorableTime}'

aws rds describe-db-snapshots \
  --db-instance-identifier <stack>-postgres \
  --query 'DBSnapshots[*].{Id:DBSnapshotIdentifier,Created:SnapshotCreateTime,Status:Status}'
```

For a prod stack, expect `Retention >= 7`, `DeletionProtection: true`, a
recent `LatestRestorableTime`, and at least one `available` snapshot. If
retention is `0`, backups are **off** — fix the stack before anything else.

## Restore procedure (stack intact)

This is the path for data loss while the stack still exists — bad data was
written, a migration went wrong, the instance failed. If the whole stack was
destroyed, start at
[Full-stack recovery](#full-stack-recovery-after-a-destroy) instead; it
recreates the prerequisites these steps reference and then re-enters here.

RDS restores never overwrite an instance in place: both point-in-time and
snapshot restores create a **new** instance. The stack's `DATABASE_URL`
secret is derived by Terraform from the database endpoint (in prod, the RDS
Proxy endpoint, whose target is the instance), and endpoints follow the
instance **identifier**. So the procedure is: restore to a temporary
identifier, then swap names so the restored instance answers at the original
endpoint, then point Terraform state at the restored instance and re-apply.
The state step is not optional: the AWS provider (v5) tracks
`aws_db_instance` by its **DbiResourceId**, which survives renames — after
the swap, Terraform still tracks the renamed-away old instance, and an apply
without the state fix would try to rename it back onto the restored one.
Step 4 covers it.

### 1. Stop writes

Scale the server to zero so no deltas land while the database is swapped.
On a stack with ECS autoscaling (the prod default), pin the autoscaling
range first or it will immediately scale back up:

```bash
aws application-autoscaling register-scalable-target \
  --service-namespace ecs \
  --resource-id service/<stack>-cluster/<stack>-server \
  --scalable-dimension ecs:service:DesiredCount \
  --min-capacity 0 --max-capacity 0

aws ecs update-service --cluster <stack>-cluster \
  --service <stack>-server --desired-count 0
```

On a dev stack autoscaling is off by default — skip the
`register-scalable-target` call (it would create a scaling target Terraform
does not manage) and run only `update-service`.

The final `terraform apply` in step 4 restores both settings.

### 2. Restore to a temporary identifier

Point-in-time (preferred — pick the moment just before the loss):

```bash
aws rds restore-db-instance-to-point-in-time \
  --source-db-instance-identifier <stack>-postgres \
  --target-db-instance-identifier <stack>-postgres-restored \
  --restore-time 2026-08-04T12:00:00Z \
  --db-subnet-group-name <stack>-postgres-subnets \
  --vpc-security-group-ids <postgres-sg-id>
```

Or from a specific snapshot (a daily automated one, or the final snapshot
during [full-stack recovery](#full-stack-recovery-after-a-destroy)):

```bash
aws rds restore-db-instance-from-db-snapshot \
  --db-instance-identifier <stack>-postgres-restored \
  --db-snapshot-identifier <snapshot-id> \
  --db-subnet-group-name <stack>-postgres-subnets \
  --vpc-security-group-ids <postgres-sg-id>
```

Pass the subnet group and security group explicitly — restores do **not**
inherit them from the source. Find the security group ID with:

```bash
aws ec2 describe-security-groups \
  --filters Name=group-name,Values=<stack>-postgres-sg \
  --query 'SecurityGroups[0].GroupId' --output text
```
Wait for the instance to become `available`
(`aws rds wait db-instance-available --db-instance-identifier <stack>-postgres-restored`).

### 3. Swap identifiers

```bash
aws rds modify-db-instance --db-instance-identifier <stack>-postgres \
  --new-db-instance-identifier <stack>-postgres-old --apply-immediately
aws rds wait db-instance-available --db-instance-identifier <stack>-postgres-old

aws rds modify-db-instance --db-instance-identifier <stack>-postgres-restored \
  --new-db-instance-identifier <stack>-postgres --apply-immediately
aws rds wait db-instance-available --db-instance-identifier <stack>-postgres
```

Skip the first rename if the original instance is already gone. Deletion
protection blocks deletion, not renames, so this works on a prod instance.

A rename changes the instance's DNS endpoint, and propagation can take up to
~10 minutes after the instance reports `available`. Before moving on,
confirm the canonical endpoint resolves:

```bash
dig +short $(aws rds describe-db-instances \
  --db-instance-identifier <stack>-postgres \
  --query 'DBInstances[0].Endpoint.Address' --output text)
```

### 4. Reconcile Terraform and restart the service

Point Terraform state at the restored instance first. The provider tracks
the database by DbiResourceId, which followed the old instance through its
rename — without this step, an apply tries to rename `<stack>-postgres-old`
back over the restored instance and fails. The state file lives at
`infra/terraform.<stack>.<stage>.tfstate` (unless `TF_STATE_PATH` was
overridden):

```bash
terraform -chdir=infra state rm \
  -state=terraform.<stack>.<stage>.tfstate aws_db_instance.postgres

terraform -chdir=infra import \
  -state=terraform.<stack>.<stage>.tfstate \
  -var stack_name=<stack> -var deployment_stage=<stage> \
  -var aws_region=<region> -var server_image_uri=unused \
  aws_db_instance.postgres <stack>-postgres
```

(`server_image_uri` is the only variable without a default; import never
reads it, but Terraform requires a value to evaluate the configuration.)

Then verify the plan shows only in-place updates to the database (no
destroy, no rename) before applying:

```bash
./scripts/aws-deploy.sh plan
./scripts/aws-deploy.sh deploy --skip-build
```

The apply re-registers the RDS Proxy target, restores deletion protection
and other instance settings Terraform manages, refreshes the `DATABASE_URL`
secret, and returns the ECS service and autoscaling to their configured
capacity. On a prod stack, confirm the proxy target is healthy:

```bash
aws rds describe-db-proxy-targets --db-proxy-name <stack>-postgres-proxy
```

### 5. Validate

```bash
./scripts/aws-deploy.sh status
curl https://<host>/
curl https://<host>/pubkey
```

Then run the relevant SDK or dashboard smoke path from
[`PRODUCTION.md`](../PRODUCTION.md#production-checklist).

### 6. Clean up

Keep `<stack>-postgres-old` until the restored stack has been validated in
real use. Then snapshot and delete it:

```bash
aws rds modify-db-instance --db-instance-identifier <stack>-postgres-old \
  --no-deletion-protection --apply-immediately
aws rds wait db-instance-available --db-instance-identifier <stack>-postgres-old
aws rds delete-db-instance --db-instance-identifier <stack>-postgres-old \
  --final-db-snapshot-identifier <stack>-postgres-old-final
```

## Full-stack recovery (after a destroy)

`./scripts/aws-deploy.sh cleanup` destroys everything Terraform manages —
not just the database but also the subnet group, security group, RDS Proxy,
ECS service, and the Terraform state entries for all of them. The final
snapshot (`<stack>-postgres-final`) survives, but automated backups are
deleted along with the instance, so point-in-time recovery is no longer
available. The stack-intact procedure cannot run against a
destroyed stack: its commands reference resources that no longer exist, and
a plain redeploy would try to *create* a database, not adopt a restored one.

Recover in two moves:

### 1. Recreate the stack with no running tasks

```bash
TF_VAR_server_desired_count=0 \
TF_VAR_server_autoscaling_max_capacity=0 \
./scripts/aws-deploy.sh deploy --skip-build
```

This recreates every prerequisite — subnet group, security group, proxy,
secrets, ECS service — plus a **fresh, empty** database at the canonical
identifier, all tracked in Terraform state again. `--skip-build` reuses the
ECR image, which survives cleanup (ECR is not Terraform-managed); drop the
flag if the image is gone too. The two overrides pin both the desired count
and the autoscaling ceiling to zero tasks (desired count alone is not
enough — in prod the ceiling would default to 6 and target tracking could
start tasks against the empty database before the swap).

### 2. Restore from the final snapshot

Run [stack-intact step 2](#2-restore-to-a-temporary-identifier) using the
snapshot path with `--db-snapshot-identifier <stack>-postgres-final`.

### 3. Reconcile the master password

Unless the stack pins `postgres_password`, the master password is a
Terraform-generated random value **per state file** — the redeploy in step 1
minted a new one, while the snapshot carries the old one. Every credential
Terraform writes (the `DATABASE_URL` secret, the proxy auth secret) uses the
new value, so the restored instance must be reset to match before the server
can log in:

```bash
NEW_PASSWORD=$(aws secretsmanager get-secret-value \
  --secret-id <stack>/server/database-url \
  --query SecretString --output text | sed -E 's|^postgres://[^:]+:([^@]+)@.*|\1|')

aws rds modify-db-instance \
  --db-instance-identifier <stack>-postgres-restored \
  --master-user-password "$NEW_PASSWORD" --apply-immediately
aws rds wait db-instance-available \
  --db-instance-identifier <stack>-postgres-restored
```

The generated password is alphanumeric-only, so extracting it from the URL
needs no decoding. If the stack pins `postgres_password` (same value across
deploys), the passwords already match and this step is a no-op.

### 4. Swap and finish

Continue the [stack-intact procedure](#restore-procedure-stack-intact) from
step 3. Step 1 (stop writes) is already satisfied, and the fresh empty
instance plays the role of the original: it gets renamed to
`<stack>-postgres-old` and eventually deleted in cleanup. Run the step 4
re-apply **without** the two `TF_VAR_server_*` overrides so the service and
autoscaling return to their configured capacity.

## After the restore: Guardian-level reconciliation

A point-in-time restore rewinds Guardian's stored state. Infrastructure-level
loss is bounded by WAL shipping (~5 minutes), but any delta that
canonicalized **on-chain** after the restore point cannot be regenerated by
the guardian: guarded accounts are private on Miden, so the chain holds only
a commitment hash and the full state lives on client devices.

Accounts whose on-chain state advanced past the restored database state fail
state-commitment verification until a client device holding the newer state
re-syncs it — the "Guardian database corruption" row in the
[`CONCEPTS.md` failure table](../CONCEPTS.md#failure-and-recovery). Accounts
with no post-restore-point activity are unaffected.

## Logical backups (optional)

For copies that live outside the AWS account (offline archives, cross-region
without a replica), take periodic `pg_dump` backups. Two constraints shape
the procedure:

- The instance is not publicly accessible, so `pg_dump` must run from inside
  the VPC.
- The Guardian runtime image ships `libpq5` only — **no `pg_dump`**
  ([`Dockerfile`](../../Dockerfile) `server-runner` stage) — so an ECS Exec
  session into the server container needs the PostgreSQL client installed
  first. The container runs as root and the install is ephemeral (gone when
  ECS replaces the task), which is fine for a one-off dump.

```bash
TASK_ARN=$(aws ecs list-tasks --cluster <stack>-cluster \
  --service-name <stack>-server --query 'taskArns[0]' --output text)
aws ecs execute-command --cluster <stack>-cluster --task "$TASK_ARN" \
  --container <stack>-server --interactive --command /bin/bash
```

The client's major version must be **at least** the server's — `pg_dump`
refuses servers newer than itself, and Debian bookworm's default
`postgresql-client` is 15 while the stack's engine is whatever AWS defaulted
to at creation (`rds_engine_version` is unpinned). Check the engine first:

```bash
aws rds describe-db-instances --db-instance-identifier <stack>-postgres \
  --query 'DBInstances[0].EngineVersion' --output text
```

Inside the session, install the matching client major from the PGDG
repository (substitute the major version from the check above), then dump
using the `DATABASE_URL` already present in the container environment:

```bash
apt-get update && apt-get install -y postgresql-common awscli
/usr/share/postgresql-common/pgdg/apt.postgresql.org.sh -y
apt-get install -y postgresql-client-<major>

pg_dump "$DATABASE_URL" --format=custom --file=/tmp/guardian-$(date +%F).dump
```

To get the dump out, paste short-lived operator credentials into the session
(the task role deliberately has no S3 grant) and upload:

```bash
export AWS_ACCESS_KEY_ID=... AWS_SECRET_ACCESS_KEY=... AWS_SESSION_TOKEN=...
aws s3 cp /tmp/guardian-*.dump s3://<your-backup-bucket>/
```

Alternatively, run a one-off Fargate task from the public `postgres:<major>`
image, attached to the server task's security group, with the same
`DATABASE_URL` secret — avoids touching a serving container at the cost of
registering a task definition.

If storage encryption is enabled, remember the dump is ciphertext without
the key secret.
