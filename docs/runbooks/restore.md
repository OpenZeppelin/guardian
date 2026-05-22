# Disaster recovery runbook

When to use this: you are restoring a Guardian deployment after data loss,
accidental destroy, or a corrupted state file. This document is a
checklist, not an automation — Guardian has no rehearsed DR path
([`infra.md`](../architecture/infra.md#things-that-are-deliberately-not-here)),
so expect to run commands by hand and verify outcomes between steps.

**Audience:** operator with AWS console access, Terraform state files,
and at least one ACK keystore backup.

## Pre-incident readiness

You cannot restore what you did not back up. Confirm these on every
stack at least quarterly:

- [ ] `rds_backup_retention_days` is **≥ 7** in `infra/variables.tf:216`
      (default `7`). Set higher for production stacks.
- [ ] `rds_skip_final_snapshot = false` for prod stacks (`variables.tf:228`,
      default `true`). Without this, a `terraform destroy` of the RDS
      module discards the final snapshot.
- [ ] `rds_deletion_protection = true` for prod stacks (`variables.tf:222`,
      default `false`). Prevents accidental console-driven deletion.
- [ ] Terraform state files (`infra/terraform.<stack>.<stage>.tfstate`)
      are committed to a private location **outside** the repo and the
      developer machine. Defaults are local-only.
- [ ] ACK keys (`guardian-prod/server/ack-falcon-secret-key`,
      `…/ack-ecdsa-secret-key`) have an offline backup. **Losing both
      ACK keys breaks every client's trust chain** and forces a full
      re-attestation flow.
- [ ] Operator allowlist payload is versioned in a place you can
      recover. Two supported sources, both Secrets Manager-backed:
      Terraform-managed via the `guardian_operator_public_keys` TF
      variable (a list of strings; see
      [`variables.tf:318`](../../infra/variables.tf#L318)) — the deploy
      script also reads it from the
      `GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON` env var
      ([`aws-deploy.sh:66`](../../scripts/aws-deploy.sh#L66)); or
      externally managed via
      `GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ARN`.

## Failure scenarios

### 1. RDS data corruption or accidental drop

Goal: roll back to an automated backup snapshot.

1. Identify the latest viable snapshot (RDS instance identifier is
   `<stack>-postgres`,
   [`data.tf:97`](../../infra/data.tf#L97)):
   ```bash
   aws rds describe-db-snapshots \
     --db-instance-identifier <stack>-postgres \
     --snapshot-type automated \
     --query 'DBSnapshots[].{Id:DBSnapshotIdentifier,Time:SnapshotCreateTime,Status:Status}' \
     --output table
   ```
2. Restore to a *new* instance (never overwrite the live one):
   ```bash
   aws rds restore-db-instance-from-db-snapshot \
     --db-instance-identifier <stack>-postgres-restore \
     --db-snapshot-identifier <snapshot-id>
   ```
3. Confirm `endpoint.address` of the restored instance, then update
   the `DATABASE_URL` secret in Secrets Manager to point at it.
4. Force a new ECS deployment so tasks pick up the updated secret:
   ```bash
   aws ecs update-service \
     --cluster <stack>-cluster --service <stack>-server \
     --force-new-deployment
   ```
5. Verify with the smoke flow in
   [`examples/`](../../examples) before declaring restored.
6. Cut over Terraform: import the new instance and remove the old one
   once you are confident nothing reads from the old endpoint.

### 2. Lost ACK signer keys

Goal: re-establish the server's attestation identity.

1. If a backup of the original `ack-keygen` JSON output exists, push
   each raw key string back (Secrets Manager stores the **raw key
   value**, not a JSON document — see
   [`aws-deploy.sh:374-398`](../../scripts/aws-deploy.sh#L374)):
   ```bash
   aws secretsmanager put-secret-value \
     --secret-id guardian-prod/server/ack-falcon-secret-key \
     --secret-string "$(jq -r '.falcon_secret_key' bootstrap-output.json)"

   aws secretsmanager put-secret-value \
     --secret-id guardian-prod/server/ack-ecdsa-secret-key \
     --secret-string "$(jq -r '.ecdsa_secret_key' bootstrap-output.json)"
   ```
   If you stored each key as a plain text file instead, use
   `--secret-string "$(cat path/to/falcon.txt)"`. Then force a
   deployment to reload.
2. If no backup exists, **clients will need to re-trust the new
   pubkey**. Generate fresh keys
   ([`docs/runbooks/secrets.md`](./secrets.md#bootstrap-first-prod-deploy)),
   publish the new pubkey via `GET /pubkey`, and coordinate with every
   integrating client — the previous attestations are unverifiable.
3. Audit `/dashboard/deltas` for `candidate` deltas that were signed
   under the old key but never canonicalized; they will need
   resubmission by the original signer because their ACK won't verify.

### 3. Terraform state file corruption or loss

Goal: rebuild state from the live AWS resources without recreating them.

1. **Do not** run `terraform apply` with an empty state — it will try
   to create duplicate resources.
2. For each resource in `infra/*.tf`, run `terraform import` with the
   live AWS identifier. Start with the long-lived resources
   (`aws_db_instance.postgres`, `aws_secretsmanager_secret.*`,
   `aws_ecs_cluster.main`) before touching service-level resources.
3. Once `terraform plan` shows no changes, commit the rebuilt state
   to your offline backup location.

### 4. Operator allowlist secret deleted or corrupted

The dashboard refuses every login until this is restored. The Guardian
account-mutation surface is unaffected — only operator endpoints break.

1. Restore the allowlist payload from your offline copy.
2. `aws secretsmanager put-secret-value --secret-id <id> --secret-string <json>`
3. The server reloads the allowlist on the next authenticated dashboard
   request — no task restart needed
   ([`secrets.md`](./secrets.md#operator-public-keys)).

## Post-incident checklist

- [ ] Run the smoke examples against the restored stack.
- [ ] Compare `GET /pubkey` to the value you previously had on file —
      a change here means clients must re-trust.
- [ ] Audit `/dashboard/info` for `degraded` markers
      ([`DASHBOARD.md`](../DASHBOARD.md#storage-mode-caveats)).
- [ ] File a post-mortem with the timeline, blast radius, and any
      gaps surfaced during the restore. Update this runbook with
      lessons learned.

## Known gaps

- No automated DR drill schedule. Run a tabletop exercise quarterly.
- No cross-region replication. A region-level outage means a full
  rebuild in a new region.
- No envelope encryption for Secrets Manager entries. Compromise of an
  IAM principal with `secretsmanager:GetSecretValue` exposes plaintext.
