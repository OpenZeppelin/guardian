# shellcheck shell=bash
# Teardown that survives cancellation. Registered as a trap by run.sh so a
# killed run does not leave a stack behind.

qual_teardown() {
  local project="$1" compose_file="$2" env_file="$3"
  if [[ "${QUAL_KEEP_STACK:-0}" == "1" ]]; then
    echo "stack kept; remove it with:"
    echo "  docker compose -p ${project} -f ${compose_file} --env-file ${env_file} down --volumes --remove-orphans"
    return 0
  fi
  docker compose -p "${project}" -f "${compose_file}" --env-file "${env_file}" \
    down --volumes --remove-orphans --timeout 10 >/dev/null 2>&1 || true
}

qual_install_teardown_trap() {
  local project="$1" compose_file="$2" env_file="$3"
  # shellcheck disable=SC2064
  trap "qual_teardown '${project}' '${compose_file}' '${env_file}'" EXIT INT TERM
}
