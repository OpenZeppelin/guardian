# shellcheck shell=bash
# Bounded diagnostics capture. Output is truncated and redacted before it is
# retained, so a failure never turns into a disclosure.

QUAL_LOG_TAIL_LINES="${QUAL_LOG_TAIL_LINES:-2000}"

# The main server's execution metrics, kept with the run's results. A live run's chain-view and
# proving histograms are the recorded per-execution baseline; nothing asserts on them.
qual_capture_execution_metrics() {
  local port="$1" out_file="$2" scrape
  if ! scrape="$(curl -sf --max-time 10 "http://127.0.0.1:${port}/metrics" 2>/dev/null)"; then
    echo "# the metrics endpoint on port ${port} did not answer" > "${out_file}"
    return 0
  fi
  # A series appears once it is first recorded, so a run that executed nothing keeps only this
  # header: proof the endpoint answered, rather than an empty file that could mean either.
  {
    echo "# scraped $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    grep '^guardian_execution_' <<<"${scrape}" || true
  } > "${out_file}"
}

qual_capture_diagnostics() {
  local project="$1" compose_file="$2" env_file="$3" out_dir="$4"
  mkdir -p "${out_dir}"

  # Every GUARDIAN in the stack. A rotation or scheme-gate failure happens on
  # one of the other two, and capturing only `server` left exactly the log that
  # would explain it out of the artifact.
  for service in server server-migration-target server-scheme-gated server-executing; do
    docker compose -p "${project}" -f "${compose_file}" --env-file "${env_file}" \
      logs --no-color --tail "${QUAL_LOG_TAIL_LINES}" "${service}" 2>/dev/null \
      > "${out_dir}/${service}.log" || true
  done

  docker compose -p "${project}" -f "${compose_file}" --env-file "${env_file}" \
    ps --all --format json 2>/dev/null > "${out_dir}/containers.json" || true

  qual_redact_dir "${out_dir}"
}
