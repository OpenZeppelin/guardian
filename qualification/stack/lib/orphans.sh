# shellcheck shell=bash
# Adopts or removes resources left by a run that died without cleaning up, so a
# killed run does not degrade the next one.

QUAL_PROJECT_PREFIX="qual-"

qual_list_orphan_projects() {
  docker compose ls --all --format json 2>/dev/null \
    | python3 -c "
import json, sys
try:
    entries = json.load(sys.stdin)
except (json.JSONDecodeError, ValueError):
    sys.exit(0)
for entry in entries:
    name = entry.get('Name', '')
    if not name.startswith('${QUAL_PROJECT_PREFIX}'):
        continue
    # A project with running containers belongs to a run that is still going,
    # very likely a concurrent one. Reaping it would stop that run's stack and
    # delete its volumes mid-flight, so only stopped projects are orphans.
    if 'running' in entry.get('Status', ''):
        continue
    print(name)
" 2>/dev/null || true
}

qual_reap_orphans() {
  local keep="${1:-}"
  local project
  while read -r project; do
    [[ -z "${project}" ]] && continue
    [[ "${project}" == "${keep}" ]] && continue
    echo "reaping orphaned qualification stack: ${project}"
    docker compose -p "${project}" down --volumes --remove-orphans --timeout 10 >/dev/null 2>&1 || true
  done < <(qual_list_orphan_projects)
}
