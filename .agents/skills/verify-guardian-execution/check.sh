#!/usr/bin/env bash
# Model-check Guardian execution (issue #254) on Postgres (model/GuardianExecution.tla).
#
#   check.sh                 quick tier (about 1 min)
#   check.sh --deep          adds two reconcilers with two proposals, and liveness (about 10 min)
#   check.sh --cell <reconcilers> <proposals> <parked> <group>
#                            run one cell, print TLC's full output and trace
#   check.sh --accept-drift  re-record fingerprints.txt once the model has
#                            been reviewed against the changed Rust
#
# Groups: "safety" checks every invariant in one run; "live" checks
# ReservationsRelease and GuardianCatchesUp under the fairness assumptions.
# <parked> is TRUE to start with a retained client delta whose transaction
# can still land.
#
# Exit codes: 0 every cell matches expected.txt and no drift; 1 a cell
# differs from expected.txt; 2 setup error; 3 cells match but Rust the
# model mirrors changed since fingerprints.txt was recorded.
set -euo pipefail

SKILL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SKILL_DIR/../../.." && pwd)"
MODEL_DIR="$SKILL_DIR/model"
TLA_VERSION="1.7.4"
TLA_SHA256="936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88"
TLA_JAR="${TLA2TOOLS_JAR:-$HOME/.cache/guardian-tla/tla2tools-$TLA_VERSION.jar}"
SAFETY_INVARIANTS="LandedNeverFailed CommittedMeansLanded SentAfterBoundary OneLandableExecution ExecutionExcludesClientCandidate StoredOnChain LandedStepRecorded"

command -v java >/dev/null || { echo "java not found (brew install openjdk)"; exit 2; }
command -v python3 >/dev/null || { echo "python3 not found"; exit 2; }

fingerprints() {
  python3 -I "$SKILL_DIR/fingerprint.py" "$REPO_ROOT" "$SKILL_DIR/mirrored.txt"
}

if [[ "${1:-}" == "--accept-drift" ]]; then
  recorded="$(fingerprints)"
  if grep -q " MISSING$" <<< "$recorded"; then
    echo "Refusing to record: these mirrored functions were not found (renamed or removed?):"
    grep " MISSING$" <<< "$recorded" | sed 's/^/  /'
    echo "Update mirrored.txt to the current names, then rerun --accept-drift."
    exit 2
  fi
  printf '%s\n' "$recorded" > "$SKILL_DIR/fingerprints.txt"
  echo "Recorded $(wc -l < "$SKILL_DIR/fingerprints.txt" | tr -d ' ') fingerprints."
  exit 0
fi

if [[ ! -f "$TLA_JAR" ]]; then
  mkdir -p "$(dirname "$TLA_JAR")"
  echo "Downloading tla2tools $TLA_VERSION to $TLA_JAR"
  curl -fsSL -o "$TLA_JAR" \
    "https://github.com/tlaplus/tlaplus/releases/download/v$TLA_VERSION/tla2tools.jar"
fi
actual_sha="$(shasum -a 256 "$TLA_JAR" | cut -d' ' -f1)"
if [[ "$actual_sha" != "$TLA_SHA256" ]]; then
  echo "tla2tools checksum mismatch: $actual_sha (expected $TLA_SHA256)"
  exit 2
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/guardian-execution-tlc.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

# The property a FAIL violated, as TLC names it: an invariant or action
# property by name, every temporal property as "temporal".
violated_property() {
  local line
  line="$(grep -m1 -E "^Error: (Invariant|Action property) [A-Za-z0-9_]+ is violated" "$1" || true)"
  if [[ -n "$line" ]]; then
    awk '{print $(NF-2)}' <<< "$line"
  else
    echo temporal
  fi
}

run_cell() {
  local reconcilers="$1" proposals="$2" parked="$3" group="$4" out="$5"
  local dir="$WORK/r$reconcilers-p$proposals-$parked-$group"
  local recs="rc1" props='"p1"' pushes=1 checks
  [[ "$reconcilers" == 2 ]] && recs="rc1, rc2"
  [[ "$proposals" == 2 ]] && props='"p1", "p2"'
  # With a parked delta the only client step is that delta's transaction,
  # so a lost step is lost to the execution path alone.
  [[ "$parked" == TRUE ]] && pushes=0
  if [[ "$group" == live ]]; then
    checks=$'SPECIFICATION LiveSpec\nPROPERTY ReservationsRelease GuardianCatchesUp'
  else
    checks=$'SPECIFICATION Spec\nCONSTRAINT StateBound\nVIEW Fingerprint\nSYMMETRY Perms\nINVARIANT '"$SAFETY_INVARIANTS"
  fi
  mkdir -p "$dir"
  cp "$MODEL_DIR/GuardianExecution.tla" "$dir/"
  printf -- '---- MODULE MC ----\nEXTENDS GuardianExecution, TLC\nPerms == Permutations(Workers) \\cup Permutations(Reconcilers)\n====\n' > "$dir/MC.tla"
  cat > "$dir/MC.cfg" <<EOF
CONSTANTS
    Workers = {w1, w2}
    Reconcilers = {$recs}
    Proposals = {$props}
    Expiry = 2
    MaxBlock = 3
    MaxLapses = 1
    MaxTok = 3
    ParkedClient = $parked
    ClientPushes = $pushes
    ExternalMoves = 1
$checks
EOF
  (cd "$dir" && java -XX:+UseParallelGC -cp "$TLA_JAR" tlc2.TLC -deadlock \
     -workers "${TLC_WORKERS:-auto}" -cleanup -metadir "$dir/states" -config MC.cfg MC.tla) \
     > "$out" 2>&1 || true
  if grep -q "Model checking completed. No error has been found." "$out"; then
    echo PASS
  elif grep -qE "^Error: .*(is|were) violated" "$out"; then
    echo "FAIL:$(violated_property "$out")"
  else
    echo ERROR
  fi
}

trace_steps() {
  grep -E "^Error: .*violated" "$1" | sed 's/^/  /'
  grep -E "^State [0-9]+:" "$1" | sed -E 's/^State ([0-9]+): <([A-Za-z]+).*/  \1. \2/'
}

if [[ "${1:-}" == "--cell" ]]; then
  out="$WORK/cell.out"
  verdict="$(run_cell "$2" "$3" "$4" "$5" "$out")"
  cat "$out"
  echo
  echo "reconcilers=$2 proposals=$3 parked=$4 group=$5: $verdict"
  [[ "$verdict" == FAIL:* ]] && { echo "Steps:"; trace_steps "$out"; }
  case "$verdict" in
    PASS) exit 0 ;;
    FAIL:*) exit 1 ;;
    *) exit 2 ;;
  esac
fi

tiers="quick"
[[ "${1:-}" == "--deep" ]] && tiers="quick deep"

status=0
while read -r tier reconcilers proposals parked group expected note; do
  [[ -z "${tier:-}" || "$tier" == \#* ]] && continue
  [[ " $tiers " == *" $tier "* ]] || continue
  out="$WORK/r$reconcilers-p$proposals-$parked-$group.out"
  started=$SECONDS
  verdict="$(run_cell "$reconcilers" "$proposals" "$parked" "$group" "$out")"
  states="$(grep -Eo '[0-9]+ distinct states found' "$out" | tail -1 || true)"
  line="reconcilers=$reconcilers proposals=$proposals parked=$parked $group: $verdict (expected $expected; ${states:-no state count}; $((SECONDS - started))s)"
  if [[ "$verdict" == "$expected" ]]; then
    echo "ok    $line${note:+ $note}"
  else
    status=1
    echo "DIFF  $line"
    if [[ "$verdict" == FAIL:* ]]; then
      echo "  counterexample (full trace: check.sh --cell $reconcilers $proposals $parked $group):"
      trace_steps "$out"
    else
      grep -E -A8 "^Error" "$out" | head -24 | sed 's/^/  /'
    fi
  fi
done < "$SKILL_DIR/expected.txt"

if ! diff -u "$SKILL_DIR/fingerprints.txt" <(fingerprints) > "$WORK/drift.diff"; then
  echo
  echo "DRIFT: Rust that the model mirrors changed since fingerprints.txt was recorded:"
  { grep -E '^[-+][^-+]' "$WORK/drift.diff" || true; } | sed 's/^/  /'
  echo "Review the matching actions in model/GuardianExecution.tla, then run check.sh --accept-drift."
  [[ $status -eq 0 ]] && status=3
fi
exit $status
