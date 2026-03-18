#!/usr/bin/env bash
set -euo pipefail

repo_root() {
  git rev-parse --show-toplevel 2>/dev/null || pwd -P
}

readonly ROOT="$(repo_root)"
readonly SPECIFY_DIR="${ROOT}/.specify"
readonly SCRIPTS_DIR="${SPECIFY_DIR}/scripts/bash"
readonly MEMORY_DIR="${SPECIFY_DIR}/memory"
readonly STATE_DIR="${SPECIFY_DIR}/state"
readonly SPECKIT_DIR="${ROOT}/speckit"
readonly FEATURES_DIR="${SPECKIT_DIR}/features"
readonly ACTIVE_FEATURE_FILE="${STATE_DIR}/active-feature"

ensure_engine_dirs() {
  mkdir -p "${SCRIPTS_DIR}" "${MEMORY_DIR}" "${STATE_DIR}" "${FEATURES_DIR}"
}

today_iso() {
  date +%F
}

current_branch() {
  git rev-parse --abbrev-ref HEAD 2>/dev/null || true
}

feature_dir_from_key() {
  printf '%s/%s\n' "${FEATURES_DIR}" "$1"
}

feature_exists() {
  [ -d "$(feature_dir_from_key "$1")" ]
}

normalize_feature_from_branch() {
  local branch="${1:-}"
  branch="${branch#codex/}"
  if [[ "${branch}" =~ ^[0-9]{3}-[a-z0-9][a-z0-9-]*$ ]] && feature_exists "${branch}"; then
    printf '%s\n' "${branch}"
  fi
}

read_active_feature_file() {
  if [ -f "${ACTIVE_FEATURE_FILE}" ]; then
    tr -d '[:space:]' < "${ACTIVE_FEATURE_FILE}"
  fi
}

single_feature_key() {
  local matches=()
  local path
  while IFS= read -r path; do
    matches+=("$(basename "${path}")")
  done < <(find "${FEATURES_DIR}" -mindepth 1 -maxdepth 1 -type d | sort)

  if [ "${#matches[@]}" -eq 1 ]; then
    printf '%s\n' "${matches[0]}"
  fi
}

resolve_feature_key() {
  ensure_engine_dirs

  local explicit="${1:-}"
  local key=""

  if [ -n "${explicit}" ]; then
    key="${explicit}"
  fi

  if [ -z "${key}" ]; then
    key="$(normalize_feature_from_branch "$(current_branch)")"
  fi

  if [ -z "${key}" ]; then
    key="$(read_active_feature_file)"
  fi

  if [ -z "${key}" ]; then
    key="$(single_feature_key)"
  fi

  if [ -z "${key}" ]; then
    echo "No active feature workspace found. Create one first or pass --feature <key>." >&2
    return 1
  fi

  if ! feature_exists "${key}"; then
    echo "Feature workspace not found: ${key}" >&2
    return 1
  fi

  printf '%s\n' "${key}"
}

set_active_feature() {
  ensure_engine_dirs
  printf '%s\n' "$1" > "${ACTIVE_FEATURE_FILE}"
}

abs_path() {
  local target="$1"
  if [ -d "${target}" ]; then
    (cd "${target}" && pwd -P)
    return
  fi

  local dir
  local base
  dir="$(dirname "${target}")"
  base="$(basename "${target}")"
  printf '%s/%s\n' "$(cd "${dir}" && pwd -P)" "${base}"
}

json_escape() {
  local s="${1:-}"
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  s=${s//$'\n'/\\n}
  s=${s//$'\r'/\\r}
  s=${s//$'\t'/\\t}
  printf '%s' "${s}"
}

json_array() {
  local out="["
  local first=1
  local item

  for item in "$@"; do
    if [ "${first}" -eq 0 ]; then
      out+=", "
    fi
    first=0
    out+="\"$(json_escape "${item}")\""
  done

  out+="]"
  printf '%s' "${out}"
}

slugify() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]' | sed -E 's/[^a-z0-9]+/-/g; s/^-+//; s/-+$//; s/-{2,}/-/g'
}

humanize_feature_key() {
  local key="${1#*-}"
  printf '%s' "${key}" | tr '-' ' ' | awk '{for (i = 1; i <= NF; i++) {$i = toupper(substr($i, 1, 1)) substr($i, 2)}}1'
}

extract_feature_title() {
  local spec_file="$1"
  if [ -f "${spec_file}" ]; then
    awk -F': ' '/^# Feature Specification:/ {print $2; exit}' "${spec_file}"
  fi
}

escape_sed() {
  local s="${1:-}"
  s=${s//\\/\\\\}
  printf '%s' "${s}" | sed -e 's/[\/&|]/\\&/g'
}

render_basic_template() {
  local template="$1"
  local dest="$2"
  local feature_title="$3"
  local feature_key="$4"
  local date="$5"
  local input="$6"

  sed \
    -e "s/\[FEATURE NAME\]/$(escape_sed "${feature_title}")/g" \
    -e "s/\[FEATURE\]/$(escape_sed "${feature_title}")/g" \
    -e "s/\[###-feature-name\]/$(escape_sed "${feature_key}")/g" \
    -e "s/\[DATE\]/$(escape_sed "${date}")/g" \
    -e "s|\$ARGUMENTS|$(escape_sed "${input}")|g" \
    "${template}" > "${dest}"
}

next_feature_number_for_short_name() {
  local short_name="$1"
  local max=0
  local dir
  local base
  local num

  while IFS= read -r dir; do
    base="$(basename "${dir}")"
    num="${base%%-*}"
    if [ "${num}" -gt "${max}" ]; then
      max="${num}"
    fi
  done < <(find "${FEATURES_DIR}" -mindepth 1 -maxdepth 1 -type d -name "[0-9][0-9][0-9]-${short_name}" | sort)

  printf '%s\n' "$((max + 1))"
}

format_feature_number() {
  printf '%03d' "$1"
}

suggested_branch_name() {
  local feature_key="$1"
  local branch
  branch="$(current_branch)"

  if [ "${branch}" = "codex/${feature_key}" ] || [ "${branch}" = "${feature_key}" ]; then
    printf '%s\n' "${branch}"
    return
  fi

  printf '%s\n' "${feature_key}"
}
