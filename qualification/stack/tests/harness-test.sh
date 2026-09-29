#!/usr/bin/env bash
set -uo pipefail

# Tests for the stack shell libraries. These need the Docker CLI for the Compose
# validation cases but never a running daemon, so they belong in ordinary CI.
#
# Every case here corresponds to a defect that reached review: a project name
# Compose rejected, redaction that destroyed the artifact identity a result
# exists to record, and a pairing check that could accept mixed image and SDK
# sources.

STACK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

# shellcheck source=../lib/env.sh
source "${STACK_DIR}/lib/env.sh"
# shellcheck source=../lib/redact.sh
source "${STACK_DIR}/lib/redact.sh"
# shellcheck source=../lib/pairing.sh
source "${STACK_DIR}/lib/pairing.sh"
# shellcheck source=../lib/phase.sh
source "${STACK_DIR}/lib/phase.sh"

PASSED=0
FAILED=0
SKIPPED=0

pass() { printf '  ok    %s\n' "$1"; PASSED=$((PASSED + 1)); }
fail() { printf '  FAIL  %s\n     %s\n' "$1" "${2:-}"; FAILED=$((FAILED + 1)); }
skip() { printf '  skip  %s (%s)\n' "$1" "${2:-}"; SKIPPED=$((SKIPPED + 1)); }

assert_eq() {
  local name="$1" expected="$2" actual="$3"
  [[ "${expected}" == "${actual}" ]] && pass "${name}" \
    || fail "${name}" "expected '${expected}', got '${actual}'"
}

assert_contains() {
  local name="$1" haystack="$2" needle="$3"
  [[ "${haystack}" == *"${needle}"* ]] && pass "${name}" \
    || fail "${name}" "expected to find '${needle}'"
}

assert_not_contains() {
  local name="$1" haystack="$2" needle="$3"
  [[ "${haystack}" != *"${needle}"* ]] && pass "${name}" \
    || fail "${name}" "did not expect to find '${needle}'"
}

compose_accepts_project() {
  local project="$1"
  # The per-run directories are required by name: Compose refuses to render
  # without them, which is what stops a run from silently mounting whatever the
  # previous one left behind.
  QUAL_SERVER_IMAGE=test:latest \
  QUAL_NETWORK_TYPE=MidenLocal \
  QUAL_MIDEN_RPC_ENDPOINT=http://rpc-stub:57291 \
  QUAL_POSTGRES_PASSWORD=placeholder \
  QUAL_ACK_KEYS_DIR=/tmp/qual-test/ack-keys \
  QUAL_ACK_KEYS_MIGRATION_DIR=/tmp/qual-test/ack-keys-migration-target \
  QUAL_OPERATOR_DIR=/tmp/qual-test/operator \
    docker compose -p "${project}" -f "${STACK_DIR}/compose.yml" config >/dev/null 2>&1
}

# A run that did not name its directories must not render at all, or two runs
# share one set of acknowledgement keys again.
compose_rejects_missing_run_dirs() {
  QUAL_SERVER_IMAGE=test:latest \
  QUAL_NETWORK_TYPE=MidenLocal \
  QUAL_MIDEN_RPC_ENDPOINT=http://rpc-stub:57291 \
  QUAL_POSTGRES_PASSWORD=placeholder \
    docker compose -p qual-test -f "${STACK_DIR}/compose.yml" config >/dev/null 2>&1
}

echo "project naming"
assert_eq "lowercases a timestamped id" \
  "qual-20260916t052309z-abc" \
  "$(qual_project_name 'QUAL-20260916T052309Z-ABC')"
assert_eq "replaces characters outside the compose alphabet" \
  "qual-run-1" \
  "$(qual_project_name 'qual.run/1')"
assert_eq "strips a leading non-alphanumeric" \
  "run1" \
  "$(qual_project_name '--run1')"

if command -v docker >/dev/null 2>&1; then
  generated="$(qual_project_name "qual-$(date -u +%Y%m%d-%H%M%S)-$(qual_random_suffix)")"
  if compose_accepts_project "${generated}"; then
    pass "compose accepts a freshly generated project name"
  else
    fail "compose accepts a freshly generated project name" "rejected '${generated}'"
  fi

  if compose_accepts_project "QUAL-20260916T052309Z"; then
    fail "compose rejects an uppercase project name" "the guard this protects is gone"
  else
    pass "compose rejects an uppercase project name"
  fi

  if compose_rejects_missing_run_dirs; then
    fail "compose refuses to render without the per-run directories" "it rendered with shared paths"
  else
    pass "compose refuses to render without the per-run directories"
  fi
else
  skip "compose project validation" "docker CLI not installed"
fi

echo
echo "redaction"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

python3 - "${WORK}/result.json" <<'PY'
import json
import sys

json.dump(
    {
        "artifact_set": {
            "image_digest": "sha256:" + "a" * 64,
            "image_revision": "383bc1d4aa11",
            "sdk_integrity": {"pkg": "sha512-" + "b" * 40},
        },
        "treasury_secret": "c" * 2562,
        "note": "kept",
    },
    open(sys.argv[1], "w"),
)
PY
qual_redact_dir "${WORK}"
redacted="$(cat "${WORK}/result.json")"

assert_contains "preserves the image digest" "${redacted}" "sha256:aaaa"
assert_contains "preserves the image revision" "${redacted}" "383bc1d4aa11"
assert_contains "preserves unrelated fields" "${redacted}" "kept"
assert_not_contains "removes key material" "${redacted}" "cccccccccc"

printf 'password: hunter2hunter2\n' > "${WORK}/server.log"
printf 'built image sha256:%s ok\n' "$(printf 'a%.0s' {1..64})" >> "${WORK}/server.log"
qual_redact_dir "${WORK}"
server_log="$(cat "${WORK}/server.log")"
assert_not_contains "removes a secret from a log line" "${server_log}" "hunter2hunter2"
assert_contains "preserves a digest outside json" "${server_log}" "sha256:aaaa"

echo
echo "secret scan"
printf 'nothing to see\n' > "${WORK}/clean.txt"
rm -f "${WORK}/server.log"
if qual_scan_for_secrets "${WORK}" "supersecretvalue" 2>/dev/null; then
  pass "passes when no configured secret is present"
else
  fail "passes when no configured secret is present"
fi

printf 'leaked supersecretvalue here\n' > "${WORK}/leak.txt"
if qual_scan_for_secrets "${WORK}" "supersecretvalue" 2>/dev/null; then
  fail "fails when a configured secret is present" "the scan did not detect it"
else
  pass "fails when a configured secret is present"
fi

rm -f "${WORK}/leak.txt"
if qual_scan_for_secrets "${WORK}" "" 2>/dev/null; then
  pass "ignores an empty secret value"
else
  fail "ignores an empty secret value"
fi

echo
echo "pairing"
assert_eq "infers branch from a built image" "branch" "$(qual_infer_pairing built '')"
assert_eq "infers release from a pulled image" "release" "$(qual_infer_pairing pulled '')"
assert_eq "an explicit pairing wins" "published" "$(qual_infer_pairing pulled published)"

qual_check_pairing branch built 2>/dev/null && pass "branch accepts a built image" \
  || fail "branch accepts a built image"
qual_check_pairing branch pulled 2>/dev/null && fail "branch rejects a pulled image" \
  || pass "branch rejects a pulled image"
qual_check_pairing release pulled 2>/dev/null && pass "release accepts a pulled image" \
  || fail "release accepts a pulled image"
# Refused for either image source: the pairing claims the SDKs were installed
# from the registry, and nothing installs them, so accepting a pulled image
# would report consumer coverage that does not exist.
qual_check_pairing published built 2>/dev/null && fail "published rejects a built image" \
  || pass "published rejects a built image"
qual_check_pairing published pulled 2>/dev/null && fail "published rejects a pulled image too" \
  || pass "published rejects a pulled image too"
# Captured rather than piped: the function exits non-zero by design, and
# pipefail would make a matching grep look like a failure.
published_reason="$(qual_check_pairing published pulled 2>&1 >/dev/null)"
assert_contains "published says why it is refused" "${published_reason}" "not implemented"
qual_check_pairing nonsense built 2>/dev/null && fail "an unknown pairing is rejected" \
  || pass "an unknown pairing is rejected"

echo
echo "digest resolution"
# `--format '{{.Manifest.Digest}}'` looked right and silently returned buildx's
# human output, because the manifest key is lowercase and Go templates are
# case-sensitive. The release pairing pulls by digest, so this decided whether a
# published image could be qualified at all.
if command -v docker >/dev/null 2>&1; then
  # shellcheck source=../lib/image.sh
  source "${STACK_DIR}/lib/image.sh"
  resolved="$(qual_resolve_digest ghcr.io/openzeppelin/guardian:v0.17.0 2>/dev/null)"
  if [[ "${resolved}" =~ ^sha256:[0-9a-f]{64}$ ]]; then
    pass "resolves a published tag to a bare digest"
  else
    fail "resolves a published tag to a bare digest" "got '${resolved}'"
  fi
else
  skip "digest resolution" "docker CLI not installed"
fi

echo
echo "exit severity"
# 3 means nothing ran that could judge the product, and the live workflow maps
# it to success, so a product or setup failure has to beat it. `qual_exit_rank`
# is the one in lib/phase.sh, not a copy: a copy here would keep passing while
# the rule the run actually applies drifted away from it.
worse_of() {
  local a="$1" b="$2"
  if (( $(qual_exit_rank "${b}") > $(qual_exit_rank "${a}") )); then echo "${b}"; else echo "${a}"; fi
}
assert_eq "a product failure beats environment-blocked" "1" "$(worse_of 3 1)"
assert_eq "a setup failure beats environment-blocked" "2" "$(worse_of 3 2)"
assert_eq "environment-blocked beats success" "3" "$(worse_of 0 3)"
assert_eq "a product failure beats a setup failure" "1" "$(worse_of 2 1)"
assert_eq "success alone stays success" "0" "$(worse_of 0 0)"


echo
echo "run phases"
# An upgrade run talks to two images, and every one of these is a way the
# reports came out describing a server the phase never reached.
PHASE_LOG=""
DRIVER=(:)
SDK="both"
PROFILE="deterministic"
REPO_ROOT="${STACK_DIR}/../.."
NETWORK=""
RPC_ENDPOINT=""
CORE_ONLY=0
QUAL_HTTP_PORT=1
QUAL_GRPC_PORT=2
QUAL_OPERATOR_ALLOWLIST=""
SCENARIOS=()
DRIVER_ARGS=(run --profile deterministic)

# Both legs are replaced: what is under test is which of them a phase runs and
# what it tells them, not what they do.
phase_fixture() {
  local rust_exit="${1:-0}" ts_exit="${2:-0}"
  PHASE_LOG=""
  PHASE_RUST_EXIT="${rust_exit}"
  PHASE_TS_EXIT="${ts_exit}"
  DRIVER=(phase_fake_driver)
}
phase_fake_driver() {
  PHASE_LOG+="rust: $* "
  return "${PHASE_RUST_EXIT}"
}
phase_fake_typescript() {
  PHASE_LOG+="typescript: run=${1} out=${2} revision=${3} "
  return "${PHASE_TS_EXIT}"
}
# The real leg shells into npm inside a subshell, which cannot report back to
# these assertions, so the seam is the leg itself.
qual_run_typescript_leg() { phase_fake_typescript "$@"; }

phase_fixture
qual_run_phase "run-seed" "/tmp/out/seed" "sha256:seed" "seedrev" selected
assert_contains "a seed phase tells the driver the seed image" "${PHASE_LOG}" "--image-revision seedrev"
assert_contains "a seed phase writes where it was told" "${PHASE_LOG}" "--out /tmp/out/seed"
assert_contains "a seed phase runs the TypeScript leg too" "${PHASE_LOG}" "typescript: run=run-seed"
assert_contains "the TypeScript leg is told the same image" "${PHASE_LOG}" "revision=seedrev"

phase_fixture
qual_run_phase "run" "/tmp/out" "sha256:target" "targetrev" selected --post-restart
assert_contains "a target phase tells the driver the target image" "${PHASE_LOG}" "--image-revision targetrev"
assert_contains "extra arguments reach the Rust leg" "${PHASE_LOG}" "--post-restart"

phase_fixture
qual_run_phase "run-post-restart" "/tmp/out" "sha256:target" "targetrev" rust --post-restart
assert_not_contains "a rust-only phase leaves the TypeScript leg alone" "${PHASE_LOG}" "typescript:"

# `rust` narrows, never widens: a TypeScript-only request has no Rust leg for a
# Rust-only phase to run.
phase_fixture
SDK="typescript"
qual_run_phase "run-post-restart" "/tmp/out" "sha256:target" "targetrev" rust --post-restart
assert_eq "a rust-only phase runs nothing for a TypeScript-only request" "" "${PHASE_LOG}"
SDK="both"

phase_fixture 0 1
qual_run_phase "run" "/tmp/out" "sha256:target" "targetrev" selected
assert_eq "a TypeScript product failure decides the phase" "1" "${PHASE_EXIT}"

phase_fixture 3 0
qual_run_phase "run" "/tmp/out" "sha256:target" "targetrev" selected
assert_eq "an environment-blocked Rust leg does not mask a clean TypeScript leg" "3" "${PHASE_EXIT}"

phase_fixture 2 3
qual_run_phase "run" "/tmp/out" "sha256:target" "targetrev" selected
assert_eq "a setup failure beats environment-blocked across legs" "2" "${PHASE_EXIT}"
echo
echo "orphan reaping"
# A project with running containers belongs to a live run; reaping it would
# delete a concurrent run's volumes mid-flight.
reap_candidates() {
  printf '%s' "$1" | python3 -c "
import json, sys
for entry in json.load(sys.stdin):
    name = entry.get('Name', '')
    if not name.startswith('qual-'):
        continue
    if 'running' in entry.get('Status', ''):
        continue
    print(name)
"
}
LS_JSON='[{"Name":"qual-live","Status":"running(4)"},{"Name":"qual-dead","Status":"exited(4)"},{"Name":"other","Status":"exited(1)"}]'
assert_eq "a stopped qualification project is reaped" "qual-dead" "$(reap_candidates "${LS_JSON}")"
assert_not_contains "a running qualification project is left alone" "$(reap_candidates "${LS_JSON}")" "qual-live"
assert_not_contains "an unrelated project is left alone" "$(reap_candidates "${LS_JSON}")" "other"

echo
printf 'passed %d, failed %d, skipped %d\n' "${PASSED}" "${FAILED}" "${SKIPPED}"
[[ "${FAILED}" -eq 0 ]]
