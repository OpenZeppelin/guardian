# Deploying GUARDIAN Server to AWS ECS

This guide covers the current AWS deployment for Guardian. The AWS stack now uses Amazon RDS for PostgreSQL and no longer supports the legacy ECS-hosted Postgres runtime.

The deployment surface supports two stage profiles:
- `DEPLOY_STAGE=dev` keeps the current low-cost, fixed-capacity behavior
- `DEPLOY_STAGE=prod` enables ECS autoscaling, RDS storage autoscaling, RDS Proxy, larger default RDS sizing, and benchmark-oriented runtime defaults

## Prerequisites

- [Terraform](https://developer.hashicorp.com/terraform/downloads) >= 1.0
- AWS CLI configured with permissions for ECS, ECR, ELB, EC2, IAM, CloudWatch, RDS, and Secrets Manager
- Docker installed locally

```bash
aws sts get-caller-identity
docker info
terraform version
```

## Quick Start

```bash
aws sso login --profile <your-profile>

set -a && source .env && set +a

# Optional: build/deploy ARM64 instead of X86_64
# export CPU_ARCHITECTURE=ARM64

# Optional: pin the server to a specific Miden network
export GUARDIAN_NETWORK_TYPE=MidenDevnet

# Optional: choose the deployment profile
export DEPLOY_STAGE=dev
# export DEPLOY_STAGE=prod

# Optional: override the stack base name or public hostname
export STACK_NAME=guardian
# export SUBDOMAIN=guardian-stg

aws sts get-caller-identity
./scripts/aws-deploy.sh deploy
./scripts/aws-deploy.sh status
```

## Terraform Variables

If you need to override defaults, use `infra/terraform.tfvars`:

```hcl
aws_region = "us-east-1"

# Optional: ECS/image architecture
# cpu_architecture = "X86_64"
# cpu_architecture = "ARM64"

# Optional: derive resource names from a base stack name
# stack_name = "guardian"

server_image_uri = "123456789012.dkr.ecr.us-east-1.amazonaws.com/guardian-server:latest"

# Optional: Postgres credentials (defaults derive from stack_name)
# postgres_db       = "guardian"
# postgres_user     = "guardian"
# postgres_password = "guardian_dev_password"

# Optional: managed database sizing overrides
# Stage defaults:
# - dev  -> db.t3.micro, 20 GiB allocated, no storage autoscaling ceiling
# - prod -> db.t3.medium, 50 GiB allocated, 200 GiB max allocated
# rds_instance_class = "db.t3.medium"
# rds_allocated_storage = 50
# rds_max_allocated_storage = 200

# Optional: Miden network for the server runtime
# server_network_type = "MidenDevnet"

# Optional: stage/runtime capacity overrides
# deployment_stage = "prod"
# server_desired_count = 2
# server_autoscaling_enabled = true
# server_autoscaling_min_capacity = 2
# server_autoscaling_max_capacity = 6
# server_autoscaling_cpu_target = 65
# server_autoscaling_memory_target = 75
# rds_proxy_enabled = true
# rds_proxy_subnet_ids = ["subnet-xxxxxxxx", "subnet-yyyyyyyy"]
# In us-east-1, avoid subnets in us-east-1e/use1-az3 for RDS Proxy.
# guardian_rate_limit_enabled = false
# guardian_rate_burst_per_sec = 200
# guardian_rate_per_min = 5000
# guardian_db_pool_max_size = 32
# guardian_metadata_db_pool_max_size = 32

# Optional: Route 53 hosted zone ID
# route53_zone_id = "Z1234567890ABC"

# Optional: Cloudflare DNS management
# cloudflare_zone_id = "..."
# cloudflare_api_token = "..."
```

## Deploy

```bash
./scripts/aws-deploy.sh deploy
```

The deploy script resolves the ECR `latest` tag to an immutable digest before calling Terraform, so image pushes always produce a real ECS task-definition revision instead of relying on tag reuse.
It also keeps separate local Terraform state files per `STACK_NAME` and `DEPLOY_STAGE`, using `infra/terraform.<stack>.<stage>.tfstate` by default.

For `DEPLOY_STAGE=prod`, bootstrap the ACK secrets once before the first deploy:

```bash
DEPLOY_STAGE=prod ./scripts/aws-deploy.sh bootstrap-ack-keys
```

The normal deploy path does not create or rotate ACK keys. It expects these prod Secrets Manager entries to already exist, and the server reads them directly at startup before importing them into the filesystem keystore:

- `guardian-prod/server/ack-falcon-secret-key`
- `guardian-prod/server/ack-ecdsa-secret-key`

If you still have an older local state file at `infra/terraform.tfstate`, move it manually before using the split-state workflow:

```bash
cp infra/terraform.tfstate infra/terraform.guardian.dev.tfstate
cp infra/terraform.tfstate.backup infra/terraform.guardian.dev.tfstate.backup 2>/dev/null || true
```

Use `--skip-build` when the image already exists in ECR and you only need infra/runtime changes:

```bash
./scripts/aws-deploy.sh deploy --skip-build
```

For benchmark-oriented production deploys, prefer explicit overrides rather than changing the base prod profile in code. A typical starting point is:

```bash
set -a && source .env && set +a

export DEPLOY_STAGE=prod
export STACK_NAME=guardian-prod
export TF_VAR_server_cpu=2048
export TF_VAR_server_memory=4096
export TF_VAR_server_desired_count=3
export TF_VAR_server_autoscaling_min_capacity=3
export TF_VAR_server_autoscaling_max_capacity=10
export TF_VAR_rds_instance_class=db.r6g.large
export TF_VAR_rds_allocated_storage=100
export TF_VAR_rds_max_allocated_storage=400
export TF_VAR_rds_proxy_subnet_ids='["subnet-25c1722b","subnet-4d0eca6c"]'
export TF_VAR_guardian_db_pool_max_size=64
export TF_VAR_guardian_metadata_db_pool_max_size=64
export TF_VAR_guardian_rate_limit_enabled=false

./scripts/aws-deploy.sh deploy --skip-build
```

## Validate

```bash
./scripts/aws-deploy.sh status
curl https://guardian.openzeppelin.com/pubkey
grpcurl -import-path crates/server/proto -proto guardian.proto -d '{}' guardian.openzeppelin.com:443 guardian.Guardian/GetPubkey
```

## Operations

### Logs

```bash
./scripts/aws-deploy.sh logs
```

### Status

```bash
./scripts/aws-deploy.sh status
```

The script reads the state file for the active `STACK_NAME` and `DEPLOY_STAGE`. The current default path is:

```text
infra/terraform.<stack>.<stage>.tfstate
```

You can override that with `TF_STATE_PATH` if needed.

### Destroy

```bash
./scripts/aws-deploy.sh cleanup
```

ECR repositories are not managed by Terraform:

```bash
aws ecr delete-repository --repository-name guardian-server --force --region us-east-1
```

## Resources Created

| Resource | Description |
|----------|-------------|
| ECS Cluster | Fargate cluster derived from `stack_name` |
| ECS Service | Guardian server service |
| Application Load Balancer | Internet-facing ALB derived from `stack_name` |
| Target Groups | HTTP target group for port `3000` and gRPC target group for port `50051` |
| RDS | Managed PostgreSQL instance and subnet group |
| RDS Proxy | Managed PostgreSQL proxy in the production profile |
| Secrets Manager | Secret containing `DATABASE_URL` for the server task |
| Secrets Manager | Secrets containing the Falcon and ECDSA ack private keys used to seed the server keystore in prod |
| Security Groups | ALB, server, and database security groups |
| CloudWatch Log Groups | Cluster execute-command logs and server logs |
| IAM Role | ECS task execution and runtime roles |

## Outputs

| Output | Description |
|--------|-------------|
| `alb_dns_name` | ALB DNS name |
| `alb_url` | Full ALB URL |
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

## Stage Profiles

### Dev

- single ECS task
- no ECS autoscaling
- direct ECS to RDS connection
- no RDS Proxy
- conservative Guardian runtime limits

### Prod

- higher ECS desired count
- ECS service autoscaling
- larger default RDS instance class and base storage
- RDS storage autoscaling
- RDS Proxy between ECS and RDS
- higher Guardian runtime rate-limit and DB-pool defaults for benchmark traffic

## HTTPS And gRPC

HTTPS is enabled when `acm_certificate_arn` is set. DNS can be managed through Cloudflare, Route 53, or both depending on which variables are provided.

When HTTPS is enabled, the ALB routes standard HTTPS requests to the server HTTP port `3000` and gRPC requests for `/guardian.Guardian/*` to the server gRPC port `50051`. The public gRPC endpoint uses the same hostname on port `443`.

On Apple Silicon hosts, `CPU_ARCHITECTURE=X86_64` builds are slower because Docker builds `linux/amd64` images under emulation. Switching to `ARM64` avoids that local emulation cost, but it also changes the ECS task runtime architecture.

## Migrating An Existing ECS-Postgres Stack

The current Terraform configuration is RDS-only. There is no supported dual-mode deployment that keeps the old ECS Postgres service alive after apply.

Use this cutover flow for an existing stack:

1. Capture the current stack state:
   ```bash
   ./scripts/aws-deploy.sh status
   ```
2. Create a logical PostgreSQL backup from the existing ECS-hosted database before applying the updated stack.
3. Apply the updated RDS-backed Terraform stack:
   ```bash
   ./scripts/aws-deploy.sh deploy --skip-build
   ```
4. Restore the backup into the new RDS database.
5. Validate the public service:
   ```bash
   ./scripts/aws-deploy.sh status
   curl https://<host>/pubkey
   grpcurl -import-path crates/server/proto -proto guardian.proto -d '{}' <host>:443 guardian.Guardian/GetPubkey
   ```
6. Confirm the old Postgres ECS service and Cloud Map database-discovery resources are gone from AWS before treating the cutover as complete.

## Troubleshooting

- If the server task fails during startup, check `./scripts/aws-deploy.sh logs` first and confirm the reported `database_endpoint` matches the expected RDS host.
- If a prod deploy fails before Terraform starts, confirm the fixed prod ACK secrets exist by running `./scripts/aws-deploy.sh bootstrap-ack-keys` once and then retrying the deploy.
- If RDS subnet-group creation fails, verify the selected subnets cover at least two subnets for the database deployment.
- If gRPC works against the ALB directly but fails on the public hostname, check Cloudflare gRPC settings on the zone.

## Legacy Script

The legacy deployment logic has been replaced by the Terraform-backed `scripts/aws-deploy.sh`.
