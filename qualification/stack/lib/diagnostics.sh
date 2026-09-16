# shellcheck shell=bash
# Bounded diagnostics capture. Output is truncated and redacted before it is
# retained, so a failure never turns into a disclosure.

QUAL_LOG_TAIL_LINES="${QUAL_LOG_TAIL_LINES:-2000}"

qual_capture_diagnostics() {
  local project="$1" compose_file="$2" env_file="$3" out_dir="$4"
  mkdir -p "${out_dir}"

  docker compose -p "${project}" -f "${compose_file}" --env-file "${env_file}" \
    logs --no-color --tail "${QUAL_LOG_TAIL_LINES}" server 2>/dev/null \
    > "${out_dir}/server.log" || true

  docker compose -p "${project}" -f "${compose_file}" --env-file "${env_file}" \
    ps --all --format json 2>/dev/null > "${out_dir}/containers.json" || true

  qual_redact_dir "${out_dir}"
}
