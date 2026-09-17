#!/usr/bin/env bash
set -euo pipefail

# Entry point for the qualification suite. CI passes the same arguments a
# person would; there is no CI-only path.

STACK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd "${STACK_DIR}/../.." && pwd -P)"

# shellcheck source=lib/env.sh
source "${STACK_DIR}/lib/env.sh"
# shellcheck source=lib/ack.sh
source "${STACK_DIR}/lib/ack.sh"
# shellcheck source=lib/wait.sh
source "${STACK_DIR}/lib/wait.sh"
# shellcheck source=lib/teardown.sh
source "${STACK_DIR}/lib/teardown.sh"
# shellcheck source=lib/orphans.sh
source "${STACK_DIR}/lib/orphans.sh"
# shellcheck source=lib/image.sh
source "${STACK_DIR}/lib/image.sh"
# shellcheck source=lib/pairing.sh
source "${STACK_DIR}/lib/pairing.sh"
# shellcheck source=lib/redact.sh
source "${STACK_DIR}/lib/redact.sh"
# shellcheck source=lib/diagnostics.sh
source "${STACK_DIR}/lib/diagnostics.sh"
# shellcheck source=lib/restart.sh
source "${STACK_DIR}/lib/restart.sh"
# shellcheck source=lib/operator.sh
source "${STACK_DIR}/lib/operator.sh"

EXIT_SUCCESS=0
EXIT_PRODUCT_FAILURE=1
EXIT_SETUP_FAILURE=2
EXIT_ENVIRONMENT_BLOCKED=3
EXIT_USAGE=4

PROFILE="deterministic"
NETWORK=""
IMAGE_SOURCE="built"
IMAGE_REF="HEAD"
IMAGE_TAG=""
UPGRADE_FROM=""
PAIRING=""
SDK="both"
TRIGGER="dispatch"
REQUESTED_BY=""
CORE_ONLY=0
OUT_DIR="${REPO_ROOT}/qualification-results"
SCENARIOS=()
SELECTORS=()

usage() {
  cat <<'USAGE'
Usage: qualification/stack/run.sh [OPTIONS]

  --profile        deterministic | live        (default: deterministic)
  --network        devnet | testnet            (required for --profile live)
  --image-source   built | pulled              (default: built)
  --image-ref      git ref to build            (default: HEAD)
  --image-tag      tag or digest to pull       (with --image-source pulled)
  --upgrade-from   published tag to seed from, then upgrade to the image
                   under test on the same database. Pair it with a scenario
                   whose assertion fails on an empty database, such as
                   det-restart-durability; a scenario that only re-registers
                   the fixture account passes either way.
  --pairing        branch | release | published
  --scenario       scenario id (repeatable)
  --select         dimension=value (repeatable)
  --sdk            rust | typescript | both    (default: both)
  --trigger        schedule | dispatch | pull-request | publication | pre-release
  --requested-by   who asked for this run (required for --trigger pull-request)
  --core-only      restrict to the core subset
  --out            results directory
  --keep-stack     leave the stack running (debugging)

Exit codes: 0 concluded successfully, 1 product failure, 2 setup failure,
3 environment blocked and nothing else ran, 4 usage error.

A zero exit does not mean full coverage; read qualification_claim in the result.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --profile) PROFILE="${2:-}"; shift 2 ;;
    --network) NETWORK="${2:-}"; shift 2 ;;
    --image-source) IMAGE_SOURCE="${2:-}"; shift 2 ;;
    --image-ref) IMAGE_REF="${2:-}"; shift 2 ;;
    --image-tag) IMAGE_TAG="${2:-}"; shift 2 ;;
    --upgrade-from) UPGRADE_FROM="${2:-}"; shift 2 ;;
    --pairing) PAIRING="${2:-}"; shift 2 ;;
    --scenario) SCENARIOS+=("${2:-}"); shift 2 ;;
    --select) SELECTORS+=("${2:-}"); shift 2 ;;
    --sdk) SDK="${2:-}"; shift 2 ;;
    --trigger) TRIGGER="${2:-}"; shift 2 ;;
    --requested-by) REQUESTED_BY="${2:-}"; shift 2 ;;
    --core-only) CORE_ONLY=1; shift ;;
    --out) OUT_DIR="${2:-}"; shift 2 ;;
    --keep-stack) QUAL_KEEP_STACK=1; shift ;;
    -h|--help) usage; exit "${EXIT_SUCCESS}" ;;
    *) echo "error: unknown option $1" >&2; usage >&2; exit "${EXIT_USAGE}" ;;
  esac
done

case "${PROFILE}" in
  deterministic|live) ;;
  *) echo "error: --profile must be deterministic or live" >&2; exit "${EXIT_USAGE}" ;;
esac

if [[ "${PROFILE}" == "live" && -z "${NETWORK}" ]]; then
  echo "error: --network is required for the live profile" >&2
  exit "${EXIT_USAGE}"
fi

if [[ "${PROFILE}" == "live" && ! "${NETWORK}" =~ ^(devnet|testnet)$ ]]; then
  echo "error: --network must be devnet or testnet" >&2
  exit "${EXIT_USAGE}"
fi

if [[ "${IMAGE_SOURCE}" == "pulled" && -z "${IMAGE_TAG}" ]]; then
  echo "error: --image-tag is required with --image-source pulled" >&2
  exit "${EXIT_USAGE}"
fi

if [[ "${TRIGGER}" == "pull-request" ]]; then
  if [[ -z "${REQUESTED_BY}" ]]; then
    echo "error: --requested-by is required for --trigger pull-request" >&2
    exit "${EXIT_USAGE}"
  fi
  # A reviewer asking for evidence on a pull request gets the core subset, not
  # the full set.
  CORE_ONLY=1
fi

PAIRING="$(qual_infer_pairing "${IMAGE_SOURCE}" "${PAIRING}")" || exit "${EXIT_USAGE}"
qual_check_pairing "${PAIRING}" "${IMAGE_SOURCE}" || exit "${EXIT_USAGE}"

# Selectors are resolved to scenario ids here rather than passed through, so
# both drivers honour them without either needing to understand dimensions.
# Collecting them and doing nothing, as this did, meant `--select scheme=ecdsa`
# ran the whole profile and spent treasury funds on scenarios nobody asked for.
if (( ${#SELECTORS[@]} > 0 )); then
  SELECTED_IDS="$(python3 - "${REPO_ROOT}/qualification/manifest/manifest.json" "${PROFILE}" \
      "${SELECTORS[@]}" <<'SELECT'
import json, sys

manifest, profile, *selectors = sys.argv[1:]
scenarios = json.load(open(manifest))
scenarios = scenarios['scenarios'] if isinstance(scenarios, dict) else scenarios

wanted = {}
for selector in selectors:
    if '=' not in selector:
        sys.exit(f"selector '{selector}' is not dimension=value")
    dimension, value = selector.split('=', 1)
    wanted.setdefault(dimension, set()).add(value)

known = {'scheme', 'shape', 'mode', 'sdk', 'runtime'}
unknown = set(wanted) - known
if unknown:
    sys.exit(f"unknown selector dimension(s): {', '.join(sorted(unknown))}")

matched = [
    s['id']
    for s in scenarios
    if s['profile'] == profile
    and all(str(s.get(d)) in v for d, v in wanted.items())
]
if not matched:
    sys.exit('no scenario matches the given selectors')
print('\n'.join(matched))
SELECT
  )" || { echo "error: ${SELECTED_IDS:-selector resolution failed}" >&2; exit "${EXIT_USAGE}"; }

  while read -r id; do
    [[ -n "${id}" ]] && SCENARIOS+=("${id}")
  done <<< "${SELECTED_IDS}"
  echo "==> selectors matched ${#SCENARIOS[@]} scenario(s)"
fi

FILTERED=0
if (( ${#SCENARIOS[@]} > 0 )) || [[ "${SDK}" != "both" ]]; then
  FILTERED=1
fi

DRIVER=(cargo run --quiet --manifest-path "${REPO_ROOT}/crates/qualification-driver/Cargo.toml" --bin qualification-driver --)

echo "==> validating the scenario manifest"
if ! "${DRIVER[@]}" validate \
      --scenarios "${REPO_ROOT}/qualification/manifest/scenarios.toml" \
      --matrix "${REPO_ROOT}/qualification/manifest/matrix.toml"; then
  exit "${EXIT_SETUP_FAILURE}"
fi

echo "==> reaping orphaned stacks from earlier runs"
qual_reap_orphans ""

if [[ "${PROFILE}" == "deterministic" ]]; then
  NETWORK_TYPE="MidenLocal"
  RPC_ENDPOINT="http://rpc-stub:57291"
else
  case "${NETWORK}" in
    devnet) NETWORK_TYPE="MidenDevnet"; RPC_ENDPOINT="https://rpc.devnet.miden.io" ;;
    testnet) NETWORK_TYPE="MidenTestnet"; RPC_ENDPOINT="https://rpc.testnet.miden.io" ;;
  esac
fi

IMAGE_REVISION=""
if [[ "${IMAGE_SOURCE}" == "built" ]]; then
  SERVER_IMAGE="guardian-qualification:${IMAGE_REF//\//-}"
  echo "==> building ${SERVER_IMAGE} from ${IMAGE_REF}"
  if ! IMAGE_REVISION="$(qual_build_image "${REPO_ROOT}" "${IMAGE_REF}" "${SERVER_IMAGE}")"; then
    echo "error: image build failed" >&2
    exit "${EXIT_SETUP_FAILURE}"
  fi
else
  echo "==> resolving ${IMAGE_TAG} to a digest"
  DIGEST="$(qual_resolve_digest "${IMAGE_TAG}")" || exit "${EXIT_SETUP_FAILURE}"
  SERVER_IMAGE="${IMAGE_TAG%%@*}@${DIGEST}"
  docker pull --quiet "${SERVER_IMAGE}" >/dev/null || exit "${EXIT_SETUP_FAILURE}"
  IMAGE_REVISION="$(qual_image_revision "${SERVER_IMAGE}")"
  if [[ -z "${IMAGE_REVISION}" ]]; then
    echo "error: image carries no source revision label; identity cannot be asserted" >&2
    exit "${EXIT_SETUP_FAILURE}"
  fi
fi

# When upgrading, the stack comes up on the older image and the image under test
# replaces it later, so its migrations run against rows the older release wrote.
BOOT_IMAGE="${SERVER_IMAGE}"
if [[ -n "${UPGRADE_FROM}" ]]; then
  echo "==> resolving ${UPGRADE_FROM} to seed from"
  SEED_DIGEST="$(qual_resolve_digest "${UPGRADE_FROM}")" || exit "${EXIT_SETUP_FAILURE}"
  BOOT_IMAGE="${UPGRADE_FROM%%@*}@${SEED_DIGEST}"
  docker pull --quiet "${BOOT_IMAGE}" >/dev/null || exit "${EXIT_SETUP_FAILURE}"
fi

ENV_FILE="${STACK_DIR}/.env.generated"
qual_generate_env "${PROFILE}" "${NETWORK_TYPE}" "${RPC_ENDPOINT}" "${BOOT_IMAGE}" "${ENV_FILE}"
COMPOSE_FILE="${STACK_DIR}/compose.yml"

echo "==> provisioning the acknowledgement identity"
if [[ "${PROFILE}" == "deterministic" ]]; then
  if ! qual_provision_fixture_ack_keys "${STACK_DIR}/ack-keys" "${REPO_ROOT}"; then
    exit "${EXIT_SETUP_FAILURE}"
  fi
elif ! qual_provision_ack_keys "${STACK_DIR}/ack-keys" "${SERVER_IMAGE}"; then
  exit "${EXIT_SETUP_FAILURE}"
fi

# The migration target needs an identity of its own: migrating to a GUARDIAN
# with the same acknowledgement key changes nothing on the account, and the
# transaction has no state change to commit.
if ! qual_provision_ack_keys "${STACK_DIR}/ack-keys-migration-target" "${SERVER_IMAGE}"; then
  exit "${EXIT_SETUP_FAILURE}"
fi

echo "==> seeding the operator allowlist"
QUAL_OPERATOR_ALLOWLIST="${STACK_DIR}/operator/operators.json"
if ! qual_write_operator_allowlist "${REPO_ROOT}" "${QUAL_OPERATOR_ALLOWLIST}" ""; then
  exit "${EXIT_SETUP_FAILURE}"
fi
export QUAL_OPERATOR_ALLOWLIST

qual_install_teardown_trap "${QUAL_PROJECT}" "${COMPOSE_FILE}" "${ENV_FILE}"

echo "==> starting the stack (project ${QUAL_PROJECT})"
if ! docker compose -p "${QUAL_PROJECT}" -f "${COMPOSE_FILE}" --env-file "${ENV_FILE}" up -d --wait --wait-timeout 180; then
  qual_capture_diagnostics "${QUAL_PROJECT}" "${COMPOSE_FILE}" "${ENV_FILE}" "${OUT_DIR}/diagnostics"
  echo "error: the stack did not start" >&2
  exit "${EXIT_SETUP_FAILURE}"
fi

echo "==> waiting for readiness on ports ${QUAL_HTTP_PORT} and ${QUAL_GRPC_PORT}"
if ! qual_wait_ready "${QUAL_HTTP_PORT}" "${QUAL_GRPC_PORT}" 180; then
  qual_capture_diagnostics "${QUAL_PROJECT}" "${COMPOSE_FILE}" "${ENV_FILE}" "${OUT_DIR}/diagnostics"
  exit "${EXIT_SETUP_FAILURE}"
fi

echo "==> waiting for the migration target on ports ${QUAL_HTTP_PORT_B} and ${QUAL_GRPC_PORT_B}"
if ! qual_wait_ready "${QUAL_HTTP_PORT_B}" "${QUAL_GRPC_PORT_B}" 180; then
  qual_capture_diagnostics "${QUAL_PROJECT}" "${COMPOSE_FILE}" "${ENV_FILE}" "${OUT_DIR}/diagnostics"
  exit "${EXIT_SETUP_FAILURE}"
fi
# The Rust SDK speaks gRPC to GUARDIAN and the TypeScript SDK speaks HTTP, so
# one shared endpoint would send one of them to a listener that cannot answer.
QUAL_GUARDIAN_MIGRATION_GRPC="http://127.0.0.1:${QUAL_GRPC_PORT_B}"
QUAL_GUARDIAN_MIGRATION_HTTP="http://127.0.0.1:${QUAL_HTTP_PORT_B}"
export QUAL_GUARDIAN_MIGRATION_GRPC QUAL_GUARDIAN_MIGRATION_HTTP

echo "==> waiting for the scheme-gated server on ports ${QUAL_HTTP_PORT_C} and ${QUAL_GRPC_PORT_C}"
if ! qual_wait_ready "${QUAL_HTTP_PORT_C}" "${QUAL_GRPC_PORT_C}" 180; then
  qual_capture_diagnostics "${QUAL_PROJECT}" "${COMPOSE_FILE}" "${ENV_FILE}" "${OUT_DIR}/diagnostics"
  exit "${EXIT_SETUP_FAILURE}"
fi
QUAL_GUARDIAN_SCHEME_GATED_GRPC="http://127.0.0.1:${QUAL_GRPC_PORT_C}"
QUAL_GUARDIAN_SCHEME_GATED_HTTP="http://127.0.0.1:${QUAL_HTTP_PORT_C}"
export QUAL_GUARDIAN_SCHEME_GATED_GRPC QUAL_GUARDIAN_SCHEME_GATED_HTTP

mkdir -p "${OUT_DIR}"

# Refuse an underfunded live run before it spends anything. Without this the
# first shortfall surfaces mid-scenario as a setup failure, after earlier
# scenarios have already transferred funds that cannot be recovered.
#
# What a run needs scales with how many accounts it funds: roughly two fundings
# per scenario leg, and a leg per SDK. The estimate is deliberately generous —
# refusing a run that would have just fitted costs a top-up, while starting one
# that does not fit costs the spend that came before the shortfall.
if [[ "${PROFILE}" == "live" ]]; then
  if (( ${#SCENARIOS[@]} > 0 )); then
    LEGS=$(( ${#SCENARIOS[@]} * 2 ))
  else
    LIVE_COUNT="$(python3 -c "
import json, sys
scenarios = json.load(open('${REPO_ROOT}/qualification/manifest/manifest.json'))
scenarios = scenarios['scenarios'] if isinstance(scenarios, dict) else scenarios
print(sum(1 for s in scenarios if s['profile'] == 'live'))
" 2>/dev/null)"
    if [[ ! "${LIVE_COUNT}" =~ ^[0-9]+$ ]]; then
      echo "error: cannot count the live scenarios to size the treasury check" >&2
      exit "${EXIT_SETUP_FAILURE}"
    fi
    LEGS=$(( LIVE_COUNT * 2 ))
  fi
  QUAL_TREASURY_REQUIRED="${QUAL_TREASURY_REQUIRED:-$(( LEGS * 400000 ))}"

  echo "==> checking the treasury covers ${LEGS} scenario leg(s)"
  set +e
  "${DRIVER[@]}" treasury-check \
    --network "${NETWORK}" \
    --required "${QUAL_TREASURY_REQUIRED}" \
    --per-run-cost 400000
  TREASURY_EXIT=$?
  set -e
  if (( TREASURY_EXIT != 0 )); then
    echo "error: the treasury cannot cover this run; nothing was spent" >&2
    exit "${EXIT_SETUP_FAILURE}"
  fi
fi

DRIVER_ARGS=(
  run
  --profile "${PROFILE}"
  --http-endpoint "http://127.0.0.1:${QUAL_HTTP_PORT}"
  --grpc-endpoint "http://127.0.0.1:${QUAL_GRPC_PORT}"
  --image-digest "${SERVER_IMAGE##*@}"
  --image-revision "${IMAGE_REVISION}"
  --pairing "${PAIRING}"
  --run-id "${QUAL_RUN_ID}"
  --trigger "${TRIGGER}"
  --out "${OUT_DIR}"
)
[[ "${PROFILE}" == "live" ]] && DRIVER_ARGS+=(--network "${NETWORK}")
# Always Rust. Without this the default `both` leaves the Rust binary selecting
# TypeScript scenarios too, which it fail-closes as unimplemented, so the run
# exits 2 however well the Rust legs went — after they have already spent.
DRIVER_ARGS+=(--sdk rust)
[[ -n "${REQUESTED_BY}" ]] && DRIVER_ARGS+=(--requested-by "${REQUESTED_BY}")
(( CORE_ONLY == 1 )) && DRIVER_ARGS+=(--core-only)
(( FILTERED == 1 )) && DRIVER_ARGS+=(--filtered)
for scenario in "${SCENARIOS[@]+"${SCENARIOS[@]}"}"; do
  DRIVER_ARGS+=(--scenario "${scenario}")
done

DRIVER_EXIT=0
if [[ "${SDK}" == "both" || "${SDK}" == "rust" ]]; then
  echo "==> running Rust scenarios"
  set +e
  "${DRIVER[@]}" "${DRIVER_ARGS[@]}"
  DRIVER_EXIT=$?
  set -e
fi

TS_EXIT=0
if [[ "${SDK}" == "both" || "${SDK}" == "typescript" ]]; then
  echo "==> running TypeScript scenarios"
  set +e
  (
    cd "${REPO_ROOT}/packages/miden-multisig-client" || exit 2
    # The deterministic profile funds nothing, so the key has no business in
    # this process. The live profile still needs it: the funding bridge spawns
    # the Rust binary, which inherits this environment to read it. Narrowing it
    # to the profile that spends is as far as this goes until funding is split
    # out into a trusted step that hands the driver ephemeral keys only.
    [[ "${PROFILE}" == "live" ]] || unset QUAL_TREASURY_KEY
    QUAL_PROFILE="${PROFILE}" \
    QUAL_OUT_DIR="${OUT_DIR}" \
    QUAL_RUN_ID="${QUAL_RUN_ID}" \
    QUAL_HTTP_ENDPOINT="http://127.0.0.1:${QUAL_HTTP_PORT}" \
    QUAL_GRPC_ENDPOINT="http://127.0.0.1:${QUAL_GRPC_PORT}" \
    QUAL_IMAGE_REVISION="${IMAGE_REVISION}" \
    QUAL_NETWORK="${NETWORK}" \
    QUAL_MIDEN_RPC_ENDPOINT="${RPC_ENDPOINT}" \
    QUAL_GUARDIAN_MIGRATION_ENDPOINT="${QUAL_GUARDIAN_MIGRATION_HTTP:-}" \
    QUAL_REPO_ROOT="${REPO_ROOT}" \
    QUAL_CORE_ONLY="${CORE_ONLY}" \
    QUAL_OPERATOR_ALLOWLIST="${QUAL_OPERATOR_ALLOWLIST}" \
    QUAL_SCENARIOS="${SCENARIOS[*]+"${SCENARIOS[*]}"}" \
      npm run --silent test:qualification
  )
  TS_EXIT=$?
  set -e
fi

# The worse of the two decides. "Worse" is not numeric order: 3 means nothing
# ran that could judge the product and the live workflow maps it to success, so
# a product failure (1) or a setup failure (2) has to beat it. Comparing
# `DRIVER_EXIT == 0` instead would let a Rust leg that was entirely
# environment-blocked swallow a TypeScript product failure and report green.
qual_exit_rank() {
  case "${1}" in
    0) echo 0 ;;
    3) echo 1 ;;
    2) echo 2 ;;
    *) echo 3 ;;
  esac
}
if (( $(qual_exit_rank "${TS_EXIT}") > $(qual_exit_rank "${DRIVER_EXIT}") )); then
  DRIVER_EXIT=${TS_EXIT}
fi

# The upgrade question, asked after the seeding scenarios have written real rows
# through the product's own API: does the image under test boot on a database an
# older release wrote, and is that data still there once its migrations have run?
# A hand-written SQL fixture would test the same path but has to be kept in step
# with a schema it does not own, so the seed is whatever the scenarios above
# actually stored.
if [[ -n "${UPGRADE_FROM}" && "${SDK}" != "typescript" ]]; then
  echo "==> upgrading from ${UPGRADE_FROM} to the image under test"
  if ! qual_swap_server_image "${QUAL_PROJECT}" "${COMPOSE_FILE}" "${ENV_FILE}" "${SERVER_IMAGE}" \
     || ! qual_wait_ready "${QUAL_HTTP_PORT}" "${QUAL_GRPC_PORT}" 180; then
    # Refusing to boot on an older release's data is the defect this looks for,
    # so it is a product failure rather than a setup problem.
    echo "error: the image under test did not become ready on the upgraded database" >&2
    qual_capture_diagnostics "${QUAL_PROJECT}" "${COMPOSE_FILE}" "${ENV_FILE}" "${OUT_DIR}/diagnostics"
    DRIVER_EXIT=${EXIT_PRODUCT_FAILURE}
  else
    UPGRADE_ARGS=()
    for arg in "${DRIVER_ARGS[@]}"; do
      UPGRADE_ARGS+=("${arg}")
    done
    for index in "${!UPGRADE_ARGS[@]}"; do
      if [[ "${UPGRADE_ARGS[${index}]}" == "${QUAL_RUN_ID}" ]]; then
        UPGRADE_ARGS[${index}]="${QUAL_RUN_ID}-post-upgrade"
        break
      fi
    done
    # `--post-restart` so the durability assertion actually asserts. Without it
    # `restart-durability` skips, and the scenarios that remain re-register the
    # fixture account, which is idempotent and would pass just as happily
    # against an empty database. An upgrade check that cannot tell a migrated
    # database from a fresh one proves nothing.
    set +e
    "${DRIVER[@]}" "${UPGRADE_ARGS[@]}" --post-restart
    UPGRADE_EXIT=$?
    set -e
    if (( $(qual_exit_rank "${UPGRADE_EXIT}") > $(qual_exit_rank "${DRIVER_EXIT}") )); then
      DRIVER_EXIT=${UPGRADE_EXIT}
    fi
  fi
fi

# Durability can only be asserted after the process that wrote the data is gone,
# so the restart happens here and the assertion runs in its own pass.
# Gated on the Rust leg having run: these are Rust arguments, and a
# TypeScript-only request has no Rust run for them to belong to.
if [[ "${PROFILE}" == "deterministic" && ( "${SDK}" == "both" || "${SDK}" == "rust" ) ]]; then
  echo "==> restarting Guardian and re-checking durability"
  if qual_restart_server "${QUAL_PROJECT}" "${COMPOSE_FILE}" "${ENV_FILE}" \
     && qual_wait_ready "${QUAL_HTTP_PORT}" "${QUAL_GRPC_PORT}" 180; then
    RESTART_ARGS=()
    for arg in "${DRIVER_ARGS[@]}"; do
      RESTART_ARGS+=("${arg}")
    done
    # One --run-id only: clap rejects a repeated flag and would exit before
    # asserting anything.
    for index in "${!RESTART_ARGS[@]}"; do
      if [[ "${RESTART_ARGS[${index}]}" == "${QUAL_RUN_ID}" ]]; then
        RESTART_ARGS[${index}]="${QUAL_RUN_ID}-post-restart"
        break
      fi
    done
    set +e
    "${DRIVER[@]}" "${RESTART_ARGS[@]}" --post-restart
    RESTART_EXIT=$?
    set -e
    if (( RESTART_EXIT != 0 && DRIVER_EXIT == 0 )); then
      DRIVER_EXIT=${RESTART_EXIT}
    fi
  else
    echo "error: Guardian did not come back after the restart" >&2
    DRIVER_EXIT=2
  fi
fi

# Both legs have written their results, so the run can be restated over the
# combined set. Until this runs, the Rust file carries a claim derived from
# Rust's required entries alone and a TypeScript failure sits in a sidecar the
# claim never saw.
if [[ "${SDK}" != "both" ]]; then
  # A single-SDK run is filtered by definition, so it can make no qualification
  # claim and there is nothing for a merged report to restate. Said out loud so
  # the missing merged/report.json reads as a consequence of the request rather
  # than a failure, and so nobody mistakes one leg's file for a run result.
  echo "==> single-SDK run (${SDK}): no merged report; this run claims no qualification"
elif [[ "${SDK}" == "both" ]]; then
  echo "==> merging results"
  # Written to a subdirectory: the merger reads every JSON file at the top of
  # the results directory, and a merged report left beside them is not a run.
  mkdir -p "${OUT_DIR}/merged"
  set +e
  "${DRIVER[@]}" report \
    --results "${OUT_DIR}" \
    --run-id "${QUAL_RUN_ID}" \
    --scenarios "${REPO_ROOT}/qualification/manifest/scenarios.toml" \
    --matrix "${REPO_ROOT}/qualification/manifest/matrix.toml" \
    > "${OUT_DIR}/merged/report.json"
  MERGE_EXIT=$?
  set -e
  if (( MERGE_EXIT != 0 )); then
    echo "error: the results could not be merged; the per-leg files stand as written" >&2
    rm -rf "${OUT_DIR}/merged"
    (( DRIVER_EXIT == 0 )) && DRIVER_EXIT=2
  fi
fi

if (( DRIVER_EXIT != 0 )); then
  qual_capture_diagnostics "${QUAL_PROJECT}" "${COMPOSE_FILE}" "${ENV_FILE}" "${OUT_DIR}/diagnostics"
fi

# Redaction runs over everything retained, not just diagnostics, and the scan
# gates the release marker. CI uploads artifacts only when that marker exists,
# so a failed scan cannot be followed by an upload of the thing that failed it.
echo "==> redacting and scanning artifacts"
rm -f "${OUT_DIR}/.scan-passed"
qual_redact_dir "${OUT_DIR}"
if ! qual_scan_for_secrets "${OUT_DIR}" "${QUAL_POSTGRES_PASSWORD}" "${QUAL_TREASURY_KEY:-}"; then
  echo "error: a configured secret reached a retained artifact" >&2
  find "${OUT_DIR}" -type f -delete 2>/dev/null || true
  exit "${EXIT_SETUP_FAILURE}"
fi
touch "${OUT_DIR}/.scan-passed"

case "${DRIVER_EXIT}" in
  0) exit "${EXIT_SUCCESS}" ;;
  1) exit "${EXIT_PRODUCT_FAILURE}" ;;
  2) exit "${EXIT_SETUP_FAILURE}" ;;
  3) exit "${EXIT_ENVIRONMENT_BLOCKED}" ;;
  *) exit "${EXIT_PRODUCT_FAILURE}" ;;
esac
