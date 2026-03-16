#!/usr/bin/env bash
set -euo pipefail

OUTPUT=${1:?output path is required}
SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BENCH_DIR=$(cd "$SCRIPT_DIR/.." && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../../../.." && pwd)

POSTGRES_SERVICE=${POSTGRES_SERVICE:-postgres}
POSTGRES_USER=${POSTGRES_USER:-psm}
POSTGRES_DB=${POSTGRES_DB:-psm}
POSTGRES_PASSWORD=${POSTGRES_PASSWORD:-psm_dev_password}

{
  echo "-- pg_stat_statements snapshot"
  echo "-- generated at $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
} > "$OUTPUT"

if ! command -v docker >/dev/null 2>&1; then
  echo "docker command not available" >> "$OUTPUT"
  exit 0
fi

if ! (cd "$REPO_ROOT" && POSTGRES_PASSWORD="$POSTGRES_PASSWORD" docker compose ps "$POSTGRES_SERVICE" >/dev/null 2>&1); then
  echo "postgres service is not running" >> "$OUTPUT"
  exit 0
fi

(cd "$REPO_ROOT" && POSTGRES_PASSWORD="$POSTGRES_PASSWORD" docker compose exec -T "$POSTGRES_SERVICE" \
  psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -f /dev/stdin <<'SQL' >> "$OUTPUT" 2>&1
CREATE EXTENSION IF NOT EXISTS pg_stat_statements;
SELECT
  calls,
  total_exec_time,
  mean_exec_time,
  rows,
  left(query, 200) AS query_sample
FROM pg_stat_statements
ORDER BY total_exec_time DESC
LIMIT 25;
SQL
) || true

{
  echo
  echo "-- pg_stat_database snapshot"
} >> "$OUTPUT"

(cd "$REPO_ROOT" && POSTGRES_PASSWORD="$POSTGRES_PASSWORD" docker compose exec -T "$POSTGRES_SERVICE" \
  psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -f /dev/stdin <<'SQL' >> "$OUTPUT" 2>&1
SELECT
  datname,
  numbackends,
  xact_commit,
  xact_rollback,
  blks_read,
  blks_hit,
  tup_returned,
  tup_fetched,
  tup_inserted,
  tup_updated,
  tup_deleted,
  temp_files,
  temp_bytes,
  deadlocks
FROM pg_stat_database
WHERE datname = current_database();

SELECT
  pg_database_size(current_database()) AS database_size_bytes,
  COUNT(*) FILTER (WHERE state = 'active') AS active_connections,
  COUNT(*) AS total_connections
FROM pg_stat_activity
WHERE datname = current_database();
SQL
) || true
