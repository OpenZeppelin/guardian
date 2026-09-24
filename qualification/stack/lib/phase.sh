# shellcheck shell=bash
# How a run's phases are executed and how their outcomes combine.
#
# Extracted from run.sh so both can be tested without bringing a stack up: the
# question these answer, which image a phase reports and which legs it runs, is
# exactly the one an upgrade run got wrong by asking it inline.

# The worse of two legs decides a phase, and the worse of the phases decides the
# run. "Worse" is not numeric order: 3 means nothing ran that could judge the
# product and the live workflow maps it to success, so a product failure (1) or
# a setup failure (2) has to beat it. Comparing `== 0` instead would let a Rust
# leg that was entirely environment-blocked swallow a TypeScript product failure
# and report green.
qual_exit_rank() {
  case "${1}" in
    0) echo 0 ;;
    3) echo 1 ;;
    2) echo 2 ;;
    *) echo 3 ;;
  esac
}

# The TypeScript leg, as its own function so a phase can be tested without one.
# It runs in a subshell because it needs a different working directory and a
# narrowed environment, and a subshell cannot report anything back but its exit
# code, which is all a phase wants from it.
qual_run_typescript_leg() {
  local run_id="${1}" out_dir="${2}" revision="${3}"
  (
    cd "${REPO_ROOT}/packages/miden-multisig-client" || exit 2
    # The deterministic profile funds nothing, so the key has no business in
    # this process. The live profile still needs it: the funding bridge spawns
    # the Rust binary, which inherits this environment to read it. Narrowing it
    # to the profile that spends is as far as this goes until funding is split
    # out into a trusted step that hands the driver ephemeral keys only.
    [[ "${PROFILE}" == "live" ]] || unset QUAL_TREASURY_KEY
    QUAL_PROFILE="${PROFILE}" \
    QUAL_OUT_DIR="${out_dir}" \
    QUAL_RUN_ID="${run_id}" \
    QUAL_HTTP_ENDPOINT="http://127.0.0.1:${QUAL_HTTP_PORT}" \
    QUAL_GRPC_ENDPOINT="http://127.0.0.1:${QUAL_GRPC_PORT}" \
    QUAL_IMAGE_REVISION="${revision}" \
    QUAL_NETWORK="${NETWORK}" \
    QUAL_MIDEN_RPC_ENDPOINT="${RPC_ENDPOINT}" \
    QUAL_GUARDIAN_MIGRATION_ENDPOINT="${QUAL_GUARDIAN_MIGRATION_HTTP:-}" \
    QUAL_REPO_ROOT="${REPO_ROOT}" \
    QUAL_CORE_ONLY="${CORE_ONLY}" \
    QUAL_OPERATOR_ALLOWLIST="${QUAL_OPERATOR_ALLOWLIST}" \
    QUAL_SCENARIOS="${SCENARIOS[*]+"${SCENARIOS[*]}"}" \
      npm run --silent test:qualification
  )
}

# Runs one phase against whichever image is currently serving, and records that
# image rather than the one the run is ultimately about. `legs` is `selected` for
# the SDKs the caller asked for, or `rust` where the extra arguments are Rust's
# alone. Sets PHASE_EXIT to the worse of the legs that ran.
qual_run_phase() {
  local run_id="${1}" out_dir="${2}" digest="${3}" revision="${4}" legs="${5}"
  shift 5
  local extra=("$@")
  local rust_exit=0 ts_exit=0

  mkdir -p "${out_dir}"

  # `rust` narrows a phase to the Rust leg; it never widens one. A phase whose
  # extra arguments are Rust's alone still must not run a leg the caller asked
  # to leave out.
  local run_rust=0 run_typescript=0
  if [[ "${SDK}" == "both" || "${SDK}" == "rust" ]]; then run_rust=1; fi
  if [[ "${legs}" == "selected" && ( "${SDK}" == "both" || "${SDK}" == "typescript" ) ]]; then
    run_typescript=1
  fi

  if (( run_rust == 1 )); then
    echo "==> running Rust scenarios (${run_id})"
    set +e
    "${DRIVER[@]}" "${DRIVER_ARGS[@]}" \
      --run-id "${run_id}" \
      --out "${out_dir}" \
      --image-digest "${digest}" \
      --image-revision "${revision}" \
      "${extra[@]+"${extra[@]}"}"
    rust_exit=$?
    set -e
  fi

  if (( run_typescript == 1 )); then
    echo "==> running TypeScript scenarios (${run_id})"
    set +e
    qual_run_typescript_leg "${run_id}" "${out_dir}" "${revision}"
    ts_exit=$?
    set -e
  fi

  PHASE_EXIT=${rust_exit}
  if (( $(qual_exit_rank "${ts_exit}") > $(qual_exit_rank "${PHASE_EXIT}") )); then
    PHASE_EXIT=${ts_exit}
  fi
}

