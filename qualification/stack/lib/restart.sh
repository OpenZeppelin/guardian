# shellcheck shell=bash
# Restarts only the Guardian container, leaving the database and its volume
# untouched. Restarting the whole stack would prove nothing: the data has to
# outlive the process that wrote it, not be rewritten by a fresh one.

qual_restart_server() {
  local project="$1" compose_file="$2" env_file="$3"
  docker compose -p "${project}" -f "${compose_file}" --env-file "${env_file}" \
    restart server >/dev/null 2>&1
}
