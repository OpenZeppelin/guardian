#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BENCH_DIR=$(cd "$SCRIPT_DIR/.." && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../../../.." && pwd)
source "$SCRIPT_DIR/lib.sh"

set -a
source "$BENCH_DIR/config/common.env"
source "$BENCH_DIR/config/postgres.env"
set +a

if [[ -n "${PSM_PQ_LIB_DIR:-}" ]] && [[ -d "$PSM_PQ_LIB_DIR" ]]; then
  if [[ -n "${LIBRARY_PATH:-}" ]]; then
    export LIBRARY_PATH="${LIBRARY_PATH}:$PSM_PQ_LIB_DIR"
  else
    export LIBRARY_PATH="$PSM_PQ_LIB_DIR"
  fi

  if [[ -n "${DYLD_FALLBACK_LIBRARY_PATH:-}" ]]; then
    export DYLD_FALLBACK_LIBRARY_PATH="${DYLD_FALLBACK_LIBRARY_PATH}:$PSM_PQ_LIB_DIR"
  else
    export DYLD_FALLBACK_LIBRARY_PATH="$PSM_PQ_LIB_DIR"
  fi
fi

"$SCRIPT_DIR/preflight.sh"

if ! command -v docker >/dev/null 2>&1; then
  echo "docker is required for postgres benchmark"
  exit 1
fi

RUN_ID=$(date +"%Y%m%d_%H%M%S")
RUN_DIR="$BENCH_DIR/results/${RUN_ID}_postgres"
mkdir -p "$RUN_DIR"

if [[ "${BENCH_SKIP_PREBUILD:-false}" != "true" ]]; then
  prebuild_bench_binaries "$REPO_ROOT" postgres
fi

SERVER_BIN="$REPO_ROOT/target/release/server"
LOADGEN_BIN="$REPO_ROOT/target/release/psm-server-bench-loadgen"

cleanup() {
  if [[ -n "${METRICS_PID:-}" ]] && kill -0 "$METRICS_PID" >/dev/null 2>&1; then
    kill "$METRICS_PID" >/dev/null 2>&1 || true
  fi
  if [[ -n "${SERVER_PID:-}" ]] && kill -0 "$SERVER_PID" >/dev/null 2>&1; then
    kill "$SERVER_PID" >/dev/null 2>&1 || true
    wait "$SERVER_PID" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

(
  cd "$REPO_ROOT"
  POSTGRES_PASSWORD="$POSTGRES_PASSWORD" docker compose up -d "$POSTGRES_SERVICE"
)

for _ in $(seq 1 90); do
  if (
    cd "$REPO_ROOT" &&
      POSTGRES_PASSWORD="$POSTGRES_PASSWORD" docker compose exec -T "$POSTGRES_SERVICE" \
        pg_isready -U "$POSTGRES_USER" -d "$POSTGRES_DB" >/dev/null 2>&1
  ); then
    break
  fi
  sleep 1
done

(
  cd "$REPO_ROOT" &&
    POSTGRES_PASSWORD="$POSTGRES_PASSWORD" docker compose exec -T "$POSTGRES_SERVICE" \
      psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c "DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public;"
) >/dev/null

rm -rf "$PSM_KEYSTORE_PATH"
mkdir -p "$PSM_KEYSTORE_PATH"

(
  cd "$REPO_ROOT"
  exec env \
    RUST_LOG="$BENCH_SERVER_LOG_LEVEL" \
    PSM_NETWORK_TYPE="$PSM_NETWORK_TYPE" \
    PSM_RATE_BURST_PER_SEC="$PSM_RATE_BURST_PER_SEC" \
    PSM_RATE_PER_MIN="$PSM_RATE_PER_MIN" \
    PSM_MAX_REQUEST_BYTES="$PSM_MAX_REQUEST_BYTES" \
    PSM_CANONICALIZATION_ENABLED="$PSM_CANONICALIZATION_ENABLED" \
    PSM_CANONICALIZATION_CHECK_INTERVAL_SECS="$PSM_CANONICALIZATION_CHECK_INTERVAL_SECS" \
    PSM_CANONICALIZATION_MAX_RETRIES="$PSM_CANONICALIZATION_MAX_RETRIES" \
    PSM_KEYSTORE_PATH="$PSM_KEYSTORE_PATH" \
    DATABASE_URL="$DATABASE_URL" \
    "$SERVER_BIN" >"$RUN_DIR/server.log" 2>&1
) &
SERVER_PID=$!

if ! wait_for_server_ready "$SERVER_PID" localhost "$PSM_GRPC_PORT" "${PSM_SERVER_START_TIMEOUT_SECS:-600}" "$RUN_DIR/server.log"; then
  exit 1
fi

"$SCRIPT_DIR/collect_server_metrics.sh" "$SERVER_PID" "$RUN_DIR/server_metrics.csv" "$BENCH_SAMPLE_INTERVAL_SECS" &
METRICS_PID=$!

SCENARIOS=(state-read state-write mixed)
if [[ "${BENCH_ENABLE_CANONICALIZATION:-false}" == "true" ]]; then
  SCENARIOS+=(canonicalization)
fi
if [[ -n "${BENCH_SCENARIOS:-}" ]]; then
  IFS=',' read -r -a SCENARIOS <<<"$BENCH_SCENARIOS"
fi

for scenario in "${SCENARIOS[@]}"; do
  LOADGEN_ARGS=(
    --psm-endpoint "http://localhost:$PSM_GRPC_PORT"
    --psm-http-endpoint "http://localhost:$PSM_HTTP_PORT"
    --transport "$BENCH_TRANSPORT"
    --users "$BENCH_USERS"
    --accounts "$BENCH_ACCOUNTS"
    --signers-per-account "$BENCH_SIGNERS_PER_ACCOUNT"
    --auth-scheme "$BENCH_AUTH_SCHEME"
    --ops-per-user "$BENCH_OPS_PER_USER"
    --scenario "$scenario"
    --mixed-write-percent "$BENCH_MIXED_WRITE_PERCENT"
    --state-sync-reads-per-push "$BENCH_STATE_SYNC_READS_PER_PUSH"
    --output "$RUN_DIR/loadgen_${scenario}.json"
  )
  if [[ "$scenario" == "canonicalization" ]]; then
    LOADGEN_ARGS+=(
      --canonicalization-poll-interval-ms "$BENCH_CANONICALIZATION_POLL_INTERVAL_MS"
      --canonicalization-timeout-secs "$BENCH_CANONICALIZATION_TIMEOUT_SECS"
    )
  fi
  (
    cd "$REPO_ROOT"
    "$LOADGEN_BIN" "${LOADGEN_ARGS[@]}" >"$RUN_DIR/loadgen_${scenario}.log" 2>&1
  )
  echo "loadgen scenario complete: $scenario"
done

if command -v k6 >/dev/null 2>&1; then
  K6_BASE=(k6 run --env PSM_HTTP_URL="http://localhost:$PSM_HTTP_PORT")
  "${K6_BASE[@]}" "$BENCH_DIR/k6/body_limit.js" >"$RUN_DIR/k6_body_limit.log" 2>&1 || true
  "${K6_BASE[@]}" "$BENCH_DIR/k6/rate_limit.js" >"$RUN_DIR/k6_rate_limit.log" 2>&1 || true
fi

"$SCRIPT_DIR/collect_pg_metrics.sh" "$RUN_DIR/pg_metrics.txt"
"$SCRIPT_DIR/summarize.sh" "$RUN_DIR"

echo "results=$RUN_DIR"
