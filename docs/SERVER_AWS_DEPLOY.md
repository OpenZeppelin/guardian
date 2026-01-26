# Deploying PSM Server to AWS ECS

This guide walks through deploying the Private State Manager (PSM) server to AWS Elastic Container Service (ECS) using the provided deployment script.

## Prerequisites

- AWS CLI configured with enough permissions to create ECS, ECR, ELB (ALB), and EC2 resources
- Docker installed locally
- Run from root of the repository

```bash
# Verify AWS CLI is configured
aws sts get-caller-identity

# Verify Docker is running
docker info
```

## Quick Start

```bash
# Deploy the server
./scripts/aws-deploy.sh deploy

# Check status and get the HTTPS URL
./scripts/aws-deploy.sh status

# View logs
./scripts/aws-deploy.sh logs

# Clean up all resources
./scripts/aws-deploy.sh cleanup
```

## Commands

| Command | Description |
|---------|-------------|
| `deploy` | Build Docker image, push to ECR, create ECS cluster/service, and set up an ALB |
| `deploy --skip-build` | Skip Docker build and reuse existing image |
| `status` | Show deployment status, running tasks, and HTTPS URL |
| `logs` | Tail CloudWatch logs (Ctrl+C to exit) |
| `cleanup` | Remove all AWS resources (ECR, ECS, API Gateway, security groups, logs) |

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `AWS_REGION` | `us-east-1` | AWS region for deployment |
| `ALB_LISTENER_PORT` | `80` | ALB listener port for HTTP traffic |
| `ACM_CERT_ARN` | empty | Optional ACM certificate ARN to enable HTTPS |

## What Gets Created

The deployment script creates:

- **ECR Repository** - Stores the Docker image
- **ECS Cluster** - Fargate cluster to run the container
- **ECS Service** - Manages the running task with public IP
- **ALB + Target Group** - Provides public endpoint for the HTTP API
- **Security Groups** - ALB ingress and server ingress rules
- **CloudWatch Log Group** - Stores container logs
- **IAM Role** - Task execution role for ECS

## Output

After deployment, you'll receive the ALB DNS name:

```
http://psm-alb-xxxxxxxx.us-east-1.elb.amazonaws.com
```

Test the deployment:

```bash
# Health check
curl http://psm-alb-xxxxxxxx.us-east-1.elb.amazonaws.com/health

# Get server public key
curl http://psm-alb-xxxxxxxx.us-east-1.elb.amazonaws.com/pubkey
```

