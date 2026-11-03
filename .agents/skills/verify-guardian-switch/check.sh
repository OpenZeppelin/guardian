#!/usr/bin/env bash
# Model-check the account status lifecycle around a guardian switch and an
# operator pause with model/GuardianSwitch.tla.
#
#   check.sh                 quick tier (under a minute)
#   check.sh --deep          adds larger bounds (a few minutes)
#   check.sh --mutations     runs every broken variant; each must fail
#   check.sh --cell <name>   run one cell from expected.txt, print TLC's
#                            full output and the trace
#   check.sh --drift-only    only compare the mirrored code with fingerprints.txt,
#                            without running TLC (exit 0 no drift, 3 drift)
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

if [[ "${1:-}" == "--drift-only" ]]; then
  if drift="$(diff -u "$SKILL_DIR/fingerprints.txt" <(fingerprints))"; then
    echo "No drift: $(wc -l < "$SKILL_DIR/fingerprints.txt" | tr -d ' ') mirrored functions match fingerprints.txt."
    exit 0
  fi
  echo "DRIFT: Rust that the model mirrors changed since fingerprints.txt was recorded:"
  { grep -E '^[-+][^-+]' <<< "$drift" || true; } | sed 's/^/  /'
  echo "Review the matching actions in model/GuardianSwitch.tla (SKILL.md, action map), then run check.sh --accept-drift."
  exit 3
fi

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

WORK="$(mktemp -d "${TMPDIR:-/tmp}/guardian-switch-tlc.XXXXXX")"
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

# run_cell <name> <configures> <max_states> <max_nonce> <max_pauses>
#          <public> <mutation> <invariants> <properties> <out>
run_cell() {
  local name="$1" configures="$2" max_states="$3" max_nonce="$4" max_pauses="$5"
  local public="$6" mutation="$7" invariants="$8" properties="$9" out="${10}"
  local dir="$WORK/$name" spec="Spec"
  [[ ",$properties," == *",SwitchIsReleased,"* ]] && spec="FairSpec"
  mkdir -p "$dir"
  cp "$MODEL_DIR/GuardianSwitch.tla" "$dir/"
  printf -- '---- MODULE MC ----\nEXTENDS GuardianSwitch, TLC\n====\n' > "$dir/MC.tla"
  {
    printf '%s\n' \
      'CONSTANTS' \
      '    Requests = {q1, q2}' \
      "    Configures = ${configures//,/, }" \
      "    MaxStates = $max_states" \
      "    MaxNonce = $max_nonce" \
      "    MaxPauses = $max_pauses" \
      "    Public = $public" \
      "    Mutation = \"$mutation\"" \
      "SPECIFICATION $spec" \
      'CHECK_DEADLOCK FALSE'
    [[ "$invariants" != "-" ]] && echo "INVARIANT ${invariants//,/ }"
    [[ "$properties" != "-" ]] && echo "PROPERTY ${properties//,/ }"
  } > "$dir/MC.cfg"
  (cd "$dir" && java -XX:+UseParallelGC -cp "$TLA_JAR" tlc2.TLC \
     -workers "${TLC_WORKERS:-auto}" -cleanup -metadir "$dir/states" -config MC.cfg MC.tla < /dev/null) \
     > "$out" 2>&1 || true
  if grep -q "Model checking completed. No error has been found." "$out"; then
    echo PASS
  elif grep -qE "^Error: .*(is|were) violated" "$out"; then
    echo "FAIL:$(violated_property "$out")"
  else
    echo ERROR
  fi
}

# One line per step: the action and the variables it changed.
trace_steps() {
  grep -E "^Error: .*violated" "$1" | sed 's/^/  /'
  python3 -I "$SKILL_DIR/trace.py" "$1" | sed 's/^/  /'
}

cells() {
  grep -vE '^\s*(#|$)' "$SKILL_DIR/expected.txt"
}

if [[ "${1:-}" == "--cell" ]]; then
  [[ -n "${2:-}" ]] || { echo "usage: check.sh --cell <name>"; exit 2; }
  row="$(cells | awk -v c="$2" '$2 == c' | head -1)"
  [[ -n "$row" ]] || { echo "no cell named $2 in expected.txt"; exit 2; }
  read -r _tier name configures max_states max_nonce max_pauses public mutation _expected invariants properties <<< "$row"
  out="$WORK/cell.out"
  verdict="$(run_cell "$name" "$configures" "$max_states" "$max_nonce" "$max_pauses" "$public" "$mutation" "$invariants" "$properties" "$out")"
  cat "$out"
  echo
  echo "$name: $verdict"
  [[ "$verdict" == FAIL:* ]] && { echo "Steps:"; trace_steps "$out"; }
  case "$verdict" in
    PASS) exit 0 ;;
    FAIL:*) exit 1 ;;
    *) exit 2 ;;
  esac
fi

case "${1:-}" in
  --deep) tiers="quick deep" ;;
  --mutations) tiers="mutation" ;;
  "") tiers="quick" ;;
  *) echo "unknown option $1"; exit 2 ;;
esac

status=0
while read -r tier name configures max_states max_nonce max_pauses public mutation expected invariants properties; do
  [[ " $tiers " == *" $tier "* ]] || continue
  out="$WORK/$name.out"
  started=$SECONDS
  verdict="$(run_cell "$name" "$configures" "$max_states" "$max_nonce" "$max_pauses" "$public" "$mutation" "$invariants" "$properties" "$out")"
  states="$(grep -Eo '[0-9]+ distinct states found' "$out" | tail -1 || true)"
  line="$name: $verdict (expected $expected; ${states:-no state count}; $((SECONDS - started))s)"
  if [[ "$verdict" == "$expected" ]]; then
    echo "ok    $line"
  else
    status=1
    echo "DIFF  $line"
    if [[ "$verdict" == FAIL:* ]]; then
      echo "  counterexample (full trace: check.sh --cell $name):"
      trace_steps "$out"
    else
      { grep -E -A8 "^Error" "$out" || true; } | head -24 | sed 's/^/  /'
    fi
  fi
done < <(cells)

if ! diff -u "$SKILL_DIR/fingerprints.txt" <(fingerprints) > "$WORK/drift.diff"; then
  echo
  echo "DRIFT: Rust that the model mirrors changed since fingerprints.txt was recorded:"
  { grep -E '^[-+][^-+]' "$WORK/drift.diff" || true; } | sed 's/^/  /'
  echo "Review the matching actions in model/GuardianSwitch.tla (SKILL.md, action map), then run check.sh --accept-drift."
  [[ $status -eq 0 ]] && status=3
fi
exit $status
