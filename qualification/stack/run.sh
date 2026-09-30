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
# shellcheck source=lib/summary.sh
source "${STACK_DIR}/lib/summary.sh"
# shellcheck source=lib/phase.sh
source "${STACK_DIR}/lib/phase.sh"
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

# An upgrade run seeds a database and then asks the image under test to boot on
# it, which is a deterministic question. Asking it on the live profile would run
# every funded scenario twice, once against each image, and pay for both.
if [[ "${PROFILE}" == "live" && -n "${UPGRADE_FROM}" ]]; then
  echo "error: --upgrade-from is for the deterministic profile; on live it would fund every scenario twice" >&2
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
# What the report records as the image this run used. A tag is not that: it can
# be rebuilt under the same name, so a locally built image is identified by its
# own content id instead, which is the same shape as a registry digest.
SERVER_DIGEST=""
if [[ "${IMAGE_SOURCE}" == "built" ]]; then
  SERVER_IMAGE="guardian-qualification:${IMAGE_REF//\//-}"
  echo "==> building ${SERVER_IMAGE} from ${IMAGE_REF}"
  if ! IMAGE_REVISION="$(qual_build_image "${REPO_ROOT}" "${IMAGE_REF}" "${SERVER_IMAGE}")"; then
    echo "error: image build failed" >&2
    exit "${EXIT_SETUP_FAILURE}"
  fi
  SERVER_DIGEST="$(qual_image_id "${SERVER_IMAGE}")" || SERVER_DIGEST=""
else
  echo "==> resolving ${IMAGE_TAG} to a digest"
  DIGEST="$(qual_resolve_digest "${IMAGE_TAG}")" || exit "${EXIT_SETUP_FAILURE}"
  SERVER_IMAGE="${IMAGE_TAG%%@*}@${DIGEST}"
  docker pull --quiet "${SERVER_IMAGE}" >/dev/null || exit "${EXIT_SETUP_FAILURE}"
  SERVER_DIGEST="${DIGEST}"
  IMAGE_REVISION="$(qual_image_revision "${SERVER_IMAGE}")"
  if [[ -z "${IMAGE_REVISION}" ]]; then
    echo "error: image carries no source revision label; identity cannot be asserted" >&2
    exit "${EXIT_SETUP_FAILURE}"
  fi
fi

# When upgrading, the stack comes up on the older image and the image under test
# replaces it later, so its migrations run against rows the older release wrote.
BOOT_IMAGE="${SERVER_IMAGE}"
BOOT_DIGEST="${SERVER_DIGEST}"
SEED_REVISION="${IMAGE_REVISION}"
if [[ -n "${UPGRADE_FROM}" ]]; then
  echo "==> resolving ${UPGRADE_FROM} to seed from"
  SEED_DIGEST="$(qual_resolve_digest "${UPGRADE_FROM}")" || exit "${EXIT_SETUP_FAILURE}"
  BOOT_IMAGE="${UPGRADE_FROM%%@*}@${SEED_DIGEST}"
  docker pull --quiet "${BOOT_IMAGE}" >/dev/null || exit "${EXIT_SETUP_FAILURE}"
  BOOT_DIGEST="${SEED_DIGEST}"
  # The seed phase runs against this image, and the identity scenario compares
  # what the server reports with what the phase was told to expect. Told the
  # target's revision, it would fail on every upgrade between two commits, which
  # is every upgrade worth running.
  SEED_REVISION="$(qual_image_revision "${BOOT_IMAGE}")"
  if [[ -z "${SEED_REVISION}" ]]; then
    echo "error: ${UPGRADE_FROM} carries no source revision label; the seed phase could not be attributed to it" >&2
    exit "${EXIT_SETUP_FAILURE}"
  fi
fi

# Named after the run, and everything the run writes goes inside it. The ports
# and the Compose project were already per run; these were not, so two runs
# shared one set of acknowledgement keys and one operator allowlist.
QUAL_RUN_ID="${QUAL_RUN_ID:-qual-$(date -u +%Y%m%d-%H%M%S)-$(qual_random_suffix)}"
QUAL_PROJECT="$(qual_project_name "${QUAL_PROJECT:-${QUAL_RUN_ID}}")"
export QUAL_RUN_ID QUAL_PROJECT
RUN_DIR="${STACK_DIR}/runs/${QUAL_PROJECT}"
mkdir -p "${RUN_DIR}"

ENV_FILE="${RUN_DIR}/.env.generated"
qual_generate_env "${PROFILE}" "${NETWORK_TYPE}" "${RPC_ENDPOINT}" "${BOOT_IMAGE}" "${ENV_FILE}" "${RUN_DIR}"
COMPOSE_FILE="${STACK_DIR}/compose.yml"

echo "==> provisioning the acknowledgement identity"
if [[ "${PROFILE}" == "deterministic" ]]; then
  if ! qual_provision_fixture_ack_keys "${QUAL_ACK_KEYS_DIR}" "${REPO_ROOT}"; then
    exit "${EXIT_SETUP_FAILURE}"
  fi
elif ! qual_provision_ack_keys "${QUAL_ACK_KEYS_DIR}" "${SERVER_IMAGE}"; then
  exit "${EXIT_SETUP_FAILURE}"
fi

# The migration target needs an identity of its own: migrating to a GUARDIAN
# with the same acknowledgement key changes nothing on the account, and the
# transaction has no state change to commit.
if ! qual_provision_ack_keys "${QUAL_ACK_KEYS_MIGRATION_DIR}" "${SERVER_IMAGE}"; then
  exit "${EXIT_SETUP_FAILURE}"
fi

echo "==> seeding the operator allowlist"
QUAL_OPERATOR_ALLOWLIST="${QUAL_OPERATOR_DIR}/operators.json"
if ! qual_write_operator_allowlist "${REPO_ROOT}" "${QUAL_OPERATOR_ALLOWLIST}" ""; then
  exit "${EXIT_SETUP_FAILURE}"
fi
export QUAL_OPERATOR_ALLOWLIST

qual_install_teardown_trap "${QUAL_PROJECT}" "${COMPOSE_FILE}" "${ENV_FILE}" "${RUN_DIR}"

# Redaction runs over everything retained, not just diagnostics, and the scan
# gates the release marker. CI uploads artifacts only when that marker exists,
# so a failed scan cannot be followed by an upload of the thing that failed it.
seal_artifacts() {
  echo "==> redacting and scanning artifacts"
  mkdir -p "${OUT_DIR}"
  rm -f "${OUT_DIR}/.scan-passed"
  qual_redact_dir "${OUT_DIR}"
  if ! qual_scan_for_secrets "${OUT_DIR}" "${QUAL_POSTGRES_PASSWORD}" "${QUAL_TREASURY_KEY:-}"; then
    echo "error: a configured secret reached a retained artifact" >&2
    find "${OUT_DIR}" -type f -delete 2>/dev/null || true
    return 1
  fi
  touch "${OUT_DIR}/.scan-passed"
}

# A stack that never became ready is exactly when its logs are wanted, so they
# go through the same scan as a finished run's rather than being captured and
# then left out of the upload for want of the marker.
stack_setup_failed() {
  qual_capture_diagnostics "${QUAL_PROJECT}" "${COMPOSE_FILE}" "${ENV_FILE}" "${OUT_DIR}/diagnostics"
  seal_artifacts || true
  exit "${EXIT_SETUP_FAILURE}"
}

echo "==> starting the stack (project ${QUAL_PROJECT})"
if ! docker compose -p "${QUAL_PROJECT}" -f "${COMPOSE_FILE}" --env-file "${ENV_FILE}" up -d --wait --wait-timeout 180; then
  echo "error: the stack did not start" >&2
  stack_setup_failed
fi

echo "==> waiting for readiness on ports ${QUAL_HTTP_PORT} and ${QUAL_GRPC_PORT}"
if ! qual_wait_ready "${QUAL_HTTP_PORT}" "${QUAL_GRPC_PORT}" 180; then
  stack_setup_failed
fi

echo "==> waiting for the migration target on ports ${QUAL_HTTP_PORT_B} and ${QUAL_GRPC_PORT_B}"
if ! qual_wait_ready "${QUAL_HTTP_PORT_B}" "${QUAL_GRPC_PORT_B}" 180; then
  stack_setup_failed
fi
# The Rust SDK speaks gRPC to GUARDIAN and the TypeScript SDK speaks HTTP, so
# one shared endpoint would send one of them to a listener that cannot answer.
QUAL_GUARDIAN_MIGRATION_GRPC="http://127.0.0.1:${QUAL_GRPC_PORT_B}"
QUAL_GUARDIAN_MIGRATION_HTTP="http://127.0.0.1:${QUAL_HTTP_PORT_B}"
export QUAL_GUARDIAN_MIGRATION_GRPC QUAL_GUARDIAN_MIGRATION_HTTP

echo "==> waiting for the scheme-gated server on ports ${QUAL_HTTP_PORT_C} and ${QUAL_GRPC_PORT_C}"
if ! qual_wait_ready "${QUAL_HTTP_PORT_C}" "${QUAL_GRPC_PORT_C}" 180; then
  stack_setup_failed
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

  # Bound what this run can move, at twice what the preflight says it needs. A
  # cap only the operator could set is a cap nobody sets, and the drain worth
  # guarding against is an unattended run funding in a loop, which is exactly
  # the nightly. Doubling leaves room for retries without leaving the treasury
  # open. Override QUAL_SPEND_CAP deliberately for a run that needs more.
  QUAL_SPEND_CAP="${QUAL_SPEND_CAP:-$(( QUAL_TREASURY_REQUIRED * 2 ))}"
  export QUAL_SPEND_CAP
  echo "==> capping this run at ${QUAL_SPEND_CAP} units"
  # Cleared per run: the tally lives beside the treasury lock so it can span the
  # processes the TypeScript leg spawns. It is keyed by QUAL_RUN_ID, so this
  # clears only this run's, and a reused run id cannot carry an earlier run's
  # spending into this one.
  "${DRIVER[@]}" spend-reset --network "${NETWORK}" >/dev/null 2>&1 || true
fi

# What every phase passes, which is everything except the identity of the image
# it is talking to and where its results go. Those are per phase on purpose: an
# upgrade run talks to two different images, and a phase that recorded the other
# one would describe a server it never reached.
DRIVER_ARGS=(
  run
  --profile "${PROFILE}"
  --http-endpoint "http://127.0.0.1:${QUAL_HTTP_PORT}"
  --grpc-endpoint "http://127.0.0.1:${QUAL_GRPC_PORT}"
  --pairing "${PAIRING}"
  --trigger "${TRIGGER}"
)
[[ "${PROFILE}" == "live" ]] && DRIVER_ARGS+=(--network "${NETWORK}")
# Always Rust. Without this the default `both` leaves the Rust binary selecting
# TypeScript scenarios too, which it fail-closes as unimplemented, so the run
# exits 2 however well the Rust legs went — after they have already spent.
DRIVER_ARGS+=(--sdk rust)
[[ -n "${REQUESTED_BY}" ]] && DRIVER_ARGS+=(--requested-by "${REQUESTED_BY}")
(( CORE_ONLY == 1 )) && DRIVER_ARGS+=(--core-only)

# Kept out of DRIVER_ARGS so a phase can ask for a different set. Only the seed
# phase does, and it is the reason this is separable at all. `--filtered` goes
# with the selection it describes: in DRIVER_ARGS it reached the seed phase a
# second time beside the seed's own, and clap refuses a repeated flag.
SELECTED_SCENARIOS=()
for scenario in "${SCENARIOS[@]+"${SCENARIOS[@]}"}"; do
  SELECTED_SCENARIOS+=(--scenario "${scenario}")
done
(( FILTERED == 1 )) && SELECTED_SCENARIOS+=(--filtered)

# What the seed phase runs: the scenarios that write rows an upgrade has to
# find again, and nothing else.
#
# Running the whole set against the older release looked thorough and was worse
# than useless. `det-scheme-gate` asserts a *refusal*, and registering its
# account on the release that had no gate yet left it already configured, so the
# target phase's registration came back idempotently successful and the scenario
# read that as the gate failing. A seed phase that quietly decides a later
# assertion is not seeding, it is interfering.
SEED_SCENARIOS=(
  --scenario det-fixture-grpc
  --scenario det-proposal-lifecycle
  --scenario det-restart-durability
  --filtered
)

DRIVER_EXIT=0

if [[ -n "${UPGRADE_FROM}" ]]; then
  # The upgrade question, asked after seeding scenarios have written real rows
  # through the product's own API: does the image under test boot on a database
  # an older release wrote, and is that data still there once its migrations
  # have run? A hand-written SQL fixture would test the same path but has to be
  # kept in step with a schema it does not own, so the seed is whatever the
  # scenarios actually stored.
  #
  # The seed phase talks to the older release, so what it reports is that
  # release's behaviour. It is recorded under the seed image's own identity, in
  # a directory the merge does not read: the run is a claim about the image
  # under test, and a seed-phase failure is not that image's failure.
  # Rust alone: everything durable the seed needs goes in through the same API
  # either leg would use, and the TypeScript leg's scenarios write nothing the
  # Rust ones do not.
  echo "==> seeding on ${UPGRADE_FROM}"
  qual_run_phase "${QUAL_RUN_ID}-seed" "${OUT_DIR}/seed" \
    "${BOOT_DIGEST}" "${SEED_REVISION}" rust "${SEED_SCENARIOS[@]}"
  # Announced, never folded into the run's verdict. A release old enough to be
  # worth upgrading from is old enough to fail scenarios written after it, which
  # is a fact about that release rather than a defect in the image under test:
  # seeding from v0.17.0 fails `det-scheme-gate` because the scheme gate did not
  # exist yet. What proves the seeding actually happened is the target phase's
  # own durability assertion, which looks for the rows this phase wrote.
  if (( PHASE_EXIT != 0 )); then
    echo "note: ${UPGRADE_FROM} did not pass every scenario while seeding (exit ${PHASE_EXIT}); its results are in seed/ and the upgrade continues" >&2
  fi

  echo "==> upgrading from ${UPGRADE_FROM} to the image under test"
  # All three are recreated on the new image, so all three are waited for: a
  # migration target or scheme-gated server still booting would fail its
  # scenario as though the image under test had refused it.
  if ! qual_swap_server_image "${QUAL_PROJECT}" "${COMPOSE_FILE}" "${ENV_FILE}" "${SERVER_IMAGE}" \
     || ! qual_wait_ready "${QUAL_HTTP_PORT}" "${QUAL_GRPC_PORT}" 180 \
     || ! qual_wait_ready "${QUAL_HTTP_PORT_B}" "${QUAL_GRPC_PORT_B}" 180 \
     || ! qual_wait_ready "${QUAL_HTTP_PORT_C}" "${QUAL_GRPC_PORT_C}" 180; then
    # Refusing to boot on an older release's data is the defect this looks
    # for, so it is a product failure rather than a setup problem.
    echo "error: the image under test did not become ready on the upgraded database" >&2
    qual_capture_diagnostics "${QUAL_PROJECT}" "${COMPOSE_FILE}" "${ENV_FILE}" "${OUT_DIR}/diagnostics"
    DRIVER_EXIT=${EXIT_PRODUCT_FAILURE}
  else
    # Both SDKs, against the upgraded target: this phase is the run's claim, and
    # a leg that only ever saw the older release cannot speak for it.
    #
    # `--post-restart` so the durability assertion actually asserts. Without it
    # `restart-durability` skips, and the scenarios that remain re-register the
    # fixture account, which is idempotent and would pass just as happily
    # against an empty database. An upgrade check that cannot tell a migrated
    # database from a fresh one proves nothing, and it is also what makes a
    # silently empty seed phase visible here.
    qual_run_phase "${QUAL_RUN_ID}" "${OUT_DIR}" \
      "${SERVER_DIGEST}" "${IMAGE_REVISION}" selected \
      "${SELECTED_SCENARIOS[@]+"${SELECTED_SCENARIOS[@]}"}" --post-restart
    DRIVER_EXIT=${PHASE_EXIT}
  fi
else
  qual_run_phase "${QUAL_RUN_ID}" "${OUT_DIR}" "${SERVER_DIGEST}" "${IMAGE_REVISION}" selected \
    "${SELECTED_SCENARIOS[@]+"${SELECTED_SCENARIOS[@]}"}"
  DRIVER_EXIT=${PHASE_EXIT}
fi
# Durability can only be asserted after the process that wrote the data is gone,
# so the restart happens here and the assertion runs in its own pass.
# Gated on the Rust leg having run: these are Rust arguments, and a
# TypeScript-only request has no Rust run for them to belong to.
if [[ "${PROFILE}" == "deterministic" && ( "${SDK}" == "both" || "${SDK}" == "rust" ) ]]; then
  echo "==> restarting Guardian and re-checking durability"
  if qual_restart_server "${QUAL_PROJECT}" "${COMPOSE_FILE}" "${ENV_FILE}" \
     && qual_wait_ready "${QUAL_HTTP_PORT}" "${QUAL_GRPC_PORT}" 180; then
    # Rust alone: `--post-restart` is a Rust argument, and the durability
    # assertion is a Rust action, so re-running the TypeScript leg here would
    # cost a second pass to assert nothing new.
    qual_run_phase "${QUAL_RUN_ID}-post-restart" "${OUT_DIR}" \
      "${SERVER_DIGEST}" "${IMAGE_REVISION}" rust \
      "${SELECTED_SCENARIOS[@]+"${SELECTED_SCENARIOS[@]}"}" --post-restart
    # Ranked, like every other combination here. The old rule only promoted the
    # restart result when the first pass was clean, so a first-pass setup or
    # environment-blocked result would hide a durability product failure behind
    # a less serious code.
    if (( $(qual_exit_rank "${PHASE_EXIT}") > $(qual_exit_rank "${DRIVER_EXIT}") )); then
      DRIVER_EXIT=${PHASE_EXIT}
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

seal_artifacts || exit "${EXIT_SETUP_FAILURE}"

# After the scan, deliberately. A reason is scenario text rather than anything
# configured, but printing artifacts before the thing that gates them is how a
# leak reaches a log that outlives the artifact.
qual_print_summary "${OUT_DIR}/merged/report.json" "${QUAL_RUN_ID}"

case "${DRIVER_EXIT}" in
  0) exit "${EXIT_SUCCESS}" ;;
  1) exit "${EXIT_PRODUCT_FAILURE}" ;;
  2) exit "${EXIT_SETUP_FAILURE}" ;;
  3) exit "${EXIT_ENVIRONMENT_BLOCKED}" ;;
  *) exit "${EXIT_PRODUCT_FAILURE}" ;;
esac
