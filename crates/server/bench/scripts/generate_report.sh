#!/usr/bin/env bash
set -euo pipefail

SUITE_INDEX=${1:?suite index path is required}
OUTPUT_PATH=${2:-}

if [[ ! -f "$SUITE_INDEX" ]]; then
  echo "suite index not found: $SUITE_INDEX"
  exit 1
fi

if [[ -z "$OUTPUT_PATH" ]]; then
  SUITE_DIR=$(dirname "$SUITE_INDEX")
  SUITE_FILE=$(basename "$SUITE_INDEX")
  SUITE_ID=${SUITE_FILE%_recommended_suite.txt}
  OUTPUT_PATH="$SUITE_DIR/${SUITE_ID}_report.md"
fi

profile_value() {
  local key=$1
  local value
  value=$(awk -F= -v k="$key" '$1==k{print substr($0, index($0,"=")+1)}' "$SUITE_INDEX" | tail -n1)
  echo "$value"
}

section_value() {
  local section=$1
  local field=$2
  awk -v s="[$section]" -v f="$field" '
    $0==s { in_section=1; next }
    /^\[/ && $0!=s { in_section=0 }
    in_section && index($0, f "=")==1 {
      print substr($0, length(f)+2)
      exit
    }
  ' "$SUITE_INDEX"
}

json_metric() {
  local run_dir=$1
  local scenario=$2
  local query=$3
  local file="$run_dir/loadgen_${scenario}.json"

  if [[ ! -f "$file" ]]; then
    echo "n/a"
    return
  fi

  if command -v jq >/dev/null 2>&1; then
    jq -r "$query" "$file"
  else
    echo "n/a"
  fi
}

server_peak_cpu() {
  local run_dir=$1
  local file="$run_dir/server_metrics.csv"
  if [[ ! -f "$file" ]]; then
    echo "n/a"
    return
  fi
  awk -F, 'NR>1{if($2>max)max=$2} END{if(max=="") print "n/a"; else printf "%.2f", max+0}' "$file"
}

server_peak_rss_mb() {
  local run_dir=$1
  local file="$run_dir/server_metrics.csv"
  if [[ ! -f "$file" ]]; then
    echo "n/a"
    return
  fi
  awk -F, 'NR>1{if($3>max)max=$3} END{if(max=="") print "n/a"; else printf "%.2f", (max+0)/1024}' "$file"
}

machine_line() {
  local line
  line=$1
  if [[ -n "$line" ]]; then
    echo "$line"
  fi
}

BASELINE_FS=$(section_value "baseline_fs" "run_dir")
BASELINE_PG=$(section_value "baseline_postgres" "run_dir")
AUTH_INDEX=$(section_value "auth_compare" "index_file")
SWEEP_INDEX=$(section_value "signer_sweep" "index_file")
SOAK_RUN=$(section_value "soak" "run_dir")

SUITE_ID=$(profile_value "suite_run_id")
SUITE_WALL_SECONDS=$(profile_value "suite_wall_seconds")
GENERATED_AT=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
HOSTNAME_VALUE=$(hostname)
UNAME_VALUE=$(uname -a)

CPU_MODEL=""
CPU_CORES=""
MEMORY_GB=""
OS_VERSION=""

if command -v sw_vers >/dev/null 2>&1; then
  OS_VERSION=$(sw_vers | tr '\n' '; ' | sed 's/; $//')
fi
if command -v sysctl >/dev/null 2>&1; then
  CPU_MODEL=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || true)
  CPU_CORES=$(sysctl -n hw.ncpu 2>/dev/null || true)
  MEM_BYTES=$(sysctl -n hw.memsize 2>/dev/null || true)
  if [[ -n "$MEM_BYTES" ]]; then
    MEMORY_GB=$(awk -v b="$MEM_BYTES" 'BEGIN{printf "%.2f", b/1024/1024/1024}')
  fi
fi
if [[ -z "$CPU_MODEL" ]] && command -v lscpu >/dev/null 2>&1; then
  CPU_MODEL=$(lscpu | awk -F: '/Model name/{gsub(/^ +/, "", $2); print $2; exit}')
  CPU_CORES=$(lscpu | awk -F: '/^CPU\(s\)/{gsub(/^ +/, "", $2); print $2; exit}')
fi
if [[ -z "$MEMORY_GB" ]] && [[ -f /proc/meminfo ]]; then
  MEM_KB=$(awk '/MemTotal/{print $2}' /proc/meminfo)
  MEMORY_GB=$(awk -v kb="$MEM_KB" 'BEGIN{printf "%.2f", kb/1024/1024}')
fi

{
  echo "# PSM Server Benchmark Report"
  echo
  echo "- suite_id: $SUITE_ID"
  if [[ -n "$SUITE_WALL_SECONDS" ]]; then
    echo "- suite_wall_seconds: $SUITE_WALL_SECONDS"
    echo "- suite_wall_minutes: $(awk -v s="$SUITE_WALL_SECONDS" 'BEGIN{printf "%.2f", s/60}')"
  fi
  echo "- generated_at: $GENERATED_AT"
  echo "- host: $HOSTNAME_VALUE"
  echo "- uname: $UNAME_VALUE"
  machine_line "- os: $OS_VERSION"
  machine_line "- cpu: $CPU_MODEL"
  machine_line "- cpu_cores: $CPU_CORES"
  machine_line "- memory_gb: $MEMORY_GB"
  echo
  echo "## Suite Profile"
  echo
  echo "- users: $(profile_value profile_users)"
  echo "- accounts: $(profile_value profile_accounts)"
  echo "- signers_per_account: $(profile_value profile_signers_per_account)"
  echo "- ops_per_user: $(profile_value profile_ops_per_user)"
  echo "- mixed_write_percent: $(profile_value profile_mixed_write_percent)"
  echo "- signer_sweep_values: $(profile_value profile_signer_sweep_values)"
  echo "- soak_users: $(profile_value profile_soak_users)"
  echo "- soak_accounts: $(profile_value profile_soak_accounts)"
  echo "- soak_signers_per_account: $(profile_value profile_soak_signers_per_account)"
  echo "- soak_ops_per_user: $(profile_value profile_soak_ops_per_user)"
  echo "- soak_mixed_write_percent: $(profile_value profile_soak_mixed_write_percent)"
  echo
  echo "## Tests Run"
  echo
  echo "1. Filesystem baseline: $BASELINE_FS"
  echo "2. Postgres baseline: $BASELINE_PG"
  echo "3. Auth-scheme compare (mixed): $AUTH_INDEX"
  echo "4. Signer sweep (mixed): $SWEEP_INDEX"
  echo "5. Soak (mixed): $SOAK_RUN"
  echo
  echo "## Baseline Results"
  echo
  echo "| backend | scenario | success_ops_per_sec | p95_ms | failed_ops | peak_cpu_percent | peak_rss_mb |"
  echo "|---|---:|---:|---:|---:|---:|---:|"

  for scenario in state-read state-write mixed; do
    echo "| filesystem | $scenario | $(json_metric "$BASELINE_FS" "$scenario" '.metrics.success_ops_per_sec') | $(json_metric "$BASELINE_FS" "$scenario" '.metrics.latency.p95_ms') | $(json_metric "$BASELINE_FS" "$scenario" '.metrics.failed_ops') | $(server_peak_cpu "$BASELINE_FS") | $(server_peak_rss_mb "$BASELINE_FS") |"
    echo "| postgres | $scenario | $(json_metric "$BASELINE_PG" "$scenario" '.metrics.success_ops_per_sec') | $(json_metric "$BASELINE_PG" "$scenario" '.metrics.latency.p95_ms') | $(json_metric "$BASELINE_PG" "$scenario" '.metrics.failed_ops') | $(server_peak_cpu "$BASELINE_PG") | $(server_peak_rss_mb "$BASELINE_PG") |"
  done

  echo
  echo "## Auth Compare (Mixed)"
  echo
  echo "| auth_scheme | success_ops_per_sec | p95_ms | failed_ops | run_dir |"
  echo "|---|---:|---:|---:|---|"

  if [[ -f "$AUTH_INDEX" ]]; then
    while read -r line; do
      if [[ "$line" == scheme=* ]]; then
        scheme=$(echo "$line" | awk '{print $1}' | awk -F= '{print $2}')
        run_dir=$(echo "$line" | awk '{print $2}' | awk -F= '{print $2}')
        echo "| $scheme | $(json_metric "$run_dir" "mixed" '.metrics.success_ops_per_sec') | $(json_metric "$run_dir" "mixed" '.metrics.latency.p95_ms') | $(json_metric "$run_dir" "mixed" '.metrics.failed_ops') | $run_dir |"
      fi
    done < "$AUTH_INDEX"
  fi

  echo
  echo "## Signer Sweep (Mixed, Falcon)"
  echo
  echo "| signers_per_account | success_ops_per_sec | p95_ms | failed_ops | run_dir |"
  echo "|---:|---:|---:|---:|---|"

  if [[ -f "$SWEEP_INDEX" ]]; then
    while read -r line; do
      if [[ "$line" == signers=* ]]; then
        signers=$(echo "$line" | awk '{print $1}' | awk -F= '{print $2}')
        run_dir=$(echo "$line" | awk '{print $2}' | awk -F= '{print $2}')
        echo "| $signers | $(json_metric "$run_dir" "mixed" '.metrics.success_ops_per_sec') | $(json_metric "$run_dir" "mixed" '.metrics.latency.p95_ms') | $(json_metric "$run_dir" "mixed" '.metrics.failed_ops') | $run_dir |"
      fi
    done < "$SWEEP_INDEX"
  fi

  echo
  echo "## Soak (Mixed)"
  echo
  echo "- run_dir: $SOAK_RUN"
  echo "- success_ops_per_sec: $(json_metric "$SOAK_RUN" "mixed" '.metrics.success_ops_per_sec')"
  echo "- p95_ms: $(json_metric "$SOAK_RUN" "mixed" '.metrics.latency.p95_ms')"
  echo "- failed_ops: $(json_metric "$SOAK_RUN" "mixed" '.metrics.failed_ops')"
  echo "- peak_cpu_percent: $(server_peak_cpu "$SOAK_RUN")"
  echo "- peak_rss_mb: $(server_peak_rss_mb "$SOAK_RUN")"

  echo
  echo "## Insights"
  echo
  echo "- Baseline FS vs Postgres should be read as local-machine signal, not absolute production capacity."
  echo "- Mixed workload is the most representative non-canonicalization path in this suite."
  echo "- Auth compare isolates signature-path overhead under the same mixed workload."
  echo "- Signer sweep highlights how signer cardinality changes latency/throughput in multisig-heavy operations."
  echo "- Rate-limit/body-limit checks remain correctness checks for DoS controls, not capacity tests."
} > "$OUTPUT_PATH"

echo "report=$OUTPUT_PATH"
