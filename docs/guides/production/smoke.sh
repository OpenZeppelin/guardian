#!/usr/bin/env bash
# Smoke test for the self-managed production Compose stack (docker-compose.yml,
# Track B of README.md). Needs no AWS. It does need outbound HTTPS to the Miden
# RPC endpoint of SMOKE_NETWORK_TYPE (default MidenTestnet, rpc.testnet.miden.io):
# the server opens that connection at startup, before the listeners bind. It
# copies the compose file into a scratch project directory with freshly
# generated secrets, a throwaway ACK identity, and the bundled Postgres, and runs
# Compose under a sanitized environment, so neither files next to this script
# nor variables exported in your shell can point the run at real infrastructure.
# It checks the production properties the guide promises and tears everything
# down.
#
# Requires: docker (Compose v2), jq, curl, openssl. The ACK identity comes from
# the image's own ack-keygen, so no Rust toolchain is needed.
# Usage:    ./smoke.sh                                    # tag from ./.env, else .env.example
#           GUARDIAN_VERSION=<release later than v0.17.0> ./smoke.sh
#           SMOKE_PULL_POLICY=missing GUARDIAN_VERSION=<tag> ./smoke.sh
#             # image built locally as ghcr.io/openzeppelin/guardian:<tag>
#             # (docker build -t ghcr.io/openzeppelin/guardian:<tag> .); the
#             # committed compose file pins pull_policy: always, which a CI
#             # job testing a branch image must relax to missing or never.
# A tag is required: the stack depends on server features v0.17.0 lacks, and
# this script's first step (ack-keygen from the image) fails on older tags.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

for tool in docker jq curl openssl; do
  command -v "$tool" >/dev/null 2>&1 || { echo "missing required tool: $tool" >&2; exit 1; }
done

version_from() { [ -f "$1" ] && grep -E '^GUARDIAN_VERSION=' "$1" | cut -d= -f2- || true; }
GUARDIAN_VERSION="${GUARDIAN_VERSION:-$(version_from .env)}"
GUARDIAN_VERSION="${GUARDIAN_VERSION:-$(version_from .env.example)}"
[ -n "$GUARDIAN_VERSION" ] || { echo "set GUARDIAN_VERSION to a Guardian release later than v0.17.0 (this stack needs ack-keygen in the image)" >&2; exit 1; }
PULL_POLICY="${SMOKE_PULL_POLICY:-always}"
case "$PULL_POLICY" in
  always|missing|never) ;;
  *) echo "SMOKE_PULL_POLICY must be always, missing, or never (got '$PULL_POLICY')" >&2; exit 1 ;;
esac
HTTP_PORT="${SMOKE_HTTP_PORT:-3300}"
GRPC_PORT="${SMOKE_GRPC_PORT:-53051}"
METRICS_PORT="${SMOKE_METRICS_PORT:-9564}"
PROJECT="guardian-production-smoke-$$"

WORK="$(mktemp -d)"
METRICS_TOKEN="$(openssl rand -hex 16)"
POSTGRES_PASSWORD="$(openssl rand -hex 16)"
BASE="http://127.0.0.1:${HTTP_PORT}"

# Compose lets exported shell variables override the project .env during
# interpolation, so run it with a scrubbed environment: only PATH, HOME, and the
# Docker client's own connection settings pass through.
compose() {
  local passthrough=(PATH="$PATH" HOME="$HOME")
  local name
  for name in DOCKER_HOST DOCKER_CONTEXT DOCKER_CONFIG DOCKER_TLS_VERIFY DOCKER_CERT_PATH; do
    if [ -n "${!name:-}" ]; then passthrough+=("$name=${!name}"); fi
  done
  env -i "${passthrough[@]}" docker compose -p "$PROJECT" \
    --project-directory "$WORK" -f "$WORK/docker-compose.yml" "$@"
}
http() { curl -sf --max-time 10 "$@"; }

cleanup() {
  local status=$?
  if [ "$status" -ne 0 ]; then
    echo "--- server logs (smoke failed) ---" >&2
    compose logs --no-color server 2>/dev/null | tail -60 >&2 || true
  fi
  compose down -v --remove-orphans >/dev/null 2>&1 || true
  rm -rf "$WORK"
  exit "$status"
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "ok   $*"; }

# The scratch copy is the committed compose file with only pull_policy
# rewritten, so a locally built image can be exercised without editing the
# guide's artifact.
sed "s/^\([[:space:]]*\)pull_policy: always$/\1pull_policy: ${PULL_POLICY}/" docker-compose.yml \
  > "$WORK/docker-compose.yml"
grep -q "^[[:space:]]*pull_policy: ${PULL_POLICY}$" "$WORK/docker-compose.yml" \
  || fail "could not set pull_policy: ${PULL_POLICY} in the scratch compose file"
echo '[]' > "$WORK/operators.json"

IMAGE="ghcr.io/openzeppelin/guardian:${GUARDIAN_VERSION}"
echo "Generating a throwaway ACK identity with the image's ack-keygen..."
mkdir -p "$WORK/ack-keys"
docker run --rm --pull "$PULL_POLICY" --user "$(id -u):$(id -g)" -v "$WORK/ack-keys:/out" \
  "$IMAGE" /app/ack-keygen --out-dir /out
[ -s "$WORK/ack-keys/ack-falcon-secret-key" ] && [ -s "$WORK/ack-keys/ack-ecdsa-secret-key" ] \
  || fail "ack-keygen did not write both key files"

echo "Generating a throwaway storage-encryption key document..."
( umask 077; printf '{"active":"k1","keys":{"k1":"%s"}}\n' "$(openssl rand -base64 32)" \
    > "$WORK/storage-encryption-keys.json" )

# The scratch project's .env is the container's env_file (the compose file
# never interpolates server variables, so nothing in the caller's shell can
# override these) and the interpolation source for GUARDIAN_VERSION and the
# ports, which the sanitized `env -i` in compose() protects.
cat > "$WORK/.env" <<EOF
GUARDIAN_VERSION=${GUARDIAN_VERSION}
GUARDIAN_NETWORK_TYPE=${SMOKE_NETWORK_TYPE:-MidenTestnet}
POSTGRES_PASSWORD=${POSTGRES_PASSWORD}
DATABASE_URL=postgres://guardian:${POSTGRES_PASSWORD}@postgres:5432/guardian
GUARDIAN_DASHBOARD_CURSOR_SECRET=$(openssl rand -hex 32)
GUARDIAN_CORS_ALLOWED_ORIGINS=https://accounts.example.com
GUARDIAN_ALLOWED_ACCOUNT_SCHEMES=ecdsa
GUARDIAN_METRICS_ENABLED=true
GUARDIAN_METRICS_BEARER_TOKEN=${METRICS_TOKEN}
GUARDIAN_LOG_FORMAT=text
GUARDIAN_HTTP_PORT=${HTTP_PORT}
GUARDIAN_GRPC_PORT=${GRPC_PORT}
GUARDIAN_METRICS_PORT=${METRICS_PORT}
EOF

echo "Starting ${GUARDIAN_VERSION} on ${BASE} ..."
compose up -d --quiet-pull

wait_for_pubkey() {
  for _ in $(seq 1 60); do
    if http "${BASE}/pubkey" >/dev/null 2>&1; then return 0; fi
    sleep 2
  done
  return 1
}
wait_for_pubkey || {
  if compose logs --no-color server 2>/dev/null | grep -q 'Failed to create network client'; then
    fail "server could not reach the Miden RPC endpoint at startup (outbound HTTPS to the network's RPC is required)"
  fi
  fail "server did not answer /pubkey after 60 attempts"
}
pass "server is up"

http "${BASE}/" >/dev/null || fail "GET / (liveness) did not return 2xx"
pass "/ answers (liveness)"

falcon="$(http "${BASE}/pubkey")"
ecdsa="$(http "${BASE}/pubkey?scheme=ecdsa")"
jq -e '.commitment | startswith("0x")' <<<"$falcon" >/dev/null || fail "Falcon /pubkey has no commitment: $falcon"
jq -e '.commitment | startswith("0x")' <<<"$ecdsa"  >/dev/null || fail "ECDSA /pubkey has no commitment: $ecdsa"
jq -e '.pubkey   | startswith("0x")' <<<"$ecdsa"    >/dev/null || fail "ECDSA /pubkey has no pubkey: $ecdsa"
pass "/pubkey serves Falcon and ECDSA commitments"

logs="$(compose logs --no-color server)"
grep -Eq 'coordination mode="?shared"? backend="?postgres"? stage="?prod"?' <<<"$logs" \
  || fail "startup banner does not report shared/postgres/prod coordination"
grep -Eq 'coordination .*cursor_secret="?configured"?' <<<"$logs" \
  || fail "startup banner does not report a configured cursor secret"
grep -Eq 'ack signers .*ecdsa_backend="?in-memory"?' <<<"$logs" \
  || fail "ECDSA backend is not in-memory (file provider)"
grep -Eq 'ack signers .*account_schemes="?ecdsa"?' <<<"$logs" \
  || fail "GUARDIAN_ALLOWED_ACCOUNT_SCHEMES=ecdsa is not reflected in the banner"
grep -Eq 'storage backend storage="?Postgres"?' <<<"$logs" \
  || fail "storage backend is not Postgres"
grep -Eq 'listeners .*metrics=' <<<"$logs" && ! grep -Eq 'listeners .*metrics="?disabled"?' <<<"$logs" \
  || fail "metrics listener is disabled; the env file did not reach the container"
pass "prod-stage guards active: shared coordination, pinned cursor secret, file-backed ACK identity, metrics listener on"

grep -Eq 'Rate limiter initialized .*burst_per_sec=200 per_min=5000' <<<"$logs" \
  || fail "prod-stage defaults not applied: rate limits are not 200/5000"
grep -Eq 'canonicalization .*max_concurrent_accounts=50' <<<"$logs" \
  || fail "prod-stage defaults not applied: canonicalization concurrency is not 50"
pass "prod-stage runtime defaults applied without listing them"

code_no_token="$(curl -s --max-time 10 -o /dev/null -w '%{http_code}' "http://127.0.0.1:${METRICS_PORT}/metrics")"
[ "$code_no_token" = "401" ] || fail "metrics without bearer token returned $code_no_token, expected 401"
http -H "Authorization: Bearer ${METRICS_TOKEN}" "http://127.0.0.1:${METRICS_PORT}/metrics" \
  | grep -q '^guardian_' || fail "metrics with bearer token did not return guardian_ series"
pass "metrics endpoint gated by bearer token"

compose restart server >/dev/null
wait_for_pubkey || fail "server did not come back after restart"
[ "$(http "${BASE}/pubkey?scheme=ecdsa")" = "$ecdsa" ] || fail "ECDSA identity changed across restart"
[ "$(http "${BASE}/pubkey")" = "$falcon" ] || fail "Falcon identity changed across restart"
pass "Guardian identity is stable across restarts"

echo "SMOKE PASSED (${GUARDIAN_VERSION})"
