---
name: deploy-guardian-aws
description: Deploy, update, inspect, and troubleshoot the repository AWS ECS environment using `scripts/aws-deploy.sh` and Terraform in `infra/`. Use when Codex needs to verify AWS auth, run the repo deploy script, reason about ECR, ECS, ALB, CloudWatch, Route 53, ACM, or Cloudflare deployment variables before changing infrastructure.
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
- Infra or runtime update without rebuilding the image: `./scripts/aws-deploy.sh deploy --skip-build`
- Outputs and URLs: `./scripts/aws-deploy.sh status`
- Server logs: `./scripts/aws-deploy.sh logs`
- Destroy: `./scripts/aws-deploy.sh cleanup`

Prefer the deploy script over raw `terraform apply` and `terraform destroy` unless the task is explicitly about Terraform debugging or plan inspection.

## Variable Discipline

Use the deploy script env vars for the normal workflow:

- `AWS_REGION`
- `CPU_ARCHITECTURE`
- `STACK_NAME`
- `DOMAIN_NAME`
- `SUBDOMAIN`
- `ACM_CERTIFICATE_ARN`
- `ROUTE53_ZONE_ID`
- `CLOUDFLARE_ZONE_ID`
- `CLOUDFLARE_API_TOKEN`
- `CLOUDFLARE_PROXIED`
- `GUARDIAN_NETWORK_TYPE`
- `IMPORT_EXISTING`

Use `TF_VAR_*` only for Terraform variables that the script does not map directly, such as:

- `TF_VAR_cluster_name`
- `TF_VAR_server_service_name`
- `TF_VAR_postgres_service_name`
- `TF_VAR_alb_name`
- `TF_VAR_sd_namespace_name`
- `TF_VAR_vpc_id`
- `TF_VAR_subnet_ids`
- `TF_VAR_postgres_db`
- `TF_VAR_postgres_user`
- `TF_VAR_postgres_password`
- `TF_VAR_alb_ingress_cidrs`
- `TF_VAR_log_retention_days`

Treat these as stale or conditional:

- legacy network env naming is stale; use `GUARDIAN_NETWORK_TYPE`
- `CPU_ARCHITECTURE=X86_64` preserves the current amd64 deployment behavior
- `CPU_ARCHITECTURE=ARM64` is the native build path on Apple Silicon and usually much faster locally, but it changes the ECS task definition runtime architecture too
- set `STACK_NAME` only when the deployment should preserve non-default resource names
- `AWS_PROFILE` is only needed for the initial SSO or `assume-role` step if temporary credentials are exported afterward
- `STS_CMD` is only a temporary shell helper and can be unset after exporting credentials
- `CLOUDFLARE_ZONE_ID` without `CLOUDFLARE_API_TOKEN` is invalid for Terraform-managed Cloudflare DNS
- `ROUTE53_ZONE_ID` is only needed if Terraform should create the AWS Route 53 record; current Terraform does not auto-discover the zone

## Validation

After every deploy:

- run `./scripts/aws-deploy.sh status`
- verify the root URL and `/pubkey`
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
