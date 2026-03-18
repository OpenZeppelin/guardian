#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
source "${SCRIPT_DIR}/common.sh"

json_output=false
paths_only=false
require_tasks=false
include_tasks=false
feature_arg=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --json)
      json_output=true
      ;;
    --paths-only)
      paths_only=true
      ;;
    --require-tasks)
      require_tasks=true
      ;;
    --include-tasks)
      include_tasks=true
      ;;
    --feature)
      feature_arg="${2:-}"
      shift
      ;;
    --help|-h)
      cat <<'EOF'
Usage: check-prerequisites.sh [--json] [--paths-only] [--require-tasks] [--include-tasks] [--feature KEY]
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
research_file="${feature_dir}/research.md"
data_model_file="${feature_dir}/data-model.md"
quickstart_file="${feature_dir}/quickstart.md"
tasks_file="${feature_dir}/tasks.md"
contracts_dir="${feature_dir}/contracts"
checklists_dir="${feature_dir}/checklists"

if [ ! -f "${spec_file}" ]; then
  echo "Missing spec file for active feature: ${spec_file}" >&2
  exit 1
fi

if [ "${require_tasks}" = true ] && [ ! -f "${tasks_file}" ]; then
  echo "Missing tasks.md for active feature. Run the tasks workflow first." >&2
  exit 1
fi

set_active_feature "${feature_key}"

available_docs=()

for candidate in \
  "${spec_file}" \
  "${plan_file}" \
  "${research_file}" \
  "${data_model_file}" \
  "${quickstart_file}" \
  "${tasks_file}"; do
  if [ -f "${candidate}" ]; then
    available_docs+=("$(basename "${candidate}")")
  fi
done

if [ -d "${contracts_dir}" ]; then
  available_docs+=("contracts")
fi

if [ "${include_tasks}" = true ] && [ -d "${checklists_dir}" ] && find "${checklists_dir}" -type f -name '*.md' -print -quit | grep -q .; then
  available_docs+=("checklists")
fi

branch_name="$(suggested_branch_name "${feature_key}")"

if [ "${json_output}" = true ]; then
  cat <<EOF
{
  "FEATURE_KEY": "$(json_escape "${feature_key}")",
  "FEATURE_DIR": "$(json_escape "$(abs_path "${feature_dir}")")",
  "FEATURE_SPEC": "$(json_escape "$(abs_path "${spec_file}")")",
  "IMPL_PLAN": "$(json_escape "$(abs_path "${plan_file}")")",
  "TASKS": "$(json_escape "$(abs_path "${tasks_file}")")",
  "RESEARCH": "$(json_escape "$(abs_path "${research_file}")")",
  "DATA_MODEL": "$(json_escape "$(abs_path "${data_model_file}")")",
  "QUICKSTART": "$(json_escape "$(abs_path "${quickstart_file}")")",
  "CONTRACTS_DIR": "$(json_escape "$(abs_path "${contracts_dir}")")",
  "CHECKLISTS_DIR": "$(json_escape "$(abs_path "${checklists_dir}")")",
  "BRANCH": "$(json_escape "${branch_name}")",
  "AVAILABLE_DOCS": $(json_array "${available_docs[@]}")
}
EOF
  exit 0
fi

if [ "${paths_only}" = true ]; then
  cat <<EOF
FEATURE_DIR=$(abs_path "${feature_dir}")
FEATURE_SPEC=$(abs_path "${spec_file}")
IMPL_PLAN=$(abs_path "${plan_file}")
TASKS=$(abs_path "${tasks_file}")
EOF
  exit 0
fi

cat <<EOF
Feature workspace: $(abs_path "${feature_dir}")
Spec: $(abs_path "${spec_file}")
Plan: $(abs_path "${plan_file}")
Tasks: $(abs_path "${tasks_file}")
Available docs: ${available_docs[*]}
EOF
