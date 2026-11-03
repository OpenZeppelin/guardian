#!/usr/bin/env bash
# Check every design of SessionReplayDesign.tla against every property.
#   run.sh                      print the verdict matrix
#   run.sh <design> <property>  print TLC's full output and trace
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
JAR="${TLA2TOOLS_JAR:-$HOME/.cache/guardian-tla/tla2tools-1.7.4.jar}"
[[ -f "$JAR" ]] || { echo "missing $JAR (run .agents/skills/verify-auth-replay/check.sh once to download it)"; exit 2; }

PROPERTIES="NoFalseReadRejection WritesAtMostOnce WritesOnlyOnSignedRoute AcceptedOnlyOnSignedRoute NoReplayedReadAfterServed NoPlaintextToReplayer"

# design -> ReadMode|Bound|RespEnc|CollidingWrite
design_params() {
  case "$1" in
    today)                 echo '"strict"|FALSE|FALSE|FALSE' ;;
    window-unbound)        echo '"window"|FALSE|FALSE|FALSE' ;;
    window-bound)          echo '"window"|TRUE|FALSE|FALSE' ;;
    window-bound-enc)      echo '"window"|TRUE|TRUE|FALSE' ;;
    today+colliding)       echo '"strict"|FALSE|FALSE|TRUE' ;;
    window-unbound+colliding) echo '"window"|FALSE|FALSE|TRUE' ;;
    window-bound+colliding)   echo '"window"|TRUE|FALSE|TRUE' ;;
    *) echo "unknown design $1" >&2; return 2 ;;
  esac
}
DESIGNS="today window-unbound window-bound window-bound-enc today+colliding window-unbound+colliding window-bound+colliding"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/session-replay-design.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

run() {
  local design="$1" prop="$2" out="$3" d="$WORK/$1-$2"
  IFS='|' read -r mode bound enc colliding <<< "$(design_params "$design")"
  local attacker=TRUE
  [[ "$prop" == NoFalseReadRejection ]] && attacker=FALSE
  mkdir -p "$d"
  cp "$DIR/SessionReplayDesign.tla" "$d/"
  cat > "$d/MC.cfg" <<EOF
CONSTANTS
    MaxRequests = 3
    MaxTime = 4
    Skew = 1
    ReadMode = $mode
    Bound = $bound
    RespEnc = $enc
    CollidingWrite = $colliding
    Attacker = $attacker
SPECIFICATION Spec
INVARIANT $prop
EOF
  (cd "$d" && java -XX:+UseParallelGC -cp "$JAR" tlc2.TLC -workers auto -cleanup -deadlock \
     -metadir "$d/states" -config MC.cfg SessionReplayDesign.tla) > "$out" 2>&1 || true
  if grep -q "No error has been found" "$out"; then echo PASS
  elif grep -q "is violated" "$out"; then echo FAIL
  else echo ERROR; fi
}

if [[ $# -eq 2 ]]; then
  out="$WORK/one.out"; v="$(run "$1" "$2" "$out")"; cat "$out"; echo; echo "$1 $2: $v"; exit 0
fi

printf '%-26s' design; for p in $PROPERTIES; do printf ' %-26s' "$p"; done; echo
for d in $DESIGNS; do
  printf '%-26s' "$d"
  for p in $PROPERTIES; do
    v="$(run "$d" "$p" "$WORK/$d-$p.out")"
    [[ "$v" == ERROR ]] && { echo; grep -E -A6 "^Error|Parse|Semantic" "$WORK/$d-$p.out" | head -20; exit 2; }
    printf ' %-26s' "$v"
  done
  echo
done
