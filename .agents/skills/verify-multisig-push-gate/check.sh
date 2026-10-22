#!/usr/bin/env bash
# Model-check the multisig push gate (issue #524).
#
#   check.sh                 quick tier: transactions calling up to 3
#                            procedures, every relay cell (about 20 s)
#   check.sh --deep          adds the threshold cells at up to 5 procedures
#   check.sh --cell <cell> <property> [max-calls]
#                            run one cell, print TLC's output and the trace
#   check.sh --accept-drift  re-record fingerprints.txt once the models have
#                            been reviewed against the changed code
#
# Cells are defined in cells.txt, verdicts in expected.txt.
# Exit codes: 0 every cell matches expected.txt and no drift; 1 a cell
# differs from expected.txt; 2 setup error; 3 cells match but code the
# models mirror changed since fingerprints.txt was recorded.
set -euo pipefail

SKILL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SKILL_DIR/../../.." && pwd)"
MODEL_DIR="$SKILL_DIR/model"
TLA_VERSION="1.7.4"
TLA_SHA256="936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88"
TLA_JAR="${TLA2TOOLS_JAR:-$HOME/.cache/guardian-tla/tla2tools-$TLA_VERSION.jar}"
WORKERS="${TLC_WORKERS:-auto}"
QUICK_CALLS=3
DEEP_CALLS=5

command -v java >/dev/null || { echo "java not found (brew install openjdk)"; exit 2; }
command -v python3 >/dev/null || { echo "python3 not found"; exit 2; }

fingerprints() {
  python3 -I "$SKILL_DIR/fingerprint.py" "$REPO_ROOT" "$SKILL_DIR/mirrored.txt"
}

if [[ "${1:-}" == "--accept-drift" ]]; then
  recorded="$(fingerprints)"
  if grep -q " MISSING$" <<< "$recorded"; then
    echo "Refusing to record: these mirrored entries were not found (renamed or removed?):"
    grep " MISSING$" <<< "$recorded" | sed 's/^/  /'
    echo "Update mirrored.txt to the current names, then rerun --accept-drift."
    exit 2
  fi
  printf '%s\n' "$recorded" > "$SKILL_DIR/fingerprints.txt"
  echo "Recorded $(wc -l < "$SKILL_DIR/fingerprints.txt" | tr -d ' ') fingerprints."
  exit 0
fi

sha256_of() {
  if command -v sha256sum >/dev/null; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null; then
    shasum -a 256 "$1" | cut -d' ' -f1
  else
    echo "Neither sha256sum nor shasum is installed" >&2
    exit 2
  fi
}

# Download to a temporary file and move it into the cache only once its
# checksum matches, so an interrupted download never leaves a file at the
# cached path. The cache is shared with the repo's other verify-* skills.
if [[ ! -f "$TLA_JAR" ]]; then
  command -v curl >/dev/null || { echo "curl is not installed"; exit 2; }
  mkdir -p "$(dirname "$TLA_JAR")"
  echo "Downloading tla2tools $TLA_VERSION to $TLA_JAR"
  partial="$TLA_JAR.partial.$$"
  if ! curl -fsSL -o "$partial" \
      "https://github.com/tlaplus/tlaplus/releases/download/v$TLA_VERSION/tla2tools.jar"; then
    rm -f "$partial"
    echo "tla2tools download failed"
    exit 2
  fi
  downloaded_sha="$(sha256_of "$partial")"
  if [[ "$downloaded_sha" != "$TLA_SHA256" ]]; then
    rm -f "$partial"
    echo "tla2tools checksum mismatch: $downloaded_sha (expected $TLA_SHA256)"
    exit 2
  fi
  mv "$partial" "$TLA_JAR"
fi
actual_sha="$(sha256_of "$TLA_JAR")"
if [[ "$actual_sha" != "$TLA_SHA256" ]]; then
  echo "tla2tools checksum mismatch: $actual_sha (expected $TLA_SHA256); delete $TLA_JAR and rerun"
  exit 2
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/multisig-push-gate-tlc.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

cell_line() {
  grep -E "^$1[[:space:]]" "$SKILL_DIR/cells.txt" || { echo "no cell $1 in cells.txt" >&2; exit 2; }
}

violated_property() {
  grep -m1 -E "^Error: Invariant [A-Za-z0-9_]+ is violated" "$1" | awk '{print $3}'
}

# Fixed constants per module; cells.txt supplies the rest.
module_constants() {
  case "$1" in
    PushGateThreshold) [[ "$3" == *MaxCalls=* ]] || printf "    MaxCalls = %s\n" "$2" ;;
    ApprovalRelay) cat <<'EOF'
    Cosigners = {"c1", "c2", "c3", "c4"}
    Executor = "c1"
    Required = 3
    Collected = {"c1", "c2", "c3"}
EOF
    ;;
    *) echo "unknown module $1" >&2; exit 2 ;;
  esac
}

run_cell() {
  local cell="$1" property="$2" max_calls="$3" out="$4" model_dir="${5:-$MODEL_DIR}"
  local -a fields
  read -r -a fields <<< "$(cell_line "$cell")"
  local module="${fields[1]}"
  local dir="$WORK/$cell-$property-$max_calls-$RANDOM"
  mkdir -p "$dir"
  cp "$model_dir/$module.tla" "$dir/"
  {
    echo "CONSTANTS"
    module_constants "$module" "$max_calls" "${fields[*]:2}"
    for assignment in "${fields[@]:2}"; do
      echo "    ${assignment%%=*} = ${assignment#*=}"
    done
    echo "SPECIFICATION Spec"
    echo "INVARIANT $property"
  } > "$dir/MC.cfg"
  (cd "$dir" && java -XX:+UseParallelGC -cp "$TLA_JAR" tlc2.TLC \
     -workers "$WORKERS" -deadlock -cleanup -metadir "$dir/states" \
     -config MC.cfg "$module.tla") > "$out" 2>&1 || true
  if grep -q "Model checking completed. No error has been found." "$out"; then
    echo PASS
  elif grep -qE "^Error: Invariant .* is violated" "$out"; then
    echo "FAIL:$(violated_property "$out")"
  else
    echo ERROR
  fi
}

# The counterexample, one line per state with its variables. A threshold
# cell fails on an initial state (its only kind of state), a relay cell on a
# trace of steps.
trace_steps() {
  awk '
    function flush() { if (line != "") print line; line = "" }
    /^Error: Invariant .* is violated/ {
      print "  " $0
      if ($0 ~ /initial state/) line = "  state:"
      next
    }
    /^State [0-9]+:/ { flush(); split($0, head, ">"); line = "  " head[1] ">"; next }
    /^$/ || /^Finished/ || /^[0-9]+ states generated/ { flush(); next }
    line != "" { gsub(/^[ \t]+/, ""); sub(/^\/\\ /, ""); line = line " " $0 }
    END { flush() }
  ' "$1"
}

if [[ "${1:-}" == "--cell" ]]; then
  [[ $# -ge 3 ]] || { echo "usage: check.sh --cell <cell> <property> [max-calls]"; exit 2; }
  out="$WORK/cell.out"
  verdict="$(run_cell "$2" "$3" "${4:-$QUICK_CALLS}" "$out")"
  cat "$out"
  echo
  echo "$2 $3 (max calls ${4:-$QUICK_CALLS}): $verdict"
  [[ "$verdict" == FAIL:* ]] && { echo "Counterexample:"; trace_steps "$out"; }
  case "$verdict" in
    PASS) exit 0 ;;
    FAIL:*) exit 1 ;;
    *) exit 2 ;;
  esac
fi

if [[ "${1:-}" == "--mutants" ]]; then
  status=0
  while IFS= read -r entry; do
    [[ -z "$entry" || "$entry" == \#* ]] && continue
    read -r id module cell property expected _ <<< "$entry"
    rest="${entry#* :: }"
    substitution="${rest%% :: *}"
    meaning="${rest#* :: }"
    mutant_dir="$WORK/mutant-$id"
    mkdir -p "$mutant_dir"
    perl -pe "$substitution" "$MODEL_DIR/$module.tla" > "$mutant_dir/$module.tla"
    if cmp -s "$MODEL_DIR/$module.tla" "$mutant_dir/$module.tla"; then
      status=1
      echo "STALE $id: the substitution no longer matches model/$module.tla; update mutants.txt"
      continue
    fi
    verdict="$(run_cell "$cell" "$property" "$QUICK_CALLS" "$WORK/mutant-$id.out" "$mutant_dir")"
    if [[ "$verdict" == "$expected" ]]; then
      echo "ok    $id $cell $property: $verdict ($meaning)"
    else
      status=1
      echo "DIFF  $id $cell $property: $verdict, expected $expected ($meaning)"
    fi
  done < "$SKILL_DIR/mutants.txt"
  exit $status
fi

tiers="$QUICK_CALLS"
[[ "${1:-}" == "--deep" ]] && tiers="$QUICK_CALLS $DEEP_CALLS"

status=0
for max_calls in $tiers; do
  while read -r cell property expected note; do
    [[ -z "${cell:-}" || "$cell" == \#* ]] && continue
    [[ "$max_calls" != "$QUICK_CALLS" && ( "$cell" != threshold-* || "$(cell_line "$cell")" == *MaxCalls=* ) ]] && continue
    out="$WORK/$cell-$property-$max_calls.out"
    started=$SECONDS
    verdict="$(run_cell "$cell" "$property" "$max_calls" "$out")"
    states="$(grep -Eo '[0-9]+ distinct states found' "$out" | tail -1 || true)"
    tier_label=""
    if [[ "$cell" == threshold-* ]]; then
      fixed="$(grep -Eo "MaxCalls=[0-9]+" <<< "$(cell_line "$cell")" || true)"
      tier_label=" calls<=${fixed#MaxCalls=}"
      [[ -z "$fixed" ]] && tier_label=" calls<=$max_calls"
    fi
    line="$cell$tier_label $property: $verdict (expected $expected; ${states:-no state count}; $((SECONDS - started))s)"
    if [[ "$verdict" == "$expected" ]]; then
      echo "ok    $line${note:+ $note}"
    else
      status=1
      echo "DIFF  $line"
      if [[ "$verdict" == FAIL:* ]]; then
        echo "  counterexample (full output: check.sh --cell $cell $property $max_calls):"
        trace_steps "$out"
      else
        grep -E -A8 "^Error" "$out" | head -24 | sed 's/^/  /'
      fi
    fi
  done < "$SKILL_DIR/expected.txt"
done

if ! diff -u "$SKILL_DIR/fingerprints.txt" <(fingerprints) > "$WORK/drift.diff"; then
  echo
  echo "DRIFT: code the models mirror changed since fingerprints.txt was recorded:"
  { grep -E '^[-+][^-+]' "$WORK/drift.diff" || true; } | sed 's/^/  /'
  echo "Review the matching definitions in model/*.tla (SKILL.md, Code map), then run check.sh --accept-drift."
  [[ $status -eq 0 ]] && status=3
fi
exit $status
