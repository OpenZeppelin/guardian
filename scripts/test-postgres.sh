#!/bin/bash
set -euo pipefail

# Run the Postgres-backed guardian-server tests.
#
# The suite resets its own database before the first test, so runs leave no
# state behind that can affect the next one. The database name must end in
# "_test"; the suite refuses to reset anything else.
#
# Usage: ./scripts/test-postgres.sh
#
# Optional environment variables:
#   DATABASE_URL - Postgres connection URL
#                  (default: postgres://guardian:guardian@localhost:5432/guardian_test)

DATABASE_URL="${DATABASE_URL:-postgres://guardian:guardian@localhost:5432/guardian_test}"
export DATABASE_URL

# The filter is a substring match on the test path, so a database-backed test
# must live under a `postgres` module segment to be selected, and an ignored
# test that does not need a database must not. A filter that matches nothing
# would otherwise report success while running no tests.
if ! LISTED=$(cargo test -p guardian-server --lib --features postgres -- \
  --ignored postgres --list); then
  echo "error: listing the Postgres-backed tests failed; see the cargo output above" >&2
  exit 1
fi

SELECTED=$(printf '%s\n' "${LISTED}" | grep -c ': test$' || true)

if [ "${SELECTED}" -eq 0 ]; then
  echo "error: the 'postgres' filter matched no ignored tests" >&2
  exit 1
fi

echo "Running ${SELECTED} Postgres-backed tests"

# Serialised: the tests share one database, and the migration test reverts the
# newest migration for the duration of its own run.
cargo test -p guardian-server --lib --features postgres -- \
  --ignored postgres --test-threads=1
