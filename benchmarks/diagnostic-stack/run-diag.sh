#!/usr/bin/env bash
# Run one diagnostic leg and capture everything needed to attribute the result.
#
#   ./run-diag.sh <label> [profile]
#
# Example one-variable sweep (FR-017d — change exactly one thing per leg):
#   docker compose --env-file .env --env-file variants/pool-16.env up -d
#   ./run-diag.sh pool-16
#   docker compose --env-file .env --env-file variants/pool-64.env up -d
#   ./run-diag.sh pool-64
#
# Captures per leg: build identity before and after (FR-A08), harness artifact,
# Prometheus snapshot, and pg_stat_statements. A leg missing any of these cannot
# support a bottleneck claim.

set -euo pipefail

LABEL="${1:?usage: ./run-diag.sh <label> [profile]}"
PROFILE="${2:-profiles/diag-read.toml}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
STACK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="${STACK_DIR}/results/${LABEL}-${STAMP}"
mkdir -p "${OUT}"

HTTP_A="http://127.0.0.1:3000"
PROM="http://127.0.0.1:9090"

# Server-side evidence. This is the half production cannot give us, and the half
# a bottleneck claim has to cite (FR-017c). Three of these answer questions the
# authoritative tier structurally cannot:
#   guardian_db_pool_pending_acquires        -> is the connection pool the constraint?
#   guardian_grpc_request_duration_seconds   -> server service time, vs the harness's
#                                               client round trip (FR-004)
#   guardian_miden_rpc_duration_seconds      -> chain time, separable from Guardian
#                                               time in the write path (FR-005)
#
# Captured BEFORE and AFTER, because these are counters and histograms
# cumulative over the server process lifetime -- not per-leg. A snapshot taken
# only at the end reports every earlier leg too: the first write leg's snapshot
# showed ~171k GetState calls for a leg that issued 64. Deltas are computed in
# summarise_metrics below; the raw endpoints are kept so a reader can recheck.
METRIC_QUERIES=(
  'guardian_db_pool_pending_acquires'
  'guardian_db_pool_connections'
  'guardian_db_pool_connections_available'
  'guardian_db_pool_connections_max'
  'guardian_grpc_request_duration_seconds_count'
  'guardian_grpc_request_duration_seconds_sum'
  'guardian_grpc_requests_in_flight'
  'guardian_grpc_requests_total'
  'guardian_storage_operation_duration_seconds_count'
  'guardian_storage_operation_duration_seconds_sum'
  'guardian_storage_operations_total'
  'guardian_miden_rpc_duration_seconds_count'
  'guardian_miden_rpc_duration_seconds_sum'
  'guardian_miden_rpc_requests_total'
  'guardian_canonicalization_runs_total'
  'guardian_canonicalization_run_duration_seconds_count'
  'guardian_canonicalization_fast_runs_total'
  'guardian_canonicalization_candidates_total'
  'guardian_canonicalization_candidate_age_seconds'
  'guardian_canonicalization_retries_total'
  'guardian_canonicalization_commitment_mismatches_total'
  'guardian_deltas_submitted_total'
  'guardian_rate_limit_rejections_total'
  'process_cpu_seconds_total'
  'process_resident_memory_bytes'
)

capture_metrics() {
  local phase="$1"
  mkdir -p "${OUT}/metrics-${phase}"
  for q in "${METRIC_QUERIES[@]}"; do
    curl -sf --get "${PROM}/api/v1/query" --data-urlencode "query=${q}" \
      > "${OUT}/metrics-${phase}/${q}.json" 2>/dev/null || true
  done
}

# Per-leg deltas. Gauges (in-flight, pool sizes, memory) are levels rather than
# counters, so their "after" value is reported as-is instead of subtracted.
summarise_metrics() {
  local gauges='guardian_db_pool_pending_acquires guardian_db_pool_connections guardian_db_pool_connections_available guardian_db_pool_connections_max guardian_grpc_requests_in_flight process_resident_memory_bytes'
  : > "${OUT}/metrics-delta.jsonl"
  for q in "${METRIC_QUERIES[@]}"; do
    local before="${OUT}/metrics-before/${q}.json"
    local after="${OUT}/metrics-after/${q}.json"
    [ -f "${after}" ] || continue
    if [[ " ${gauges} " == *" ${q} "* ]]; then
      jq -c --arg metric "${q}" \
        '{metric: $metric, kind: "gauge", series: [.data.result[] | {labels: (.metric|del(.__name__,.job,.instance)), value: (.value[1]|tonumber)}]}' \
        "${after}" >> "${OUT}/metrics-delta.jsonl" 2>/dev/null || true
    else
      jq -c -n --arg metric "${q}" \
        --slurpfile before "${before}" --slurpfile after "${after}" \
        '{metric: $metric, kind: "counter", series: [
           ($after[0].data.result // [])[] as $a
           | ($a.metric|del(.__name__,.job,.instance)) as $labels
           | (($before[0].data.result // []) | map(select((.metric|del(.__name__,.job,.instance)) == $labels)) | .[0].value[1] // "0" | tonumber) as $b
           | {labels: $labels, delta: (($a.value[1]|tonumber) - $b)}
           | select(.delta != 0)
         ]}' >> "${OUT}/metrics-delta.jsonl" 2>/dev/null || true
    fi
  done
}


echo "==> leg '${LABEL}' -> ${OUT}"

# Build identity BEFORE. Production exposes /status unauthenticated, and so does
# this stack; started_at is what makes a mid-run restart detectable.
curl -sf "${HTTP_A}/status" > "${OUT}/status-start.json" \
  || { echo "server-a not answering /status — is the stack up?" >&2; exit 1; }
echo "    build: $(jq -r '.version + " / " + .git_commit' "${OUT}/status-start.json")"
if [ "$(jq -r '.git_commit' "${OUT}/status-start.json")" = "unknown" ]; then
  echo "    WARNING: git_commit is 'unknown' — this leg cannot record which build" \
       "it measured (FR-A08). Rebuild with:" >&2
  echo "      GUARDIAN_GIT_SHA=\$(git rev-parse --short=12 HEAD) docker compose up -d --build" >&2
  touch "${OUT}/UNKNOWN_BUILD"
fi

# Record the configuration actually in effect, not what .env says it should be.
docker compose ps --format json > "${OUT}/compose-ps.json" 2>/dev/null || true
docker inspect "$(docker compose ps -q server-a)" \
  --format '{{json .Config.Env}}' | jq 'map(select(startswith("GUARDIAN_")))' \
  > "${OUT}/server-env.json"
docker stats --no-stream --format json > "${OUT}/docker-stats-before.json" 2>/dev/null || true

# Reset query stats so the snapshot afterwards covers this leg only.
docker compose exec -T postgres psql -U guardian -d guardian \
  -c 'SELECT pg_stat_statements_reset();' > /dev/null 2>&1 \
  || echo "    warn: pg_stat_statements_reset failed; query attribution unavailable" >&2

capture_metrics before

echo "==> running harness"

# Sample container CPU DURING the measured window. A single sample taken after
# the harness exits shows an idle stack and would let a generator-bound or
# CPU-pinned leg pass as a clean measurement (FR-013).
( while true; do
    printf '{"t":"%s","stats":' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    docker stats --no-stream --format json 2>/dev/null | jq -sc . || echo 'null'
    printf '}\n'
    command sleep 5
  done ) > "${OUT}/docker-stats-timeline.jsonl" 2>/dev/null &
SAMPLER_PID=$!
disown "${SAMPLER_PID}" 2>/dev/null || true
trap 'kill "${SAMPLER_PID}" 2>/dev/null || true' EXIT

( cd "${ROOT_DIR}" && cargo run --quiet --manifest-path benchmarks/prod-server/Cargo.toml -- \
    worker-run --profile "${STACK_DIR}/${PROFILE}" \
               --run-id "${LABEL}-${STAMP}" --shard-index 0 --shard-count 1 ) \
  > "${OUT}/worker-artifact.txt"

kill "${SAMPLER_PID}" 2>/dev/null || true
trap - EXIT

# Peak CPU per container across the run — the figure the saturation call rests on.
jq -rs '[.[] | .stats[]? | {name: .Name, cpu: (.CPUPerc | rtrimstr("%") | tonumber)}]
        | group_by(.name) | map({name: .[0].name, peak_cpu_percent: (map(.cpu) | max)})
        | sort_by(-.peak_cpu_percent)' \
  "${OUT}/docker-stats-timeline.jsonl" > "${OUT}/cpu-peaks.json" 2>/dev/null \
  || echo "    warn: could not summarise CPU peaks" >&2

# Decode the base64 artifact line into readable JSON.
sed -n 's/^BENCH_WORKER_ARTIFACT_BASE64=//p' "${OUT}/worker-artifact.txt" \
  | base64 -d | jq . > "${OUT}/worker-artifact.json" 2>/dev/null \
  || echo "    warn: could not decode worker artifact" >&2

# Build identity AFTER. A changed commit or started_at means the run spanned a
# restart and its numbers cover more than one server instance (FR-A08).
curl -sf "${HTTP_A}/status" > "${OUT}/status-end.json" || true
if ! diff -q <(jq -r '.git_commit + .started_at' "${OUT}/status-start.json") \
             <(jq -r '.git_commit + .started_at' "${OUT}/status-end.json") > /dev/null 2>&1; then
  echo "    WARNING: server restarted mid-run — this leg carries no valid measurement" \
    | tee "${OUT}/SPANNED_RESTART"
fi

docker stats --no-stream --format json > "${OUT}/docker-stats-after.json" 2>/dev/null || true

echo "==> capturing server-side metrics"
capture_metrics after
summarise_metrics
curl -sf "${PROM}/api/v1/targets" > "${OUT}/prom-targets.json" 2>/dev/null || true

docker compose exec -T postgres psql -U guardian -d guardian -A -F',' -c \
  "SELECT calls, round(total_exec_time::numeric,1) AS total_ms,
          round(mean_exec_time::numeric,2) AS mean_ms, left(query, 120) AS query
   FROM pg_stat_statements ORDER BY total_exec_time DESC LIMIT 25;" \
  > "${OUT}/pg-stat-statements.csv" 2>/dev/null \
  || echo "    warn: pg_stat_statements unavailable" >&2

echo "==> done: ${OUT}"
echo "    NOTE: diagnostic tier — bottleneck evidence only, never a target verdict."
