#!/usr/bin/env bash
set -euo pipefail

PID=${1:?server pid is required}
OUTPUT=${2:?output path is required}
INTERVAL=${3:-1}

echo "timestamp,cpu_percent,rss_kb,vsz_kb" > "$OUTPUT"

while kill -0 "$PID" >/dev/null 2>&1; do
  TS=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
  ROW=$(ps -p "$PID" -o %cpu=,rss=,vsz= | awk '{gsub(/^ +| +$/, ""); gsub(/ +/, ","); print}')
  if [[ -n "$ROW" ]]; then
    echo "$TS,$ROW" >> "$OUTPUT"
  fi
  sleep "$INTERVAL"
done
