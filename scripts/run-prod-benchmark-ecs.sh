#!/bin/bash
set -euo pipefail

PATH="/usr/local/bin:/opt/homebrew/bin:${PATH}"
export AWS_PAGER=""

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
BENCH_MANIFEST="${ROOT_DIR}/benchmarks/prod-server/Cargo.toml"

PROFILE_PATH=""
WORKERS="${BENCH_ECS_WORKERS:-4}"
BENCH_WORKER_CPU="${BENCH_WORKER_CPU:-2048}"
BENCH_WORKER_MEMORY="${BENCH_WORKER_MEMORY:-4096}"
IMAGE_URI="${BENCH_IMAGE_URI:-}"
SKIP_BUILD=false
KEEP_IMAGE=false
KEEP_TASK_DEFINITION=false
NO_CLEANUP=false
RUN_ID=""

AWS_BIN=""
DOCKER_BIN=""
JQ_BIN=""
CARGO_BIN=""
SESSION_MANAGER_PLUGIN=""

AWS_PROFILE_NAME="${AWS_PROFILE:-}"
AWS_REGION_NAME="${AWS_REGION:-}"
ECS_CLUSTER_NAME=""
ECS_SERVICE_NAME=""
BENCH_ECR_REPO_NAME="${BENCH_ECR_REPO_NAME:-${ECR_REPO_NAME:-guardian-server}}"

SERVER_TASK_DEFINITION_ARN=""
BENCH_TASK_DEFINITION_ARN=""
BENCH_IMAGE_TAG=""
BENCH_REPO_CREATED=false
TASK_ARNS=()
TASK_IDS=()

SERVER_LOG_GROUP=""
ASSIGN_PUBLIC_IP="ENABLED"
SUBNETS_JSON="[]"
SECURITY_GROUPS_JSON="[]"
CPU_ARCHITECTURE_NAME=""
TASK_EXECUTION_ROLE_ARN=""
TASK_ROLE_ARN=""
WORKER_ARTIFACT_DIR=""

usage() {
  cat <<'EOF'
Usage: ./scripts/run-prod-benchmark-ecs.sh --profile <profile.toml> [options]

Options:
  --profile <path>            Benchmark profile to execute
  --workers <count>           Number of ECS worker tasks to launch (default: 4)
  --run-id <value>            Override the generated benchmark run ID
  --image-uri <uri>           Use an existing benchmark image instead of building one
  --skip-build                Skip building and pushing the benchmark image
  --no-cleanup                Aggregate reports but skip database cleanup
  --keep-image                Keep the temporary ECR image tag after the run
  --keep-task-definition      Keep the temporary ECS task definition after the run
  --help                      Show this message
EOF
}

log_info() {
  printf '[INFO] %s\n' "$1"
}

log_warn() {
  printf '[WARN] %s\n' "$1"
}

log_error() {
  printf '[ERROR] %s\n' "$1" >&2
}

require_cmd() {
  local cmd="$1"
  command -v "$cmd" >/dev/null 2>&1 || {
    log_error "missing required command: $cmd"
    exit 1
  }
}

resolve_binaries() {
  AWS_BIN="$(command -v aws)"
  DOCKER_BIN="$(command -v docker)"
  JQ_BIN="$(command -v jq)"
  CARGO_BIN="$(command -v cargo)"
  SESSION_MANAGER_PLUGIN="$(command -v session-manager-plugin || true)"

  require_cmd "$AWS_BIN"
  require_cmd "$DOCKER_BIN"
  require_cmd "$JQ_BIN"
  require_cmd "$CARGO_BIN"

  if [ -z "$SESSION_MANAGER_PLUGIN" ]; then
    log_error "session-manager-plugin is required for final cleanup"
    exit 1
  fi
}

read_toml_string() {
  local file="$1"
  local section="$2"
  local key="$3"
  awk -v want_section="$section" -v want_key="$key" '
    BEGIN { section = "" }
    /^[[:space:]]*\[/ {
      line = $0
      gsub(/^[[:space:]]*\[/, "", line)
      gsub(/\][[:space:]]*$/, "", line)
      section = line
      next
    }
    /^[[:space:]]*[A-Za-z0-9_]+[[:space:]]*=/ {
      line = $0
      current_key = line
      sub(/=.*/, "", current_key)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", current_key)
      if (current_key == want_key && section == want_section) {
        value = line
        sub(/^[^=]*=/, "", value)
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
        gsub(/^"/, "", value)
        gsub(/"$/, "", value)
        print value
        exit
      }
    }
  ' "$file"
}

aws_cmd() {
  local args=()
  if [ -n "$AWS_PROFILE_NAME" ]; then
    args+=(--profile "$AWS_PROFILE_NAME")
  fi
  if [ -n "$AWS_REGION_NAME" ]; then
    args+=(--region "$AWS_REGION_NAME")
  fi
  if [ "${#args[@]}" -gt 0 ]; then
    "$AWS_BIN" "${args[@]}" "$@"
  else
    "$AWS_BIN" "$@"
  fi
}

docker_platform_for_arch() {
  case "$1" in
    X86_64) echo "linux/amd64" ;;
    ARM64) echo "linux/arm64" ;;
    *)
      log_error "unsupported CPU architecture: $1"
      exit 1
      ;;
  esac
}

generate_run_id() {
  if [ -n "$RUN_ID" ]; then
    printf '%s' "$RUN_ID"
    return
  fi

  printf '%s-%s' "$(date -u +%Y%m%dT%H%M%SZ)" "$(openssl rand -hex 4)"
}

cleanup() {
  set +e

  if [ "${#TASK_ARNS[@]}" -gt 0 ]; then
    local described
    described="$(aws_cmd ecs describe-tasks --cluster "$ECS_CLUSTER_NAME" --tasks "${TASK_ARNS[@]}" 2>/dev/null || true)"
    if [ -n "$described" ]; then
      local running_arns=()
      while IFS= read -r arn; do
        [ -n "$arn" ] && running_arns+=("$arn")
      done < <(printf '%s' "$described" | "$JQ_BIN" -r '.tasks[] | select(.lastStatus != "STOPPED") | .taskArn')

      if [ "${#running_arns[@]}" -gt 0 ]; then
        for arn in "${running_arns[@]}"; do
          aws_cmd ecs stop-task --cluster "$ECS_CLUSTER_NAME" --task "$arn" >/dev/null 2>&1 || true
        done
      fi
    fi
  fi

  if [ -n "$BENCH_TASK_DEFINITION_ARN" ] && [ "$KEEP_TASK_DEFINITION" != true ]; then
    aws_cmd ecs deregister-task-definition --task-definition "$BENCH_TASK_DEFINITION_ARN" >/dev/null 2>&1 || true
  fi

  if [ -n "$BENCH_IMAGE_TAG" ] && [ -z "$IMAGE_URI" ] && [ "$KEEP_IMAGE" != true ]; then
    aws_cmd ecr batch-delete-image \
      --repository-name "$BENCH_ECR_REPO_NAME" \
      --image-ids imageTag="$BENCH_IMAGE_TAG" >/dev/null 2>&1 || true
  fi

  if [ "$BENCH_REPO_CREATED" = true ] && [ "$KEEP_IMAGE" != true ]; then
    aws_cmd ecr delete-repository --repository-name "$BENCH_ECR_REPO_NAME" --force >/dev/null 2>&1 || true
  fi

  if [ -n "$WORKER_ARTIFACT_DIR" ] && [ -d "$WORKER_ARTIFACT_DIR" ]; then
    rm -rf "$WORKER_ARTIFACT_DIR"
  fi
}

trap cleanup EXIT

while [ "$#" -gt 0 ]; do
  case "$1" in
    --profile)
      PROFILE_PATH="$2"
      shift 2
      ;;
    --workers)
      WORKERS="$2"
      shift 2
      ;;
    --run-id)
      RUN_ID="$2"
      shift 2
      ;;
    --image-uri)
      IMAGE_URI="$2"
      shift 2
      ;;
    --skip-build)
      SKIP_BUILD=true
      shift
      ;;
    --keep-image)
      KEEP_IMAGE=true
      shift
      ;;
    --keep-task-definition)
      KEEP_TASK_DEFINITION=true
      shift
      ;;
    --no-cleanup)
      NO_CLEANUP=true
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      log_error "unknown argument: $1"
      usage
      exit 1
      ;;
  esac
done

if [ -z "$PROFILE_PATH" ]; then
  log_error "--profile is required"
  usage
  exit 1
fi

if [ ! -f "$PROFILE_PATH" ]; then
  log_error "profile not found: $PROFILE_PATH"
  exit 1
fi

PROFILE_PATH="$(cd "$(dirname "$PROFILE_PATH")" && pwd)/$(basename "$PROFILE_PATH")"

if [ -f "${ROOT_DIR}/.env" ]; then
  set -a
  source "${ROOT_DIR}/.env"
  set +a
fi

resolve_binaries

aws_cmd sts get-caller-identity >/dev/null
"$DOCKER_BIN" info >/dev/null

AWS_PROFILE_NAME="${AWS_PROFILE_NAME:-$(read_toml_string "$PROFILE_PATH" "aws" "profile")}"
AWS_REGION_NAME="${AWS_REGION_NAME:-$(read_toml_string "$PROFILE_PATH" "aws" "region")}"
ECS_CLUSTER_NAME="$(read_toml_string "$PROFILE_PATH" "aws" "ecs_cluster")"
ECS_SERVICE_NAME="$(read_toml_string "$PROFILE_PATH" "aws" "ecs_service")"

if [ -z "$AWS_REGION_NAME" ] || [ -z "$ECS_CLUSTER_NAME" ] || [ -z "$ECS_SERVICE_NAME" ]; then
  log_error "profile must define aws.region, aws.ecs_cluster, and aws.ecs_service"
  exit 1
fi

RUN_ID="$(generate_run_id)"
WORKER_ARTIFACT_DIR="$(mktemp -d)"

log_info "run_id=${RUN_ID}"
log_info "profile=${PROFILE_PATH}"
log_info "workers=${WORKERS}"

SERVICE_JSON="$(aws_cmd ecs describe-services --cluster "$ECS_CLUSTER_NAME" --services "$ECS_SERVICE_NAME" --output json)"
SERVER_TASK_DEFINITION_ARN="$(printf '%s' "$SERVICE_JSON" | "$JQ_BIN" -r '.services[0].taskDefinition')"
SUBNETS_JSON="$(printf '%s' "$SERVICE_JSON" | "$JQ_BIN" '.services[0].networkConfiguration.awsvpcConfiguration.subnets')"
SECURITY_GROUPS_JSON="$(printf '%s' "$SERVICE_JSON" | "$JQ_BIN" '.services[0].networkConfiguration.awsvpcConfiguration.securityGroups')"
ASSIGN_PUBLIC_IP="$(printf '%s' "$SERVICE_JSON" | "$JQ_BIN" -r '.services[0].networkConfiguration.awsvpcConfiguration.assignPublicIp')"

if [ -z "$SERVER_TASK_DEFINITION_ARN" ] || [ "$SERVER_TASK_DEFINITION_ARN" = "null" ]; then
  log_error "failed to resolve the current server task definition"
  exit 1
fi

TASK_DEF_JSON="$(aws_cmd ecs describe-task-definition --task-definition "$SERVER_TASK_DEFINITION_ARN" --output json)"
TASK_EXECUTION_ROLE_ARN="$(printf '%s' "$TASK_DEF_JSON" | "$JQ_BIN" -r '.taskDefinition.executionRoleArn')"
TASK_ROLE_ARN="$(printf '%s' "$TASK_DEF_JSON" | "$JQ_BIN" -r '.taskDefinition.taskRoleArn')"
CPU_ARCHITECTURE_NAME="${CPU_ARCHITECTURE:-$(printf '%s' "$TASK_DEF_JSON" | "$JQ_BIN" -r '.taskDefinition.runtimePlatform.cpuArchitecture // empty')}"
SERVER_LOG_GROUP="$(printf '%s' "$TASK_DEF_JSON" | "$JQ_BIN" -r '.taskDefinition.containerDefinitions[0].logConfiguration.options["awslogs-group"]')"

if [ -z "$CPU_ARCHITECTURE_NAME" ] || [ -z "$SERVER_LOG_GROUP" ]; then
  log_error "failed to resolve worker runtime architecture or log group"
  exit 1
fi

if [ -z "$IMAGE_URI" ]; then
  if [ "$SKIP_BUILD" = true ]; then
    log_error "--skip-build requires --image-uri"
    exit 1
  fi

  AWS_ACCOUNT_ID="$(aws_cmd sts get-caller-identity --query Account --output text)"
  REPO_URI="${AWS_ACCOUNT_ID}.dkr.ecr.${AWS_REGION_NAME}.amazonaws.com/${BENCH_ECR_REPO_NAME}"
  BENCH_IMAGE_TAG="bench-${RUN_ID}"

  if ! aws_cmd ecr describe-repositories --repository-names "$BENCH_ECR_REPO_NAME" >/dev/null 2>&1; then
    aws_cmd ecr create-repository --repository-name "$BENCH_ECR_REPO_NAME" >/dev/null
    BENCH_REPO_CREATED=true
  fi

  aws_cmd ecr get-login-password | "$DOCKER_BIN" login --username AWS --password-stdin "${AWS_ACCOUNT_ID}.dkr.ecr.${AWS_REGION_NAME}.amazonaws.com" >/dev/null

  log_info "building benchmark image ${REPO_URI}:${BENCH_IMAGE_TAG}"
  "$DOCKER_BIN" build \
    --platform "$(docker_platform_for_arch "$CPU_ARCHITECTURE_NAME")" \
    --target benchmark-runner \
    --quiet \
    -t "${REPO_URI}:${BENCH_IMAGE_TAG}" \
    "$ROOT_DIR"

  log_info "pushing benchmark image ${REPO_URI}:${BENCH_IMAGE_TAG}"
  "$DOCKER_BIN" push "${REPO_URI}:${BENCH_IMAGE_TAG}" >/dev/null
  IMAGE_URI="${REPO_URI}:${BENCH_IMAGE_TAG}"
fi

PROFILE_B64="$(base64 < "$PROFILE_PATH" | tr -d '\n')"
BENCH_TASK_FAMILY="guardian-prod-bench-${RUN_ID}"
BENCH_CONTAINER_NAME="guardian-prod-benchmarks"
LOG_STREAM_PREFIX="bench-${RUN_ID}"
TASK_COMMAND='set -e; printf %s "$BENCH_PROFILE_B64" | base64 -d >/tmp/profile.toml; exec /app/guardian-prod-benchmarks worker-run --profile /tmp/profile.toml --run-id "$BENCH_RUN_ID" --shard-index "$BENCH_SHARD_INDEX" --shard-count "$BENCH_SHARD_COUNT"'

TASK_DEF_FILE="$(mktemp)"
"$JQ_BIN" -n \
  --arg family "$BENCH_TASK_FAMILY" \
  --arg image "$IMAGE_URI" \
  --arg execution_role_arn "$TASK_EXECUTION_ROLE_ARN" \
  --arg task_role_arn "$TASK_ROLE_ARN" \
  --arg cpu "$BENCH_WORKER_CPU" \
  --arg memory "$BENCH_WORKER_MEMORY" \
  --arg log_group "$SERVER_LOG_GROUP" \
  --arg log_prefix "$LOG_STREAM_PREFIX" \
  --arg region "$AWS_REGION_NAME" \
  --arg architecture "$CPU_ARCHITECTURE_NAME" \
  --arg container_name "$BENCH_CONTAINER_NAME" \
  --arg task_command "$TASK_COMMAND" \
  '{
    family: $family,
    networkMode: "awsvpc",
    requiresCompatibilities: ["FARGATE"],
    cpu: $cpu,
    memory: $memory,
    executionRoleArn: $execution_role_arn,
    taskRoleArn: $task_role_arn,
    runtimePlatform: {
      cpuArchitecture: $architecture,
      operatingSystemFamily: "LINUX"
    },
    containerDefinitions: [
      {
        name: $container_name,
        image: $image,
        essential: true,
        entryPoint: ["sh", "-lc"],
        command: [$task_command],
        environment: [
          { name: "RUST_LOG", value: "info" }
        ],
        logConfiguration: {
          logDriver: "awslogs",
          options: {
            "awslogs-group": $log_group,
            "awslogs-region": $region,
            "awslogs-stream-prefix": $log_prefix
          }
        }
      }
    ]
  }' > "$TASK_DEF_FILE"

BENCH_TASK_DEFINITION_ARN="$(aws_cmd ecs register-task-definition --cli-input-json "file://${TASK_DEF_FILE}" --query 'taskDefinition.taskDefinitionArn' --output text)"
rm -f "$TASK_DEF_FILE"

for (( shard_index=0; shard_index<WORKERS; shard_index++ )); do
  RUN_TASK_FILE="$(mktemp)"
  "$JQ_BIN" -n \
    --arg cluster "$ECS_CLUSTER_NAME" \
    --arg task_definition "$BENCH_TASK_DEFINITION_ARN" \
    --arg assign_public_ip "$ASSIGN_PUBLIC_IP" \
    --argjson subnets "$SUBNETS_JSON" \
    --argjson security_groups "$SECURITY_GROUPS_JSON" \
    --arg container_name "$BENCH_CONTAINER_NAME" \
    --arg run_id "$RUN_ID" \
    --arg profile_b64 "$PROFILE_B64" \
    --arg shard_index "$shard_index" \
    --arg shard_count "$WORKERS" \
    '{
      cluster: $cluster,
      taskDefinition: $task_definition,
      launchType: "FARGATE",
      count: 1,
      networkConfiguration: {
        awsvpcConfiguration: {
          subnets: $subnets,
          securityGroups: $security_groups,
          assignPublicIp: $assign_public_ip
        }
      },
      overrides: {
        containerOverrides: [
          {
            name: $container_name,
            environment: [
              { name: "BENCH_RUN_ID", value: $run_id },
              { name: "BENCH_PROFILE_B64", value: $profile_b64 },
              { name: "BENCH_SHARD_INDEX", value: $shard_index },
              { name: "BENCH_SHARD_COUNT", value: $shard_count }
            ]
          }
        ]
      }
    }' > "$RUN_TASK_FILE"

  TASK_ARN="$(aws_cmd ecs run-task --cli-input-json "file://${RUN_TASK_FILE}" --query 'tasks[0].taskArn' --output text)"
  rm -f "$RUN_TASK_FILE"

  if [ -z "$TASK_ARN" ] || [ "$TASK_ARN" = "None" ]; then
    log_error "failed to start shard ${shard_index}"
    exit 1
  fi

  TASK_ARNS+=("$TASK_ARN")
  TASK_IDS+=("${TASK_ARN##*/}")
  log_info "started shard ${shard_index}: ${TASK_ARN}"
done

aws_cmd ecs wait tasks-stopped --cluster "$ECS_CLUSTER_NAME" --tasks "${TASK_ARNS[@]}"

DESCRIBED_TASKS_JSON="$(aws_cmd ecs describe-tasks --cluster "$ECS_CLUSTER_NAME" --tasks "${TASK_ARNS[@]}" --output json)"

for task_arn in "${TASK_ARNS[@]}"; do
  task_id="${task_arn##*/}"
  exit_code="$(printf '%s' "$DESCRIBED_TASKS_JSON" | "$JQ_BIN" -r --arg task_arn "$task_arn" '.tasks[] | select(.taskArn == $task_arn) | .containers[0].exitCode // empty')"
  if [ -z "$exit_code" ] || [ "$exit_code" != "0" ]; then
    log_warn "worker task ${task_id} exited with code ${exit_code:-unknown}"
  fi
done

for task_id in "${TASK_IDS[@]}"; do
  log_stream="${LOG_STREAM_PREFIX}/${BENCH_CONTAINER_NAME}/${task_id}"
  log_file="${WORKER_ARTIFACT_DIR}/${task_id}.log"
  artifact_file="${WORKER_ARTIFACT_DIR}/${task_id}.json"
  artifact_line=""

  for _ in $(seq 1 20); do
    if aws_cmd logs get-log-events \
      --log-group-name "$SERVER_LOG_GROUP" \
      --log-stream-name "$log_stream" \
      --output json > "$log_file" 2>/dev/null; then
      artifact_line="$( "$JQ_BIN" -r '.events[].message' "$log_file" | grep '^BENCH_WORKER_ARTIFACT_BASE64=' | tail -n1 || true )"
    fi
    if [ -n "$artifact_line" ]; then
      break
    fi
    sleep 3
  done

  if [ -z "$artifact_line" ]; then
    log_error "failed to retrieve worker artifact from log stream ${log_stream}"
    if [ -f "$log_file" ]; then
      "$JQ_BIN" -r '.events[].message' "$log_file" | tail -n 50 >&2 || true
    fi
    exit 1
  fi

  printf '%s' "${artifact_line#BENCH_WORKER_ARTIFACT_BASE64=}" | base64 -d > "$artifact_file"
  log_info "collected worker artifact ${artifact_file}"
done

AGGREGATE_CMD=(
  "$CARGO_BIN" run
  --manifest-path "$BENCH_MANIFEST"
  --
  aggregate
  --profile "$PROFILE_PATH"
  --run-id "$RUN_ID"
)

for artifact_file in "${WORKER_ARTIFACT_DIR}"/*.json; do
  AGGREGATE_CMD+=(--worker-artifact "$artifact_file")
done

if [ "$NO_CLEANUP" = true ]; then
  AGGREGATE_CMD+=(--no-cleanup)
fi

log_info "aggregating worker artifacts"
"${AGGREGATE_CMD[@]}"

log_info "distributed benchmark complete"
