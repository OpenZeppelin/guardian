# shellcheck shell=bash
# Generates the per-run environment: unique project name, free host ports, and
# the server configuration the profile needs.

qual_random_suffix() {
  head -c 8 /dev/urandom | od -An -tx1 | tr -d ' \n'
}

qual_free_port() {
  python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
}

# Compose rejects a project name that is not lowercase and restricted to
# [a-z0-9_-], so the run id is kept in that alphabet rather than sanitized after
# the fact.
qual_project_name() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | tr -c 'a-z0-9_-' '-' | sed 's/^[^a-z0-9]*//'
}

qual_generate_env() {
  local profile="$1" network_type="$2" rpc_endpoint="$3" image="$4" out="$5"
  # Everything a run writes lives under one directory named after the run.
  # Shared paths looked harmless because ports and project names were already
  # per run, but a second invocation overwrote the acknowledgement keys and the
  # operator allowlist the first one's server was still mounting, so its restart
  # and operator scenarios read another run's configuration.
  local run_dir="${6:-$(dirname "${out}")}"

  QUAL_RUN_ID="${QUAL_RUN_ID:-qual-$(date -u +%Y%m%d-%H%M%S)-$(qual_random_suffix)}"
  QUAL_PROJECT="$(qual_project_name "${QUAL_PROJECT:-${QUAL_RUN_ID}}")"
  QUAL_HTTP_PORT="${QUAL_HTTP_PORT:-$(qual_free_port)}"
  QUAL_GRPC_PORT="${QUAL_GRPC_PORT:-$(qual_free_port)}"
  QUAL_HTTP_PORT_B="${QUAL_HTTP_PORT_B:-$(qual_free_port)}"
  QUAL_GRPC_PORT_B="${QUAL_GRPC_PORT_B:-$(qual_free_port)}"
  QUAL_HTTP_PORT_C="${QUAL_HTTP_PORT_C:-$(qual_free_port)}"
  QUAL_GRPC_PORT_C="${QUAL_GRPC_PORT_C:-$(qual_free_port)}"
  QUAL_POSTGRES_PASSWORD="${QUAL_POSTGRES_PASSWORD:-$(qual_random_suffix)}"

  QUAL_ACK_KEYS_DIR="${run_dir}/ack-keys"
  QUAL_ACK_KEYS_MIGRATION_DIR="${run_dir}/ack-keys-migration-target"
  QUAL_OPERATOR_DIR="${run_dir}/operator"
  mkdir -p "${QUAL_ACK_KEYS_DIR}" "${QUAL_ACK_KEYS_MIGRATION_DIR}" "${QUAL_OPERATOR_DIR}"

  cat > "${out}" <<ENV
QUAL_RUN_ID=${QUAL_RUN_ID}
QUAL_PROJECT=${QUAL_PROJECT}
QUAL_PROFILE=${profile}
QUAL_SERVER_IMAGE=${image}
QUAL_NETWORK_TYPE=${network_type}
QUAL_MIDEN_RPC_ENDPOINT=${rpc_endpoint}
QUAL_HTTP_PORT=${QUAL_HTTP_PORT}
QUAL_GRPC_PORT=${QUAL_GRPC_PORT}
QUAL_HTTP_PORT_B=${QUAL_HTTP_PORT_B}
QUAL_GRPC_PORT_B=${QUAL_GRPC_PORT_B}
QUAL_HTTP_PORT_C=${QUAL_HTTP_PORT_C}
QUAL_GRPC_PORT_C=${QUAL_GRPC_PORT_C}
QUAL_POSTGRES_PASSWORD=${QUAL_POSTGRES_PASSWORD}
QUAL_RATE_BURST_PER_SEC=${QUAL_RATE_BURST_PER_SEC:-500}
QUAL_RATE_PER_MIN=${QUAL_RATE_PER_MIN:-20000}
QUAL_RUST_LOG=${QUAL_RUST_LOG:-info}
QUAL_ACK_KEYS_DIR=${QUAL_ACK_KEYS_DIR}
QUAL_ACK_KEYS_MIGRATION_DIR=${QUAL_ACK_KEYS_MIGRATION_DIR}
QUAL_OPERATOR_DIR=${QUAL_OPERATOR_DIR}
ENV

  export QUAL_RUN_ID QUAL_PROJECT QUAL_HTTP_PORT QUAL_GRPC_PORT QUAL_POSTGRES_PASSWORD
  export QUAL_HTTP_PORT_B QUAL_GRPC_PORT_B
  export QUAL_HTTP_PORT_C QUAL_GRPC_PORT_C
  export QUAL_ACK_KEYS_DIR QUAL_ACK_KEYS_MIGRATION_DIR QUAL_OPERATOR_DIR
}
