# shellcheck shell=bash
# Teardown that survives cancellation. Registered as a trap by run.sh so a
# killed run does not leave a stack behind.

qual_teardown() {
  local project="$1" compose_file="$2" env_file="$3" run_dir="${4:-}"
  if [[ "${QUAL_KEEP_STACK:-0}" == "1" ]]; then
    echo "stack kept; remove it with:"
    echo "  docker compose -p ${project} -f ${compose_file} --env-file ${env_file} down --volumes --remove-orphans"
    [[ -n "${run_dir}" ]] && echo "  rm -rf ${run_dir}"
    return 0
  fi
  docker compose -p "${project}" -f "${compose_file}" --env-file "${env_file}" \
    down --volumes --remove-orphans --timeout 10 >/dev/null 2>&1 || true
  # The run directory holds this run's acknowledgement keys, so it goes with the
  # stack rather than being left on disk for the next run to inherit. Guarded on
  # the expected shape: this deletes recursively, and a caller that passed
  # something else must not have it removed.
  if [[ -n "${run_dir}" && "${run_dir}" == */stack/runs/* && -d "${run_dir}" ]]; then
    rm -rf "${run_dir}"
  fi
}

qual_install_teardown_trap() {
  local project="$1" compose_file="$2" env_file="$3" run_dir="${4:-}"
  # shellcheck disable=SC2064
  trap "qual_teardown '${project}' '${compose_file}' '${env_file}' '${run_dir}'" EXIT
  # A signal exits, and the exit runs the teardown once. Tearing down in the
  # signal trap itself returned to the script afterwards, so an interrupted run
  # carried on driving scenarios against the stack it had just removed.
  trap 'exit 130' INT
  trap 'exit 143' TERM
}
