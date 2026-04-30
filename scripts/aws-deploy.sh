#!/bin/bash
set -euo pipefail

# GUARDIAN Server AWS Deployment Script
# Usage: ./scripts/aws-deploy.sh [command] [options]
#
# Commands:
#   deploy   - Build/push image and run Terraform apply
#   bootstrap-ack-keys - Create the prod ACK key secrets in Secrets Manager
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
#   GUARDIAN_SERVER_FEATURES   - Cargo features for guardian-server Docker build (default: postgres)
#   GUARDIAN_CORS_ALLOWED_ORIGINS - Comma-separated explicit HTTP origins allowed by CORS (optional)
#   GUARDIAN_CORS_ALLOW_CREDENTIALS - Whether CORS includes Access-Control-Allow-Credentials (default: false)
#   GUARDIAN_EVM_CHAIN_CONFIG_FILE - JSON file used to derive EVM chain IDs, RPC URLs, and EntryPoint address (default: config/evm/chains.json)
#   GUARDIAN_EVM_ALLOWED_CHAIN_IDS - Comma-separated EVM chain IDs allowed by the server; creates a stack Secrets Manager secret (optional)
#   GUARDIAN_EVM_ALLOWED_CHAIN_IDS_SECRET_ARN - Secrets Manager ARN with comma-separated EVM chain IDs (optional)
#   GUARDIAN_EVM_RPC_URLS - Comma-separated chain_id=url EVM RPC map; creates a stack Secrets Manager secret (optional)
#   GUARDIAN_EVM_RPC_URLS_SECRET_ARN - Secrets Manager ARN with comma-separated EVM RPC map (optional)
#   GUARDIAN_EVM_ENTRYPOINT_ADDRESS - Shared EVM EntryPoint address (default: EntryPoint v0.9)
#   GUARDIAN_EVM_SESSION_COOKIE_DOMAIN - Optional Domain attribute for the EVM session cookie
#   GUARDIAN_EVM_SESSION_COOKIE_SAME_SITE - Optional SameSite attribute for the EVM session cookie (Strict, Lax, None)
#   GUARDIAN_EVM_SESSION_COOKIE_SECURE - Whether the EVM session cookie includes Secure (default: false)
#   GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON - JSON array of Falcon operator public keys; creates a stack Secrets Manager secret (optional)
#   GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ARN - Secrets Manager ARN with dashboard operator public keys JSON (optional)

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
GUARDIAN_SERVER_FEATURES="${GUARDIAN_SERVER_FEATURES:-postgres}"
GUARDIAN_CORS_ALLOWED_ORIGINS="${GUARDIAN_CORS_ALLOWED_ORIGINS:-${TF_VAR_guardian_cors_allowed_origins:-}}"
GUARDIAN_CORS_ALLOW_CREDENTIALS="${GUARDIAN_CORS_ALLOW_CREDENTIALS:-${TF_VAR_guardian_cors_allow_credentials:-false}}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEFAULT_GUARDIAN_EVM_CHAIN_CONFIG_FILE="${SCRIPT_DIR}/../config/evm/chains.json"
DEFAULT_GUARDIAN_EVM_ENTRYPOINT_ADDRESS="0x433709009b8330fda32311df1c2afa402ed8d009"
GUARDIAN_EVM_CHAIN_CONFIG_FILE="${GUARDIAN_EVM_CHAIN_CONFIG_FILE:-$DEFAULT_GUARDIAN_EVM_CHAIN_CONFIG_FILE}"
GUARDIAN_EVM_ALLOWED_CHAIN_IDS="${GUARDIAN_EVM_ALLOWED_CHAIN_IDS:-${TF_VAR_guardian_evm_allowed_chain_ids:-}}"
GUARDIAN_EVM_ALLOWED_CHAIN_IDS_SECRET_ARN="${GUARDIAN_EVM_ALLOWED_CHAIN_IDS_SECRET_ARN:-${TF_VAR_guardian_evm_allowed_chain_ids_secret_arn:-}}"
GUARDIAN_EVM_RPC_URLS="${GUARDIAN_EVM_RPC_URLS:-${TF_VAR_guardian_evm_rpc_urls:-}}"
GUARDIAN_EVM_RPC_URLS_SECRET_ARN="${GUARDIAN_EVM_RPC_URLS_SECRET_ARN:-${TF_VAR_guardian_evm_rpc_urls_secret_arn:-}}"
GUARDIAN_EVM_ENTRYPOINT_ADDRESS="${GUARDIAN_EVM_ENTRYPOINT_ADDRESS:-${TF_VAR_guardian_evm_entrypoint_address:-}}"
GUARDIAN_EVM_SESSION_COOKIE_DOMAIN="${GUARDIAN_EVM_SESSION_COOKIE_DOMAIN:-${TF_VAR_guardian_evm_session_cookie_domain:-}}"
GUARDIAN_EVM_SESSION_COOKIE_SAME_SITE="${GUARDIAN_EVM_SESSION_COOKIE_SAME_SITE:-${TF_VAR_guardian_evm_session_cookie_same_site:-}}"
GUARDIAN_EVM_SESSION_COOKIE_SECURE="${GUARDIAN_EVM_SESSION_COOKIE_SECURE:-${TF_VAR_guardian_evm_session_cookie_secure:-false}}"
GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON="${GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON:-}"
GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ARN="${GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ARN:-${TF_VAR_guardian_operator_public_keys_secret_arn:-}}"
TF_DIR="${SCRIPT_DIR}/../infra"
TF_STATE_PATH_OVERRIDE="${TF_STATE_PATH:-}"
TF_STATE_BACKUP_PATH_OVERRIDE="${TF_STATE_BACKUP_PATH:-}"
TF_STATE_PATH="${TF_STATE_PATH_OVERRIDE:-${TF_DIR}/terraform.${STACK_NAME}.${DEPLOY_STAGE}.tfstate}"
TF_STATE_BACKUP_PATH="${TF_STATE_BACKUP_PATH_OVERRIDE:-${TF_STATE_PATH}.backup}"
TF_VARS=()

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log_info() { echo -e "${GREEN}[INFO]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

evm_feature_enabled() {
  local normalized_features="${GUARDIAN_SERVER_FEATURES//[[:space:]]/}"
  [[ ",${normalized_features}," == *",evm,"* ]]
}

is_bool() {
  case "$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')" in
    true|false|1|0|yes|no)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

is_true() {
  case "$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')" in
    true|1|yes)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

load_evm_chain_config_file() {
  if ! evm_feature_enabled; then
    return 0
  fi

  local needs_allowed_chain_ids=false
  local needs_rpc_urls=false
  local needs_entrypoint_address=false

  if [ -z "$GUARDIAN_EVM_ALLOWED_CHAIN_IDS" ] && [ -z "$GUARDIAN_EVM_ALLOWED_CHAIN_IDS_SECRET_ARN" ]; then
    needs_allowed_chain_ids=true
  fi
  if [ -z "$GUARDIAN_EVM_RPC_URLS" ] && [ -z "$GUARDIAN_EVM_RPC_URLS_SECRET_ARN" ]; then
    needs_rpc_urls=true
  fi
  if [ -z "$GUARDIAN_EVM_ENTRYPOINT_ADDRESS" ]; then
    needs_entrypoint_address=true
  fi

  if [ "$needs_allowed_chain_ids" = false ] && \
    [ "$needs_rpc_urls" = false ] && \
    [ "$needs_entrypoint_address" = false ]; then
    return 0
  fi

  if [ ! -f "$GUARDIAN_EVM_CHAIN_CONFIG_FILE" ]; then
    log_error "EVM chain config file not found: ${GUARDIAN_EVM_CHAIN_CONFIG_FILE}"
    return 1
  fi

  if ! command -v jq >/dev/null 2>&1; then
    log_error "jq is required to read GUARDIAN_EVM_CHAIN_CONFIG_FILE"
    return 1
  fi

  if ! jq -e '
    (.entrypointAddress | type == "string")
    and (.chains | type == "array" and length > 0)
    and all(.chains[]; (.chainId | type == "number") and (.chainId > 0) and (.rpcUrl | type == "string") and (.rpcUrl | test("^https?://")))
  ' "$GUARDIAN_EVM_CHAIN_CONFIG_FILE" >/dev/null; then
    log_error "Invalid EVM chain config file: ${GUARDIAN_EVM_CHAIN_CONFIG_FILE}"
    return 1
  fi

  local chain_count
  local unique_chain_count
  chain_count=$(jq '.chains | length' "$GUARDIAN_EVM_CHAIN_CONFIG_FILE")
  unique_chain_count=$(jq '[.chains[].chainId] | unique | length' "$GUARDIAN_EVM_CHAIN_CONFIG_FILE")
  if [ "$chain_count" != "$unique_chain_count" ]; then
    log_error "EVM chain config file has duplicate chainId values: ${GUARDIAN_EVM_CHAIN_CONFIG_FILE}"
    return 1
  fi

  if [ "$needs_allowed_chain_ids" = true ]; then
    GUARDIAN_EVM_ALLOWED_CHAIN_IDS="$(jq -r '[.chains[].chainId] | join(",")' "$GUARDIAN_EVM_CHAIN_CONFIG_FILE")"
  fi
  if [ "$needs_rpc_urls" = true ]; then
    GUARDIAN_EVM_RPC_URLS="$(jq -r '[.chains[] | "\(.chainId)=\(.rpcUrl)"] | join(",")' "$GUARDIAN_EVM_CHAIN_CONFIG_FILE")"
  fi
  if [ "$needs_entrypoint_address" = true ]; then
    GUARDIAN_EVM_ENTRYPOINT_ADDRESS="$(jq -r '.entrypointAddress // empty' "$GUARDIAN_EVM_CHAIN_CONFIG_FILE")"
  fi
  if [ -z "$GUARDIAN_EVM_ENTRYPOINT_ADDRESS" ]; then
    GUARDIAN_EVM_ENTRYPOINT_ADDRESS="$DEFAULT_GUARDIAN_EVM_ENTRYPOINT_ADDRESS"
  fi
}

validate_deploy_config() {
  local cloudflare_api_token="${CLOUDFLARE_API_TOKEN:-${TF_VAR_cloudflare_api_token:-}}"
  local normalized_features="${GUARDIAN_SERVER_FEATURES//[[:space:]]/}"

  load_evm_chain_config_file || return 1

  if [ -n "$CLOUDFLARE_ZONE_ID" ] && [ -z "$cloudflare_api_token" ]; then
    log_error "CLOUDFLARE_ZONE_ID is set but CLOUDFLARE_API_TOKEN is empty"
    return 1
  fi

  if [[ ",${normalized_features}," != *",postgres,"* ]]; then
    log_error "GUARDIAN_SERVER_FEATURES must include postgres for AWS deployments"
    return 1
  fi

  if [[ ",${normalized_features}," == *",evm,"* ]]; then
    if [ -z "$GUARDIAN_EVM_ALLOWED_CHAIN_IDS" ] && \
      [ -z "$GUARDIAN_EVM_ALLOWED_CHAIN_IDS_SECRET_ARN" ]; then
      log_error "GUARDIAN_SERVER_FEATURES includes evm, so set GUARDIAN_EVM_ALLOWED_CHAIN_IDS or GUARDIAN_EVM_ALLOWED_CHAIN_IDS_SECRET_ARN"
      return 1
    fi
    if [ -z "$GUARDIAN_EVM_RPC_URLS" ] && \
      [ -z "$GUARDIAN_EVM_RPC_URLS_SECRET_ARN" ]; then
      log_error "GUARDIAN_SERVER_FEATURES includes evm, so set GUARDIAN_EVM_RPC_URLS or GUARDIAN_EVM_RPC_URLS_SECRET_ARN"
      return 1
    fi
    if [[ ! "$GUARDIAN_EVM_ENTRYPOINT_ADDRESS" =~ ^0x[0-9a-fA-F]{40}$ ]]; then
      log_error "GUARDIAN_EVM_ENTRYPOINT_ADDRESS must be a 20-byte 0x-prefixed hex address"
      return 1
    fi
    if [ -n "$GUARDIAN_EVM_SESSION_COOKIE_SAME_SITE" ] && \
      [[ ! "$GUARDIAN_EVM_SESSION_COOKIE_SAME_SITE" =~ ^(Strict|strict|Lax|lax|None|none)$ ]]; then
      log_error "GUARDIAN_EVM_SESSION_COOKIE_SAME_SITE must be Strict, Lax, or None"
      return 1
    fi
    if ! is_bool "$GUARDIAN_EVM_SESSION_COOKIE_SECURE"; then
      log_error "GUARDIAN_EVM_SESSION_COOKIE_SECURE must be true or false"
      return 1
    fi
    if [[ "${GUARDIAN_EVM_SESSION_COOKIE_DOMAIN}" =~ [\;\,\ ] ]]; then
      log_error "GUARDIAN_EVM_SESSION_COOKIE_DOMAIN must not contain spaces, commas, or semicolons"
      return 1
    fi
    if [ "$(printf '%s' "$GUARDIAN_EVM_SESSION_COOKIE_SAME_SITE" | tr '[:upper:]' '[:lower:]')" = "none" ] && \
      ! is_true "$GUARDIAN_EVM_SESSION_COOKIE_SECURE"; then
      log_error "GUARDIAN_EVM_SESSION_COOKIE_SAME_SITE=None requires GUARDIAN_EVM_SESSION_COOKIE_SECURE=true"
      return 1
    fi
  fi

  if ! is_bool "$GUARDIAN_CORS_ALLOW_CREDENTIALS"; then
    log_error "GUARDIAN_CORS_ALLOW_CREDENTIALS must be true or false"
    return 1
  fi
  if is_true "$GUARDIAN_CORS_ALLOW_CREDENTIALS" && [ -z "$GUARDIAN_CORS_ALLOWED_ORIGINS" ]; then
    log_error "GUARDIAN_CORS_ALLOW_CREDENTIALS=true requires GUARDIAN_CORS_ALLOWED_ORIGINS"
    return 1
  fi
  if [[ "$GUARDIAN_CORS_ALLOWED_ORIGINS" =~ (^|,)[[:space:]]*\*[[:space:]]*(,|$) ]]; then
    log_error "GUARDIAN_CORS_ALLOWED_ORIGINS must use explicit origins, not wildcard"
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
  TF_VARS+=("-var" "guardian_cors_allow_credentials=${GUARDIAN_CORS_ALLOW_CREDENTIALS}")
  TF_VARS+=("-var" "guardian_evm_session_cookie_secure=${GUARDIAN_EVM_SESSION_COOKIE_SECURE}")
  TF_VARS+=("-var" "guardian_evm_allowed_chain_ids_secret_arn=${GUARDIAN_EVM_ALLOWED_CHAIN_IDS_SECRET_ARN}")
  TF_VARS+=("-var" "guardian_evm_rpc_urls_secret_arn=${GUARDIAN_EVM_RPC_URLS_SECRET_ARN}")
  TF_VARS+=("-var" "guardian_operator_public_keys_secret_arn=${GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ARN}")
  if [ -n "$GUARDIAN_CORS_ALLOWED_ORIGINS" ]; then
    TF_VARS+=("-var" "guardian_cors_allowed_origins=${GUARDIAN_CORS_ALLOWED_ORIGINS}")
  fi
  if [ -n "$GUARDIAN_EVM_ALLOWED_CHAIN_IDS" ]; then
    TF_VARS+=("-var" "guardian_evm_allowed_chain_ids=${GUARDIAN_EVM_ALLOWED_CHAIN_IDS}")
  fi
  if [ -n "$GUARDIAN_EVM_RPC_URLS" ]; then
    TF_VARS+=("-var" "guardian_evm_rpc_urls=${GUARDIAN_EVM_RPC_URLS}")
  fi
  if evm_feature_enabled; then
    TF_VARS+=("-var" "guardian_evm_entrypoint_address=${GUARDIAN_EVM_ENTRYPOINT_ADDRESS}")
  fi
  if [ -n "$GUARDIAN_EVM_SESSION_COOKIE_DOMAIN" ]; then
    TF_VARS+=("-var" "guardian_evm_session_cookie_domain=${GUARDIAN_EVM_SESSION_COOKIE_DOMAIN}")
  fi
  if [ -n "$GUARDIAN_EVM_SESSION_COOKIE_SAME_SITE" ]; then
    TF_VARS+=("-var" "guardian_evm_session_cookie_same_site=${GUARDIAN_EVM_SESSION_COOKIE_SAME_SITE}")
  fi
  if [ -n "$GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON" ]; then
    TF_VARS+=("-var" "guardian_operator_public_keys=${GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON}")
  fi

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
  terraform -chdir="$TF_DIR" output -state="$TF_STATE_PATH" -raw "$output_name" 2>/dev/null || true
}

ack_falcon_secret_name() {
  echo "guardian-prod/server/ack-falcon-secret-key"
}

ack_ecdsa_secret_name() {
  echo "guardian-prod/server/ack-ecdsa-secret-key"
}

secret_exists() {
  local secret_id="$1"
  aws secretsmanager describe-secret --secret-id "$secret_id" --region "$AWS_REGION" >/dev/null 2>&1
}

validate_ack_secrets_exist() {
  if [ "$DEPLOY_STAGE" != "prod" ]; then
    return 0
  fi

  local falcon_secret_name
  local ecdsa_secret_name
  falcon_secret_name=$(ack_falcon_secret_name)
  ecdsa_secret_name=$(ack_ecdsa_secret_name)

  if ! secret_exists "$falcon_secret_name"; then
    log_error "Missing Falcon ACK secret ${falcon_secret_name}. Run ./scripts/aws-deploy.sh bootstrap-ack-keys first."
    return 1
  fi

  if ! secret_exists "$ecdsa_secret_name"; then
    log_error "Missing ECDSA ACK secret ${ecdsa_secret_name}. Run ./scripts/aws-deploy.sh bootstrap-ack-keys first."
    return 1
  fi
}

cmd_bootstrap_ack_keys() {
  local falcon_secret_name
  local ecdsa_secret_name
  local existing_secrets=()
  local generated_keys
  local falcon_secret_value
  local ecdsa_secret_value
  falcon_secret_name=$(ack_falcon_secret_name)
  ecdsa_secret_name=$(ack_ecdsa_secret_name)

  if secret_exists "$falcon_secret_name"; then
    existing_secrets+=("$falcon_secret_name")
  fi
  if secret_exists "$ecdsa_secret_name"; then
    existing_secrets+=("$ecdsa_secret_name")
  fi

  if [ "${#existing_secrets[@]}" -gt 0 ]; then
    log_error "Refusing to overwrite existing ACK secret(s): ${existing_secrets[*]}"
    return 1
  fi

  log_info "Generating ACK keys locally..."
  generated_keys=$(cargo run --quiet --package guardian-server --bin ack-keygen)
  falcon_secret_value=$(printf '%s' "$generated_keys" | jq -r '.falcon_secret_key')
  ecdsa_secret_value=$(printf '%s' "$generated_keys" | jq -r '.ecdsa_secret_key')

  if [ -z "$falcon_secret_value" ] || [ "$falcon_secret_value" = "null" ]; then
    log_error "Failed to generate Falcon ACK key material"
    return 1
  fi
  if [ -z "$ecdsa_secret_value" ] || [ "$ecdsa_secret_value" = "null" ]; then
    log_error "Failed to generate ECDSA ACK key material"
    return 1
  fi

  log_info "Creating Falcon ACK secret ${falcon_secret_name}"
  aws secretsmanager create-secret \
    --name "$falcon_secret_name" \
    --secret-string "$falcon_secret_value" \
    --region "$AWS_REGION" >/dev/null

  log_info "Creating ECDSA ACK secret ${ecdsa_secret_name}"
  aws secretsmanager create-secret \
    --name "$ecdsa_secret_name" \
    --secret-string "$ecdsa_secret_value" \
    --region "$AWS_REGION" >/dev/null

  log_info "ACK key bootstrap complete"
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
  docker build --platform "$docker_platform" --build-arg "GUARDIAN_SERVER_FEATURES=${GUARDIAN_SERVER_FEATURES}" --no-cache -t "${ECR_REPO_NAME}:latest" .

  log_info "Tagging and pushing to ECR..."
  docker tag "${ECR_REPO_NAME}:latest" "${ecr_repo_uri}:latest"
  docker push "${ecr_repo_uri}:latest"

  log_info "Image pushed successfully"
}

cmd_deploy() {
  log_info "Deploying GUARDIAN server with Terraform..."
  validate_deploy_config
  validate_ack_secrets_exist || return 1

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
  log_info "Using Terraform state ${TF_STATE_PATH}"
  log_info "Applying Terraform..."
  terraform -chdir="$TF_DIR" apply -auto-approve -state="$TF_STATE_PATH" -backup="$TF_STATE_BACKUP_PATH" "${TF_VARS[@]}"

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
  local SERVER_CPU
  local SERVER_MEMORY
  local RATE_LIMIT_ENABLED
  local RATE_BURST
  local RATE_PER_MIN
  local DB_POOL_MAX
  local METADATA_DB_POOL_MAX
  local DATABASE_URL_SECRET_ARN
  local EVM_ALLOWED_CHAIN_IDS_SECRET_ARN
  local EVM_RPC_URLS_SECRET_ARN
  local EVM_ENTRYPOINT_ADDRESS
  local CORS_ALLOWED_ORIGINS
  local CORS_ALLOW_CREDENTIALS
  local EVM_SESSION_COOKIE_DOMAIN
  local EVM_SESSION_COOKIE_SAME_SITE
  local EVM_SESSION_COOKIE_SECURE
  ALB_URL=$(terraform_output_raw alb_url)
  ALB_DNS=$(terraform_output_raw alb_dns_name)
  CUSTOM_DOMAIN_URL=$(terraform_output_raw custom_domain_url)
  GRPC_ENDPOINT=$(terraform_output_raw grpc_endpoint)
  DATABASE_ENDPOINT=$(terraform_output_raw database_endpoint)
  DEPLOYMENT_STAGE_OUTPUT=$(terraform_output_raw deployment_stage)
  RDS_PROXY_ENDPOINT=$(terraform_output_raw rds_proxy_endpoint)
  RDS_PROXY_ENABLED=$(terraform_output_raw rds_proxy_enabled)
  RDS_MAX_ALLOCATED_STORAGE=$(terraform_output_raw rds_max_allocated_storage)
  SERVER_CPU=$(terraform_output_raw server_cpu)
  SERVER_MEMORY=$(terraform_output_raw server_memory)
  SERVER_AUTOSCALING_ENABLED=$(terraform_output_raw server_autoscaling_enabled)
  SERVER_AUTOSCALING_MIN_CAPACITY=$(terraform_output_raw server_autoscaling_min_capacity)
  SERVER_AUTOSCALING_MAX_CAPACITY=$(terraform_output_raw server_autoscaling_max_capacity)
  RATE_LIMIT_ENABLED=$(terraform_output_raw guardian_rate_limit_enabled)
  RATE_BURST=$(terraform_output_raw guardian_rate_burst_per_sec)
  RATE_PER_MIN=$(terraform_output_raw guardian_rate_per_min)
  DB_POOL_MAX=$(terraform_output_raw guardian_db_pool_max_size)
  METADATA_DB_POOL_MAX=$(terraform_output_raw guardian_metadata_db_pool_max_size)
  DATABASE_URL_SECRET_ARN=$(terraform_output_raw database_url_secret_arn)
  EVM_ALLOWED_CHAIN_IDS_SECRET_ARN=$(terraform_output_raw guardian_evm_allowed_chain_ids_secret_arn)
  EVM_RPC_URLS_SECRET_ARN=$(terraform_output_raw guardian_evm_rpc_urls_secret_arn)
  EVM_ENTRYPOINT_ADDRESS=$(terraform_output_raw guardian_evm_entrypoint_address)
  CORS_ALLOWED_ORIGINS=$(terraform_output_raw guardian_cors_allowed_origins)
  CORS_ALLOW_CREDENTIALS=$(terraform_output_raw guardian_cors_allow_credentials)
  EVM_SESSION_COOKIE_DOMAIN=$(terraform_output_raw guardian_evm_session_cookie_domain)
  EVM_SESSION_COOKIE_SAME_SITE=$(terraform_output_raw guardian_evm_session_cookie_same_site)
  EVM_SESSION_COOKIE_SECURE=$(terraform_output_raw guardian_evm_session_cookie_secure)
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
    if [ -n "$SERVER_CPU" ] && [ -n "$SERVER_MEMORY" ]; then
      echo "  ECS task size: cpu=${SERVER_CPU} memory=${SERVER_MEMORY}"
    fi
    if [ -n "$SERVER_AUTOSCALING_ENABLED" ]; then
      echo "  ECS autoscaling enabled: ${SERVER_AUTOSCALING_ENABLED}"
    fi
    if [ -n "$SERVER_AUTOSCALING_MIN_CAPACITY" ] && [ -n "$SERVER_AUTOSCALING_MAX_CAPACITY" ]; then
      echo "  ECS autoscaling range: ${SERVER_AUTOSCALING_MIN_CAPACITY}-${SERVER_AUTOSCALING_MAX_CAPACITY}"
    fi
    if [ -n "$RATE_LIMIT_ENABLED" ]; then
      echo "  HTTP rate limiting enabled: ${RATE_LIMIT_ENABLED}"
    fi
    if [ "$RATE_LIMIT_ENABLED" = "true" ] && [ -n "$RATE_BURST" ] && [ -n "$RATE_PER_MIN" ]; then
      echo "  HTTP rate limits: burst=${RATE_BURST}/sec sustained=${RATE_PER_MIN}/min"
    fi
    if [ -n "$DB_POOL_MAX" ] && [ -n "$METADATA_DB_POOL_MAX" ]; then
      echo "  DB pool sizes: storage=${DB_POOL_MAX} metadata=${METADATA_DB_POOL_MAX}"
    fi
    if [ -n "$DATABASE_URL_SECRET_ARN" ]; then
      echo "  Database URL secret: ${DATABASE_URL_SECRET_ARN}"
    fi
    if [ -n "$EVM_ALLOWED_CHAIN_IDS_SECRET_ARN" ]; then
      echo "  EVM chain IDs secret: ${EVM_ALLOWED_CHAIN_IDS_SECRET_ARN}"
    fi
    if [ -n "$EVM_RPC_URLS_SECRET_ARN" ]; then
      echo "  EVM RPC URLs secret: ${EVM_RPC_URLS_SECRET_ARN}"
    fi
    if [ -n "$EVM_ENTRYPOINT_ADDRESS" ]; then
      echo "  EVM EntryPoint address: ${EVM_ENTRYPOINT_ADDRESS}"
    fi
    if [ -n "$CORS_ALLOWED_ORIGINS" ]; then
      echo "  CORS allowed origins: ${CORS_ALLOWED_ORIGINS}"
    fi
    if [ -n "$CORS_ALLOW_CREDENTIALS" ]; then
      echo "  CORS allow credentials: ${CORS_ALLOW_CREDENTIALS}"
    fi
    if [ -n "$EVM_SESSION_COOKIE_DOMAIN" ]; then
      echo "  EVM session cookie domain: ${EVM_SESSION_COOKIE_DOMAIN}"
    fi
    if [ -n "$EVM_SESSION_COOKIE_SAME_SITE" ]; then
      echo "  EVM session cookie SameSite: ${EVM_SESSION_COOKIE_SAME_SITE}"
    fi
    if [ -n "$EVM_SESSION_COOKIE_SECURE" ]; then
      echo "  EVM session cookie Secure: ${EVM_SESSION_COOKIE_SECURE}"
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
  ensure_terraform_init || return 1

  if [ ! -f "$TF_STATE_PATH" ]; then
    log_warn "No Terraform state found at ${TF_STATE_PATH} (run deploy first)"
    return 0
  fi

  log_info "Using Terraform state ${TF_STATE_PATH}"
  terraform -chdir="$TF_DIR" output -state="$TF_STATE_PATH" 2>/dev/null || log_warn "No Terraform outputs found (run deploy first)"
}

cmd_logs() {
  log_info "Tailing CloudWatch logs (Ctrl+C to exit)..."
  require_terraform_dir || return 1
  ensure_terraform_init || return 1

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

  log_info "Using Terraform state ${TF_STATE_PATH}"
  log_info "Running Terraform destroy..."
  terraform -chdir="$TF_DIR" destroy -auto-approve -state="$TF_STATE_PATH" -backup="$TF_STATE_BACKUP_PATH" "${TF_VARS[@]}"

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
  bootstrap-ack-keys)
    cmd_bootstrap_ack_keys
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
    echo "  bootstrap-ack-keys  Create the prod ACK key secrets in Secrets Manager"
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
    echo "  TF_STATE_PATH= Override the Terraform state file path (default: infra/terraform.<stack>.<stage>.tfstate)"
    echo "  GUARDIAN_NETWORK_TYPE= Runtime Miden network for the server (default: MidenTestnet)"
    echo "  GUARDIAN_SERVER_FEATURES= Cargo features for guardian-server Docker build (default: postgres)"
    echo "  GUARDIAN_CORS_ALLOWED_ORIGINS= Comma-separated explicit HTTP origins allowed by CORS"
    echo "  GUARDIAN_CORS_ALLOW_CREDENTIALS= Whether CORS includes Access-Control-Allow-Credentials"
    echo "  GUARDIAN_EVM_CHAIN_CONFIG_FILE= JSON file for EVM chain IDs, RPC URLs, and EntryPoint address"
    echo "  GUARDIAN_EVM_ALLOWED_CHAIN_IDS= Comma-separated EVM chain IDs; creates a stack Secrets Manager secret"
    echo "  GUARDIAN_EVM_ALLOWED_CHAIN_IDS_SECRET_ARN= Secrets Manager ARN with comma-separated EVM chain IDs"
    echo "  GUARDIAN_EVM_RPC_URLS= Comma-separated chain_id=url EVM RPC map; creates a stack Secrets Manager secret"
    echo "  GUARDIAN_EVM_RPC_URLS_SECRET_ARN= Secrets Manager ARN with comma-separated EVM RPC map"
    echo "  GUARDIAN_EVM_ENTRYPOINT_ADDRESS= Shared EVM EntryPoint address (default: v0.9)"
    echo "  GUARDIAN_EVM_SESSION_COOKIE_DOMAIN= Optional Domain attribute for the EVM session cookie"
    echo "  GUARDIAN_EVM_SESSION_COOKIE_SAME_SITE= Optional SameSite attribute for the EVM session cookie"
    echo "  GUARDIAN_EVM_SESSION_COOKIE_SECURE= Whether the EVM session cookie includes Secure"
    echo "  GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON= JSON array of Falcon operator public keys; creates a stack Secrets Manager secret"
    echo "  GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ARN= Secrets Manager ARN with dashboard operator public keys JSON"
    echo ""
    echo "Examples:"
    echo "  ./scripts/aws-deploy.sh deploy"
    echo "  DEPLOY_STAGE=prod ./scripts/aws-deploy.sh bootstrap-ack-keys"
    echo "  DEPLOY_STAGE=dev STACK_NAME=guardian SUBDOMAIN=guardian-stg ./scripts/aws-deploy.sh deploy"
    echo "  DEPLOY_STAGE=prod STACK_NAME=guardian-prod SUBDOMAIN=guardian ./scripts/aws-deploy.sh deploy --skip-build"
    echo "  ./scripts/aws-deploy.sh deploy --skip-build"
    echo "  ./scripts/aws-deploy.sh status"
    echo "  ./scripts/aws-deploy.sh cleanup"
    ;;
esac
