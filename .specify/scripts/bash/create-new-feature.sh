#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
source "${SCRIPT_DIR}/common.sh"

json_output=false
feature_number=""
short_name=""
feature_arg=""
description_parts=()

while [ "$#" -gt 0 ]; do
  case "$1" in
    --json)
      json_output=true
      ;;
    --number)
      feature_number="${2:-}"
      shift
      ;;
    --short-name)
      short_name="${2:-}"
      shift
      ;;
    --feature)
      feature_arg="${2:-}"
      shift
      ;;
    --help|-h)
      cat <<'EOF'
Usage: create-new-feature.sh [--json] [--number N] [--short-name NAME] "Feature description"
EOF
      exit 0
      ;;
    *)
      description_parts+=("$1")
      ;;
  esac
  shift
done

description="${description_parts[*]}"

if [ -z "${description}" ]; then
  echo "Feature description is required." >&2
  exit 1
fi

ensure_engine_dirs

if [ -z "${short_name}" ]; then
  short_name="$(slugify "${description}")"
fi

if [ -z "${short_name}" ]; then
  echo "Unable to derive a short feature name." >&2
  exit 1
fi

if [ -z "${feature_number}" ]; then
  feature_number="$(next_feature_number_for_short_name "${short_name}")"
fi

if ! [[ "${feature_number}" =~ ^[0-9]+$ ]]; then
  echo "Feature number must be numeric." >&2
  exit 1
fi

feature_key="$(format_feature_number "${feature_number}")-${short_name}"
if [ -n "${feature_arg}" ]; then
  feature_key="${feature_arg}"
fi

feature_dir="$(feature_dir_from_key "${feature_key}")"
spec_file="${feature_dir}/spec.md"

if [ -e "${feature_dir}" ]; then
  echo "Feature workspace already exists: ${feature_dir}" >&2
  exit 1
fi

mkdir -p "${feature_dir}/checklists"

render_basic_template \
  "${ROOT}/.specify/templates/spec-template.md" \
  "${spec_file}" \
  "${description}" \
  "${feature_key}" \
  "$(today_iso)" \
  "${description}"

set_active_feature "${feature_key}"
branch_name="$(suggested_branch_name "${feature_key}")"

if [ "${json_output}" = true ]; then
  cat <<EOF
{
  "FEATURE_KEY": "$(json_escape "${feature_key}")",
  "FEATURE_DIR": "$(json_escape "$(abs_path "${feature_dir}")")",
  "SPEC_FILE": "$(json_escape "$(abs_path "${spec_file}")")",
  "BRANCH_NAME": "$(json_escape "${branch_name}")"
}
EOF
  exit 0
fi

cat <<EOF
Feature workspace created: $(abs_path "${feature_dir}")
Spec file: $(abs_path "${spec_file}")
Suggested branch: ${branch_name}
EOF
