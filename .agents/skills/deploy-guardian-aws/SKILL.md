---
name: deploy-guardian-aws
description: Deploy, update, inspect, and troubleshoot the repository AWS ECS environment using `scripts/aws-deploy.sh` and Terraform in `infra/`. Use when Codex needs to verify AWS auth, run the repo deploy script, reason about ECR, ECS, ALB, CloudWatch, RDS, Secrets Manager, Route 53, ACM, or Cloudflare deployment variables before changing infrastructure.
---

# Deploy AWS Stack

Read the current source of truth at the start of every task:

- `docs/SERVER_AWS_DEPLOY.md`
- `scripts/aws-deploy.sh`
- `infra/variables.tf`
- `infra/terraform.tfvars.example`
- the relevant `infra/*.tf` files for the behavior being changed

Trust these sources in this order:

1. `scripts/aws-deploy.sh` for supported commands, flags, and shell env vars
2. `infra/*.tf` and `infra/variables.tf` for actual Terraform behavior
3. `docs/SERVER_AWS_DEPLOY.md` and `infra/README.md` for operator workflow

## Preflight

1. Verify AWS identity, Docker, and Terraform:
   ```bash
   aws sts get-caller-identity
   docker info
   terraform version
   ```
2. Load repo env when the deployment expects values from `.env`:
   ```bash
   set -a && source .env && set +a
   ```
3. If the environment uses AWS SSO plus an assumed role, refresh SSO, export temporary credentials, and verify them before deploy commands.
4. Run `terraform -chdir=infra output` or `./scripts/aws-deploy.sh status` before the first mutating command in a session.

## Primary Commands

- Normal deploy: `./scripts/aws-deploy.sh deploy`
- Create the prod ACK secrets once: `./scripts/aws-deploy.sh bootstrap-ack-keys`
- Infra or runtime update without rebuilding the image: `./scripts/aws-deploy.sh deploy --skip-build`
- Outputs and URLs: `./scripts/aws-deploy.sh status`
- Server logs: `./scripts/aws-deploy.sh logs`
- Destroy: `./scripts/aws-deploy.sh cleanup`

Prefer the deploy script over raw `terraform apply` and `terraform destroy` unless the task is explicitly about Terraform debugging or plan inspection.

## Current Deployment Model

- The AWS stack is RDS-backed. There is no supported `database_mode` or legacy ECS Postgres path anymore.
- `./scripts/aws-deploy.sh deploy` provisions or updates the ECS server service, the RDS instance, and the Secrets Manager `DATABASE_URL` wiring.
- `./scripts/aws-deploy.sh bootstrap-ack-keys` is the create-only command for the prod Falcon/ECDSA ack key secrets used by `prod`.
- The deploy script resolves the ECR `latest` tag to an immutable digest before calling Terraform, so a new image push should produce a new ECS task-definition revision even if the repo tag stays `latest`.
- The deploy script keeps separate local Terraform state files per `STACK_NAME` and `DEPLOY_STAGE`, using `infra/terraform.<stack>.<stage>.tfstate` by default.
- If an older local state still exists at `infra/terraform.tfstate`, handle that move explicitly instead of relying on the script to migrate it.
- Do not tell the user to preserve or re-enable the retired Postgres ECS or Cloud Map resources.
- If the task involves an old stack that still has ECS-hosted Postgres data, treat it as an operator-managed cutover outside the steady-state Terraform design.

## Variable Discipline

Use the deploy script env vars for the normal workflow:

- `AWS_REGION`
- `CPU_ARCHITECTURE`
- `STACK_NAME`
- `DEPLOY_STAGE`
- `TF_STATE_PATH`
- `DOMAIN_NAME`
- `SUBDOMAIN`
- `ACM_CERTIFICATE_ARN`
- `ROUTE53_ZONE_ID`
- `CLOUDFLARE_ZONE_ID`
- `CLOUDFLARE_API_TOKEN`
- `CLOUDFLARE_PROXIED`
- `GUARDIAN_NETWORK_TYPE`

Use `TF_VAR_*` only for Terraform variables that the script does not map directly, such as:

- `TF_VAR_cluster_name`
- `TF_VAR_server_service_name`
- `TF_VAR_alb_name`
- `TF_VAR_vpc_id`
- `TF_VAR_subnet_ids`
- `TF_VAR_postgres_db`
- `TF_VAR_postgres_user`
- `TF_VAR_postgres_password`
- `TF_VAR_rds_instance_class`
- `TF_VAR_rds_allocated_storage`
- `TF_VAR_rds_max_allocated_storage`
- `TF_VAR_rds_engine_version`
- `TF_VAR_rds_backup_retention_days`
- `TF_VAR_rds_deletion_protection`
- `TF_VAR_rds_skip_final_snapshot`
- `TF_VAR_rds_publicly_accessible`
- `TF_VAR_rds_proxy_enabled`
- `TF_VAR_server_desired_count`
- `TF_VAR_server_autoscaling_enabled`
- `TF_VAR_server_autoscaling_min_capacity`
- `TF_VAR_server_autoscaling_max_capacity`
- `TF_VAR_server_autoscaling_cpu_target`
- `TF_VAR_server_autoscaling_memory_target`
- `TF_VAR_guardian_rate_burst_per_sec`
- `TF_VAR_guardian_rate_per_min`
- `TF_VAR_guardian_db_pool_max_size`
- `TF_VAR_guardian_metadata_db_pool_max_size`
- `TF_VAR_alb_ingress_cidrs`
- `TF_VAR_log_retention_days`

Treat these as stale or conditional:

- legacy network env naming is stale; use `GUARDIAN_NETWORK_TYPE`
- `DEPLOY_STAGE=dev` keeps the stack close to the current fixed-capacity profile
- `DEPLOY_STAGE=prod` enables ECS autoscaling, RDS storage autoscaling, RDS Proxy, larger default RDS sizing, and benchmark-oriented runtime defaults
- `CPU_ARCHITECTURE=X86_64` preserves the current amd64 deployment behavior
- `CPU_ARCHITECTURE=ARM64` is the native build path on Apple Silicon and usually much faster locally, but it changes the ECS task definition runtime architecture too
- set `STACK_NAME` only when the deployment should preserve non-default resource names
- `AWS_PROFILE` is only needed for the initial SSO or `assume-role` step if temporary credentials are exported afterward
- `STS_CMD` is only a temporary shell helper and can be unset after exporting credentials
- `CLOUDFLARE_ZONE_ID` without `CLOUDFLARE_API_TOKEN` is invalid for Terraform-managed Cloudflare DNS
- `ROUTE53_ZONE_ID` is only needed if Terraform should create the AWS Route 53 record; current Terraform does not auto-discover the zone
- `DATABASE_MODE` is stale and should not appear in commands or advice
- old ECS Postgres naming overrides such as `TF_VAR_postgres_service_name`, `TF_VAR_sd_namespace_name`, `TF_VAR_postgres_task_family`, and `TF_VAR_postgres_log_group_name` are stale and should not be used

## Validation

After every deploy:

- run `./scripts/aws-deploy.sh status`
- verify the root URL and `/pubkey`
- verify the gRPC endpoint when HTTPS is enabled
- verify the running ECS task definition or image reference when the task is specifically about image rollout or stale containers
- note the RDS endpoint, `database_url_secret_arn`, and the ack secret names
- note whether the server is using direct RDS or the RDS Proxy endpoint
- note whether the active URL is the ALB DNS name or the custom domain
- record the AWS account, region, network type, and DNS mode used

## Output Shape

Default to giving the user the exact commands to run for the requested deployment task.

- Prefer one short ordered command sequence over a prose-heavy explanation
- Include `export` lines only for variables that matter for the requested task
- If the needed deploy vars are already stored in `.env`, prefer `set -a && source .env && set +a` over repeating individual `export` lines
- Omit stale or unnecessary variables
- Use placeholders only for secrets or values the user has not provided
- If the task is risky or destructive, separate inspection commands from mutating commands

## Reporting

Report:

- the exact commands the user should run
- commands run
- auth mode used
- env vars and `TF_VAR_*` overrides used
- Terraform outputs that changed
- health checks performed
- blockers found between state, docs, and Terraform code
