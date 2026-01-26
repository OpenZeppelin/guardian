#!/bin/bash
set -e

# PSM Server AWS Deployment Script
# Usage: ./scripts/aws-deploy.sh [command] [options]
#
# Commands:
#   deploy   - Build/push image and run Terraform apply
#   status   - Show deployment status
#   logs     - Tail CloudWatch logs
#   cleanup  - Remove all AWS resources
#
# Options:
#   --skip-build - Skip Docker build and push (use existing image)
#
# Optional environment variables:
#   AWS_REGION       - AWS region (default: us-east-1)

AWS_REGION="${AWS_REGION:-us-east-1}"
SKIP_BUILD=false
ECR_REPO_NAME="psm-server"
DOMAIN_NAME="openzeppelin.com"
SUBDOMAIN="psm"
ROUTE53_ZONE_ID=""

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

resolve_route53_zone() {
  if [ -z "$DOMAIN_NAME" ] || [ -n "$ROUTE53_ZONE_ID" ]; then
    return 0
  fi

  local hosted_zone_id
  hosted_zone_id=$(aws route53 list-hosted-zones-by-name \
    --dns-name "${DOMAIN_NAME}." \
    --region "$AWS_REGION" \
    --query "HostedZones[?Name=='${DOMAIN_NAME}.']|[0].Id" --output text 2>/dev/null || true)

  if [ -n "$hosted_zone_id" ] && [ "$hosted_zone_id" != "None" ]; then
    ROUTE53_ZONE_ID="${hosted_zone_id##*/}"
    log_info "Using existing Route 53 hosted zone ${ROUTE53_ZONE_ID}"
    return 0
  fi
  log_error "No Route 53 hosted zone found for ${DOMAIN_NAME}"
  return 1
}

tf_state_has() {
  local tf_dir="$1"
  local address="$2"
  terraform -chdir="$tf_dir" state list 2>/dev/null | grep -q "^${address}$"
}

tf_import_if_exists() {
  local tf_dir="$1"
  local address="$2"
  local resource_id="$3"

  if [ -z "$resource_id" ] || [ "$resource_id" = "None" ]; then
    return 0
  fi

  if tf_state_has "$tf_dir" "$address"; then
    return 0
  fi

  log_info "Importing existing resource: $address"
  if ! terraform -chdir="$tf_dir" import -input=false "$address" "$resource_id"; then
    log_error "Failed to import $address"
    return 1
  fi
}

cmd_import_existing_resources() {
  local tf_dir="$1"
  local image_uri="$2"
  local aws_region="$3"
  local domain_name="$4"
  local subdomain="$5"
  local route53_zone_id="$6"

  export TF_VAR_server_image_uri="$image_uri"
  export TF_VAR_aws_region="$aws_region"
  export TF_VAR_domain_name="$domain_name"
  export TF_VAR_subdomain="$subdomain"
  export TF_VAR_route53_zone_id="$route53_zone_id"

  local failed=0
  local cluster_name="${TF_CLUSTER_NAME:-psm-cluster}"
  local alb_name="${TF_ALB_NAME:-psm-alb}"
  local target_group_name="${TF_TG_NAME:-psm-server-tg}"
  local alb_sg_name="${TF_ALB_SG_NAME:-psm-alb-sg}"
  local server_sg_name="${TF_SERVER_SG_NAME:-psm-server-sg}"
  local postgres_sg_name="${TF_POSTGRES_SG_NAME:-psm-postgres-sg}"
  local namespace_name="${TF_SD_NAMESPACE_NAME:-psm.local}"
  local sd_service_name="${TF_SD_SERVICE_NAME:-psm-postgres}"
  local server_service_name="${TF_SERVER_SERVICE_NAME:-psm-server}"
  local postgres_service_name="${TF_POSTGRES_SERVICE_NAME:-psm-postgres}"
  local log_group_server="/ecs/psm-server"
  local log_group_postgres="/ecs/psm-postgres"
  local log_group_cluster="/aws/ecs/${cluster_name}/cluster"

  local cluster_status
  cluster_status=$(aws ecs describe-clusters \
    --clusters "$cluster_name" \
    --region "$AWS_REGION" \
    --query 'clusters[0].status' --output text 2>/dev/null || true)
  if [ -n "$cluster_status" ] && [ "$cluster_status" != "None" ]; then
    tf_import_if_exists "$tf_dir" "aws_ecs_cluster.main" "$cluster_name" || failed=1
    tf_import_if_exists "$tf_dir" "aws_ecs_cluster_capacity_providers.main" "$cluster_name" || failed=1
  fi

  local role_name="psm-ecs-task-execution"
  local role_arn
  role_arn=$(aws iam get-role --role-name "$role_name" --query 'Role.Arn' --output text 2>/dev/null || true)
  tf_import_if_exists "$tf_dir" "aws_iam_role.ecs_task_execution" "$role_name" || failed=1

  local alb_arn
  alb_arn=$(aws elbv2 describe-load-balancers \
    --names "$alb_name" \
    --region "$AWS_REGION" \
    --query 'LoadBalancers[0].LoadBalancerArn' --output text 2>/dev/null || true)
  tf_import_if_exists "$tf_dir" "aws_lb.main" "$alb_arn" || failed=1

  local tg_arn
  tg_arn=$(aws elbv2 describe-target-groups \
    --names "$target_group_name" \
    --region "$AWS_REGION" \
    --query 'TargetGroups[0].TargetGroupArn' --output text 2>/dev/null || true)
  tf_import_if_exists "$tf_dir" "aws_lb_target_group.server" "$tg_arn" || failed=1

  if [ -n "$alb_arn" ] && [ "$alb_arn" != "None" ]; then
    local http_listener_arn
    http_listener_arn=$(aws elbv2 describe-listeners \
      --load-balancer-arn "$alb_arn" \
      --region "$AWS_REGION" \
      --query "Listeners[?Port==\`80\`].ListenerArn" --output text 2>/dev/null || true)
    tf_import_if_exists "$tf_dir" "aws_lb_listener.http" "$http_listener_arn" || failed=1

    if [ -n "$domain_name" ]; then
      local https_listener_arn
      https_listener_arn=$(aws elbv2 describe-listeners \
        --load-balancer-arn "$alb_arn" \
        --region "$AWS_REGION" \
        --query "Listeners[?Port==\`443\`].ListenerArn" --output text 2>/dev/null || true)
      tf_import_if_exists "$tf_dir" "aws_lb_listener.https[0]" "$https_listener_arn" || failed=1
    fi
  fi

  local alb_sg_id
  alb_sg_id=$(aws ec2 describe-security-groups \
    --filters "Name=group-name,Values=${alb_sg_name}" \
    --region "$AWS_REGION" \
    --query 'SecurityGroups[0].GroupId' --output text 2>/dev/null || true)
  tf_import_if_exists "$tf_dir" "aws_security_group.alb" "$alb_sg_id" || failed=1

  local server_sg_id
  server_sg_id=$(aws ec2 describe-security-groups \
    --filters "Name=group-name,Values=${server_sg_name}" \
    --region "$AWS_REGION" \
    --query 'SecurityGroups[0].GroupId' --output text 2>/dev/null || true)
  tf_import_if_exists "$tf_dir" "aws_security_group.server" "$server_sg_id" || failed=1

  local postgres_sg_id
  postgres_sg_id=$(aws ec2 describe-security-groups \
    --filters "Name=group-name,Values=${postgres_sg_name}" \
    --region "$AWS_REGION" \
    --query 'SecurityGroups[0].GroupId' --output text 2>/dev/null || true)
  tf_import_if_exists "$tf_dir" "aws_security_group.postgres" "$postgres_sg_id" || failed=1

  local namespace_id
  namespace_id=$(aws servicediscovery list-namespaces \
    --region "$AWS_REGION" \
    --query "Namespaces[?Name=='${namespace_name}'].Id" --output text 2>/dev/null || true)
  local namespace_vpc_id="${TF_VPC_ID:-}"
  if [ -z "$namespace_vpc_id" ]; then
    namespace_vpc_id=$(aws ec2 describe-vpcs \
      --filters "Name=is-default,Values=true" \
      --region "$AWS_REGION" \
      --query 'Vpcs[0].VpcId' --output text 2>/dev/null || true)
  fi
  if [ -n "$namespace_id" ] && [ "$namespace_id" != "None" ] && [ -n "$namespace_vpc_id" ] && [ "$namespace_vpc_id" != "None" ]; then
    tf_import_if_exists "$tf_dir" "aws_service_discovery_private_dns_namespace.main" "${namespace_id}:${namespace_vpc_id}" || failed=1
  fi

  local sd_service_id
  sd_service_id=$(aws servicediscovery list-services \
    --region "$AWS_REGION" \
    --query "Services[?Name=='${sd_service_name}'].Id" --output text 2>/dev/null || true)
  tf_import_if_exists "$tf_dir" "aws_service_discovery_service.postgres" "$sd_service_id" || failed=1

  tf_import_if_exists "$tf_dir" "aws_cloudwatch_log_group.cluster" "$log_group_cluster" || failed=1
  tf_import_if_exists "$tf_dir" "aws_cloudwatch_log_group.server" "$log_group_server" || failed=1
  tf_import_if_exists "$tf_dir" "aws_cloudwatch_log_group.postgres" "$log_group_postgres" || failed=1

  local server_service_status
  local server_service_arn
  server_service_status=$(aws ecs describe-services \
    --cluster "$cluster_name" \
    --services "$server_service_name" \
    --region "$AWS_REGION" \
    --query 'services[0].status' --output text 2>/dev/null || true)
  server_service_arn=$(aws ecs describe-services \
    --cluster "$cluster_name" \
    --services "$server_service_name" \
    --region "$AWS_REGION" \
    --query 'services[0].serviceArn' --output text 2>/dev/null || true)
  if [ -n "$server_service_status" ] && [ "$server_service_status" != "INACTIVE" ] && [ "$server_service_status" != "None" ] && \
     [ -n "$server_service_arn" ] && [ "$server_service_arn" != "None" ]; then
    tf_import_if_exists "$tf_dir" "aws_ecs_service.server" "${cluster_name}/${server_service_name}" || failed=1
  fi

  local postgres_service_status
  local postgres_service_arn
  postgres_service_status=$(aws ecs describe-services \
    --cluster "$cluster_name" \
    --services "$postgres_service_name" \
    --region "$AWS_REGION" \
    --query 'services[0].status' --output text 2>/dev/null || true)
  postgres_service_arn=$(aws ecs describe-services \
    --cluster "$cluster_name" \
    --services "$postgres_service_name" \
    --region "$AWS_REGION" \
    --query 'services[0].serviceArn' --output text 2>/dev/null || true)
  if [ -n "$postgres_service_status" ] && [ "$postgres_service_status" != "INACTIVE" ] && [ "$postgres_service_status" != "None" ] && \
     [ -n "$postgres_service_arn" ] && [ "$postgres_service_arn" != "None" ]; then
    tf_import_if_exists "$tf_dir" "aws_ecs_service.postgres" "${cluster_name}/${postgres_service_name}" || failed=1
  fi

  if [ "$failed" -ne 0 ]; then
    log_error "One or more imports failed. Fix import errors and rerun deploy."
    return 1
  fi
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

cmd_deploy() {
  log_info "Deploying PSM server with Terraform..."

  resolve_route53_zone

  if [ "$SKIP_BUILD" = false ]; then
    cmd_build_and_push
  else
    log_info "Skipping Docker build (--skip-build)"
  fi

  local AWS_ACCOUNT_ID=$(get_aws_account_id)
  local IMAGE_URI="${AWS_ACCOUNT_ID}.dkr.ecr.${AWS_REGION}.amazonaws.com/${ECR_REPO_NAME}:latest"
  local SCRIPT_DIR
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  local TF_DIR="${SCRIPT_DIR}/../infra"

  if [ ! -d "$TF_DIR" ]; then
    log_error "Terraform directory not found: $TF_DIR"
    return 1
  fi

  if [ ! -d "$TF_DIR/.terraform" ]; then
    log_info "Initializing Terraform..."
    terraform -chdir="$TF_DIR" init
  fi

  cmd_import_existing_resources "$TF_DIR" "$IMAGE_URI" "$AWS_REGION" \
    "$DOMAIN_NAME" "$SUBDOMAIN" "$ROUTE53_ZONE_ID"

  log_info "Applying Terraform..."
  local tf_vars=()
  tf_vars+=("-var" "aws_region=${AWS_REGION}")
  tf_vars+=("-var" "server_image_uri=${IMAGE_URI}")
  if [ -n "$DOMAIN_NAME" ]; then
    tf_vars+=("-var" "domain_name=${DOMAIN_NAME}")
    tf_vars+=("-var" "subdomain=${SUBDOMAIN}")
    if [ -n "$ROUTE53_ZONE_ID" ]; then
      tf_vars+=("-var" "route53_zone_id=${ROUTE53_ZONE_ID}")
    fi
  fi

  terraform -chdir="$TF_DIR" apply -auto-approve "${tf_vars[@]}"

  local ALB_URL
  local ALB_DNS
  local HTTPS_URL
  local CUSTOM_DOMAIN_URL
  ALB_URL=$(terraform -chdir="$TF_DIR" output -raw alb_url 2>/dev/null || true)
  ALB_DNS=$(terraform -chdir="$TF_DIR" output -raw alb_dns_name 2>/dev/null || true)
  CUSTOM_DOMAIN_URL=$(terraform -chdir="$TF_DIR" output -raw custom_domain_url 2>/dev/null || true)
  if [ -n "$ALB_DNS" ] && [[ "$ALB_URL" == https://* ]]; then
    HTTPS_URL="https://${ALB_DNS}"
  fi

  echo ""
  log_info "Deployment complete!"
  if [ -n "$ALB_URL" ]; then
    echo ""
    echo "  URL: ${ALB_URL}"
    if [ -n "$HTTPS_URL" ]; then
      echo "  HTTPS URL: ${HTTPS_URL}"
    fi
    if [ -n "$CUSTOM_DOMAIN_URL" ]; then
      echo "  Custom domain: ${CUSTOM_DOMAIN_URL}"
    fi
    echo ""
    echo "  Health check: curl ${ALB_URL}/"
    echo "  Public key:   curl ${ALB_URL}/pubkey"
  fi
  echo ""
}

cmd_status() {
  log_info "Checking Terraform outputs..."

  local SCRIPT_DIR
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  local TF_DIR="${SCRIPT_DIR}/../infra"

  if [ ! -d "$TF_DIR" ]; then
    log_error "Terraform directory not found: $TF_DIR"
    return 1
  fi

  terraform -chdir="$TF_DIR" output 2>/dev/null || log_warn "No Terraform outputs found (run deploy first)"
}

cmd_logs() {
  log_info "Tailing CloudWatch logs (Ctrl+C to exit)..."

  local SCRIPT_DIR
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  local TF_DIR="${SCRIPT_DIR}/../infra"

  if [ ! -d "$TF_DIR" ]; then
    log_error "Terraform directory not found: $TF_DIR"
    return 1
  fi

  local LOG_GROUP
  LOG_GROUP=$(terraform -chdir="$TF_DIR" output -raw server_log_group 2>/dev/null || true)
  if [ -z "$LOG_GROUP" ]; then
    log_warn "Log group not found. Run deploy first."
    return 0
  fi

  aws logs tail "$LOG_GROUP" --follow --region $AWS_REGION
}

cmd_cleanup() {
  log_warn "This will delete ALL PSM server AWS resources (Terraform destroy)"
  read -p "Are you sure? (yes/no): " confirm
  if [ "$confirm" != "yes" ]; then
    echo "Aborted"
    exit 0
  fi

  resolve_route53_zone

  local AWS_ACCOUNT_ID=$(get_aws_account_id)
  local IMAGE_URI="${AWS_ACCOUNT_ID}.dkr.ecr.${AWS_REGION}.amazonaws.com/${ECR_REPO_NAME}:latest"
  local SCRIPT_DIR
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  local TF_DIR="${SCRIPT_DIR}/../infra"

  if [ ! -d "$TF_DIR" ]; then
    log_error "Terraform directory not found: $TF_DIR"
    return 1
  fi

  if [ ! -d "$TF_DIR/.terraform" ]; then
    log_info "Initializing Terraform..."
    terraform -chdir="$TF_DIR" init
  fi

  log_info "Running Terraform destroy..."
  local tf_vars=()
  tf_vars+=("-var" "aws_region=${AWS_REGION}")
  tf_vars+=("-var" "server_image_uri=${IMAGE_URI}")
  if [ -n "$DOMAIN_NAME" ]; then
    tf_vars+=("-var" "domain_name=${DOMAIN_NAME}")
    tf_vars+=("-var" "subdomain=${SUBDOMAIN}")
    if [ -n "$ROUTE53_ZONE_ID" ]; then
      tf_vars+=("-var" "route53_zone_id=${ROUTE53_ZONE_ID}")
    fi
  fi

  terraform -chdir="$TF_DIR" destroy -auto-approve "${tf_vars[@]}"

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
    echo "  deploy   Build/push image and run Terraform apply"
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
