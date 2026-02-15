#!/usr/bin/env bash
set -euo pipefail

wait_for_port() {
  local host=$1
  local port=$2
  local timeout_secs=$3

  local elapsed=0
  while (( elapsed < timeout_secs )); do
    if nc -z "$host" "$port" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done
  return 1
}

wait_for_server_ready() {
  local pid=$1
  local host=$2
  local port=$3
  local timeout_secs=$4
  local log_file=$5

  local elapsed=0
  while (( elapsed < timeout_secs )); do
    if nc -z "$host" "$port" >/dev/null 2>&1; then
      return 0
    fi

    if ! kill -0 "$pid" >/dev/null 2>&1; then
      echo "server exited before readiness"
      tail -n 80 "$log_file" >/dev/null 2>&1 && tail -n 80 "$log_file"
      return 1
    fi

    sleep 1
    elapsed=$((elapsed + 1))
  done

  echo "server did not start in time (${timeout_secs}s)"
  tail -n 80 "$log_file" >/dev/null 2>&1 && tail -n 80 "$log_file"
  return 1
}

prebuild_bench_binaries() {
  local repo_root=$1
  local server_features=${2:-}

  (
    cd "$repo_root"
    if [[ -n "$server_features" ]]; then
      cargo build -p private-state-manager-server --release --bin server --features "$server_features"
    else
      cargo build -p private-state-manager-server --release --bin server
    fi
    cargo build -p psm-server-bench-loadgen --release
  )
}
