# GUARDIAN Server AWS Infrastructure (Terraform)

This directory contains Terraform configuration to deploy the Guardian server to AWS ECS with Fargate, behind an Application Load Balancer.

## Prerequisites

- [Terraform](https://developer.hashicorp.com/terraform/downloads) >= 1.0
- AWS CLI configured with appropriate permissions
- Docker image already pushed to ECR (see below)

## Architecture

```
Internet → ALB (HTTP/HTTPS) → ECS Service (guardian-server) → Cloud Map → ECS Service (postgres)
```

Resources created:
- ECS Cluster (Fargate)
- ECS Services: guardian-server, guardian-postgres
- Application Load Balancer + Target Group + Listener
- Cloud Map namespace for service discovery
- Security Groups (ALB, server, postgres)
- CloudWatch Log Groups
- IAM Role for ECS task execution

## Usage

### 1. Build and Push Docker Image

Before running Terraform, build and push the Docker image to ECR:

```bash
# Set variables
export AWS_REGION=us-east-1
export AWS_ACCOUNT_ID=$(aws sts get-caller-identity --query Account --output text)

# Create ECR repository (if it doesn't exist)
aws ecr create-repository --repository-name guardian-server --region $AWS_REGION 2>/dev/null || true

# Login to ECR
aws ecr get-login-password --region $AWS_REGION | \
  docker login --username AWS --password-stdin $AWS_ACCOUNT_ID.dkr.ecr.$AWS_REGION.amazonaws.com

# Build and push (from repo root)
docker build --platform linux/amd64 -t guardian-server .
docker tag guardian-server:latest $AWS_ACCOUNT_ID.dkr.ecr.$AWS_REGION.amazonaws.com/guardian-server:latest
docker push $AWS_ACCOUNT_ID.dkr.ecr.$AWS_REGION.amazonaws.com/guardian-server:latest
```

### 2. Configure Variables

Create a `terraform.tfvars` file:

```hcl
aws_region = "us-east-1"

# Required: ECR image URI
server_image_uri = "123456789012.dkr.ecr.us-east-1.amazonaws.com/guardian-server:latest"

# Optional: Use specific VPC/subnets (defaults to default VPC)
# vpc_id     = "vpc-xxxxxxxx"
# subnet_ids = ["subnet-xxxxxxxx", "subnet-yyyyyyyy"]

# Optional: Postgres credentials (defaults shown)
# postgres_db       = "guardian"
# postgres_user     = "guardian"
# postgres_password = "guardian_dev_password"

# Optional: Route 53 hosted zone ID for openzeppelin.com
# route53_zone_id = "Z1234567890ABC"
```

### 3. Deploy

```bash
cd infra

# Initialize Terraform
terraform init

# Review the plan
terraform plan

# Apply changes
terraform apply
```

### 4. Get Outputs

```bash
# Get the ALB DNS name
terraform output alb_dns_name

# Get all outputs
terraform output
```

### 5. Test

```bash
ALB_DNS=$(terraform output -raw alb_dns_name)

# Health check
curl http://$ALB_DNS/

# Get public key
curl http://$ALB_DNS/pubkey

# Custom domain (requires Route 53 hosted zone for openzeppelin.com)
curl https://guardian.openzeppelin.com/pubkey
```

### 6. Destroy

```bash
terraform destroy
```

Note: ECR repository is not managed by Terraform to avoid accidental image deletion. Delete manually if needed:

```bash
aws ecr delete-repository --repository-name guardian-server --force --region $AWS_REGION
```

## Variables Reference

| Variable | Default | Description |
|----------|---------|-------------|
| `aws_region` | `us-east-1` | AWS region |
| `server_image_uri` | (required) | ECR image URI for guardian-server |
| `vpc_id` | (default VPC) | VPC ID |
| `subnet_ids` | (all subnets in VPC) | Subnet IDs for ECS tasks and ALB |
| `postgres_db` | `guardian` | Postgres database name |
| `postgres_user` | `guardian` | Postgres username |
| `postgres_password` | `guardian_dev_password` | Postgres password |
| `domain_name` | `openzeppelin.com` | Root domain for HTTPS endpoint |
| `subdomain` | `guardian` | Subdomain for HTTPS endpoint |
| `route53_zone_id` | `""` | Route 53 hosted zone ID for the domain |
| `alb_ingress_cidrs` | `["0.0.0.0/0"]` | CIDR blocks allowed to reach the ALB |
| `server_cpu` | `512` | Server task CPU units |
| `server_memory` | `1024` | Server task memory (MB) |
| `postgres_cpu` | `512` | Postgres task CPU units |
| `postgres_memory` | `1024` | Postgres task memory (MB) |
| `log_retention_days` | `7` | CloudWatch log retention in days |

## Outputs

| Output | Description |
|--------|-------------|
| `alb_dns_name` | ALB DNS name for accessing the server |
| `alb_url` | Full URL (http or https) |
| `custom_domain_url` | Custom domain URL when configured |
| `ecs_cluster_arn` | ECS cluster ARN |
| `server_service_arn` | Server ECS service ARN |
| `postgres_service_arn` | Postgres ECS service ARN |
| `server_log_group` | CloudWatch log group for server |
| `cluster_log_group` | CloudWatch log group for ECS execute command |
| `postgres_log_group` | CloudWatch log group for postgres |

## HTTPS Configuration

HTTPS is automated via Route 53 + ACM for `guardian.openzeppelin.com`. Terraform:

1. Requests an ACM certificate for `guardian.openzeppelin.com`
2. Creates the DNS validation records in the existing Route 53 hosted zone
3. Creates the ALB alias record

Ensure the `openzeppelin.com` hosted zone exists in the AWS account and the
deployer has Route 53 permissions. Set `route53_zone_id` if auto-lookup fails.
