#!/usr/bin/env bash
# Model-check the multisig proposal lifecycle (model/ProposalLifecycle.tla).
#
#   check.sh                 quick tier: 3 proposals, every SDK pairing and
#                            queue depth (under a minute)
#   check.sh --deep          adds the same cells with 4 proposals (about 9 min)
#   check.sh --cell <sdk1> <sdk2> <depth> <property> [proposals]
#                            run one cell, print TLC's full output and trace
#   check.sh --accept-drift  re-record fingerprints.txt once the model has
#                            been reviewed against the changed Rust or TS
#
# Exit codes: 0 every cell matches expected.txt and no drift; 1 a cell
# differs from expected.txt; 2 setup error; 3 cells match but code the
# model mirrors changed since fingerprints.txt was recorded.
set -euo pipefail

SKILL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SKILL_DIR/../../.." && pwd)"
MODEL_DIR="$SKILL_DIR/model"
TLA_VERSION="1.7.4"
TLA_SHA256="936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88"
TLA_JAR="${TLA2TOOLS_JAR:-$HOME/.cache/guardian-tla/tla2tools-$TLA_VERSION.jar}"
WORKERS="${TLC_WORKERS:-auto}"

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

WORK="$(mktemp -d "${TMPDIR:-/tmp}/proposal-lifecycle-tlc.XXXXXX")"
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
  local sdk1="$1" sdk2="$2" depth="$3" property="$4" proposals="$5" out="$6"
  local dir="$WORK/$sdk1-$sdk2-$depth-$property-$proposals"
  local checks=$'SPECIFICATION Spec\nINVARIANT '"$property"
  [[ "$property" == GuardianCatchesUp ]] && checks=$'SPECIFICATION LiveSpec\nPROPERTY GuardianCatchesUp'
  mkdir -p "$dir"
  cp "$MODEL_DIR/ProposalLifecycle.tla" "$dir/"
  cat > "$dir/MC.tla" <<EOF
---- MODULE MC ----
EXTENDS ProposalLifecycle, TLC
MCDevices == {"d1", "d2"}
MCSdk == [d \in MCDevices |-> IF d = "d1" THEN "$sdk1" ELSE "$sdk2"]
====
EOF
  cat > "$dir/MC.cfg" <<EOF
CONSTANTS
    Devices <- MCDevices
    Sdk <- MCSdk
    MaxSummaries = $proposals
    MaxPending = $depth
    Labels = {"nonce", "timestamp"}
    Supersede = TRUE
$checks
EOF
  (cd "$dir" && java -XX:+UseParallelGC -cp "$TLA_JAR" tlc2.TLC \
     -workers "$WORKERS" -cleanup -metadir "$dir/states" -config MC.cfg MC.tla) \
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
  verdict="$(run_cell "$2" "$3" "$4" "$5" "${6:-3}" "$out")"
  cat "$out"
  echo
  echo "$2+$3 depth=$4 $5: $verdict"
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
for tier in $tiers; do
  proposals=3
  [[ "$tier" == deep ]] && proposals=4
  while read -r sdk1 sdk2 depth property expected note; do
    [[ -z "${sdk1:-}" || "$sdk1" == \#* ]] && continue
    out="$WORK/$sdk1-$sdk2-$depth-$property-$proposals.out"
    started=$SECONDS
    verdict="$(run_cell "$sdk1" "$sdk2" "$depth" "$property" "$proposals" "$out")"
    states="$(grep -Eo '[0-9]+ distinct states found' "$out" | tail -1 || true)"
    line="$sdk1+$sdk2 depth=$depth proposals=$proposals $property: $verdict (expected $expected; ${states:-no state count}; $((SECONDS - started))s)"
    if [[ "$verdict" == "$expected" ]]; then
      echo "ok    $line${note:+ $note}"
    else
      status=1
      echo "DIFF  $line"
      if [[ "$verdict" == FAIL:* ]]; then
        echo "  counterexample (full trace: check.sh --cell $sdk1 $sdk2 $depth $property $proposals):"
        trace_steps "$out"
      else
        grep -E -A8 "^Error" "$out" | head -24 | sed 's/^/  /'
      fi
    fi
  done < "$SKILL_DIR/expected.txt"
done

if ! diff -u "$SKILL_DIR/fingerprints.txt" <(fingerprints) > "$WORK/drift.diff"; then
  echo
  echo "DRIFT: code the model mirrors changed since fingerprints.txt was recorded:"
  { grep -E '^[-+][^-+]' "$WORK/drift.diff" || true; } | sed 's/^/  /'
  echo "Review the matching actions in model/ProposalLifecycle.tla, then run check.sh --accept-drift."
  [[ $status -eq 0 ]] && status=3
fi
exit $status
