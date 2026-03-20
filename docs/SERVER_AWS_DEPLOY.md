# Deploying GUARDIAN Server to AWS ECS

This guide walks through deploying the Guardian server to AWS Elastic Container Service (ECS) using Terraform.

## Prerequisites

- [Terraform](https://developer.hashicorp.com/terraform/downloads) >= 1.0
- AWS CLI configured with permissions for ECS, ECR, ELB, EC2, IAM, CloudWatch, and Service Discovery
- Docker installed locally

```bash
# Verify AWS CLI is configured
aws sts get-caller-identity

# Verify Docker is running
docker info

# Verify Terraform is installed
terraform version
```

## Quick Start

```bash
# Authenticate via SSO (if using AWS SSO)
aws sso login --profile <your-profile>
export AWS_PROFILE=<your-profile>

# Load environment variables
set -a && source .env && set +a

# Optional: select ECS/image architecture
# export CPU_ARCHITECTURE=ARM64

# Optional: pin the server to a specific Miden network
export GUARDIAN_NETWORK_TYPE=MidenTestnet

# Optional: pick a stack base name and custom subdomain
export STACK_NAME=guardian
# export STACK_NAME=psm
# export SUBDOMAIN=psm-stg

# Verify AWS credentials
aws sts get-caller-identity

# Deploy infrastructure (builds/pushes image and runs Terraform)
./scripts/aws-deploy.sh deploy

# Get the deployment URLs
./scripts/aws-deploy.sh status
```

## Step-by-Step Deployment

### 1. Build and Push Docker Image

The deploy script handles ECR login, build, and push automatically:

```bash
./scripts/aws-deploy.sh deploy
```

### 2. Configure Terraform Variables

If you need to override defaults, edit `infra/terraform.tfvars`:

```hcl
aws_region = "us-east-1"

# Optional: ECS/image architecture
# cpu_architecture = "X86_64"
# cpu_architecture = "ARM64"

# Optional: derive resource names from a base stack name
# stack_name = "guardian"

server_image_uri = "123456789012.dkr.ecr.us-east-1.amazonaws.com/guardian-server:latest"

# Optional: Postgres credentials (defaults shown)
# postgres_db       = "guardian"
# postgres_user     = "guardian"
# postgres_password = "guardian_dev_password"

# Optional: Miden network for the server runtime
# server_network_type = "MidenTestnet"

# Optional: Route 53 hosted zone ID for openzeppelin.com
# route53_zone_id = "Z1234567890ABC"

# Optional: Cloudflare DNS management
# cloudflare_zone_id = "..."
# cloudflare_api_token = "..."
```

### 3. Deploy Infrastructure

```bash
./scripts/aws-deploy.sh deploy
```

### 4. Get Deployment URL

```bash
./scripts/aws-deploy.sh status
```

### 5. Test the Deployment

```bash
curl https://guardian.openzeppelin.com/pubkey
```

## Operations

### View Logs

```bash
./scripts/aws-deploy.sh logs
```

### Check Status

```bash
./scripts/aws-deploy.sh status
```

### Update Server Image

Re-run the deploy script after pushing a new image:

```bash
./scripts/aws-deploy.sh deploy
```

### Destroy Infrastructure

```bash
./scripts/aws-deploy.sh cleanup
```

Note: ECR repository is not managed by Terraform. Delete manually if needed:

```bash
aws ecr delete-repository --repository-name guardian-server --force --region us-east-1
```

## Configuration Reference

Defaults assume `guardian.openzeppelin.com`. See `infra/terraform.tfvars.example`
for all available options.

### Resources Created

| Resource | Description |
|----------|-------------|
| ECS Cluster | Fargate cluster derived from `stack_name` |
| ECS Services | Services derived from `stack_name` |
| Application Load Balancer | Internet-facing ALB derived from `stack_name` |
| Target Group | Routes to server on port 3000 |
| Cloud Map Namespace | Service discovery namespace derived from `stack_name` |
| Security Groups | ALB, server, and postgres SGs |
| CloudWatch Log Groups | Log groups derived from service names |
| IAM Role | ECS task execution role |

### Outputs

| Output | Description |
|--------|-------------|
| `alb_dns_name` | ALB DNS name |
| `alb_url` | Full URL (http or https) |
| `ecs_cluster_arn` | ECS cluster ARN |
| `server_service_arn` | Server service ARN |

## HTTPS Configuration

HTTPS is enabled when `acm_certificate_arn` is set. Cloudflare DNS records are managed only when both `cloudflare_zone_id` and `cloudflare_api_token` are set. Route 53 records are managed only when `route53_zone_id` is set.

On Apple Silicon hosts, `CPU_ARCHITECTURE=X86_64` builds are slower because Docker builds `linux/amd64` images under emulation. Switching to `ARM64` avoids that local emulation cost, but it also changes the ECS task definition runtime architecture.

## Legacy Script

The legacy deployment logic has been replaced by the Terraform-backed `scripts/aws-deploy.sh`.
