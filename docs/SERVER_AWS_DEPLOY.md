# Deploying PSM Server to AWS ECS

This guide walks through deploying the Private State Manager (PSM) server to AWS Elastic Container Service (ECS) using the provided Dockerfile.

## Table of Contents

- [Prerequisites](#prerequisites)
- [Step 1: Build and Push Docker Image](#step-1-build-and-push-docker-image)
- [Step 2: Create ECS Cluster](#step-2-create-ecs-cluster)
- [Step 3: Create Task Definition](#step-3-create-task-definition)
- [Step 4: Create ECS Service](#step-4-create-ecs-service)
- [Step 5: Verify Deployment](#step-5-verify-deployment)
- [Testing with curl](#testing-with-curl)
- [Troubleshooting](#troubleshooting)

---

## Prerequisites

- AWS CLI configured with enough permissions to create ECS resources
- Docker installed locally
- Run from root of the repository

```bash
# Verify AWS CLI is configured with admin user
aws sts get-caller-identity

# Verify Docker is running
docker info
```

---

## Step 1: Build and Push Docker Image

### Create ECR Repository

```bash
# Set variables
AWS_REGION=us-east-1
AWS_ACCOUNT_ID=$(aws sts get-caller-identity --query Account --output text)
ECR_REPO_NAME=psm-server

# Create ECR repository
aws ecr create-repository \
  --repository-name $ECR_REPO_NAME \
  --region $AWS_REGION

# Get login token
aws ecr get-login-password --region $AWS_REGION | \
  docker login --username AWS --password-stdin $AWS_ACCOUNT_ID.dkr.ecr.$AWS_REGION.amazonaws.com
```

### Build and Push Image

```bash
# Ensure variables are set (run these first if starting a new terminal)
export AWS_REGION=us-east-1
export AWS_ACCOUNT_ID=$(aws sts get-caller-identity --query Account --output text)
export ECR_REPO_NAME=psm-server

# Verify variables are set correctly
echo "Region: $AWS_REGION"
echo "Account: $AWS_ACCOUNT_ID"
echo "Repo: $ECR_REPO_NAME"

# Build for linux/amd64 (required for ECS)
docker build --platform linux/amd64 -t psm-server .

# Tag for ECR
docker tag psm-server:latest $AWS_ACCOUNT_ID.dkr.ecr.$AWS_REGION.amazonaws.com/psm-server:latest

# Push to ECR
docker push $AWS_ACCOUNT_ID.dkr.ecr.$AWS_REGION.amazonaws.com/psm-server:latest
```

Verify

```bash
# Verify ECR repository exists
aws ecr describe-repositories --repository-names psm-server --region $AWS_REGION
# Should show repository info with "repositoryUri"

# Verify image was pushed
aws ecr list-images --repository-name psm-server --region $AWS_REGION
# Should show at least one image with "imageTag": "latest"
```

---

## Step 2: Create ECS Cluster

```bash
# Create cluster
export CLUSTER_NAME=psm-cluster

aws ecs create-cluster \
  --cluster-name $CLUSTER_NAME \
  --capacity-providers FARGATE FARGATE_SPOT \
  --default-capacity-provider-strategy capacityProvider=FARGATE,weight=1
```

---

## Step 3: Create Task Definition

### Create IAM Role

```bash
# Create task execution role (ignore error if already exists)
aws iam create-role \
  --role-name ecsTaskExecutionRole \
  --assume-role-policy-document '{
    "Version": "2012-10-17",
    "Statement": [{
      "Effect": "Allow",
      "Principal": {"Service": "ecs-tasks.amazonaws.com"},
      "Action": "sts:AssumeRole"
    }]
  }'

aws iam attach-role-policy \
  --role-name ecsTaskExecutionRole \
  --policy-arn arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy
```

### Create CloudWatch Log Group

```bash
aws logs create-log-group --log-group-name /ecs/psm-server --region $AWS_REGION
```

### Generate and Register Task Definition

```bash
# Generate task-definition.json with your account ID
cat > task-definition.json << EOF
{
  "family": "psm-server",
  "networkMode": "awsvpc",
  "requiresCompatibilities": ["FARGATE"],
  "cpu": "512",
  "memory": "1024",
  "executionRoleArn": "arn:aws:iam::${AWS_ACCOUNT_ID}:role/ecsTaskExecutionRole",
  "containerDefinitions": [
    {
      "name": "psm-server",
      "image": "${AWS_ACCOUNT_ID}.dkr.ecr.${AWS_REGION}.amazonaws.com/psm-server:latest",
      "essential": true,
      "portMappings": [
        {"containerPort": 3000, "protocol": "tcp"},
        {"containerPort": 50051, "protocol": "tcp"}
      ],
      "environment": [
        {"name": "RUST_LOG", "value": "info"}
      ],
      "logConfiguration": {
        "logDriver": "awslogs",
        "options": {
          "awslogs-group": "/ecs/psm-server",
          "awslogs-region": "${AWS_REGION}",
          "awslogs-stream-prefix": "ecs"
        }
      }
    }
  ]
}
EOF

# Register the task definition
aws ecs register-task-definition --cli-input-json file://task-definition.json
```

> **Note:** This simple setup uses ephemeral storage. Data is lost when containers restart.
> For production with persistent storage, add EFS volumes to the task definition.

---

## Step 4: Create ECS Service

```bash
# Get default VPC and first subnet
export VPC_ID=$(aws ec2 describe-vpcs --filters "Name=is-default,Values=true" --query 'Vpcs[0].VpcId' --output text)
export SUBNET_ID=$(aws ec2 describe-subnets --filters "Name=vpc-id,Values=$VPC_ID" --query 'Subnets[0].SubnetId' --output text)

# Create security group
export SG_ID=$(aws ec2 create-security-group \
  --group-name psm-server-sg \
  --description "PSM server" \
  --vpc-id $VPC_ID \
  --query 'GroupId' --output text)

# Allow inbound traffic on ports 3000 (HTTP) and 50051 (gRPC)
aws ec2 authorize-security-group-ingress --group-id $SG_ID --protocol tcp --port 3000 --cidr 0.0.0.0/0
aws ec2 authorize-security-group-ingress --group-id $SG_ID --protocol tcp --port 50051 --cidr 0.0.0.0/0

# Create service
aws ecs create-service \
  --cluster $CLUSTER_NAME \
  --service-name psm-server \
  --task-definition psm-server \
  --desired-count 1 \
  --launch-type FARGATE \
  --platform-version LATEST \
  --network-configuration "awsvpcConfiguration={subnets=[$SUBNET_ID],securityGroups=[$SG_ID],assignPublicIp=ENABLED}"
```

---

## Step 5: Verify Deployment

```bash
# Check service status
aws ecs describe-services \
  --cluster $CLUSTER_NAME \
  --services psm-server \
  --query 'services[0].{status:status,runningCount:runningCount,desiredCount:desiredCount}'

# List running tasks
aws ecs list-tasks \
  --cluster $CLUSTER_NAME \
  --service-name psm-server
```

### Get the Public IP

```bash
# Get task ARN
export TASK_ARN=$(aws ecs list-tasks \
  --cluster $CLUSTER_NAME \
  --service-name psm-server \
  --query 'taskArns[0]' --output text)

# Get ENI ID
export ENI_ID=$(aws ecs describe-tasks \
  --cluster $CLUSTER_NAME \
  --tasks $TASK_ARN \
  --query 'tasks[0].attachments[0].details[?name==`networkInterfaceId`].value' --output text)

# Get public IP
export PSM_IP=$(aws ec2 describe-network-interfaces \
  --network-interface-ids $ENI_ID \
  --query 'NetworkInterfaces[0].Association.PublicIp' --output text)

echo "PSM Server IP: $PSM_IP"
```

---

## Testing with curl

Test the server endpoints. Use `$PSM_IP` (task public IP) or ALB DNS if you configured a load balancer.

### Health Check

```bash
curl -s http://$PSM_IP:3000/health
# Expected: {"status":"ok"}
```

### Get Server Public Key

```bash
curl -s http://$PSM_IP:3000/pubkey
# Expected: {"pubkey":"0x..."}
```

View logs in **AWS Console → CloudWatch → Log groups → /ecs/psm-server**

Or via CLI:
```bash
aws logs tail /ecs/psm-server --follow
```

---

### Cleanup

```bash
# 1. Delete service (scale down first)
aws ecs update-service --cluster $CLUSTER_NAME --service psm-server --desired-count 0
aws ecs delete-service --cluster $CLUSTER_NAME --service psm-server

# 2. Wait for tasks to stop (~30 seconds), check status:
aws ecs describe-services --cluster $CLUSTER_NAME --services psm-server --query 'services[0].status'
# Repeat until it shows "INACTIVE"

# 3. Delete cluster
aws ecs delete-cluster --cluster $CLUSTER_NAME

# 4. Delete security group (only works after tasks are fully stopped)
aws ec2 delete-security-group --group-id $SG_ID

# 5. Delete ECR repository
aws ecr delete-repository --repository-name psm-server --force

# 6. Delete CloudWatch log group
aws logs delete-log-group --log-group-name /ecs/psm-server --region $AWS_REGION
```

Verify cleanup is complete:

```bash
# All these should return empty or "not found" errors
aws ecs describe-clusters --clusters psm-cluster --query 'clusters[?status==`ACTIVE`]'
aws ec2 describe-security-groups --group-ids $SG_ID 2>&1 | grep -q "InvalidGroup" && echo "SG deleted"
aws ecr describe-repositories --repository-names psm-server 2>&1 | grep -q "RepositoryNotFoundException" && echo "ECR deleted"
```

---

## Production Considerations

1. **Load balancer**: Add ALB for stable DNS and health checks
2. **HTTPS**: Configure ACM certificate and HTTPS listener on ALB
3. **Persistent storage**: Add EFS volumes to task definition
4. **Monitoring**: Set up CloudWatch alarms for CPU, memory, and error rates
5. **Multi-AZ**: Deploy across multiple availability zones for high availability
