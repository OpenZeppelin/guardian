#!/bin/bash
set -e

# PSM Server AWS Deployment Script
# Usage: ./scripts/aws-deploy.sh [command] [options]
#
# Commands:
#   deploy   - Deploy PSM server behind an ALB
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
POSTGRES_SERVICE_NAME="psm-postgres"
POSTGRES_TASK_FAMILY="psm-postgres"
POSTGRES_DB="${POSTGRES_DB:-psm}"
POSTGRES_USER="${POSTGRES_USER:-psm}"
POSTGRES_PASSWORD="${POSTGRES_PASSWORD:-psm_dev_password}"
SD_NAMESPACE_NAME="psm.local"
SD_SERVICE_NAME="psm-postgres"
ALB_NAME="psm-alb"
ALB_SG_NAME="psm-alb-sg"
ALB_TG_NAME="psm-server-tg"
ALB_LISTENER_PORT="${ALB_LISTENER_PORT:-80}"
ACM_CERT_ARN="${ACM_CERT_ARN:-}"
LOG_GROUP_SERVER="/ecs/psm-server"
LOG_GROUP_POSTGRES="/ecs/psm-postgres"

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
  local service_name="${1:-$SERVICE_NAME}"
  log_info "Waiting for service to stabilize ($service_name)..."

  if aws ecs wait services-stable \
    --cluster $CLUSTER_NAME \
    --services $service_name \
    --region $AWS_REGION >/dev/null 2>&1; then
    log_info "Service is stable"
    return 0
  fi

  log_error "Service failed to stabilize"
  aws ecs describe-services \
    --cluster $CLUSTER_NAME \
    --services $service_name \
    --region $AWS_REGION \
    --query 'services[0].{status:status,desiredCount:desiredCount,runningCount:runningCount,pendingCount:pendingCount,deployments:deployments}' \
    --output json 2>/dev/null || true
  return 1
}

cleanup_old_task_definitions() {
  local family="$1"
  local keep_count="${2:-3}"

  log_info "Cleaning up old task definitions for $family (keeping last $keep_count)..."

  local revisions=$(aws ecs list-task-definitions \
    --family-prefix $family \
    --sort DESC \
    --region $AWS_REGION \
    --query 'taskDefinitionArns' --output json 2>/dev/null)

  local count=$(echo "$revisions" | jq length)
  if [ "$count" -le "$keep_count" ]; then
    log_info "No old task definitions to clean up"
    return 0
  fi

  echo "$revisions" | jq -r ".[$keep_count:][]" | while read -r arn; do
    local revision=$(echo "$arn" | grep -oE ':[0-9]+$' | tr -d ':')
    log_info "Deregistering $family:$revision..."
    aws ecs deregister-task-definition --task-definition "$arn" --region $AWS_REGION >/dev/null 2>&1 || true
    aws ecs delete-task-definitions --task-definitions "$arn" --region $AWS_REGION >/dev/null 2>&1 || true
  done
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
  docker build --platform linux/amd64 --no-cache -t psm-server .

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
  aws logs create-log-group --log-group-name $LOG_GROUP_SERVER --region $AWS_REGION 2>/dev/null || \
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
        {"name": "RUST_LOG", "value": "info"},
        {"name": "DATABASE_URL", "value": "postgres://${POSTGRES_USER}:${POSTGRES_PASSWORD}@${SD_SERVICE_NAME}.${SD_NAMESPACE_NAME}:5432/${POSTGRES_DB}"}
      ],
      "logConfiguration": {
        "logDriver": "awslogs",
        "options": {
          "awslogs-group": "${LOG_GROUP_SERVER}",
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

cmd_create_postgres_task_definition() {
  local AWS_ACCOUNT_ID=$(get_aws_account_id)

  log_info "Creating CloudWatch log group for Postgres..."
  aws logs create-log-group --log-group-name $LOG_GROUP_POSTGRES --region $AWS_REGION 2>/dev/null || \
    log_warn "Log group already exists"

  log_info "Registering Postgres task definition..."
  cat > /tmp/postgres-task-definition.json << EOF
{
  "family": "${POSTGRES_TASK_FAMILY}",
  "networkMode": "awsvpc",
  "requiresCompatibilities": ["FARGATE"],
  "cpu": "512",
  "memory": "1024",
  "executionRoleArn": "arn:aws:iam::${AWS_ACCOUNT_ID}:role/ecsTaskExecutionRole",
  "containerDefinitions": [
    {
      "name": "${POSTGRES_SERVICE_NAME}",
      "image": "postgres:16-alpine",
      "essential": true,
      "portMappings": [
        {"containerPort": 5432, "protocol": "tcp"}
      ],
      "environment": [
        {"name": "POSTGRES_USER", "value": "${POSTGRES_USER}"},
        {"name": "POSTGRES_PASSWORD", "value": "${POSTGRES_PASSWORD}"},
        {"name": "POSTGRES_DB", "value": "${POSTGRES_DB}"}
      ],
      "logConfiguration": {
        "logDriver": "awslogs",
        "options": {
          "awslogs-group": "${LOG_GROUP_POSTGRES}",
          "awslogs-region": "${AWS_REGION}",
          "awslogs-stream-prefix": "ecs"
        }
      }
    }
  ]
}
EOF

  aws ecs register-task-definition --cli-input-json file:///tmp/postgres-task-definition.json --region $AWS_REGION
  rm /tmp/postgres-task-definition.json
}

cmd_create_service_discovery() {
  local vpc_id="$1"

  local namespace_id=$(aws servicediscovery list-namespaces \
    --region $AWS_REGION \
    --query "Namespaces[?Name=='${SD_NAMESPACE_NAME}'].Id" --output text 2>/dev/null)

  if [ -z "$namespace_id" ] || [ "$namespace_id" == "None" ]; then
    log_info "Creating Cloud Map namespace (${SD_NAMESPACE_NAME})..." >&2
    local operation_id=$(aws servicediscovery create-private-dns-namespace \
      --name $SD_NAMESPACE_NAME \
      --vpc $vpc_id \
      --region $AWS_REGION \
      --query 'OperationId' --output text)

    local status="PENDING"
    local attempt=0
    while [ "$status" != "SUCCESS" ] && [ $attempt -lt 30 ]; do
      status=$(aws servicediscovery get-operation \
        --operation-id $operation_id \
        --region $AWS_REGION \
        --query 'Operation.Status' --output text 2>/dev/null)
      attempt=$((attempt + 1))
      sleep 2
    done

    namespace_id=$(aws servicediscovery list-namespaces \
      --region $AWS_REGION \
      --query "Namespaces[?Name=='${SD_NAMESPACE_NAME}'].Id" --output text 2>/dev/null)
  fi

  local service_arn=$(aws servicediscovery list-services \
    --region $AWS_REGION \
    --query "Services[?Name=='${SD_SERVICE_NAME}'].Arn" --output text 2>/dev/null)

  if [ -z "$service_arn" ] || [ "$service_arn" == "None" ]; then
    log_info "Creating Cloud Map service (${SD_SERVICE_NAME})..." >&2
    service_arn=$(aws servicediscovery create-service \
      --name $SD_SERVICE_NAME \
      --dns-config "NamespaceId=${namespace_id},DnsRecords=[{Type=A,TTL=10}]" \
      --health-check-custom-config FailureThreshold=1 \
      --region $AWS_REGION \
      --query 'Service.Arn' --output text)
  fi

  printf '%s' "$service_arn"
}

cmd_create_postgres_service() {
  local subnet_id="$1"
  local postgres_sg_id="$2"
  local sd_service_arn="$3"

  if [ -z "$subnet_id" ] || [ -z "$postgres_sg_id" ]; then
    log_error "Missing subnet or security group id for Postgres service"
    return 1
  fi
  if [[ "$postgres_sg_id" != sg-* ]]; then
    log_error "Invalid Postgres security group id: $postgres_sg_id"
    return 1
  fi

  local service_registries=""
  if [ -n "$sd_service_arn" ] && [ "$sd_service_arn" != "None" ]; then
    service_registries="--service-registries registryArn=$sd_service_arn"
  else
    log_warn "Cloud Map service not available; creating Postgres service without service discovery"
  fi

  local existing=$(aws ecs describe-services \
    --cluster $CLUSTER_NAME \
    --services $POSTGRES_SERVICE_NAME \
    --region $AWS_REGION \
    --query 'services[0].serviceName' --output text 2>/dev/null)

  if [ "$existing" != "$POSTGRES_SERVICE_NAME" ]; then
    log_info "Creating Postgres ECS service..."
    aws ecs create-service \
      --cluster $CLUSTER_NAME \
      --service-name $POSTGRES_SERVICE_NAME \
      --task-definition $POSTGRES_TASK_FAMILY \
      --desired-count 1 \
      --launch-type FARGATE \
      --platform-version LATEST \
      --region $AWS_REGION \
      $service_registries \
      --network-configuration "awsvpcConfiguration={subnets=[$subnet_id],securityGroups=[$postgres_sg_id],assignPublicIp=ENABLED}"
  else
    log_info "Postgres service already exists, updating..."
    aws ecs update-service \
      --cluster $CLUSTER_NAME \
      --service $POSTGRES_SERVICE_NAME \
      --task-definition $POSTGRES_TASK_FAMILY \
      --force-new-deployment \
      --region $AWS_REGION >/dev/null
  fi
}

cmd_create_alb() {
  local subnet_ids="$1"
  local alb_sg_id="$2"

  local alb_arn=$(aws elbv2 describe-load-balancers \
    --names $ALB_NAME \
    --region $AWS_REGION \
    --query 'LoadBalancers[0].LoadBalancerArn' --output text 2>/dev/null)

  if [ -z "$alb_arn" ] || [ "$alb_arn" == "None" ]; then
    log_info "Creating ALB..." >&2
    alb_arn=$(aws elbv2 create-load-balancer \
      --name $ALB_NAME \
      --subnets $subnet_ids \
      --security-groups $alb_sg_id \
      --scheme internet-facing \
      --type application \
      --ip-address-type ipv4 \
      --region $AWS_REGION \
      --query 'LoadBalancers[0].LoadBalancerArn' --output text)
  else
    log_info "ALB already exists" >&2
  fi

  printf '%s' "$alb_arn"
}

cmd_create_alb_target_group() {
  local vpc_id="$1"

  local tg_arn=$(aws elbv2 describe-target-groups \
    --names $ALB_TG_NAME \
    --region $AWS_REGION \
    --query 'TargetGroups[0].TargetGroupArn' --output text 2>/dev/null)

  if [ -z "$tg_arn" ] || [ "$tg_arn" == "None" ]; then
    log_info "Creating ALB target group..." >&2
    tg_arn=$(aws elbv2 create-target-group \
      --name $ALB_TG_NAME \
      --protocol HTTP \
      --port 3000 \
      --vpc-id $vpc_id \
      --target-type ip \
      --health-check-path / \
      --region $AWS_REGION \
      --query 'TargetGroups[0].TargetGroupArn' --output text)
  else
    log_info "ALB target group already exists" >&2
  fi

  printf '%s' "$tg_arn"
}

cmd_create_alb_listener() {
  local alb_arn="$1"
  local tg_arn="$2"

  if [ -n "$ACM_CERT_ARN" ]; then
    # HTTPS listener on 443
    local https_listener_arn=$(aws elbv2 describe-listeners \
      --load-balancer-arn $alb_arn \
      --region $AWS_REGION \
      --query "Listeners[?Port==\`443\`].ListenerArn" --output text 2>/dev/null)

    if [ -z "$https_listener_arn" ] || [ "$https_listener_arn" == "None" ]; then
      log_info "Creating ALB HTTPS listener on port 443..." >&2
      aws elbv2 create-listener \
        --load-balancer-arn $alb_arn \
        --protocol HTTPS \
        --port 443 \
        --certificates CertificateArn=$ACM_CERT_ARN \
        --ssl-policy ELBSecurityPolicy-2016-08 \
        --default-actions Type=forward,TargetGroupArn=$tg_arn \
        --region $AWS_REGION >/dev/null
    else
      log_info "Updating ALB HTTPS listener on port 443..." >&2
      aws elbv2 modify-listener \
        --listener-arn $https_listener_arn \
        --certificates CertificateArn=$ACM_CERT_ARN \
        --ssl-policy ELBSecurityPolicy-2016-08 \
        --default-actions Type=forward,TargetGroupArn=$tg_arn \
        --region $AWS_REGION >/dev/null
    fi

    # HTTP listener on 80 redirects to HTTPS
    local http_listener_arn=$(aws elbv2 describe-listeners \
      --load-balancer-arn $alb_arn \
      --region $AWS_REGION \
      --query "Listeners[?Port==\`80\`].ListenerArn" --output text 2>/dev/null)

    if [ -z "$http_listener_arn" ] || [ "$http_listener_arn" == "None" ]; then
      log_info "Creating ALB HTTP listener on port 80 (redirect to HTTPS)..." >&2
      aws elbv2 create-listener \
        --load-balancer-arn $alb_arn \
        --protocol HTTP \
        --port 80 \
        --default-actions Type=redirect,RedirectConfig='{Protocol=HTTPS,Port=443,StatusCode=HTTP_301}' \
        --region $AWS_REGION >/dev/null
    else
      log_info "Updating ALB HTTP listener on port 80 (redirect to HTTPS)..." >&2
      aws elbv2 modify-listener \
        --listener-arn $http_listener_arn \
        --default-actions Type=redirect,RedirectConfig='{Protocol=HTTPS,Port=443,StatusCode=HTTP_301}' \
        --region $AWS_REGION >/dev/null
    fi
  else
    local listener_port="$ALB_LISTENER_PORT"
    local listener_arn=$(aws elbv2 describe-listeners \
      --load-balancer-arn $alb_arn \
      --region $AWS_REGION \
      --query "Listeners[?Port==\`$listener_port\`].ListenerArn" --output text 2>/dev/null)

    if [ -z "$listener_arn" ] || [ "$listener_arn" == "None" ]; then
      log_info "Creating ALB listener on port $listener_port..." >&2
      aws elbv2 create-listener \
        --load-balancer-arn $alb_arn \
        --protocol HTTP \
        --port $listener_port \
        --default-actions Type=forward,TargetGroupArn=$tg_arn \
        --region $AWS_REGION >/dev/null
    else
      log_info "Updating ALB listener on port $listener_port..." >&2
      aws elbv2 modify-listener \
        --listener-arn $listener_arn \
        --default-actions Type=forward,TargetGroupArn=$tg_arn \
        --region $AWS_REGION >/dev/null
    fi
  fi
}

cmd_deploy() {
  log_info "Deploying PSM server behind an ALB..."

  if [ "$SKIP_BUILD" = false ]; then
    cmd_build_and_push
  else
    log_info "Skipping Docker build (--skip-build)"
  fi
  cmd_create_cluster
  cmd_create_task_definition

  local VPC_ID=$(aws ec2 describe-vpcs --filters "Name=is-default,Values=true" \
    --query 'Vpcs[0].VpcId' --output text --region $AWS_REGION)
  local SUBNET_IDS=$(aws ec2 describe-subnets --filters "Name=vpc-id,Values=$VPC_ID" \
    --query 'Subnets[].SubnetId' --output text --region $AWS_REGION)
  local SUBNET_ID=$(echo "$SUBNET_IDS" | awk '{print $1}')

  log_info "Creating security group for server..."
  local SG_ID
  SG_ID=$(aws ec2 create-security-group \
    --group-name psm-server-sg \
    --description "PSM server" \
    --vpc-id $VPC_ID \
    --region $AWS_REGION \
    --query 'GroupId' --output text 2>/dev/null) || \
    SG_ID=$(aws ec2 describe-security-groups --region $AWS_REGION \
      --filters "Name=group-name,Values=psm-server-sg" \
      --query 'SecurityGroups[0].GroupId' --output text)

  log_info "Creating security group for ALB..."
  local ALB_SG_ID
  ALB_SG_ID=$(aws ec2 create-security-group \
    --group-name $ALB_SG_NAME \
    --description "PSM ALB" \
    --vpc-id $VPC_ID \
    --region $AWS_REGION \
    --query 'GroupId' --output text 2>/dev/null) || \
    ALB_SG_ID=$(aws ec2 describe-security-groups --region $AWS_REGION \
      --filters "Name=group-name,Values=$ALB_SG_NAME" \
      --query 'SecurityGroups[0].GroupId' --output text)

  log_info "Creating security group for Postgres..."
  local PG_SG_ID
  PG_SG_ID=$(aws ec2 create-security-group \
    --group-name psm-postgres-sg \
    --description "PSM postgres" \
    --vpc-id $VPC_ID \
    --region $AWS_REGION \
    --query 'GroupId' --output text 2>/dev/null) || \
    PG_SG_ID=$(aws ec2 describe-security-groups --region $AWS_REGION \
      --filters "Name=group-name,Values=psm-postgres-sg" \
      --query 'SecurityGroups[0].GroupId' --output text)

  # Allow ALB ingress
  aws ec2 authorize-security-group-ingress --group-id $ALB_SG_ID --protocol tcp --port 80 --cidr 0.0.0.0/0 --region $AWS_REGION 2>/dev/null || true
  aws ec2 authorize-security-group-ingress --group-id $ALB_SG_ID --protocol tcp --port 443 --cidr 0.0.0.0/0 --region $AWS_REGION 2>/dev/null || true

  # Allow ALB to reach the server HTTP port
  aws ec2 authorize-security-group-ingress --group-id $SG_ID --protocol tcp --port 3000 --source-group $ALB_SG_ID --region $AWS_REGION 2>/dev/null || true
  aws ec2 authorize-security-group-ingress --group-id $SG_ID --protocol tcp --port 50051 --cidr 0.0.0.0/0 --region $AWS_REGION 2>/dev/null || true

  # Allow server to access Postgres
  aws ec2 authorize-security-group-ingress --group-id $PG_SG_ID --protocol tcp --port 5432 --source-group $SG_ID --region $AWS_REGION 2>/dev/null || true

  local ALB_ARN=$(cmd_create_alb "$SUBNET_IDS" "$ALB_SG_ID")
  local TG_ARN=$(cmd_create_alb_target_group "$VPC_ID")
  cmd_create_alb_listener "$ALB_ARN" "$TG_ARN"

  cmd_create_postgres_task_definition
  local SD_SERVICE_ARN=$(cmd_create_service_discovery $VPC_ID)
  cmd_create_postgres_service $SUBNET_ID $PG_SG_ID $SD_SERVICE_ARN
  wait_for_service $POSTGRES_SERVICE_NAME

  log_info "Creating ECS service..."
  if aws ecs create-service \
    --cluster $CLUSTER_NAME \
    --service-name $SERVICE_NAME \
    --task-definition psm-server \
    --desired-count 1 \
    --launch-type FARGATE \
    --platform-version LATEST \
    --region $AWS_REGION \
    --health-check-grace-period-seconds 30 \
    --load-balancers "targetGroupArn=$TG_ARN,containerName=psm-server,containerPort=3000" \
    --network-configuration "awsvpcConfiguration={subnets=[$SUBNET_ID],securityGroups=[$SG_ID],assignPublicIp=ENABLED}" 2>/dev/null; then
    log_info "Service created"
  else
    log_info "Service already exists, updating to latest task definition..."
    aws ecs update-service \
      --cluster $CLUSTER_NAME \
      --service $SERVICE_NAME \
      --task-definition psm-server \
      --load-balancers "targetGroupArn=$TG_ARN,containerName=psm-server,containerPort=3000" \
      --force-new-deployment \
      --region $AWS_REGION >/dev/null
  fi

  wait_for_service $SERVICE_NAME

  local ALB_DNS=$(aws elbv2 describe-load-balancers \
    --load-balancer-arns $ALB_ARN \
    --region $AWS_REGION \
    --query 'LoadBalancers[0].DNSName' --output text)

  # Clean up old task definitions
  cleanup_old_task_definitions "psm-server"
  cleanup_old_task_definitions "psm-postgres"

  echo ""
  log_info "Deployment complete!"
  echo ""
  local scheme="http"
  if [ -n "$ACM_CERT_ARN" ]; then
    scheme="https"
  fi
  echo "  URL: ${scheme}://$ALB_DNS"
  echo ""
  echo "  Health check: curl ${scheme}://$ALB_DNS/health"
  echo "  Public key:   curl ${scheme}://$ALB_DNS/pubkey"
  echo ""
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
  echo "=== ALB ==="
  local ALB_ARN=$(aws elbv2 describe-load-balancers \
    --names $ALB_NAME \
    --region $AWS_REGION \
    --query 'LoadBalancers[0].LoadBalancerArn' --output text 2>/dev/null)

  if [ -n "$ALB_ARN" ] && [ "$ALB_ARN" != "None" ]; then
    local ALB_DNS=$(aws elbv2 describe-load-balancers \
      --load-balancer-arns $ALB_ARN \
      --region $AWS_REGION \
      --query 'LoadBalancers[0].DNSName' --output text 2>/dev/null)
    echo "ALB DNS: $ALB_DNS"
  else
    echo "No ALB configured"
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

  log_info "Scaling down ECS services..."
  aws ecs update-service --cluster $CLUSTER_NAME --service $POSTGRES_SERVICE_NAME --desired-count 0 --region $AWS_REGION 2>/dev/null || true
  aws ecs update-service --cluster $CLUSTER_NAME --service $SERVICE_NAME --desired-count 0 --region $AWS_REGION 2>/dev/null || true

  log_info "Deleting ECS services..."
  aws ecs delete-service --cluster $CLUSTER_NAME --service $POSTGRES_SERVICE_NAME --region $AWS_REGION 2>/dev/null || true
  aws ecs delete-service --cluster $CLUSTER_NAME --service $SERVICE_NAME --region $AWS_REGION 2>/dev/null || true

  log_info "Waiting for service to stop..."
  sleep 30

  # Delete ALB resources
  local ALB_ARN=$(aws elbv2 describe-load-balancers \
    --names $ALB_NAME \
    --region $AWS_REGION \
    --query 'LoadBalancers[0].LoadBalancerArn' --output text 2>/dev/null)
  if [ -n "$ALB_ARN" ] && [ "$ALB_ARN" != "None" ]; then
    log_info "Deleting ALB listeners..."
    local LISTENER_ARNS=$(aws elbv2 describe-listeners \
      --load-balancer-arn $ALB_ARN \
      --region $AWS_REGION \
      --query 'Listeners[].ListenerArn' --output text 2>/dev/null)
    for listener_arn in $LISTENER_ARNS; do
      aws elbv2 delete-listener --listener-arn $listener_arn --region $AWS_REGION 2>/dev/null || true
    done

    log_info "Deleting ALB..."
    aws elbv2 delete-load-balancer --load-balancer-arn $ALB_ARN --region $AWS_REGION 2>/dev/null || true
    aws elbv2 wait load-balancers-deleted --load-balancer-arns $ALB_ARN --region $AWS_REGION 2>/dev/null || true
  fi

  local TG_ARN=$(aws elbv2 describe-target-groups \
    --names $ALB_TG_NAME \
    --region $AWS_REGION \
    --query 'TargetGroups[0].TargetGroupArn' --output text 2>/dev/null)
  if [ -n "$TG_ARN" ] && [ "$TG_ARN" != "None" ]; then
    log_info "Deleting ALB target group..."
    aws elbv2 delete-target-group --target-group-arn $TG_ARN --region $AWS_REGION 2>/dev/null || true
  fi

  # Delete security groups
  local pg_sg_id=$(aws ec2 describe-security-groups --region $AWS_REGION \
    --filters "Name=group-name,Values=psm-postgres-sg" \
    --query 'SecurityGroups[0].GroupId' --output text 2>/dev/null)
  if [ -n "$pg_sg_id" ] && [ "$pg_sg_id" != "None" ]; then
    aws ec2 delete-security-group --group-id $pg_sg_id --region $AWS_REGION 2>/dev/null || true
  fi

  local sg_id=$(aws ec2 describe-security-groups --region $AWS_REGION \
    --filters "Name=group-name,Values=psm-server-sg" \
    --query 'SecurityGroups[0].GroupId' --output text 2>/dev/null)
  if [ -n "$sg_id" ] && [ "$sg_id" != "None" ]; then
    aws ec2 delete-security-group --group-id $sg_id --region $AWS_REGION 2>/dev/null || true
  fi

  local alb_sg_id=$(aws ec2 describe-security-groups --region $AWS_REGION \
    --filters "Name=group-name,Values=$ALB_SG_NAME" \
    --query 'SecurityGroups[0].GroupId' --output text 2>/dev/null)
  if [ -n "$alb_sg_id" ] && [ "$alb_sg_id" != "None" ]; then
    aws ec2 delete-security-group --group-id $alb_sg_id --region $AWS_REGION 2>/dev/null || true
  fi

  log_info "Deleting ECS cluster..."
  aws ecs delete-cluster --cluster $CLUSTER_NAME --region $AWS_REGION 2>/dev/null || true

  log_info "Deleting ECR repository..."
  aws ecr delete-repository --repository-name $ECR_REPO_NAME --force --region $AWS_REGION 2>/dev/null || true

  log_info "Deleting CloudWatch log groups..."
  aws logs delete-log-group --log-group-name $LOG_GROUP_SERVER --region $AWS_REGION 2>/dev/null || true
  aws logs delete-log-group --log-group-name $LOG_GROUP_POSTGRES --region $AWS_REGION 2>/dev/null || true

  log_info "Deleting Cloud Map service and namespace..."
  local sd_service_id=$(aws servicediscovery list-services \
    --region $AWS_REGION \
    --query "Services[?Name=='${SD_SERVICE_NAME}'].Id" --output text 2>/dev/null)
  if [ -n "$sd_service_id" ] && [ "$sd_service_id" != "None" ]; then
    aws servicediscovery delete-service --id $sd_service_id --region $AWS_REGION 2>/dev/null || true
  fi
  local namespace_id=$(aws servicediscovery list-namespaces \
    --region $AWS_REGION \
    --query "Namespaces[?Name=='${SD_NAMESPACE_NAME}'].Id" --output text 2>/dev/null)
  if [ -n "$namespace_id" ] && [ "$namespace_id" != "None" ]; then
    aws servicediscovery delete-namespace --id $namespace_id --region $AWS_REGION 2>/dev/null || true
  fi

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
    echo "  deploy   Deploy PSM server behind an ALB"
    echo "  status   Show deployment status and URLs"
    echo "  logs     Tail CloudWatch logs"
    echo "  cleanup  Remove all AWS resources"
    echo ""
    echo "Options:"
    echo "  --skip-build  Skip Docker build and push (use existing image)"
    echo ""
    echo "Examples:"
    echo "  ./scripts/aws-deploy.sh deploy"
    echo "  ./scripts/aws-deploy.sh deploy --skip-build"
    echo "  ./scripts/aws-deploy.sh status"
    echo "  ./scripts/aws-deploy.sh cleanup"
    ;;
esac
