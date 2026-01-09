#!/bin/bash
set -e

# PSM Server AWS Deployment Script
# Usage: ./scripts/aws-deploy.sh [command] [options]
#
# Commands:
#   deploy   - Deploy PSM server with HTTPS via API Gateway
#   status   - Show deployment status
#   logs     - Tail CloudWatch logs
#   cleanup  - Remove all AWS resources
#
# Options:
#   --skip-build - Skip Docker build and push (use existing image)
#
# Optional environment variables:
#   AWS_REGION  - AWS region (default: us-east-1)

AWS_REGION="${AWS_REGION:-us-east-1}"
SKIP_BUILD=false
CLUSTER_NAME="psm-cluster"
ECR_REPO_NAME="psm-server"
SERVICE_NAME="psm-server"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log_info() { echo -e "${GREEN}[INFO]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

get_aws_account_id() {
  aws sts get-caller-identity --query Account --output text
}

wait_for_service() {
  log_info "Waiting for service to stabilize..."
  local max_attempts=30
  local attempt=0
  while [ $attempt -lt $max_attempts ]; do
    local running=$(aws ecs describe-services \
      --cluster $CLUSTER_NAME \
      --services $SERVICE_NAME \
      --region $AWS_REGION \
      --query 'services[0].runningCount' --output text 2>/dev/null)
    if [ "$running" == "1" ]; then
      log_info "Service is running"
      return 0
    fi
    attempt=$((attempt + 1))
    echo -n "."
    sleep 10
  done
  log_error "Service failed to stabilize"
  return 1
}

cmd_build_and_push() {
  local AWS_ACCOUNT_ID=$(get_aws_account_id)

  log_info "Creating ECR repository..."
  aws ecr create-repository \
    --repository-name $ECR_REPO_NAME \
    --region $AWS_REGION 2>/dev/null || log_warn "ECR repository already exists"

  log_info "Logging into ECR..."
  aws ecr get-login-password --region $AWS_REGION | \
    docker login --username AWS --password-stdin $AWS_ACCOUNT_ID.dkr.ecr.$AWS_REGION.amazonaws.com

  log_info "Building Docker image..."
  docker build --platform linux/amd64 -t psm-server .

  log_info "Tagging and pushing to ECR..."
  docker tag psm-server:latest $AWS_ACCOUNT_ID.dkr.ecr.$AWS_REGION.amazonaws.com/psm-server:latest
  docker push $AWS_ACCOUNT_ID.dkr.ecr.$AWS_REGION.amazonaws.com/psm-server:latest

  log_info "Image pushed successfully"
}

cmd_create_cluster() {
  log_info "Creating ECS cluster..."
  aws ecs create-cluster \
    --cluster-name $CLUSTER_NAME \
    --region $AWS_REGION \
    --capacity-providers FARGATE FARGATE_SPOT \
    --default-capacity-provider-strategy capacityProvider=FARGATE,weight=1 2>/dev/null || \
    log_warn "Cluster already exists"
}

cmd_create_task_definition() {
  local AWS_ACCOUNT_ID=$(get_aws_account_id)

  log_info "Creating IAM role..."
  aws iam create-role \
    --role-name ecsTaskExecutionRole \
    --assume-role-policy-document '{
      "Version": "2012-10-17",
      "Statement": [{
        "Effect": "Allow",
        "Principal": {"Service": "ecs-tasks.amazonaws.com"},
        "Action": "sts:AssumeRole"
      }]
    }' 2>/dev/null || log_warn "IAM role already exists"

  aws iam attach-role-policy \
    --role-name ecsTaskExecutionRole \
    --policy-arn arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy 2>/dev/null || true

  log_info "Creating CloudWatch log group..."
  aws logs create-log-group --log-group-name /ecs/psm-server --region $AWS_REGION 2>/dev/null || \
    log_warn "Log group already exists"

  log_info "Registering task definition..."
  cat > /tmp/task-definition.json << EOF
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

  aws ecs register-task-definition --cli-input-json file:///tmp/task-definition.json --region $AWS_REGION
  rm /tmp/task-definition.json
}

cmd_deploy() {
  log_info "Deploying PSM server with HTTPS via API Gateway..."

  if [ "$SKIP_BUILD" = false ]; then
    cmd_build_and_push
  else
    log_info "Skipping Docker build (--skip-build)"
  fi
  cmd_create_cluster
  cmd_create_task_definition

  local VPC_ID=$(aws ec2 describe-vpcs --filters "Name=is-default,Values=true" \
    --query 'Vpcs[0].VpcId' --output text --region $AWS_REGION)
  local SUBNET_ID=$(aws ec2 describe-subnets --filters "Name=vpc-id,Values=$VPC_ID" \
    --query 'Subnets[0].SubnetId' --output text --region $AWS_REGION)

  log_info "Creating security group..."
  local SG_ID=$(aws ec2 create-security-group \
    --group-name psm-server-sg \
    --description "PSM server" \
    --vpc-id $VPC_ID \
    --region $AWS_REGION \
    --query 'GroupId' --output text 2>/dev/null) || \
    SG_ID=$(aws ec2 describe-security-groups --region $AWS_REGION \
      --filters "Name=group-name,Values=psm-server-sg" \
      --query 'SecurityGroups[0].GroupId' --output text)

  # Allow traffic from anywhere (API Gateway uses public IPs)
  aws ec2 authorize-security-group-ingress --group-id $SG_ID --protocol tcp --port 3000 --cidr 0.0.0.0/0 --region $AWS_REGION 2>/dev/null || true
  aws ec2 authorize-security-group-ingress --group-id $SG_ID --protocol tcp --port 50051 --cidr 0.0.0.0/0 --region $AWS_REGION 2>/dev/null || true

  log_info "Creating ECS service..."
  aws ecs create-service \
    --cluster $CLUSTER_NAME \
    --service-name $SERVICE_NAME \
    --task-definition psm-server \
    --desired-count 1 \
    --launch-type FARGATE \
    --platform-version LATEST \
    --region $AWS_REGION \
    --network-configuration "awsvpcConfiguration={subnets=[$SUBNET_ID],securityGroups=[$SG_ID],assignPublicIp=ENABLED}" 2>/dev/null || \
    log_warn "Service already exists, updating..."

  wait_for_service

  # Get the task's public IP
  local TASK_ARN=$(aws ecs list-tasks \
    --cluster $CLUSTER_NAME \
    --service-name $SERVICE_NAME \
    --region $AWS_REGION \
    --query 'taskArns[0]' --output text)

  local ENI_ID=$(aws ecs describe-tasks \
    --cluster $CLUSTER_NAME \
    --tasks $TASK_ARN \
    --region $AWS_REGION \
    --query 'tasks[0].attachments[0].details[?name==`networkInterfaceId`].value' --output text)

  local PSM_IP=$(aws ec2 describe-network-interfaces \
    --network-interface-ids $ENI_ID \
    --region $AWS_REGION \
    --query 'NetworkInterfaces[0].Association.PublicIp' --output text)

  log_info "ECS task IP: $PSM_IP"

  # Create API Gateway HTTP API
  log_info "Creating API Gateway HTTP API..."

  # Check for existing API
  local API_ID=$(aws apigatewayv2 get-apis \
    --region $AWS_REGION \
    --query "Items[?Name=='psm-server-api'].ApiId" --output text 2>/dev/null)

  if [ -n "$API_ID" ] && [ "$API_ID" != "None" ]; then
    log_info "Found existing API Gateway: $API_ID"
    # Update the integration with new IP
    local INTEGRATION_ID=$(aws apigatewayv2 get-integrations \
      --api-id $API_ID \
      --region $AWS_REGION \
      --query "Items[0].IntegrationId" --output text 2>/dev/null)

    if [ -n "$INTEGRATION_ID" ] && [ "$INTEGRATION_ID" != "None" ]; then
      log_info "Updating integration with new IP..."
      aws apigatewayv2 update-integration \
        --api-id $API_ID \
        --integration-id $INTEGRATION_ID \
        --integration-uri "http://$PSM_IP:3000/{proxy}" \
        --region $AWS_REGION >/dev/null
    fi
  else
    # Create new API
    API_ID=$(aws apigatewayv2 create-api \
      --name psm-server-api \
      --protocol-type HTTP \
      --region $AWS_REGION \
      --query 'ApiId' --output text)

    log_info "Created API Gateway: $API_ID"

    # Create HTTP proxy integration
    local INTEGRATION_ID=$(aws apigatewayv2 create-integration \
      --api-id $API_ID \
      --integration-type HTTP_PROXY \
      --integration-method ANY \
      --integration-uri "http://$PSM_IP:3000/{proxy}" \
      --payload-format-version "1.0" \
      --region $AWS_REGION \
      --query 'IntegrationId' --output text)

    # Create catch-all route
    aws apigatewayv2 create-route \
      --api-id $API_ID \
      --route-key 'ANY /{proxy+}' \
      --target "integrations/$INTEGRATION_ID" \
      --region $AWS_REGION >/dev/null

    # Create root route for /health etc
    aws apigatewayv2 create-route \
      --api-id $API_ID \
      --route-key 'ANY /' \
      --target "integrations/$INTEGRATION_ID" \
      --region $AWS_REGION >/dev/null 2>/dev/null || true

    # Create default stage with auto-deploy
    aws apigatewayv2 create-stage \
      --api-id $API_ID \
      --stage-name '$default' \
      --auto-deploy \
      --region $AWS_REGION >/dev/null
  fi

  # Get the API endpoint
  local API_ENDPOINT=$(aws apigatewayv2 get-api \
    --api-id $API_ID \
    --region $AWS_REGION \
    --query 'ApiEndpoint' --output text)

  echo ""
  log_info "Deployment complete!"
  echo ""
  echo "  HTTPS URL: $API_ENDPOINT"
  echo ""
  echo "  Health check: curl $API_ENDPOINT/health"
  echo "  Public key:   curl $API_ENDPOINT/pubkey"
  echo ""
  log_warn "Note: If you redeploy and the ECS task gets a new IP, run 'deploy --skip-build' to update the API Gateway"
}

cmd_status() {
  log_info "Checking deployment status..."

  echo ""
  echo "=== ECS Service ==="
  aws ecs describe-services \
    --cluster $CLUSTER_NAME \
    --services $SERVICE_NAME \
    --region $AWS_REGION \
    --query 'services[0].{status:status,runningCount:runningCount,desiredCount:desiredCount,taskDefinition:taskDefinition}' 2>/dev/null || \
    echo "Service not found"

  echo ""
  echo "=== Running Tasks ==="
  local TASK_ARN=$(aws ecs list-tasks \
    --cluster $CLUSTER_NAME \
    --service-name $SERVICE_NAME \
    --region $AWS_REGION \
    --query 'taskArns[0]' --output text 2>/dev/null)

  if [ -n "$TASK_ARN" ] && [ "$TASK_ARN" != "None" ]; then
    local ENI_ID=$(aws ecs describe-tasks \
      --cluster $CLUSTER_NAME \
      --tasks $TASK_ARN \
      --region $AWS_REGION \
      --query 'tasks[0].attachments[0].details[?name==`networkInterfaceId`].value' --output text 2>/dev/null)

    if [ -n "$ENI_ID" ] && [ "$ENI_ID" != "None" ]; then
      local PSM_IP=$(aws ec2 describe-network-interfaces \
        --network-interface-ids $ENI_ID \
        --region $AWS_REGION \
        --query 'NetworkInterfaces[0].Association.PublicIp' --output text 2>/dev/null)
      echo "Task Public IP: $PSM_IP"
    fi
  else
    echo "No running tasks"
  fi

  echo ""
  echo "=== API Gateway ==="
  local API_ID=$(aws apigatewayv2 get-apis \
    --region $AWS_REGION \
    --query "Items[?Name=='psm-server-api'].ApiId" --output text 2>/dev/null)

  if [ -n "$API_ID" ] && [ "$API_ID" != "None" ]; then
    local API_ENDPOINT=$(aws apigatewayv2 get-api \
      --api-id $API_ID \
      --region $AWS_REGION \
      --query 'ApiEndpoint' --output text)
    echo "API Gateway ID: $API_ID"
    echo "HTTPS URL: $API_ENDPOINT"
  else
    echo "No API Gateway configured"
  fi
}

cmd_logs() {
  log_info "Tailing CloudWatch logs (Ctrl+C to exit)..."
  aws logs tail /ecs/psm-server --follow --region $AWS_REGION
}

cmd_cleanup() {
  log_warn "This will delete ALL PSM server AWS resources"
  read -p "Are you sure? (yes/no): " confirm
  if [ "$confirm" != "yes" ]; then
    echo "Aborted"
    exit 0
  fi

  log_info "Scaling down ECS service..."
  aws ecs update-service --cluster $CLUSTER_NAME --service $SERVICE_NAME --desired-count 0 --region $AWS_REGION 2>/dev/null || true

  log_info "Deleting ECS service..."
  aws ecs delete-service --cluster $CLUSTER_NAME --service $SERVICE_NAME --region $AWS_REGION 2>/dev/null || true

  log_info "Waiting for service to stop..."
  sleep 30

  # Delete API Gateway
  local API_ID=$(aws apigatewayv2 get-apis \
    --region $AWS_REGION \
    --query "Items[?Name=='psm-server-api'].ApiId" --output text 2>/dev/null)
  if [ -n "$API_ID" ] && [ "$API_ID" != "None" ]; then
    log_info "Deleting API Gateway..."
    aws apigatewayv2 delete-api --api-id $API_ID --region $AWS_REGION 2>/dev/null || true
  fi

  # Delete security group
  local sg_id=$(aws ec2 describe-security-groups --region $AWS_REGION \
    --filters "Name=group-name,Values=psm-server-sg" \
    --query 'SecurityGroups[0].GroupId' --output text 2>/dev/null)
  if [ -n "$sg_id" ] && [ "$sg_id" != "None" ]; then
    log_info "Deleting security group..."
    aws ec2 delete-security-group --group-id $sg_id --region $AWS_REGION 2>/dev/null || true
  fi

  log_info "Deleting ECS cluster..."
  aws ecs delete-cluster --cluster $CLUSTER_NAME --region $AWS_REGION 2>/dev/null || true

  log_info "Deleting ECR repository..."
  aws ecr delete-repository --repository-name $ECR_REPO_NAME --force --region $AWS_REGION 2>/dev/null || true

  log_info "Deleting CloudWatch log group..."
  aws logs delete-log-group --log-group-name /ecs/psm-server --region $AWS_REGION 2>/dev/null || true

  log_info "Cleanup complete!"
}

# Parse arguments
COMMAND=""
for arg in "$@"; do
  case "$arg" in
    --skip-build)
      SKIP_BUILD=true
      ;;
    *)
      if [ -z "$COMMAND" ]; then
        COMMAND="$arg"
      fi
      ;;
  esac
done

# Main
case "${COMMAND:-}" in
  deploy)
    cmd_deploy
    ;;
  status)
    cmd_status
    ;;
  logs)
    cmd_logs
    ;;
  cleanup)
    cmd_cleanup
    ;;
  *)
    echo "PSM Server AWS Deployment Script"
    echo ""
    echo "Usage: $0 <command> [options]"
    echo ""
    echo "Commands:"
    echo "  deploy   Deploy PSM server with HTTPS (auto-generated URL via API Gateway)"
    echo "  status   Show deployment status and URLs"
    echo "  logs     Tail CloudWatch logs"
    echo "  cleanup  Remove all AWS resources"
    echo ""
    echo "Options:"
    echo "  --skip-build  Skip Docker build and push (use existing image)"
    echo ""
    echo "Examples:"
    echo "  ./scripts/aws-deploy.sh deploy"
    echo "  ./scripts/aws-deploy.sh deploy --skip-build  # Update API Gateway with new IP"
    echo "  ./scripts/aws-deploy.sh status"
    echo "  ./scripts/aws-deploy.sh cleanup"
    ;;
esac
