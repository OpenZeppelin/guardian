# GUARDIAN Server AWS Infrastructure (Terraform)

This directory contains the Terraform configuration for the current Guardian AWS deployment: ECS/Fargate behind an ALB, backed by Amazon RDS for PostgreSQL.

The deployment is stage-aware:
- `deployment_stage = "dev"` keeps the stack close to the current fixed-capacity profile
- `deployment_stage = "prod"` enables ECS autoscaling, RDS storage autoscaling, RDS Proxy, and larger default RDS sizing for benchmark traffic

## Architecture

```text
Internet → ALB (HTTP/HTTPS + gRPC over HTTPS) → ECS Service (server) → RDS PostgreSQL
```

Resources created:
- ECS Cluster (Fargate)
- ECS service derived from `stack_name` for the Guardian server
- Application Load Balancer with HTTP and gRPC target groups
- RDS PostgreSQL instance and subnet group
- Secrets Manager secret for `DATABASE_URL`
- Secrets Manager secrets for stable Falcon and ECDSA ack keys in `prod`
- Security groups for the ALB, server task, and database
- CloudWatch log groups
- IAM roles for ECS task execution and runtime

## Usage

### 1. Build And Push Docker Image

```bash
export AWS_REGION=us-east-1
export CPU_ARCHITECTURE=X86_64
export AWS_ACCOUNT_ID=$(aws sts get-caller-identity --query Account --output text)

export STACK_NAME=guardian
export ECR_REPO_NAME="${STACK_NAME}-server"

aws ecr create-repository --repository-name "$ECR_REPO_NAME" --region "$AWS_REGION" 2>/dev/null || true

aws ecr get-login-password --region "$AWS_REGION" | \
  docker login --username AWS --password-stdin "$AWS_ACCOUNT_ID.dkr.ecr.$AWS_REGION.amazonaws.com"

docker build --platform linux/amd64 -t "$ECR_REPO_NAME" .
docker tag "$ECR_REPO_NAME:latest" "$AWS_ACCOUNT_ID.dkr.ecr.$AWS_REGION.amazonaws.com/$ECR_REPO_NAME:latest"
docker push "$AWS_ACCOUNT_ID.dkr.ecr.$AWS_REGION.amazonaws.com/$ECR_REPO_NAME:latest"
```

### 2. Configure Variables

Create `terraform.tfvars`:

```hcl
aws_region = "us-east-1"

# Optional: image/task architecture
# cpu_architecture = "X86_64"
# cpu_architecture = "ARM64"

# Optional: stack base name
# stack_name = "guardian"

# Optional: deployment profile
# deployment_stage = "dev"
# deployment_stage = "prod"

server_image_uri = "123456789012.dkr.ecr.us-east-1.amazonaws.com/guardian-server@sha256:..."

# Optional: Use specific VPC/subnets
# vpc_id     = "vpc-xxxxxxxx"
# subnet_ids = ["subnet-xxxxxxxx", "subnet-yyyyyyyy"]
# rds_proxy_subnet_ids = ["subnet-xxxxxxxx", "subnet-yyyyyyyy"]
# In us-east-1, avoid subnets in us-east-1e/use1-az3 for RDS Proxy.

# Optional: Postgres credentials
# postgres_db       = "guardian"
# postgres_user     = "guardian"
# postgres_password = "guardian_dev_password"

# Optional: RDS sizing overrides
# Stage defaults:
# - dev  -> db.t3.micro, 20 GiB allocated, no storage autoscaling ceiling
# - prod -> db.t3.medium, 50 GiB allocated, 200 GiB max allocated
# rds_instance_class = "db.t3.medium"
# rds_allocated_storage = 50
# rds_max_allocated_storage = 200
# rds_proxy_enabled = true

# Optional: stage/runtime capacity overrides
# server_desired_count = 2
# server_autoscaling_enabled = true
# server_autoscaling_min_capacity = 2
# server_autoscaling_max_capacity = 6
# guardian_rate_limit_enabled = false
# guardian_rate_burst_per_sec = 200
# guardian_rate_per_min = 5000
# guardian_db_pool_max_size = 32
# guardian_metadata_db_pool_max_size = 32

# Optional: Route 53 hosted zone ID
# route53_zone_id = "Z1234567890ABC"
```

### 3. Deploy

```bash
cd infra
terraform init
terraform plan
terraform apply
```

When you use `scripts/aws-deploy.sh`, it keeps separate local Terraform state files per `STACK_NAME` and `deployment_stage`, using:

```text
infra/terraform.<stack>.<stage>.tfstate
```

by default.

If you still have an older local state file at `infra/terraform.tfstate`, move it manually before switching to the split-state workflow:

```bash
cp infra/terraform.tfstate infra/terraform.guardian.dev.tfstate
cp infra/terraform.tfstate.backup infra/terraform.guardian.dev.tfstate.backup 2>/dev/null || true
```

For `prod`, create the ACK key secrets once before the first deploy:

```bash
DEPLOY_STAGE=prod ./scripts/aws-deploy.sh bootstrap-ack-keys
```

Normal deploys do not create or rotate ACK keys. The server fetches these prod Secrets Manager entries at startup, imports them into the filesystem keystore, and then signs through the filesystem keystore like every other environment:

- `guardian-prod/server/ack-falcon-secret-key`
- `guardian-prod/server/ack-ecdsa-secret-key`

### 4. Get Outputs

```bash
terraform output alb_dns_name
terraform output
```

### 5. Test

```bash
ALB_DNS=$(terraform output -raw alb_dns_name)

curl "http://$ALB_DNS/"
curl "http://$ALB_DNS/pubkey"
curl "https://guardian.openzeppelin.com/pubkey"
grpcurl -import-path ../crates/server/proto -proto guardian.proto -d '{}' guardian.openzeppelin.com:443 guardian.Guardian/GetPubkey
```

### 6. Destroy

```bash
terraform destroy
```

ECR repositories are not managed by Terraform:

```bash
aws ecr delete-repository --repository-name "$ECR_REPO_NAME" --force --region "$AWS_REGION"
```

## Variables Reference

| Variable | Default | Description |
|----------|---------|-------------|
| `aws_region` | `us-east-1` | AWS region |
| `cpu_architecture` | `X86_64` | ECS task and image architecture |
| `stack_name` | `guardian` | Base name used to derive stack resource names |
| `deployment_stage` | `dev` | Deployment stage profile |
| `server_image_uri` | (required) | ECR image URI for the server, preferably pinned to a digest |
| `vpc_id` | (default VPC) | VPC ID |
| `subnet_ids` | (all subnets in VPC) | Subnet IDs for ECS tasks and ALB |
| `rds_proxy_subnet_ids` | filtered `subnet_ids` | Optional dedicated subnet IDs for RDS Proxy |
| `postgres_db` | `guardian` | Postgres database name |
| `postgres_user` | `guardian` | Postgres username |
| `postgres_password` | `guardian_dev_password` | Postgres password |
| `rds_instance_class` | `db.t3.micro` in dev, `db.t3.medium` in prod | RDS instance class |
| `rds_allocated_storage` | `20` in dev, `50` in prod | RDS allocated storage in GiB |
| `rds_max_allocated_storage` | `null` in dev, `200` in prod | RDS storage autoscaling ceiling |
| `rds_proxy_enabled` | `false` in dev, `true` in prod | Whether RDS Proxy is enabled |
| `domain_name` | `openzeppelin.com` | Root domain for HTTPS endpoint |
| `subdomain` | `guardian` | Subdomain for HTTPS endpoint |
| `route53_zone_id` | `""` | Route 53 hosted zone ID for the domain |
| `alb_ingress_cidrs` | `["0.0.0.0/0"]` | CIDR blocks allowed to reach the ALB |
| `server_cpu` | `512` | Server task CPU units |
| `server_memory` | `1024` | Server task memory (MB) |
| `server_desired_count` | `1` in dev, `2` in prod | ECS service desired task count |
| `server_autoscaling_enabled` | `false` in dev, `true` in prod | Whether ECS autoscaling is enabled |
| `guardian_rate_limit_enabled` | `true` | Whether Guardian HTTP rate limiting is enabled |
| `guardian_rate_burst_per_sec` | `10` in dev, `200` in prod | Guardian HTTP burst limit |
| `guardian_rate_per_min` | `60` in dev, `5000` in prod | Guardian HTTP sustained limit |
| `guardian_db_pool_max_size` | `16` in dev, `32` in prod | Guardian storage DB pool size |
| `guardian_metadata_db_pool_max_size` | matches storage by default | Guardian metadata DB pool size |
| `log_retention_days` | `7` | CloudWatch log retention in days |

## Outputs

| Output | Description |
|--------|-------------|
| `alb_dns_name` | ALB DNS name for accessing the server |
| `alb_url` | Full URL (http or https) |
| `custom_domain_url` | Custom domain URL when configured |
| `grpc_endpoint` | Public gRPC endpoint when HTTPS is enabled |
| `database_endpoint` | RDS endpoint used by the server |
| `rds_proxy_endpoint` | RDS Proxy endpoint when enabled |
| `rds_instance_class` | Effective RDS instance class |
| `rds_allocated_storage` | Effective allocated RDS storage in GiB |
| `database_url_secret_arn` | Secrets Manager ARN for the server `DATABASE_URL` |
| `ack_falcon_secret_name` | Secrets Manager name for the Falcon ack key |
| `ack_ecdsa_secret_name` | Secrets Manager name for the ECDSA ack key |
| `ecs_cluster_arn` | ECS cluster ARN |
| `server_service_arn` | Server ECS service ARN |
| `server_log_group` | CloudWatch log group for the server |
| `cluster_log_group` | CloudWatch log group for ECS execute command |
| `guardian_rate_limit_enabled` | Whether HTTP rate limiting is enabled |

## Stage Profiles

### Dev

- single ECS task
- direct ECS to RDS connectivity
- no ECS autoscaling
- no RDS Proxy
- conservative runtime rate limits and DB pool sizes

### Prod

- higher ECS desired count
- ECS target-tracking autoscaling
- larger default RDS instance class and base storage
- RDS storage autoscaling
- RDS Proxy between ECS and RDS
- higher runtime rate limits and DB pool sizes for benchmark traffic

## HTTPS Configuration

HTTPS is enabled when `acm_certificate_arn` is set. DNS can be managed through Cloudflare, Route 53, or both depending on which variables are provided. In the current `guardian-stg` deployment, Terraform state shows Cloudflare DNS management and no Route 53 record.

When HTTPS is enabled, the ALB routes standard HTTPS requests to the server HTTP port `3000` and gRPC requests for `/guardian.Guardian/*` to the server gRPC port `50051`. The public gRPC endpoint uses the same hostname on port `443`.

On Apple Silicon hosts, building `X86_64` images is slower because Docker builds `linux/amd64` images under emulation. If you want faster native local builds and your ECS deployment can run ARM64, set `cpu_architecture = "ARM64"` and deploy an ARM64 task definition.

## Existing Stack Cutover

This Terraform stack is RDS-only. Existing stacks that still run ECS-hosted Postgres must be migrated with an explicit backup-and-restore cutover:

1. Capture the current outputs with `terraform output` or `./scripts/aws-deploy.sh status`.
2. Create a logical backup from the existing Postgres runtime before applying the updated stack.
3. Apply the updated Terraform stack so the server is wired to RDS.
4. Restore the backup into the RDS database.
5. Validate the public Guardian endpoints.
6. Confirm the old Postgres ECS and Cloud Map resources are gone from AWS before considering the cutover complete.

## Troubleshooting

- If the server task does not start, inspect the server log group and confirm the `database_endpoint` output points to the expected RDS host.
- If RDS subnet-group creation fails, verify the selected subnets cover at least two subnets for the database deployment.
- If gRPC works against the ALB but not the public hostname, check Cloudflare gRPC settings.
