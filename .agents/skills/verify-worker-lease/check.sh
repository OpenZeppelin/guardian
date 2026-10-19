#!/usr/bin/env bash
# Model-check the Postgres worker lease and fenced writes (model/WorkerLease.tla).
#
#   check.sh                 quick tier (seconds)
#   check.sh --deep          adds a longer clock and more writes
#   check.sh --cell <max_time> <max_writes>
#                            run one bound, print TLC's full output and trace
#   check.sh --accept-drift  re-record fingerprints.txt once the model has
#                            been reviewed against the changed Rust
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

WORK="$(mktemp -d "${TMPDIR:-/tmp}/worker-lease-tlc.XXXXXX")"
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
  local max_time="$1" max_writes="$2" out="$3"
  local dir="$WORK/t$max_time-w$max_writes"
  mkdir -p "$dir"
  cp "$MODEL_DIR/WorkerLease.tla" "$dir/"
  printf -- '---- MODULE MC ----\nEXTENDS WorkerLease, TLC\nPerms == Permutations(Replicas)\n====\n' > "$dir/MC.tla"
  printf '%s\n' \
    'CONSTANTS' \
    '    Replicas = {r1, r2, r3}' \
    '    TTL = 3' \
    "    MaxTime = $max_time" \
    "    MaxWrites = $max_writes" \
    'SYMMETRY Perms' \
    'SPECIFICATION Spec' \
    'INVARIANT SingleBelievedHolder WritesInTokenOrder OneHolderPerToken' \
    'PROPERTY TokenAdvancesOnSteal' > "$dir/MC.cfg"
  (cd "$dir" && java -XX:+UseParallelGC -cp "$TLA_JAR" tlc2.TLC \
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
  verdict="$(run_cell "$2" "$3" "$out")"
  cat "$out"
  echo
  echo "max_time=$2 max_writes=$3: $verdict"
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
while read -r tier max_time max_writes expected note; do
  [[ -z "${tier:-}" || "$tier" == \#* ]] && continue
  [[ " $tiers " == *" $tier "* ]] || continue
  out="$WORK/t$max_time-w$max_writes.out"
  started=$SECONDS
  verdict="$(run_cell "$max_time" "$max_writes" "$out")"
  states="$(grep -Eo '[0-9]+ distinct states found' "$out" | tail -1 || true)"
  line="max_time=$max_time max_writes=$max_writes: $verdict (expected $expected; ${states:-no state count}; $((SECONDS - started))s)"
  if [[ "$verdict" == "$expected" ]]; then
    echo "ok    $line${note:+ $note}"
  else
    status=1
    echo "DIFF  $line"
    if [[ "$verdict" == FAIL:* ]]; then
      echo "  counterexample (full trace: check.sh --cell $max_time $max_writes):"
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
  echo "Review the matching actions in model/WorkerLease.tla, then run check.sh --accept-drift."
  [[ $status -eq 0 ]] && status=3
fi
exit $status
