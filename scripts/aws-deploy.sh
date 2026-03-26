#!/bin/bash
set -euo pipefail

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
#   DEPLOY_STAGE          - Deployment profile (dev or prod, default: dev)
#   ECR_REPO_NAME         - ECR repository/image name (default: <stack-name>-server)
#   DOMAIN_NAME           - Root domain (default: openzeppelin.com)
#   SUBDOMAIN             - Subdomain (default: guardian)
#   ROUTE53_ZONE_ID       - Route 53 hosted zone ID (optional)
#   CLOUDFLARE_ZONE_ID    - Cloudflare zone ID (optional)
#   CLOUDFLARE_API_TOKEN  - Cloudflare API token (optional)
#   CLOUDFLARE_PROXIED    - Cloudflare proxied setting (true/false)
#   ACM_CERTIFICATE_ARN   - ACM certificate ARN for HTTPS
#   GUARDIAN_NETWORK_TYPE      - Runtime Miden network for the server (default: MidenTestnet)

AWS_REGION="${AWS_REGION:-us-east-1}"
SKIP_BUILD=false
CPU_ARCHITECTURE="${CPU_ARCHITECTURE:-${TF_VAR_cpu_architecture:-X86_64}}"
STACK_NAME="${STACK_NAME:-${TF_VAR_stack_name:-guardian}}"
DEPLOY_STAGE="${DEPLOY_STAGE:-${TF_VAR_deployment_stage:-dev}}"
ECR_REPO_NAME="${ECR_REPO_NAME:-${STACK_NAME}-server}"
DOMAIN_NAME="${DOMAIN_NAME-openzeppelin.com}"
SUBDOMAIN="${SUBDOMAIN-guardian}"
ROUTE53_ZONE_ID="${ROUTE53_ZONE_ID-}"
CLOUDFLARE_ZONE_ID="${CLOUDFLARE_ZONE_ID-}"
CLOUDFLARE_PROXIED="${CLOUDFLARE_PROXIED:-true}"
ACM_CERTIFICATE_ARN="${ACM_CERTIFICATE_ARN-}"
GUARDIAN_NETWORK_TYPE="${GUARDIAN_NETWORK_TYPE:-MidenTestnet}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TF_DIR="${SCRIPT_DIR}/../infra"
TF_VARS=()

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log_info() { echo -e "${GREEN}[INFO]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

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

  case "$DEPLOY_STAGE" in
    dev|prod)
      ;;
    *)
      log_error "DEPLOY_STAGE must be dev or prod"
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

get_ecr_repo_uri() {
  local aws_account_id
  aws_account_id=$(get_aws_account_id)
  echo "${aws_account_id}.dkr.ecr.${AWS_REGION}.amazonaws.com/${ECR_REPO_NAME}"
}

resolve_deploy_image_uri() {
  local repo_uri
  local image_digest
  repo_uri=$(get_ecr_repo_uri)
  image_digest=$(aws ecr describe-images \
    --repository-name "$ECR_REPO_NAME" \
    --region "$AWS_REGION" \
    --image-ids imageTag=latest \
    --query 'imageDetails[0].imageDigest' \
    --output text 2>/dev/null || true)

  if [ -z "$image_digest" ] || [ "$image_digest" = "None" ]; then
    log_error "Could not resolve ${ECR_REPO_NAME}:latest from ECR. Build/push the image first or remove --skip-build."
    return 1
  fi

  echo "${repo_uri}@${image_digest}"
}

require_terraform_dir() {
  if [ ! -d "$TF_DIR" ]; then
    log_error "Terraform directory not found: $TF_DIR"
    return 1
  fi
}

ensure_terraform_init() {
  require_terraform_dir || return 1
  if [ ! -d "$TF_DIR/.terraform" ]; then
    log_info "Initializing Terraform..."
    terraform -chdir="$TF_DIR" init
  fi
}

build_tf_vars() {
  local image_uri="$1"
  TF_VARS=()
  TF_VARS+=("-var" "aws_region=${AWS_REGION}")
  TF_VARS+=("-var" "cpu_architecture=${CPU_ARCHITECTURE}")
  TF_VARS+=("-var" "stack_name=${STACK_NAME}")
  TF_VARS+=("-var" "deployment_stage=${DEPLOY_STAGE}")
  TF_VARS+=("-var" "server_image_uri=${image_uri}")
  TF_VARS+=("-var" "server_network_type=${GUARDIAN_NETWORK_TYPE}")

  if [ -n "$DOMAIN_NAME" ]; then
    TF_VARS+=("-var" "domain_name=${DOMAIN_NAME}")
    TF_VARS+=("-var" "subdomain=${SUBDOMAIN}")
    TF_VARS+=("-var" "acm_certificate_arn=${ACM_CERTIFICATE_ARN}")
    if [ -n "$CLOUDFLARE_ZONE_ID" ]; then
      TF_VARS+=("-var" "cloudflare_zone_id=${CLOUDFLARE_ZONE_ID}")
      TF_VARS+=("-var" "cloudflare_proxied=${CLOUDFLARE_PROXIED}")
    fi
    if [ -n "$ROUTE53_ZONE_ID" ]; then
      TF_VARS+=("-var" "route53_zone_id=${ROUTE53_ZONE_ID}")
    fi
  fi
  if [ -n "${CLOUDFLARE_API_TOKEN:-}" ]; then
    TF_VARS+=("-var" "cloudflare_api_token=${CLOUDFLARE_API_TOKEN}")
  fi
}

terraform_output_raw() {
  local output_name="$1"
  terraform -chdir="$TF_DIR" output -raw "$output_name" 2>/dev/null || true
}

cmd_build_and_push() {
  local ecr_repo_uri
  local docker_platform
  ecr_repo_uri=$(get_ecr_repo_uri)
  docker_platform=$(docker_platform_for_arch "$CPU_ARCHITECTURE")

  log_info "Creating ECR repository..."
  aws ecr create-repository \
    --repository-name "$ECR_REPO_NAME" \
    --region "$AWS_REGION" 2>/dev/null || log_warn "ECR repository already exists"

  log_info "Logging into ECR..."
  aws ecr get-login-password --region "$AWS_REGION" | \
    docker login --username AWS --password-stdin "${ecr_repo_uri%/*}"

  log_info "Building Docker image..."
  docker build --platform "$docker_platform" --build-arg GUARDIAN_SERVER_FEATURES=postgres --no-cache -t "${ECR_REPO_NAME}:latest" .

  log_info "Tagging and pushing to ECR..."
  docker tag "${ECR_REPO_NAME}:latest" "${ecr_repo_uri}:latest"
  docker push "${ecr_repo_uri}:latest"

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

  local IMAGE_URI
  IMAGE_URI=$(resolve_deploy_image_uri) || return 1
  ensure_terraform_init || return 1
  build_tf_vars "$IMAGE_URI"

  log_info "Deploying image ${IMAGE_URI}"
  log_info "Applying Terraform..."
  terraform -chdir="$TF_DIR" apply -auto-approve "${TF_VARS[@]}"

  local ALB_URL
  local ALB_DNS
  local HTTPS_URL
  local CUSTOM_DOMAIN_URL
  local GRPC_ENDPOINT
  local DATABASE_ENDPOINT
  local DEPLOYMENT_STAGE_OUTPUT
  local RDS_PROXY_ENDPOINT
  local RDS_PROXY_ENABLED
  local RDS_MAX_ALLOCATED_STORAGE
  local SERVER_AUTOSCALING_ENABLED
  local SERVER_AUTOSCALING_MIN_CAPACITY
  local SERVER_AUTOSCALING_MAX_CAPACITY
  local RATE_BURST
  local RATE_PER_MIN
  local DB_POOL_MAX
  local METADATA_DB_POOL_MAX
  local DATABASE_URL_SECRET_ARN
  ALB_URL=$(terraform_output_raw alb_url)
  ALB_DNS=$(terraform_output_raw alb_dns_name)
  CUSTOM_DOMAIN_URL=$(terraform_output_raw custom_domain_url)
  GRPC_ENDPOINT=$(terraform_output_raw grpc_endpoint)
  DATABASE_ENDPOINT=$(terraform_output_raw database_endpoint)
  DEPLOYMENT_STAGE_OUTPUT=$(terraform_output_raw deployment_stage)
  RDS_PROXY_ENDPOINT=$(terraform_output_raw rds_proxy_endpoint)
  RDS_PROXY_ENABLED=$(terraform_output_raw rds_proxy_enabled)
  RDS_MAX_ALLOCATED_STORAGE=$(terraform_output_raw rds_max_allocated_storage)
  SERVER_AUTOSCALING_ENABLED=$(terraform_output_raw server_autoscaling_enabled)
  SERVER_AUTOSCALING_MIN_CAPACITY=$(terraform_output_raw server_autoscaling_min_capacity)
  SERVER_AUTOSCALING_MAX_CAPACITY=$(terraform_output_raw server_autoscaling_max_capacity)
  RATE_BURST=$(terraform_output_raw guardian_rate_burst_per_sec)
  RATE_PER_MIN=$(terraform_output_raw guardian_rate_per_min)
  DB_POOL_MAX=$(terraform_output_raw guardian_db_pool_max_size)
  METADATA_DB_POOL_MAX=$(terraform_output_raw guardian_metadata_db_pool_max_size)
  DATABASE_URL_SECRET_ARN=$(terraform_output_raw database_url_secret_arn)
  if [ -n "$ALB_DNS" ] && [[ "$ALB_URL" == https://* ]]; then
    HTTPS_URL="https://${ALB_DNS}"
  fi

  echo ""
  log_info "Deployment complete!"
  if [ -n "$DEPLOYMENT_STAGE_OUTPUT" ]; then
    echo "  Stage: ${DEPLOYMENT_STAGE_OUTPUT}"
  fi
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
    if [ -n "$DATABASE_ENDPOINT" ]; then
      echo "  Database endpoint: ${DATABASE_ENDPOINT}"
    fi
    if [ -n "$RDS_PROXY_ENABLED" ]; then
      echo "  RDS proxy enabled: ${RDS_PROXY_ENABLED}"
    fi
    if [ -n "$RDS_PROXY_ENDPOINT" ]; then
      echo "  RDS proxy endpoint: ${RDS_PROXY_ENDPOINT}"
    fi
    if [ -n "$RDS_MAX_ALLOCATED_STORAGE" ]; then
      echo "  RDS max allocated storage: ${RDS_MAX_ALLOCATED_STORAGE}"
    fi
    if [ -n "$SERVER_AUTOSCALING_ENABLED" ]; then
      echo "  ECS autoscaling enabled: ${SERVER_AUTOSCALING_ENABLED}"
    fi
    if [ -n "$SERVER_AUTOSCALING_MIN_CAPACITY" ] && [ -n "$SERVER_AUTOSCALING_MAX_CAPACITY" ]; then
      echo "  ECS autoscaling range: ${SERVER_AUTOSCALING_MIN_CAPACITY}-${SERVER_AUTOSCALING_MAX_CAPACITY}"
    fi
    if [ -n "$RATE_BURST" ] && [ -n "$RATE_PER_MIN" ]; then
      echo "  HTTP rate limits: burst=${RATE_BURST}/sec sustained=${RATE_PER_MIN}/min"
    fi
    if [ -n "$DB_POOL_MAX" ] && [ -n "$METADATA_DB_POOL_MAX" ]; then
      echo "  DB pool sizes: storage=${DB_POOL_MAX} metadata=${METADATA_DB_POOL_MAX}"
    fi
    if [ -n "$DATABASE_URL_SECRET_ARN" ]; then
      echo "  Database URL secret: ${DATABASE_URL_SECRET_ARN}"
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
  require_terraform_dir || return 1

  terraform -chdir="$TF_DIR" output 2>/dev/null || log_warn "No Terraform outputs found (run deploy first)"
}

cmd_logs() {
  log_info "Tailing CloudWatch logs (Ctrl+C to exit)..."
  require_terraform_dir || return 1

  local LOG_GROUP
  LOG_GROUP=$(terraform_output_raw server_log_group)
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
  ensure_terraform_init || return 1
  build_tf_vars "$IMAGE_URI"

  log_info "Running Terraform destroy..."
  terraform -chdir="$TF_DIR" destroy -auto-approve "${TF_VARS[@]}"

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
    echo "Environment:"
    echo "  CPU_ARCHITECTURE=  ECS/image architecture (X86_64 or ARM64, default: X86_64)"
    echo "  STACK_NAME=   Base stack name for AWS resources (default: guardian)"
    echo "  DEPLOY_STAGE= Deployment profile (dev or prod, default: dev)"
    echo "  ECR_REPO_NAME= Override the ECR/image repository name (default: <stack-name>-server)"
    echo "  GUARDIAN_NETWORK_TYPE= Runtime Miden network for the server (default: MidenTestnet)"
    echo ""
    echo "Examples:"
    echo "  ./scripts/aws-deploy.sh deploy"
    echo "  DEPLOY_STAGE=dev STACK_NAME=guardian SUBDOMAIN=guardian-stg ./scripts/aws-deploy.sh deploy"
    echo "  DEPLOY_STAGE=prod STACK_NAME=guardian SUBDOMAIN=guardian-stg ./scripts/aws-deploy.sh deploy --skip-build"
    echo "  ./scripts/aws-deploy.sh deploy --skip-build"
    echo "  ./scripts/aws-deploy.sh status"
    echo "  ./scripts/aws-deploy.sh cleanup"
    ;;
esac
