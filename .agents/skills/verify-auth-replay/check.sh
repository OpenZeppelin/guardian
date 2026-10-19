#!/usr/bin/env bash
# Model-check Guardian's signed-request replay protection (model/AuthReplay.tla).
#
#   check.sh                 quick tier, under a minute
#   check.sh --deep          adds a wallet-plus-session cell, about 6 more min
#   check.sh --cell <cell> <group>
#                            run one cell, print TLC's full output and trace
#   check.sh --accept-drift  re-record fingerprints.txt once the model has
#                            been reviewed against the changed Rust
#
# Groups: "accept" checks AcceptedAtMostOnce and FloorsOnlyRise; "writes"
# checks WritesAtMostOnce; "all" checks all three in one run.
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
# checksum matches, so an interrupted or corrupted download never leaves a
# file at the cached path.
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

WORK="$(mktemp -d "${TMPDIR:-/tmp}/auth-replay-tlc.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

# Cell parameters: scenario, signing keys, request kinds, MaxRequests,
# MaxTime, MaxStall. Skew = 1 and Ttl = 2 clock units throughout, so a
# MaxStall of 3 is "a handler finishes within max TTL plus skew" and 99 is
# unbounded (the code today).
cell_params() {
  case "$1" in
    configure-fresh)     echo 'fresh|{"s1", "s2"}|{"configure", "push"}|2|4|3' ;;
    configure-existing)  echo 'existing|{"s1", "s2"}|{"configure", "push"}|2|4|3' ;;
    migrated)            echo 'migrated|{"s1", "s2"}|{"configure", "push"}|2|4|3' ;;
    session-bounded)     echo 'existing|{"sess"}|{"push"}|1|6|3' ;;
    session-unbounded)   echo 'existing|{"sess"}|{"push"}|1|6|99' ;;
    session-mixed)       echo 'existing|{"s1", "sess"}|{"configure", "push"}|2|6|3' ;;
    *) echo "unknown cell $1" >&2; return 2 ;;
  esac
}

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
  local cell="$1" group="$2" out="$3"
  local dir="$WORK/$cell-$group" params checks
  params="$(cell_params "$cell")"
  IFS='|' read -r scenario keys kinds max_requests max_time max_stall <<< "$params"
  case "$group" in
    accept) checks=$'INVARIANT AcceptedAtMostOnce\nPROPERTY FloorsOnlyRise' ;;
    writes) checks='INVARIANT WritesAtMostOnce' ;;
    all) checks=$'INVARIANT AcceptedAtMostOnce WritesAtMostOnce\nPROPERTY FloorsOnlyRise' ;;
    *) echo "unknown group $group" >&2; return 2 ;;
  esac
  mkdir -p "$dir"
  cp "$MODEL_DIR/AuthReplay.tla" "$dir/"
  printf -- '---- MODULE MC ----\nEXTENDS AuthReplay, TLC\nPerms == Permutations(Handlers)\n====\n' > "$dir/MC.tla"
  cat > "$dir/MC.cfg" <<EOF
CONSTANTS
    Handlers = {h1, h2}
    MaxRequests = $max_requests
    MaxTime = $max_time
    Skew = 1
    Ttl = 2
    SignKeys = $keys
    Kinds = $kinds
    MaxStall = $max_stall
    Scenario = "$scenario"
SYMMETRY Perms
SPECIFICATION Spec
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
  verdict="$(run_cell "$2" "$3" "$out")"
  cat "$out"
  echo
  echo "$2 $3: $verdict"
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
while read -r tier cell group expected note; do
  [[ -z "${tier:-}" || "$tier" == \#* ]] && continue
  [[ " $tiers " == *" $tier "* ]] || continue
  out="$WORK/$cell-$group.out"
  started=$SECONDS
  verdict="$(run_cell "$cell" "$group" "$out")"
  states="$(grep -Eo '[0-9]+ distinct states found' "$out" | tail -1 || true)"
  line="$cell $group: $verdict (expected $expected; ${states:-no state count}; $((SECONDS - started))s)"
  if [[ "$verdict" == "$expected" ]]; then
    echo "ok    $line${note:+ $note}"
  else
    status=1
    echo "DIFF  $line"
    if [[ "$verdict" == FAIL:* ]]; then
      echo "  counterexample (full trace: check.sh --cell $cell $group):"
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
  echo "Review the matching actions in model/AuthReplay.tla, then run check.sh --accept-drift."
  [[ $status -eq 0 ]] && status=3
fi
exit $status
