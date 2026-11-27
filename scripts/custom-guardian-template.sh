#!/usr/bin/env bash
# Builds the custom-guardian template of a Guardian release: a standalone Cargo project that
# depends on guardian-server and guardian-module-sdk at the release tag and pins every other
# dependency to the versions in the release's Cargo.lock. Operators add their own module crates
# to it and build with --locked (RFC 0002, section 2.5).
#
#   scripts/custom-guardian-template.sh --tag v0.20.0 --out dist/custom-guardian
#   scripts/custom-guardian-template.sh --rev "$(git rev-parse HEAD)" \
#     --git "file://$PWD" --out /tmp/custom-guardian        # try a local commit
#
# Run it from a checkout of the same release: the lockfile and the example sources are taken
# from the working tree, and --tag refuses a checkout at any other commit. It resolves the git
# dependency, so it needs network access to --git.
set -euo pipefail

die() {
  echo "custom-guardian-template: $*" >&2
  exit 1
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
git_url="https://github.com/OpenZeppelin/guardian"
kind=""
reference=""
out=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag) [[ $# -ge 2 ]] || die "--tag needs a value"; kind="tag"; reference="$2"; shift 2 ;;
    --rev) [[ $# -ge 2 ]] || die "--rev needs a value"; kind="rev"; reference="$2"; shift 2 ;;
    --git) [[ $# -ge 2 ]] || die "--git needs a value"; git_url="$2"; shift 2 ;;
    --out) [[ $# -ge 2 ]] || die "--out needs a value"; out="$2"; shift 2 ;;
    *) die "unknown argument $1" ;;
  esac
done
[[ -n "${reference}" ]] || die "pass --tag or --rev"
[[ "${reference}" =~ ^[0-9A-Za-z][0-9A-Za-z._/-]*$ ]] || die "invalid ${kind} ${reference}"
[[ "${git_url}" != *'"'* ]] || die "invalid --git ${git_url}"
if [[ "${kind}" == "tag" ]]; then
  tagged="$(git -C "${repo_root}" rev-parse --verify --quiet "${reference}^{commit}")" \
    || die "tag ${reference} is not in this checkout"
  [[ "${tagged}" == "$(git -C "${repo_root}" rev-parse HEAD)" ]] \
    || die "the checkout is not at ${reference}; check out the tag first"
fi
[[ -n "${out}" ]] || die "pass --out"
[[ ! -e "${out}" ]] || die "${out} already exists"

example="${repo_root}/examples/custom-guardian"
note_policy="${repo_root}/crates/guardian-module-sdk/examples/note_policy/mod.rs"
mkdir -p "${out}/src"

cp "${example}/Dockerfile" "${example}/.dockerignore" "${out}/"
sed -e '/^## In this repository$/,$d' \
  -e "s#](\.\./\.\./#](${git_url%.git}/blob/${reference}/#g" \
  "${example}/README.md" > "${out}/README.md"
cp "${repo_root}/rust-toolchain.toml" "${repo_root}/Cargo.lock" "${out}/"
cp "${note_policy}" "${out}/src/note_policy.rs"
grep -v '^#\[path = ' "${example}/src/main.rs" > "${out}/src/main.rs"

dependency() {
  echo "$1 = { git = \"${git_url}\", ${kind} = \"${reference}\" }"
}

{
  sed -n '1,/^\[dependencies\]$/p' "${example}/Cargo.toml"
  dependency guardian-server
  dependency guardian-module-sdk
  sed -n '/^\[dependencies\]$/,$p' "${example}/Cargo.toml" | tail -n +2 | grep -v '^guardian-'
  echo
  echo "# A module crate published to crates.io depends on the crates.io guardian-module-sdk;"
  echo "# this makes it the release's copy, so its factory has the trait the server expects."
  echo "[patch.crates-io]"
  dependency guardian-module-sdk
  echo
  echo "# The release profile of the official build."
  sed -n '/^\[profile\.release\]$/,/^$/p' "${repo_root}/Cargo.toml"
  echo "# Standalone: not part of an enclosing workspace."
  echo "[workspace]"
} > "${out}/Cargo.toml"

grep -qE '(\{|,) *path *=' "${out}/Cargo.toml" && die "the template manifest still has a path dependency"

# Resolves the git dependencies into the copied lockfile. Cargo keeps every entry that still
# satisfies the manifest, so only the Guardian crates change source and unused entries go.
cargo metadata --format-version 1 --manifest-path "${out}/Cargo.toml" > /dev/null
cargo metadata --format-version 1 --locked --manifest-path "${out}/Cargo.toml" > /dev/null

python3 - "${repo_root}/Cargo.lock" "${out}/Cargo.lock" "git+${git_url}" <<'PY'
import sys
import tomllib

def packages(path):
    with open(path, "rb") as lock:
        return {
            (package["name"], package["version"], package.get("source", ""))
            for package in tomllib.load(lock)["package"]
        }

release, template = packages(sys.argv[1]), packages(sys.argv[2])
guardian = sys.argv[3]
drifted = sorted(
    f"{name} {version} {source}"
    for name, version, source in template - release
    if not source.startswith((guardian + "?", guardian + "#"))
)
if drifted:
    sys.exit("template lockfile resolved versions the release does not pin:\n  " + "\n  ".join(drifted))
PY

echo "custom-guardian template written to ${out}"
