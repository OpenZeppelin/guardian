#!/bin/bash
set -e

# GUARDIAN Server AWS Deployment Script
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
#   AWS_REGION            - AWS region (default: us-east-1)
#   CPU_ARCHITECTURE      - ECS/image architecture (X86_64 or ARM64, default: X86_64)
#   STACK_NAME            - Base stack name used for AWS resources (default: guardian)
#   ECR_REPO_NAME         - ECR repository/image name (default: <stack-name>-server)
#   DOMAIN_NAME           - Root domain (default: openzeppelin.com)
#   SUBDOMAIN             - Subdomain (default: guardian)
#   ROUTE53_ZONE_ID       - Route 53 hosted zone ID (optional)
#   CLOUDFLARE_ZONE_ID    - Cloudflare zone ID (optional)
#   CLOUDFLARE_API_TOKEN  - Cloudflare API token (optional)
#   CLOUDFLARE_PROXIED    - Cloudflare proxied setting (true/false)
#   ACM_CERTIFICATE_ARN   - ACM certificate ARN for HTTPS
#   GUARDIAN_NETWORK_TYPE      - Runtime Miden network for the server (default: MidenTestnet)
#   IMPORT_EXISTING       - Import existing AWS resources (true/false)

AWS_REGION="${AWS_REGION:-us-east-1}"
SKIP_BUILD=false
CPU_ARCHITECTURE="${CPU_ARCHITECTURE:-${TF_VAR_cpu_architecture:-X86_64}}"
STACK_NAME="${STACK_NAME:-${TF_VAR_stack_name:-guardian}}"
ECR_REPO_NAME="${ECR_REPO_NAME:-${STACK_NAME}-server}"
DOMAIN_NAME="${DOMAIN_NAME-openzeppelin.com}"
SUBDOMAIN="${SUBDOMAIN-guardian}"
ROUTE53_ZONE_ID="${ROUTE53_ZONE_ID-}"
CLOUDFLARE_ZONE_ID="${CLOUDFLARE_ZONE_ID-}"
CLOUDFLARE_PROXIED="${CLOUDFLARE_PROXIED:-true}"
ACM_CERTIFICATE_ARN="${ACM_CERTIFICATE_ARN-}"
GUARDIAN_NETWORK_TYPE="${GUARDIAN_NETWORK_TYPE:-MidenTestnet}"
IMPORT_EXISTING="${IMPORT_EXISTING:-false}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log_info() { echo -e "${GREEN}[INFO]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

tf_var_or_default() {
  local name="$1"
  local default_value="$2"
  local current_value="${!name:-}"
  if [ -n "$current_value" ]; then
    echo "$current_value"
  else
    echo "$default_value"
  fi
}

validate_deploy_config() {
  local cloudflare_api_token="${CLOUDFLARE_API_TOKEN:-${TF_VAR_cloudflare_api_token:-}}"
  if [ -n "$CLOUDFLARE_ZONE_ID" ] && [ -z "$cloudflare_api_token" ]; then
    log_error "CLOUDFLARE_ZONE_ID is set but CLOUDFLARE_API_TOKEN is empty"
    return 1
  fi

  case "$CPU_ARCHITECTURE" in
    X86_64|ARM64)
      ;;
    *)
      log_error "CPU_ARCHITECTURE must be X86_64 or ARM64"
      return 1
      ;;
  esac
}

docker_platform_for_arch() {
  case "$1" in
    X86_64)
      echo "linux/amd64"
      ;;
    ARM64)
      echo "linux/arm64"
      ;;
  esac
}

get_aws_account_id() {
  aws sts get-caller-identity --query Account --output text
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
  export TF_VAR_cpu_architecture="$CPU_ARCHITECTURE"
  export TF_VAR_stack_name="$STACK_NAME"
  export TF_VAR_domain_name="$domain_name"
  export TF_VAR_subdomain="$subdomain"
  export TF_VAR_route53_zone_id="$route53_zone_id"
  export TF_VAR_server_network_type="$GUARDIAN_NETWORK_TYPE"
  export TF_VAR_cloudflare_zone_id="$CLOUDFLARE_ZONE_ID"
  export TF_VAR_cloudflare_proxied="$CLOUDFLARE_PROXIED"
  export TF_VAR_acm_certificate_arn="$ACM_CERTIFICATE_ARN"
  if [ -n "${CLOUDFLARE_API_TOKEN:-}" ]; then
    export TF_VAR_cloudflare_api_token="$CLOUDFLARE_API_TOKEN"
  fi

  local failed=0
  local cluster_name
  cluster_name=$(tf_var_or_default TF_VAR_cluster_name "${STACK_NAME}-cluster")
  local alb_name
  alb_name=$(tf_var_or_default TF_VAR_alb_name "${STACK_NAME}-alb")
  local target_group_name
  target_group_name=$(tf_var_or_default TF_VAR_target_group_name "${STACK_NAME}-server-tg")
  local alb_sg_name
  alb_sg_name=$(tf_var_or_default TF_VAR_alb_security_group_name "${STACK_NAME}-alb-sg")
  local server_sg_name
  server_sg_name=$(tf_var_or_default TF_VAR_server_security_group_name "${STACK_NAME}-server-sg")
  local postgres_sg_name
  postgres_sg_name=$(tf_var_or_default TF_VAR_postgres_security_group_name "${STACK_NAME}-postgres-sg")
  local namespace_name
  namespace_name=$(tf_var_or_default TF_VAR_sd_namespace_name "${STACK_NAME}.local")
  local server_service_name
  server_service_name=$(tf_var_or_default TF_VAR_server_service_name "${STACK_NAME}-server")
  local postgres_service_name
  postgres_service_name=$(tf_var_or_default TF_VAR_postgres_service_name "${STACK_NAME}-postgres")
  local sd_service_name="$postgres_service_name"
  local log_group_server
  log_group_server=$(tf_var_or_default TF_VAR_server_log_group_name "/ecs/${server_service_name}")
  local log_group_postgres
  log_group_postgres=$(tf_var_or_default TF_VAR_postgres_log_group_name "/ecs/${postgres_service_name}")
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

  local role_name
  role_name=$(tf_var_or_default TF_VAR_task_execution_role_name "${STACK_NAME}-ecs-task-execution")
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
  local namespace_vpc_id="${TF_VAR_vpc_id:-}"
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
  local docker_platform
  docker_platform=$(docker_platform_for_arch "$CPU_ARCHITECTURE")

  log_info "Creating ECR repository..."
  aws ecr create-repository \
    --repository-name $ECR_REPO_NAME \
    --region $AWS_REGION 2>/dev/null || log_warn "ECR repository already exists"

  log_info "Logging into ECR..."
  aws ecr get-login-password --region $AWS_REGION | \
    docker login --username AWS --password-stdin $AWS_ACCOUNT_ID.dkr.ecr.$AWS_REGION.amazonaws.com

  log_info "Building Docker image..."
  docker build --platform "$docker_platform" --no-cache -t "${ECR_REPO_NAME}:latest" .

  log_info "Tagging and pushing to ECR..."
  docker tag "${ECR_REPO_NAME}:latest" "$AWS_ACCOUNT_ID.dkr.ecr.$AWS_REGION.amazonaws.com/${ECR_REPO_NAME}:latest"
  docker push "$AWS_ACCOUNT_ID.dkr.ecr.$AWS_REGION.amazonaws.com/${ECR_REPO_NAME}:latest"

  log_info "Image pushed successfully"
}

cmd_deploy() {
  log_info "Deploying GUARDIAN server with Terraform..."
  validate_deploy_config

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

  if [ "$IMPORT_EXISTING" = true ]; then
    cmd_import_existing_resources "$TF_DIR" "$IMAGE_URI" "$AWS_REGION" \
      "$DOMAIN_NAME" "$SUBDOMAIN" "$ROUTE53_ZONE_ID"
  else
    log_info "Skipping resource imports (IMPORT_EXISTING=false)"
  fi

  log_info "Applying Terraform..."
  local tf_vars=()
  tf_vars+=("-var" "aws_region=${AWS_REGION}")
  tf_vars+=("-var" "cpu_architecture=${CPU_ARCHITECTURE}")
  tf_vars+=("-var" "stack_name=${STACK_NAME}")
  tf_vars+=("-var" "server_image_uri=${IMAGE_URI}")
  tf_vars+=("-var" "server_network_type=${GUARDIAN_NETWORK_TYPE}")
  if [ -n "$DOMAIN_NAME" ]; then
    tf_vars+=("-var" "domain_name=${DOMAIN_NAME}")
    tf_vars+=("-var" "subdomain=${SUBDOMAIN}")
    tf_vars+=("-var" "acm_certificate_arn=${ACM_CERTIFICATE_ARN}")
    if [ -n "$CLOUDFLARE_ZONE_ID" ]; then
      tf_vars+=("-var" "cloudflare_zone_id=${CLOUDFLARE_ZONE_ID}")
      tf_vars+=("-var" "cloudflare_proxied=${CLOUDFLARE_PROXIED}")
    fi
    if [ -n "$ROUTE53_ZONE_ID" ]; then
      tf_vars+=("-var" "route53_zone_id=${ROUTE53_ZONE_ID}")
    fi
  fi
  if [ -n "${CLOUDFLARE_API_TOKEN:-}" ]; then
    tf_vars+=("-var" "cloudflare_api_token=${CLOUDFLARE_API_TOKEN}")
  fi

  terraform -chdir="$TF_DIR" apply -auto-approve "${tf_vars[@]}"

  local ALB_URL
  local ALB_DNS
  local HTTPS_URL
  local CUSTOM_DOMAIN_URL
  local GRPC_ENDPOINT
  ALB_URL=$(terraform -chdir="$TF_DIR" output -raw alb_url 2>/dev/null || true)
  ALB_DNS=$(terraform -chdir="$TF_DIR" output -raw alb_dns_name 2>/dev/null || true)
  CUSTOM_DOMAIN_URL=$(terraform -chdir="$TF_DIR" output -raw custom_domain_url 2>/dev/null || true)
  GRPC_ENDPOINT=$(terraform -chdir="$TF_DIR" output -raw grpc_endpoint 2>/dev/null || true)
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
    if [ -n "$GRPC_ENDPOINT" ]; then
      echo "  gRPC endpoint: ${GRPC_ENDPOINT}"
    fi
    echo ""
    echo "  Health check: curl ${ALB_URL}/"
    echo "  Public key:   curl ${ALB_URL}/pubkey"
    if [ -n "$GRPC_ENDPOINT" ]; then
      echo "  gRPC check:   grpcurl -import-path crates/server/proto -proto guardian.proto -d '{}' ${GRPC_ENDPOINT#https://}:443 guardian.Guardian/GetPubkey"
    fi
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
  log_warn "This will delete ALL GUARDIAN server AWS resources (Terraform destroy)"
  validate_deploy_config
  read -p "Are you sure? (yes/no): " confirm
  if [ "$confirm" != "yes" ]; then
    echo "Aborted"
    exit 0
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

  log_info "Running Terraform destroy..."
  local tf_vars=()
  tf_vars+=("-var" "aws_region=${AWS_REGION}")
  tf_vars+=("-var" "cpu_architecture=${CPU_ARCHITECTURE}")
  tf_vars+=("-var" "stack_name=${STACK_NAME}")
  tf_vars+=("-var" "server_image_uri=${IMAGE_URI}")
  tf_vars+=("-var" "server_network_type=${GUARDIAN_NETWORK_TYPE}")
  if [ -n "$DOMAIN_NAME" ]; then
    tf_vars+=("-var" "domain_name=${DOMAIN_NAME}")
    tf_vars+=("-var" "subdomain=${SUBDOMAIN}")
    tf_vars+=("-var" "acm_certificate_arn=${ACM_CERTIFICATE_ARN}")
    if [ -n "$CLOUDFLARE_ZONE_ID" ]; then
      tf_vars+=("-var" "cloudflare_zone_id=${CLOUDFLARE_ZONE_ID}")
      tf_vars+=("-var" "cloudflare_proxied=${CLOUDFLARE_PROXIED}")
    fi
    if [ -n "$ROUTE53_ZONE_ID" ]; then
      tf_vars+=("-var" "route53_zone_id=${ROUTE53_ZONE_ID}")
    fi
  fi
  if [ -n "${CLOUDFLARE_API_TOKEN:-}" ]; then
    tf_vars+=("-var" "cloudflare_api_token=${CLOUDFLARE_API_TOKEN}")
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
    --domain=*)
      DOMAIN_NAME="${arg#*=}"
      ;;
    --subdomain=*)
      SUBDOMAIN="${arg#*=}"
      ;;
    --route53-zone-id=*)
      ROUTE53_ZONE_ID="${arg#*=}"
      ;;
    --cloudflare-zone-id=*)
      CLOUDFLARE_ZONE_ID="${arg#*=}"
      ;;
    --cloudflare-proxied=*)
      CLOUDFLARE_PROXIED="${arg#*=}"
      ;;
    --acm-certificate-arn=*)
      ACM_CERTIFICATE_ARN="${arg#*=}"
      ;;
    --import-existing)
      IMPORT_EXISTING=true
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
    echo "GUARDIAN Server AWS Deployment Script"
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
    echo "  --domain=     Override root domain (default: openzeppelin.com)"
    echo "  --subdomain=  Override subdomain (default: guardian)"
    echo "  --route53-zone-id=  Route 53 hosted zone ID (optional)"
    echo "  --cloudflare-zone-id=  Cloudflare zone ID (optional)"
    echo "  --cloudflare-proxied=  Cloudflare proxied setting (true/false)"
    echo "  --acm-certificate-arn= ACM certificate ARN for HTTPS"
    echo "  --import-existing Import existing AWS resources into state"
    echo ""
    echo "Environment:"
    echo "  CPU_ARCHITECTURE=  ECS/image architecture (X86_64 or ARM64, default: X86_64)"
    echo "  STACK_NAME=   Base stack name for AWS resources (default: guardian)"
    echo "  ECR_REPO_NAME= Override the ECR/image repository name (default: <stack-name>-server)"
    echo ""
    echo "Examples:"
    echo "  ./scripts/aws-deploy.sh deploy"
    echo "  STACK_NAME=psm SUBDOMAIN=psm-stg ./scripts/aws-deploy.sh deploy"
    echo "  ./scripts/aws-deploy.sh deploy --skip-build"
    echo "  ./scripts/aws-deploy.sh status"
    echo "  ./scripts/aws-deploy.sh cleanup"
    ;;
esac
