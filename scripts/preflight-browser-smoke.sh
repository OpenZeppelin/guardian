#!/usr/bin/env bash
#
# Preflight for the browser example harnesses (examples/smoke-web, examples/web).
#
# Both apps alias @openzeppelin/miden-multisig-client and @openzeppelin/guardian-client
# to packages/<name>/dist/index.js in their vite.config.ts, so they run the BUILT
# package, never src/. dist/ is gitignored, which means it survives every branch
# switch: a dist built on another protocol line (e.g. miden-0.16) keeps being served
# on a 0.15 branch until something rebuilds it. The MASM baked into that dist then
# meets a different WASM assembler and fails with a bare
# "Failed to compile account component: invalid syntax".
#
# The same applies to node_modules: a foreign branch can leave an out-of-range
# @miden-sdk/miden-sdk installed that the workspace's own lockfile does not pin.
#
# This script reports what drifted, fixes it, then re-verifies.
#
# Usage:
#   ./scripts/preflight-browser-smoke.sh          # report, fix, verify
#   ./scripts/preflight-browser-smoke.sh --check   # report only, never write
#
# Exit codes: 0 = clean (or all issues fixed), 1 = drift remains / fix failed,
#             2 = usage error.

set -uo pipefail

CHECK_ONLY=0
case "$#" in
  0) ;;
  1) case "$1" in
       --check) CHECK_ONLY=1 ;;
       *) echo "error: expected --check or no arguments" >&2; exit 2 ;;
     esac ;;
  *) echo "error: expected --check or no arguments" >&2; exit 2 ;;
esac

ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || {
  echo "error: not inside a git repository" >&2
  exit 1
}
cd "$ROOT"

# Packages the browser apps alias to dist/index.js. Keep in sync with the
# resolve.alias blocks in examples/web/vite.config.ts and examples/smoke-web/vite.config.ts.
DIST_PACKAGES=(
  packages/guardian-client
  packages/miden-multisig-client
)

# Workspaces that declare @miden-sdk/miden-sdk and must match their own lockfile pin.
SDK_WORKSPACES=(
  packages/miden-multisig-client
  examples/_shared/multisig-browser
  examples/smoke-web
  examples/web
  examples/operator-smoke-web
)

RED=$'\033[31m'; GRN=$'\033[32m'; YEL=$'\033[33m'; DIM=$'\033[2m'; OFF=$'\033[0m'
[[ -t 1 ]] || { RED=""; GRN=""; YEL=""; DIM=""; OFF=""; }

ok()   { printf '  %s%-46s%s %s\n' "$DIM" "$1" "$OFF" "${GRN}ok${OFF}"; }
bad()  { printf '  %s%-46s%s %s\n' "$DIM" "$1" "$OFF" "${RED}$2${OFF}"; }
note() { printf '      %s%s%s\n' "$DIM" "$1" "$OFF"; }

# Version installed in a workspace's node_modules, or empty.
installed_sdk() {
  python3 -c "
import json,sys
try: print(json.load(open('$1/node_modules/@miden-sdk/miden-sdk/package.json'))['version'])
except Exception: print('')
" 2>/dev/null
}

# Version the workspace's own lockfile pins, or empty.
locked_sdk() {
  python3 -c "
import json,sys
try:
    d=json.load(open('$1/package-lock.json'))['packages']
    print(d.get('node_modules/@miden-sdk/miden-sdk',{}).get('version',''))
except Exception: print('')
" 2>/dev/null
}

declared_sdk() {
  python3 -c "
import json,sys
try:
    d=json.load(open('$1/package.json'))
    print(d.get('dependencies',{}).get('@miden-sdk/miden-sdk','') or d.get('devDependencies',{}).get('@miden-sdk/miden-sdk',''))
except Exception: print('')
" 2>/dev/null
}

# Count src/*.ts files newer than the built entrypoint. Echoes STALE reason or empty.
dist_staleness() {
  local pkg="$1" entry="$1/dist/index.js"
  [[ -d "$pkg/dist" ]] || { echo "dist/ missing"; return; }
  [[ -f "$entry" ]]    || { echo "dist/index.js missing"; return; }
  local n
  n=$(find "$pkg/src" -name '*.ts' -newer "$entry" 2>/dev/null | wc -l | tr -d ' ')
  [[ "$n" -gt 0 ]] && echo "$n src file(s) newer than dist"
}

SDK_DRIFT=(); LOCK_ERRORS=(); STALE=(); ISSUES=0

scan() {
  SDK_DRIFT=(); LOCK_ERRORS=(); STALE=()

  printf '\n%s[1/4]%s @miden-sdk/miden-sdk: installed vs lockfile pin\n' "$YEL" "$OFF"
  for w in "${SDK_WORKSPACES[@]}"; do
    [[ -d "$w" ]] || continue
    local dec ins loc
    dec="$(declared_sdk "$w")"; [[ -n "$dec" ]] || continue
    ins="$(installed_sdk "$w")"; loc="$(locked_sdk "$w")"
    if [[ -z "$loc" ]]; then
      bad "$w" "lockfile pin missing"
      note "declared $dec -> run npm install to update package-lock.json"
      LOCK_ERRORS+=("$w"); continue
    fi
    if [[ -z "$ins" ]]; then
      bad "$w" "not installed"; note "declared $dec -> run npm install"
      SDK_DRIFT+=("$w"); continue
    fi
    if [[ "$ins" != "$loc" ]]; then
      bad "$w" "MISMATCH"
      note "declared $dec | lockfile $loc | installed $ins"
      note "out-of-range install from another branch -> npm ci"
      SDK_DRIFT+=("$w")
    else
      ok "$w  $ins"
    fi
  done

  printf '\n%s[2/4]%s built dist freshness (apps alias dist, not src)\n' "$YEL" "$OFF"
  for p in "${DIST_PACKAGES[@]}"; do
    [[ -d "$p" ]] || continue
    local why
    why="$(dist_staleness "$p")"
    if [[ -n "$why" ]]; then
      bad "$p/dist" "STALE"; note "$why"
      STALE+=("$p")
    else
      ok "$p/dist"
    fi
  done

  ISSUES=$(( ${#SDK_DRIFT[@]} + ${#LOCK_ERRORS[@]} + ${#STALE[@]} ))
}

scan
FOUND=$ISSUES

if [[ "$ISSUES" -eq 0 ]]; then
  printf '\n%s[3/4]%s rebuild: not needed\n' "$YEL" "$OFF"
  printf '%s[4/4]%s vite caches: left alone\n' "$YEL" "$OFF"
  printf '\n%sPREFLIGHT OK%s  browser harnesses are safe to smoke\n\n' "$GRN" "$OFF"
  exit 0
fi

if [[ "$CHECK_ONLY" -eq 1 ]]; then
  printf '\n%s[3/4]%s rebuild: skipped (--check)\n' "$YEL" "$OFF"
  printf '%s[4/4]%s vite caches: skipped (--check)\n' "$YEL" "$OFF"
  printf '\n%sPREFLIGHT FAILED%s  %d issue(s); re-run without --check to fix\n\n' "$RED" "$OFF" "$FOUND"
  exit 1
fi

if [[ ${#SDK_DRIFT[@]} -eq 0 && ${#STALE[@]} -eq 0 ]]; then
  printf '\n%s[3/4]%s rebuild: skipped (lockfile pin missing; npm ci cannot create it)\n' "$YEL" "$OFF"
  printf '%s[4/4]%s vite caches: left alone\n' "$YEL" "$OFF"
  printf '\n%sPREFLIGHT FAILED%s  %d issue(s); add the missing lockfile pin(s) then re-run\n\n' "$RED" "$OFF" "$FOUND"
  exit 1
fi

printf '\n%s[3/4]%s fixing\n' "$YEL" "$OFF"
FIX_FAILED=0

for w in ${SDK_DRIFT[@]+"${SDK_DRIFT[@]}"}; do
  printf '  npm ci  %s ... ' "$w"
  if (cd "$w" && npm ci >/dev/null 2>&1); then
    echo "${GRN}ok${OFF}"
    # A reinstall invalidates anything already built here.
    for p in "${DIST_PACKAGES[@]}"; do
      [[ "$p" == "$w" ]] && { case " ${STALE[*]+${STALE[*]}} " in *" $p "*) ;; *) STALE+=("$p");; esac; }
    done
  else
    echo "${RED}FAILED${OFF}"; FIX_FAILED=1
  fi
done

for p in ${STALE[@]+"${STALE[@]}"}; do
  # clean, not a bare build: export names change across protocol lines, and tsc
  # leaves orphaned files from the previous build behind.
  printf '  clean+build %s ... ' "$p"
  if (cd "$p" && npm run clean >/dev/null 2>&1 && npm run build >/dev/null 2>&1); then
    echo "${GRN}ok${OFF}"
  else
    echo "${RED}FAILED${OFF}"; FIX_FAILED=1
  fi
done

printf '\n%s[4/4]%s clearing vite dep caches\n' "$YEL" "$OFF"
shopt -s nullglob
for c in examples/*/node_modules/.vite examples/_shared/*/node_modules/.vite; do
  rm -rf "$c" && printf '  removed %s\n' "$c"
done
shopt -u nullglob

printf '\n%sre-verifying%s\n' "$DIM" "$OFF"
scan

if [[ "$ISSUES" -eq 0 && "$FIX_FAILED" -eq 0 ]]; then
  printf '\n%sPREFLIGHT FIXED%s  %d issue(s) resolved\n' "$GRN" "$OFF" "$FOUND"
  printf '%sRestart any running vite dev server so it picks up the rebuilt dist.%s\n\n' "$DIM" "$OFF"
  exit 0
fi

printf '\n%sPREFLIGHT FAILED%s  %d issue(s) remain after fix\n\n' "$RED" "$OFF" "$ISSUES"
exit 1
