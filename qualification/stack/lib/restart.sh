# shellcheck shell=bash
# Restarts only the Guardian container, leaving the database and its volume
# untouched. Restarting the whole stack would prove nothing: the data has to
# outlive the process that wrote it, not be rewritten by a fresh one.

qual_restart_server() {
  local project="$1" compose_file="$2" env_file="$3"
  docker compose -p "${project}" -f "${compose_file}" --env-file "${env_file}" \
    restart server >/dev/null 2>&1
}

# Replaces the Guardian container with a different image, leaving the database
# and its volume untouched.
#
# This is the upgrade question, and it is not the same as the restart above. A
# restart proves data outlives the process; this proves it outlives the *build*,
# so migrations run against rows an older release wrote rather than against an
# empty schema. A migration that fails on existing data, or a server that
# refuses to boot on it, is a release-stopping defect that no fresh-database run
# can see.
#
# The data is seeded through the product's own API by running scenarios against
# the old image first, rather than from a hand-written SQL fixture: a fixture
# has to be kept in step with a schema it does not own, and drifts silently.
qual_swap_server_image() {
  local project="$1" compose_file="$2" env_file="$3" image="$4"

  # Rewritten in place so compose brings the service back up on the new image
  # while every other service, and the volume, stays exactly where it was.
  local tmp="${env_file}.swap"
  sed "s|^QUAL_SERVER_IMAGE=.*|QUAL_SERVER_IMAGE=${image}|" "${env_file}" > "${tmp}" \
    && mv "${tmp}" "${env_file}" || return 1

  docker compose -p "${project}" -f "${compose_file}" --env-file "${env_file}" \
    up -d --no-deps --force-recreate server >/dev/null 2>&1
}
