#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BENCH_DIR=$(cd "$SCRIPT_DIR/.." && pwd)

SCHEMES_CSV="${BENCH_AUTH_SCHEMES:-falcon,ecdsa}"
SCENARIOS="${BENCH_SCENARIOS:-mixed}"
RUN_ID=$(date +"%Y%m%d_%H%M%S")
INDEX_FILE="$BENCH_DIR/results/${RUN_ID}_postgres_auth_compare.txt"

BENCH_SKIP_PREBUILD="${BENCH_SKIP_PREBUILD:-false}"

echo "auth_compare_run_id=$RUN_ID" > "$INDEX_FILE"
echo "scenarios=$SCENARIOS" >> "$INDEX_FILE"

IFS=',' read -r -a SCHEMES <<<"$SCHEMES_CSV"
for scheme in "${SCHEMES[@]}"; do
  scheme=$(echo "$scheme" | xargs)
  if [[ -z "$scheme" ]]; then
    continue
  fi

  TMP_LOG=$(mktemp)
  BENCH_AUTH_SCHEME="$scheme" \
    BENCH_SCENARIOS="$SCENARIOS" \
    BENCH_SKIP_PREBUILD="$BENCH_SKIP_PREBUILD" \
    "$SCRIPT_DIR/run_postgres.sh" | tee "$TMP_LOG"

  RUN_DIR=$(awk -F= '/^results=/{print $2}' "$TMP_LOG" | tail -n1)
  rm -f "$TMP_LOG"

  echo "scheme=$scheme run_dir=$RUN_DIR" >> "$INDEX_FILE"

  if [[ "$BENCH_SKIP_PREBUILD" != "true" ]]; then
    BENCH_SKIP_PREBUILD=true
  fi
done

echo "index=$INDEX_FILE"
