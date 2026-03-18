#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
source "${SCRIPT_DIR}/common.sh"

json_output=false
feature_arg=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --json)
      json_output=true
      ;;
    --feature)
      feature_arg="${2:-}"
      shift
      ;;
    --help|-h)
      cat <<'EOF'
Usage: setup-plan.sh [--json] [--feature KEY]
EOF
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      exit 1
      ;;
  esac
  shift
done

feature_key="$(resolve_feature_key "${feature_arg}")"
feature_dir="$(feature_dir_from_key "${feature_key}")"
spec_file="${feature_dir}/spec.md"
plan_file="${feature_dir}/plan.md"

if [ ! -f "${spec_file}" ]; then
  echo "Missing spec file for active feature. Create or generate spec.md first." >&2
  exit 1
fi

if [ ! -f "${plan_file}" ]; then
  feature_title="$(extract_feature_title "${spec_file}")"
  if [ -z "${feature_title}" ]; then
    feature_title="$(humanize_feature_key "${feature_key}")"
  fi

  render_basic_template \
    "${ROOT}/.specify/templates/plan-template.md" \
    "${plan_file}" \
    "${feature_title}" \
    "${feature_key}" \
    "$(today_iso)" \
    "Feature specification in ${feature_key}"

  tmp_file="$(mktemp)"
  sed 's|\[link\]|[spec.md](./spec.md)|g' "${plan_file}" > "${tmp_file}"
  mv "${tmp_file}" "${plan_file}"
fi

set_active_feature "${feature_key}"
branch_name="$(suggested_branch_name "${feature_key}")"

if [ "${json_output}" = true ]; then
  cat <<EOF
{
  "FEATURE_KEY": "$(json_escape "${feature_key}")",
  "FEATURE_SPEC": "$(json_escape "$(abs_path "${spec_file}")")",
  "IMPL_PLAN": "$(json_escape "$(abs_path "${plan_file}")")",
  "SPECS_DIR": "$(json_escape "$(abs_path "${feature_dir}")")",
  "BRANCH": "$(json_escape "${branch_name}")"
}
EOF
  exit 0
fi

cat <<EOF
Feature workspace: $(abs_path "${feature_dir}")
Plan file: $(abs_path "${plan_file}")
Suggested branch: ${branch_name}
EOF
