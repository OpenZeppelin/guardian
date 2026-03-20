# Current PSM Staging Deployment

Use this reference when the target endpoint is `psm-stg.openzeppelin.com` or when deployment state still uses `psm-*` names.

## Current OpenZeppelin Login Flow

Use the active OpenZeppelin AWS SSO profile and assume the target account access role before running deploy commands. A generic pattern is:

```bash
aws sso login --profile <sso-profile>

export STS_CMD=$(aws sts assume-role \
  --role-arn <target-role-arn> \
  --role-session-name "<session-name>" \
  --profile <sso-profile>)

export AWS_ACCESS_KEY_ID="$(echo "$STS_CMD" | jq -r '.Credentials.AccessKeyId')"
export AWS_SECRET_ACCESS_KEY="$(echo "$STS_CMD" | jq -r '.Credentials.SecretAccessKey')"
export AWS_SESSION_TOKEN="$(echo "$STS_CMD" | jq -r '.Credentials.SessionToken')"
export AWS_REGION="us-east-1"
```

After exporting temporary credentials:

- `AWS_PROFILE` can remain set, but it is not required for later `aws`, `docker`, or `terraform` commands
- `STS_CMD` is only a convenience shell variable and can be unset
- verify the active identity with `aws sts get-caller-identity`
- if deployment vars already live in repo `.env`, prefer `set -a && source .env && set +a` instead of re-exporting them one by one

## Recommended Variable Set

Keep these when deploying the named staging environment:

- `CPU_ARCHITECTURE=X86_64` if you want to preserve the current amd64 deployment behavior
- `STACK_NAME=psm`
- `AWS_REGION=us-east-1`
- `DOMAIN_NAME=openzeppelin.com`
- `SUBDOMAIN=psm-stg`
- `ACM_CERTIFICATE_ARN=...` when the HTTPS listener should remain enabled
- `GUARDIAN_NETWORK_TYPE=MidenTestnet`
- `IMPORT_EXISTING=false` unless the task is specifically importing existing resources into Terraform state

For the existing `psm-stg` stack, treat the domain and DNS vars as part of desired state, not as harmless shell clutter. Unsetting `DOMAIN_NAME`, `SUBDOMAIN`, `ACM_CERTIFICATE_ARN`, or Cloudflare vars changes what Terraform will try to manage on the next apply.

Remove or rename these:

- remove `PSM_NETWORK_TYPE`; current code only consumes `GUARDIAN_NETWORK_TYPE`
- remove `CLOUDFLARE_ZONE_ID` only if you are intentionally stopping Terraform-managed Cloudflare DNS for this stack; otherwise keep it and also provide `CLOUDFLARE_API_TOKEN`
- keep `ROUTE53_ZONE_ID` empty unless Terraform should manage the AWS Route 53 record; current code does not auto-look it up

Choose one DNS mode per deploy:

1. Cloudflare-managed DNS
   - set `CLOUDFLARE_ZONE_ID`
   - set `CLOUDFLARE_API_TOKEN`
   - optionally set `CLOUDFLARE_PROXIED`
   - keep `ROUTE53_ZONE_ID` only if Terraform should also create the Route 53 CNAME toward Cloudflare
2. No DNS automation
   - unset `CLOUDFLARE_ZONE_ID`
   - unset `CLOUDFLARE_API_TOKEN`
   - unset `ROUTE53_ZONE_ID`
   - rely on the existing DNS records

Choose architecture deliberately:

1. Preserve current behavior
   - set `CPU_ARCHITECTURE=X86_64`
   - expect slower local builds on Apple Silicon because Docker will build `linux/amd64`
2. Optimize local build speed on Apple Silicon
   - set `CPU_ARCHITECTURE=ARM64`
   - this also updates the ECS task definition runtime architecture to ARM64

## Current Drift Between State and Code

The checked-in state still reflects older `psm` naming:

- cluster and service names like `psm-cluster`, `psm-server`, and `psm-postgres`
- ALB and domain values like `psm-alb` and `psm-stg.openzeppelin.com`
- Cloud Map namespace `psm.local`
- legacy runtime env name `PSM_NETWORK_TYPE`
- non-default ECR image, IAM role, log group, security group, and task family names

Current code supports variable overrides for:

- `stack_name`
- `cluster_name`
- `server_service_name`
- `postgres_service_name`
- `alb_name`
- `sd_namespace_name`
- `target_group_name`
- `alb_security_group_name`
- `server_security_group_name`
- `postgres_security_group_name`
- `task_execution_role_name`
- `task_role_name`
- `server_task_family`
- `postgres_task_family`
- `server_container_name`
- `server_log_group_name`
- `postgres_log_group_name`

`STACK_NAME=psm` now aligns the deploy script defaults with the current `psm-*` stack shape. Inspect the plan anyway before applying because Terraform state still captures prior naming and DNS decisions.
