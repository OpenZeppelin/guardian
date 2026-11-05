#!/bin/bash
# Temporary benchmark environment next to the devnet Guardian, outside Terraform.
#
#   up      build the current HEAD, or with BENCH_IMAGE_SOURCE=worktree the
#           working tree including uncommitted and untracked files
#           (canonicalization off), create the bench database, security
#           groups, service discovery and ECS service
#   status  show every bench resource and the endpoint
#   down    delete everything `up` created; `down --dry-run` lists what it
#           would delete or skip and changes nothing
#
# The bench service runs in the devnet cluster with its own RDS instance, so
# the devnet service, its database and its Terraform state are untouched.
# Every resource is tagged purpose=guardian-bench. The bench signs with its own
# ACK identity (generated once, kept in a state directory per AWS account,
# region and BENCH_NAME), never devnet's
# Secrets Manager keys or KMS key. The bench task gets no task role, no
# inherited container secrets and no ECS exec, so nothing running in it can
# reach devnet's secrets or KMS key. BENCH_NAME must contain "bench" and must
# not share a prefix with the source service.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

AWS_PROFILE_NAME="${AWS_PROFILE_NAME:-terraform}"
AWS_REGION_NAME="${AWS_REGION_NAME:-us-east-1}"
CLUSTER="${BENCH_CLUSTER:-guardian-cluster}"
SOURCE_SERVICE="${BENCH_SOURCE_SERVICE:-guardian-server}"
SOURCE_CONTAINER="${BENCH_SOURCE_CONTAINER:-guardian-server}"
SOURCE_DB="${BENCH_SOURCE_DB:-guardian-postgres}"
NAME="${BENCH_NAME:-guardian-bench}"
DB_CLASS="${BENCH_DB_CLASS:-db.m7g.large}"
DB_MULTI_AZ="${BENCH_DB_MULTI_AZ:-false}"
DB_STORAGE_GB="${BENCH_DB_STORAGE_GB:-100}"
TASKS="${BENCH_TASKS:-2}"
TASK_CPU="${BENCH_TASK_CPU:-2048}"
TASK_MEMORY="${BENCH_TASK_MEMORY:-4096}"
POOL_SIZE="${BENCH_POOL_SIZE:-32}"
BENCH_IMAGE_SOURCE="${BENCH_IMAGE_SOURCE:-head}"
STATE_ROOT="${GUARDIAN_BENCH_STATE_DIR:-$HOME/.guardian-bench}"

DB_ID="${NAME}-pg"
SERVER_SG_NAME="${NAME}-server-sg"
DB_SG_NAME="${NAME}-db-sg"
ECR_REPO="${NAME}"
NAMESPACE="${NAME}.local"
DISCOVERY_SERVICE="server"
ENDPOINT="http://${DISCOVERY_SERVICE}.${NAMESPACE}:50051"
TAGS_EC2="ResourceType=security-group,Tags=[{Key=purpose,Value=${NAME}}]"
LOG_GROUP="/ecs/${NAME}"

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }
die() { log "error: $*"; exit 1; }
aws_cmd() { aws --profile "$AWS_PROFILE_NAME" --region "$AWS_REGION_NAME" "$@"; }

require_bench_name() {
  [[ "$NAME" =~ ^[a-z0-9-]+$ ]] || die "BENCH_NAME ${NAME} must be lowercase letters, digits and dashes"
  [[ "$NAME" == *bench* ]] || die "BENCH_NAME ${NAME} must contain 'bench'"
  case "$SOURCE_SERVICE" in
    "$NAME"*) die "BENCH_NAME ${NAME} is a prefix of the source service ${SOURCE_SERVICE}" ;;
  esac
  case "$NAME" in
    "$SOURCE_SERVICE"*) die "the source service ${SOURCE_SERVICE} is a prefix of BENCH_NAME ${NAME}" ;;
  esac
}

require_tools() {
  for tool in aws jq docker git openssl; do
    command -v "$tool" >/dev/null || die "$tool is required"
  done
  local account
  account="$(aws_cmd sts get-caller-identity --query Account --output text)" \
    || die "AWS auth failed for profile ${AWS_PROFILE_NAME}"
  STATE_DIR="${STATE_ROOT}/${account}-${AWS_REGION_NAME}-${NAME}"
  PASSWORD_FILE="${STATE_DIR}/db-password"
  ACK_KEYS_FILE="${STATE_DIR}/ack-keys.json"
}

source_service_json() {
  aws_cmd ecs describe-services --cluster "$CLUSTER" --services "$SOURCE_SERVICE" \
    --query 'services[0]' --output json
}

vpc_id() {
  aws_cmd rds describe-db-instances --db-instance-identifier "$SOURCE_DB" \
    --query 'DBInstances[0].DBSubnetGroup.VpcId' --output text
}

sg_id() {
  aws_cmd ec2 describe-security-groups \
    --filters "Name=group-name,Values=$1" "Name=vpc-id,Values=$(vpc_id)" \
    --query 'SecurityGroups[0].GroupId' --output text 2>/dev/null | grep -v '^None$' || true
}

namespace_id() {
  aws_cmd servicediscovery list-namespaces \
    --query "Namespaces[?Name=='${NAMESPACE}'].Id | [0]" --output text | grep -v '^None$' || true
}

purpose_of() {
  local id arn
  case "$1" in
    service)
      [ "$(service_status)" = "ACTIVE" ] || { echo absent; return; }
      aws_cmd ecs describe-services --cluster "$CLUSTER" --services "$NAME" --include TAGS \
        --query "services[0].tags[?key=='purpose'].value | [0]" --output text ;;
    database)
      [ "$(db_status)" != "absent" ] || { echo absent; return; }
      aws_cmd rds describe-db-instances --db-instance-identifier "$DB_ID" \
        --query "DBInstances[0].TagList[?Key=='purpose'].Value | [0]" --output text ;;
    server-sg|db-sg)
      id="$(sg_id "$([ "$1" = server-sg ] && echo "$SERVER_SG_NAME" || echo "$DB_SG_NAME")")"
      [ -n "$id" ] || { echo absent; return; }
      aws_cmd ec2 describe-security-groups --group-ids "$id" \
        --query "SecurityGroups[0].Tags[?Key=='purpose'].Value | [0]" --output text ;;
    ecr)
      arn="$(aws_cmd ecr describe-repositories --repository-names "$ECR_REPO" \
        --query 'repositories[0].repositoryArn' --output text 2>/dev/null)" || { echo absent; return; }
      aws_cmd ecr list-tags-for-resource --resource-arn "$arn" \
        --query "tags[?Key=='purpose'].Value | [0]" --output text ;;
    log-group)
      [ "$(aws_cmd logs describe-log-groups --log-group-name-prefix "$LOG_GROUP" \
        --query "logGroups[?logGroupName=='${LOG_GROUP}'] | length(@)" --output text)" != "0" ] \
        || { echo absent; return; }
      aws_cmd logs list-tags-log-group --log-group-name "$LOG_GROUP" --query 'tags.purpose' --output text ;;
    namespace)
      id="$(namespace_id)"
      [ -n "$id" ] || { echo absent; return; }
      arn="$(aws_cmd servicediscovery get-namespace --id "$id" --query Namespace.Arn --output text)"
      aws_cmd servicediscovery list-tags-for-resource --resource-arn "$arn" \
        --query "Tags[?Key=='purpose'].Value | [0]" --output text ;;
  esac
}

owned() { [ "$(purpose_of "$1")" = "$NAME" ]; }

require_owned_or_absent() {
  local kind purpose foreign=()
  for kind in service database server-sg db-sg ecr log-group namespace; do
    purpose="$(purpose_of "$kind")"
    [ "$purpose" = absent ] || [ "$purpose" = "$NAME" ] || foreign+=("$kind")
  done
  [ "${#foreign[@]}" -eq 0 ] \
    || die "not tagged purpose=${NAME}, so not created by this script: ${foreign[*]}; set BENCH_NAME to another name"
}

skip_foreign() {
  owned "$1" && return 1
  log "skipping ${1}: it exists but is not tagged purpose=${NAME}"
}

discovery_service_id() {
  local ns="$1"
  [ -n "$ns" ] || return 0
  aws_cmd servicediscovery list-services \
    --filters "Name=NAMESPACE_ID,Values=${ns},Condition=EQ" \
    --query "Services[?Name=='${DISCOVERY_SERVICE}'].Id | [0]" --output text | grep -v '^None$' || true
}

wait_operation() {
  local op="$1" status
  for _ in $(seq 1 60); do
    status="$(aws_cmd servicediscovery get-operation --operation-id "$op" --query 'Operation.Status' --output text)"
    case "$status" in
      SUCCESS) return 0 ;;
      FAIL) die "service discovery operation $op failed" ;;
    esac
    sleep 5
  done
  die "service discovery operation $op timed out"
}

db_status() {
  local out
  if out="$(aws_cmd rds describe-db-instances --db-instance-identifier "$DB_ID" \
      --query 'DBInstances[0].DBInstanceStatus' --output text 2>&1)"; then
    echo "$out"
  elif grep -q DBInstanceNotFound <<<"$out"; then
    echo "absent"
  else
    die "cannot read database ${DB_ID}: ${out}"
  fi
}

service_status() {
  aws_cmd ecs describe-services --cluster "$CLUSTER" --services "$NAME" \
    --query 'services[0].status' --output text 2>/dev/null | grep -v '^None$' || echo "absent"
}

create_security_groups() {
  local vpc server_sg db_sg
  vpc="$(vpc_id)"
  server_sg="$(sg_id "$SERVER_SG_NAME")"
  if [ -z "$server_sg" ]; then
    log "creating security group ${SERVER_SG_NAME}"
    server_sg="$(aws_cmd ec2 create-security-group --group-name "$SERVER_SG_NAME" \
      --description "Guardian bench server and load workers" --vpc-id "$vpc" \
      --tag-specifications "$TAGS_EC2" --query GroupId --output text)"
    aws_cmd ec2 authorize-security-group-ingress --group-id "$server_sg" \
      --ip-permissions "IpProtocol=tcp,FromPort=50051,ToPort=50051,UserIdGroupPairs=[{GroupId=${server_sg}}]" \
      "IpProtocol=tcp,FromPort=3000,ToPort=3000,UserIdGroupPairs=[{GroupId=${server_sg}}]" >/dev/null
  fi
  db_sg="$(sg_id "$DB_SG_NAME")"
  if [ -z "$db_sg" ]; then
    log "creating security group ${DB_SG_NAME}"
    db_sg="$(aws_cmd ec2 create-security-group --group-name "$DB_SG_NAME" \
      --description "Guardian bench database" --vpc-id "$vpc" \
      --tag-specifications "$TAGS_EC2" --query GroupId --output text)"
    aws_cmd ec2 authorize-security-group-ingress --group-id "$db_sg" \
      --ip-permissions "IpProtocol=tcp,FromPort=5432,ToPort=5432,UserIdGroupPairs=[{GroupId=${server_sg}}]" >/dev/null
  fi
}

create_database() {
  if [ "$(db_status)" != "absent" ]; then
    log "database ${DB_ID} exists ($(db_status))"
  else
    mkdir -p "$STATE_DIR" && chmod 700 "$STATE_DIR"
    [ -s "$PASSWORD_FILE" ] || (umask 077 && openssl rand -hex 24 > "$PASSWORD_FILE")
    local subnet_group engine_version multi_az
    subnet_group="$(aws_cmd rds describe-db-instances --db-instance-identifier "$SOURCE_DB" \
      --query 'DBInstances[0].DBSubnetGroup.DBSubnetGroupName' --output text)"
    engine_version="$(aws_cmd rds describe-db-instances --db-instance-identifier "$SOURCE_DB" \
      --query 'DBInstances[0].EngineVersion' --output text)"
    multi_az="--no-multi-az"
    [ "$DB_MULTI_AZ" = "true" ] && multi_az="--multi-az"
    log "creating ${DB_CLASS} (${multi_az#--}) Postgres ${engine_version} as ${DB_ID}; takes 15 to 20 minutes"
    aws_cmd rds create-db-instance \
      --db-instance-identifier "$DB_ID" \
      --engine postgres --engine-version "$engine_version" \
      --db-instance-class "$DB_CLASS" "$multi_az" \
      --storage-type gp3 --allocated-storage "$DB_STORAGE_GB" --storage-encrypted \
      --db-name guardian --master-username guardian \
      --master-user-password "$(cat "$PASSWORD_FILE")" \
      --db-subnet-group-name "$subnet_group" \
      --vpc-security-group-ids "$(sg_id "$DB_SG_NAME")" \
      --no-publicly-accessible --backup-retention-period 0 --no-deletion-protection \
      --enable-performance-insights --performance-insights-retention-period 7 \
      --tags "Key=purpose,Value=${NAME}" >/dev/null
  fi
  log "waiting for ${DB_ID} to become available"
  aws_cmd rds wait db-instance-available --db-instance-identifier "$DB_ID"
}

database_url() {
  [ -s "$PASSWORD_FILE" ] || die "missing ${PASSWORD_FILE}; it is written when ${DB_ID} is created"
  local host
  host="$(aws_cmd rds describe-db-instances --db-instance-identifier "$DB_ID" \
    --query 'DBInstances[0].Endpoint.Address' --output text)"
  printf 'postgres://guardian:%s@%s:5432/guardian?sslmode=require' "$(cat "$PASSWORD_FILE")" "$host"
}

worktree_files() {
  git -C "$ROOT_DIR" ls-files -c -o --exclude-standard \
    | grep -vxF -f <(git -C "$ROOT_DIR" ls-files -d; echo /dev/null/none)
}

worktree_digest() {
  (cd "$ROOT_DIR" && worktree_files | LC_ALL=C sort | tr '\n' '\0' | xargs -0 shasum -a 256) \
    | shasum -a 256 | cut -c1-8
}

build_image() {
  local arch="$1" sha repo_uri account src platform
  sha="$(git -C "$ROOT_DIR" rev-parse --short HEAD)"
  case "$BENCH_IMAGE_SOURCE" in
    head)
      if [ -n "$(git -C "$ROOT_DIR" status --porcelain -- crates Cargo.toml Cargo.lock Dockerfile)" ]; then
        log "warning: uncommitted server changes are not in the image; it is built from HEAD ${sha} (BENCH_IMAGE_SOURCE=worktree includes them)"
      fi
      ;;
    worktree) sha="${sha}-wt$(worktree_digest)" ;;
    *) die "BENCH_IMAGE_SOURCE must be head or worktree, got ${BENCH_IMAGE_SOURCE}" ;;
  esac
  account="$(aws_cmd sts get-caller-identity --query Account --output text)"
  repo_uri="${account}.dkr.ecr.${AWS_REGION_NAME}.amazonaws.com/${ECR_REPO}"
  IMAGE_URI="${repo_uri}:bench-${sha}"
  case "$arch" in
    ARM64) platform="linux/arm64" ;;
    X86_64) platform="linux/amd64" ;;
    *) die "unknown CPU architecture ${arch}" ;;
  esac
  BUILT_PLATFORM="$platform"
  if aws_cmd ecr describe-images --repository-name "$ECR_REPO" --image-ids "imageTag=bench-${sha}" >/dev/null 2>&1; then
    log "image ${IMAGE_URI} already pushed"
    aws_cmd ecr get-login-password | docker login --username AWS --password-stdin "${repo_uri%%/*}" >/dev/null
    return
  fi
  aws_cmd ecr describe-repositories --repository-names "$ECR_REPO" >/dev/null 2>&1 \
    || aws_cmd ecr create-repository --repository-name "$ECR_REPO" \
      --tags "Key=purpose,Value=${NAME}" >/dev/null

  src="$(mktemp -d)"
  if [ "$BENCH_IMAGE_SOURCE" = worktree ]; then
    worktree_files | (cd "$ROOT_DIR" && tar -cf - -T -) | tar -x -C "$src"
  else
    git -C "$ROOT_DIR" archive HEAD | tar -x -C "$src"
  fi
  sed -i.orig 's/\.with_canonicalization(Some(/.with_canonicalization(std::env::var("GUARDIAN_BENCH_NO_CANONICALIZATION").is_err().then_some(/' \
    "$src/crates/server/src/main.rs"
  grep -q 'GUARDIAN_BENCH_NO_CANONICALIZATION' "$src/crates/server/src/main.rs" \
    || die "the canonicalization switch patch did not apply to crates/server/src/main.rs"

  log "building ${IMAGE_URI} (${platform}) from ${BENCH_IMAGE_SOURCE} ${sha} with canonicalization switchable off"
  docker build --platform "$platform" --target server-runner \
    --build-arg GUARDIAN_SERVER_FEATURES=postgres --build-arg "GUARDIAN_GIT_SHA=${sha}" \
    -t "$IMAGE_URI" "$src"
  aws_cmd ecr get-login-password | docker login --username AWS --password-stdin "${repo_uri%%/*}" >/dev/null
  docker push "$IMAGE_URI" >/dev/null
  rm -rf "$src"
}

ack_keys() {
  if [ ! -s "$ACK_KEYS_FILE" ]; then
    log "generating a bench-only ACK identity in ${ACK_KEYS_FILE}"
    mkdir -p "$STATE_DIR"
    (umask 077 && docker run --rm --platform "$1" "$IMAGE_URI" /app/ack-keygen > "$ACK_KEYS_FILE")
  fi
  jq -e '.falcon_secret_key and .ecdsa_secret_key' "$ACK_KEYS_FILE" >/dev/null \
    || die "${ACK_KEYS_FILE} does not hold an ACK identity; delete it and run up again"
}

create_log_group() {
  if [ "$(aws_cmd logs describe-log-groups --log-group-name-prefix "$LOG_GROUP" \
      --query "logGroups[?logGroupName=='${LOG_GROUP}'] | length(@)" --output text)" = "0" ]; then
    log "creating log group ${LOG_GROUP}"
    aws_cmd logs create-log-group --log-group-name "$LOG_GROUP" --tags "purpose=${NAME}"
    aws_cmd logs put-retention-policy --log-group-name "$LOG_GROUP" --retention-in-days 7
  fi
}

create_discovery() {
  local ns op svc
  ns="$(namespace_id)"
  if [ -z "$ns" ]; then
    log "creating private DNS namespace ${NAMESPACE}"
    op="$(aws_cmd servicediscovery create-private-dns-namespace --name "$NAMESPACE" --vpc "$(vpc_id)" \
      --tags "Key=purpose,Value=${NAME}" --query OperationId --output text)"
    wait_operation "$op"
    ns="$(namespace_id)"
  fi
  svc="$(discovery_service_id "$ns")"
  if [ -z "$svc" ]; then
    log "creating discovery service ${DISCOVERY_SERVICE}.${NAMESPACE}"
    aws_cmd servicediscovery create-service --name "$DISCOVERY_SERVICE" --namespace-id "$ns" \
      --dns-config "RoutingPolicy=MULTIVALUE,DnsRecords=[{Type=A,TTL=10}]" \
      --health-check-custom-config FailureThreshold=1 \
      --tags "Key=purpose,Value=${NAME}" >/dev/null
  fi
  DISCOVERY_ARN="$(aws_cmd servicediscovery get-service --id "$(discovery_service_id "$ns")" \
    --query 'Service.Arn' --output text)"
}

register_task_definition() {
  local source_td="$1" image="$2" url="$3" file
  file="$(mktemp)"
  aws_cmd ecs describe-task-definition --task-definition "$source_td" --output json | jq \
    --arg family "$NAME" --arg image "$image" --arg url "$url" \
    --arg container "$SOURCE_CONTAINER" --arg cpu "$TASK_CPU" --arg memory "$TASK_MEMORY" \
    --arg pool "$POOL_SIZE" --arg replicas "$TASKS" --arg tag "$NAME" --arg log_group "$LOG_GROUP" \
    --arg ack_falcon "$(jq -r .falcon_secret_key "$ACK_KEYS_FILE")" \
    --arg ack_ecdsa "$(jq -r .ecdsa_secret_key "$ACK_KEYS_FILE")" '
    .taskDefinition as $td
    | ($td.containerDefinitions[] | select(.name == $container)) as $c
    | {
        "DATABASE_URL": $url,
        "GUARDIAN_BENCH_NO_CANONICALIZATION": "1",
        "GUARDIAN_RATE_LIMIT_ENABLED": "false",
        "GUARDIAN_PROVING_ENABLED": "false",
        "GUARDIAN_RELEASE_SWEEP_ENABLED": "false",
        "GUARDIAN_DB_POOL_MAX_SIZE": $pool,
        "GUARDIAN_METADATA_DB_POOL_MAX_SIZE": $pool,
        "GUARDIAN_MAX_REPLICAS": $replicas,
        "GUARDIAN_ACK_SECRET_PROVIDER": "file",
        "GUARDIAN_ACK_FALCON_SECRET_PATH": "/tmp/ack/ack-falcon-secret-key",
        "GUARDIAN_ACK_ECDSA_SECRET_PATH": "/tmp/ack/ack-ecdsa-secret-key",
        "BENCH_ACK_FALCON_SECRET_KEY": $ack_falcon,
        "BENCH_ACK_ECDSA_SECRET_KEY": $ack_ecdsa
      } as $overrides
    | ["GUARDIAN_ACK_ECDSA_BACKEND", "GUARDIAN_ACK_ECDSA_KMS_KEY_ID",
       "GUARDIAN_ACK_FALCON_SECRET_ID", "GUARDIAN_ACK_ECDSA_SECRET_ID"] as $dropped
    | {
        family: $family,
        networkMode: "awsvpc",
        requiresCompatibilities: ["FARGATE"],
        cpu: $cpu,
        memory: $memory,
        executionRoleArn: $td.executionRoleArn,
        runtimePlatform: $td.runtimePlatform,
        tags: [{key: "purpose", value: $tag}],
        containerDefinitions: [
          ($c
            | .image = $image
            | .environment = ([(.environment // [])[]
                  | select(.name as $n | ($overrides | has($n) | not) and ($dropped | index($n) | not))]
                + ($overrides | to_entries | map({name: .key, value: .value})))
            | .secrets = []
            | .logConfiguration.options["awslogs-group"] = $log_group
            | .logConfiguration.options["awslogs-stream-prefix"] = $family
            | .entryPoint = ["/bin/sh", "-c"]
            | .command = ["umask 077 && mkdir -p /tmp/ack && printf %s \"$BENCH_ACK_FALCON_SECRET_KEY\" > /tmp/ack/ack-falcon-secret-key && printf %s \"$BENCH_ACK_ECDSA_SECRET_KEY\" > /tmp/ack/ack-ecdsa-secret-key && exec /app/server"]
            | del(.dependsOn))
        ]
      }' > "$file"
  TASK_DEFINITION_ARN="$(aws_cmd ecs register-task-definition --cli-input-json "file://${file}" \
    --query 'taskDefinition.taskDefinitionArn' --output text)"
  rm -f "$file"
}

up() {
  require_tools
  require_owned_or_absent
  local source arch source_td subnets public_ip
  source="$(source_service_json)"
  source_td="$(jq -r '.taskDefinition' <<<"$source")"
  subnets="$(jq -c '.networkConfiguration.awsvpcConfiguration.subnets' <<<"$source")"
  public_ip="$(jq -r '.networkConfiguration.awsvpcConfiguration.assignPublicIp' <<<"$source")"
  arch="$(aws_cmd ecs describe-task-definition --task-definition "$source_td" \
    --query 'taskDefinition.runtimePlatform.cpuArchitecture' --output text)"

  create_security_groups
  create_database
  build_image "$arch"
  ack_keys "$BUILT_PLATFORM"
  create_discovery
  create_log_group
  register_task_definition "$source_td" "$IMAGE_URI" "$(database_url)"

  local network
  network="$(jq -nc --argjson subnets "$subnets" --arg sg "$(sg_id "$SERVER_SG_NAME")" --arg ip "$public_ip" \
    '{awsvpcConfiguration: {subnets: $subnets, securityGroups: [$sg], assignPublicIp: $ip}}')"
  if [ "$(service_status)" = "ACTIVE" ]; then
    log "updating service ${NAME} to ${TASK_DEFINITION_ARN##*/}"
    aws_cmd ecs update-service --cluster "$CLUSTER" --service "$NAME" \
      --task-definition "$TASK_DEFINITION_ARN" --desired-count "$TASKS" --force-new-deployment >/dev/null
  else
    log "creating service ${NAME} with ${TASKS} tasks"
    aws_cmd ecs create-service --cluster "$CLUSTER" --service-name "$NAME" \
      --task-definition "$TASK_DEFINITION_ARN" --desired-count "$TASKS" --launch-type FARGATE \
      --network-configuration "$network" \
      --service-registries "registryArn=${DISCOVERY_ARN}" \
      --propagate-tags SERVICE \
      --tags "key=purpose,value=${NAME}" >/dev/null
  fi
  log "waiting for ${NAME} to become stable"
  aws_cmd ecs wait services-stable --cluster "$CLUSTER" --services "$NAME"
  status
}

status() {
  local ns svc
  ns="$(namespace_id)"
  svc="$(discovery_service_id "$ns")"
  printf 'cluster          %s\n' "$CLUSTER"
  printf 'service          %s %s\n' "$NAME" "$(aws_cmd ecs describe-services --cluster "$CLUSTER" --services "$NAME" \
    --query 'services[0].[status,runningCount,desiredCount,taskDefinition]' --output text 2>/dev/null || echo absent)"
  printf 'database         %s %s %s\n' "$DB_ID" "$(db_status)" "$(aws_cmd rds describe-db-instances --db-instance-identifier "$DB_ID" \
    --query 'DBInstances[0].[DBInstanceClass,MultiAZ,StorageType]' --output text 2>/dev/null || true)"
  printf 'security groups  %s=%s %s=%s\n' "$SERVER_SG_NAME" "$(sg_id "$SERVER_SG_NAME")" "$DB_SG_NAME" "$(sg_id "$DB_SG_NAME")"
  printf 'discovery        namespace=%s service=%s\n' "${ns:-absent}" "${svc:-absent}"
  printf 'ecr              %s\n' "$(aws_cmd ecr describe-images --repository-name "$ECR_REPO" \
    --query 'imageDetails[].imageTags[]' --output text 2>/dev/null || echo absent)"
  printf 'log group        %s\n' "$LOG_GROUP"
  printf 'endpoint         %s\n' "$ENDPOINT"
}

bench_task_definitions() {
  local arn
  for arn in $(aws_cmd ecs list-task-definitions --family-prefix "$NAME" --status ACTIVE \
      --query 'taskDefinitionArns[]' --output text); do
    [[ "$arn" == *:task-definition/"${NAME}":* ]] && echo "$arn"
  done
  return 0
}

task_definition_purpose() {
  aws_cmd ecs describe-task-definition --task-definition "$1" --include TAGS \
    --query "tags[?key=='purpose'].value | [0]" --output text
}

down_plan() {
  require_tools
  local kind label purpose arn file
  printf 'down would, in this order:\n'
  for kind in service database namespace db-sg server-sg ecr log-group; do
    case "$kind" in
      service) label="ECS service ${NAME} in ${CLUSTER}" ;;
      database) label="RDS instance ${DB_ID} (no final snapshot)" ;;
      namespace) label="Cloud Map namespace ${NAMESPACE} and its service ${DISCOVERY_SERVICE}" ;;
      db-sg) label="security group ${DB_SG_NAME} $(sg_id "$DB_SG_NAME")" ;;
      server-sg) label="security group ${SERVER_SG_NAME} $(sg_id "$SERVER_SG_NAME")" ;;
      ecr) label="ECR repository ${ECR_REPO} with every image" ;;
      log-group) label="log group ${LOG_GROUP}" ;;
    esac
    purpose="$(purpose_of "$kind")"
    if [ "$purpose" = absent ]; then
      printf '  %-7s %s\n' "absent" "$label"
    elif [ "$purpose" = "$NAME" ]; then
      printf '  %-7s %s\n' "DELETE" "$label"
    else
      printf '  %-7s %s (tag purpose=%s)\n' "skip" "$label" "$purpose"
    fi
    if [ "$kind" = service ]; then
      for arn in $(bench_task_definitions); do
        purpose="$(task_definition_purpose "$arn")"
        if [ "$purpose" = "$NAME" ]; then
          printf '  %-7s task definition %s\n' "DEREG" "${arn##*/}"
        else
          printf '  %-7s task definition %s (tag purpose=%s)\n' "skip" "${arn##*/}" "$purpose"
        fi
      done
    fi
  done
  for file in "$PASSWORD_FILE" "$ACK_KEYS_FILE"; do
    [ -e "$file" ] || continue
    if [ "$(purpose_of database)" = "$NAME" ] || [ "$(purpose_of database)" = absent ]; then
      printf '  %-7s local file %s\n' "DELETE" "$file"
    else
      printf '  %-7s local file %s (database not owned)\n' "keep" "$file"
    fi
  done
  printf 'nothing was changed\n'
}

down() {
  require_tools
  if [ "$(purpose_of service)" != absent ] && ! skip_foreign service; then
    log "deleting service ${NAME}"
    aws_cmd ecs update-service --cluster "$CLUSTER" --service "$NAME" --desired-count 0 >/dev/null
    aws_cmd ecs delete-service --cluster "$CLUSTER" --service "$NAME" --force >/dev/null
    aws_cmd ecs wait services-inactive --cluster "$CLUSTER" --services "$NAME"
  fi
  for arn in $(bench_task_definitions); do
    if [ "$(task_definition_purpose "$arn")" = "$NAME" ]; then
      aws_cmd ecs deregister-task-definition --task-definition "$arn" >/dev/null
    else
      log "skipping task definition ${arn##*/}: it is not tagged purpose=${NAME}"
    fi
  done

  if [ "$(purpose_of database)" != absent ] && ! skip_foreign database; then
    log "deleting database ${DB_ID} (no final snapshot)"
    aws_cmd rds delete-db-instance --db-instance-identifier "$DB_ID" \
      --skip-final-snapshot --delete-automated-backups >/dev/null 2>&1 || true
    aws_cmd rds wait db-instance-deleted --db-instance-identifier "$DB_ID"
  fi

  local ns svc op
  ns=""
  if [ "$(purpose_of namespace)" != absent ] && ! skip_foreign namespace; then
    ns="$(namespace_id)"
  fi
  svc="$(discovery_service_id "$ns")"
  if [ -n "$svc" ]; then
    log "deleting discovery service"
    for _ in $(seq 1 30); do
      aws_cmd servicediscovery delete-service --id "$svc" >/dev/null 2>&1 && break
      sleep 10
    done
  fi
  if [ -n "$ns" ]; then
    log "deleting namespace ${NAMESPACE}"
    op="$(aws_cmd servicediscovery delete-namespace --id "$ns" --query OperationId --output text)"
    wait_operation "$op"
  fi

  for kind in db-sg server-sg; do
    local id name
    name="$([ "$kind" = server-sg ] && echo "$SERVER_SG_NAME" || echo "$DB_SG_NAME")"
    [ "$(purpose_of "$kind")" != absent ] || continue
    skip_foreign "$kind" && continue
    id="$(sg_id "$name")"
    log "deleting security group ${name}"
    for _ in $(seq 1 30); do
      aws_cmd ec2 delete-security-group --group-id "$id" >/dev/null 2>&1 && break
      sleep 10
    done
  done

  if [ "$(purpose_of ecr)" != absent ] && ! skip_foreign ecr; then
    log "deleting ECR repository ${ECR_REPO}"
    aws_cmd ecr delete-repository --repository-name "$ECR_REPO" --force >/dev/null
  fi
  if [ "$(purpose_of log-group)" != absent ] && ! skip_foreign log-group; then
    log "deleting log group ${LOG_GROUP}"
    aws_cmd logs delete-log-group --log-group-name "$LOG_GROUP"
  fi
  if [ "$(purpose_of database)" = absent ]; then
    rm -f "$PASSWORD_FILE" "$ACK_KEYS_FILE"
    rmdir "$STATE_DIR" 2>/dev/null || true
  else
    log "keeping ${STATE_DIR}: database ${DB_ID} still exists"
  fi

  log "remaining resources tagged purpose=${NAME}:"
  aws_cmd resourcegroupstaggingapi get-resources --tag-filters "Key=purpose,Values=${NAME}" \
    --query 'ResourceTagMappingList[].ResourceARN' --output text
}

require_bench_name
case "${1:-}" in
  up) up ;;
  down)
    case "${2:-}" in
      --dry-run) down_plan ;;
      "") down ;;
      *) echo "usage: $0 down [--dry-run]" >&2; exit 2 ;;
    esac ;;
  status) require_tools; status ;;
  *) echo "usage: $0 up|status|down [--dry-run]" >&2; exit 2 ;;
esac
