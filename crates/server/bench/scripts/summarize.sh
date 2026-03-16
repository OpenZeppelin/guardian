#!/usr/bin/env bash
set -euo pipefail

RUN_DIR=${1:?run directory is required}
SUMMARY_FILE="$RUN_DIR/summary.txt"

{
  echo "run_dir=$RUN_DIR"
  echo "generated_at=$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
  echo

  for scenario in state-read state-write mixed state-sync canonicalization; do
    REPORT_FILE="$RUN_DIR/loadgen_${scenario}.json"
    if [[ -f "$REPORT_FILE" ]]; then
      if command -v jq >/dev/null 2>&1; then
        jq -r '"scenario=" + (.config.scenario|ascii_downcase) +
               " total_ops=" + (.metrics.total_ops|tostring) +
               " success_ops=" + (.metrics.success_ops|tostring) +
               " failed_ops=" + (.metrics.failed_ops|tostring) +
               " success_ops_per_sec=" + (.metrics.success_ops_per_sec|tostring) +
               " p95_ms=" + (.metrics.latency.p95_ms|tostring)' "$REPORT_FILE"
      else
        echo "scenario=$scenario report=$REPORT_FILE"
      fi
    fi
  done

  echo
  if [[ -f "$RUN_DIR/server_metrics.csv" ]]; then
    echo "server_metrics=$RUN_DIR/server_metrics.csv"
  fi
  if [[ -f "$RUN_DIR/pg_metrics.txt" ]]; then
    echo "pg_metrics=$RUN_DIR/pg_metrics.txt"
  fi
} > "$SUMMARY_FILE"

echo "summary=$SUMMARY_FILE"
