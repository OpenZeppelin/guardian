# infra/ — Agent notes

Terraform for the AWS ECS deployment (ALB + ECS Fargate + RDS + Secrets Manager
+ CloudWatch + Route 53/ACM, fronted by Cloudflare).

See repo root `AGENTS.md`. Use the `deploy-guardian-aws` skill for any
deploy/inspect/troubleshoot action — it knows the canonical sequence and the
`scripts/aws-deploy.sh` wrapper.

## Safety rules

- **Never commit `terraform.tfstate*`, `*.tfstate.backup`, or `.terraform/`.**
  Live tfstate has been accidentally committed before. Before any commit
  touching infra/, run `git status` and verify only `*.tf`, `*.tfvars.example`,
  and README/docs are staged.
- **Never commit `terraform.tfvars`** (the real one, without `.example`). It
  carries secrets.
- `terraform apply`, force-unlock, taint, and state mutations are
  high-blast-radius — confirm with the user before running, even if a previous
  apply in the session was approved. Each apply is a new authorization.
- Prefer `terraform plan` and read the diff with the user before applying.

## Files

| File | Purpose |
|------|---------|
| `alb.tf` | Application Load Balancer + listeners + target groups |
| `dns.tf` | Route 53 records, ACM certs |
| `ecs.tf` | ECS cluster, task def, service |
| `ecs_autoscaling.tf` | Service autoscaling policies |
| `iam.tf` | Task role, execution role, secrets policies |
| `logs.tf` | CloudWatch log groups + retention |
| `operator_secrets.tf` | Operator API key + signing key in Secrets Manager |
| `rds.tf` | Postgres instance, parameter group, subnet group |
| `security_groups.tf` | SG rules ALB ↔ ECS ↔ RDS |
| `data.tf` | Data sources (VPC, subnets, AMI lookups) |
| `outputs.tf` | Outputs consumed by `aws-deploy.sh` |

## Sizing defaults

Prod and dev share the same Terraform; sizing is via variables. Run
`deploy-guardian-aws` to inspect current sizing before reasoning about
cost/capacity.
