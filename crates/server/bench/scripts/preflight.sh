#!/usr/bin/env bash
set -euo pipefail

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found"
  exit 1
fi

if ! command -v nc >/dev/null 2>&1; then
  echo "nc not found"
  exit 1
fi

if ! nc -z localhost 57291 >/dev/null 2>&1; then
  if ! nc -z 0.0.0.0 57291 >/dev/null 2>&1; then
    echo "miden node is not reachable on localhost:57291"
    exit 1
  fi
fi

if ! command -v docker >/dev/null 2>&1; then
  echo "docker not found (required for postgres benchmark)"
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "jq not found (summary output will be limited)"
fi

if ! command -v k6 >/dev/null 2>&1; then
  echo "k6 not found (k6 checks will be skipped)"
fi
