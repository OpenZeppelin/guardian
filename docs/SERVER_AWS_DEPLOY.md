# Deploying PSM Server to AWS ECS

This guide walks through deploying the Private State Manager (PSM) server to AWS Elastic Container Service (ECS) using Terraform.

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
server_image_uri = "123456789012.dkr.ecr.us-east-1.amazonaws.com/psm-server:latest"

# Optional: Postgres credentials (defaults shown)
# postgres_db       = "psm"
# postgres_user     = "psm"
# postgres_password = "psm_dev_password"

# Optional: Route 53 hosted zone ID for openzeppelin.com
# route53_zone_id = "Z1234567890ABC"
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
curl https://psm.openzeppelin.com/pubkey
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
aws ecr delete-repository --repository-name psm-server --force --region us-east-1
```

## Configuration Reference

Defaults assume `psm.openzeppelin.com`. See `infra/terraform.tfvars.example`
for all available options.

### Resources Created

| Resource | Description |
|----------|-------------|
| ECS Cluster | Fargate cluster (`psm-cluster`) |
| ECS Services | `psm-server`, `psm-postgres` |
| Application Load Balancer | Internet-facing ALB (`psm-alb`) |
| Target Group | Routes to server on port 3000 |
| Cloud Map Namespace | Service discovery (`psm.local`) |
| Security Groups | ALB, server, and postgres SGs |
| CloudWatch Log Groups | `/ecs/psm-server`, `/ecs/psm-postgres` |
| IAM Role | ECS task execution role |

### Outputs

| Output | Description |
|--------|-------------|
| `alb_dns_name` | ALB DNS name |
| `alb_url` | Full URL (http or https) |
| `ecs_cluster_arn` | ECS cluster ARN |
| `server_service_arn` | Server service ARN |

## HTTPS Configuration

HTTPS is automated via Route 53 + ACM for `psm.openzeppelin.com`. Terraform:

1. Requests an ACM certificate for `psm.openzeppelin.com`
2. Creates the DNS validation records in the existing Route 53 hosted zone
3. Creates the ALB alias record

Ensure the `openzeppelin.com` hosted zone exists in the AWS account and the
deployer has Route 53 permissions. Set `route53_zone_id` if auto-lookup fails.

## Legacy Script

The legacy deployment logic has been replaced by the Terraform-backed `scripts/aws-deploy.sh`.
