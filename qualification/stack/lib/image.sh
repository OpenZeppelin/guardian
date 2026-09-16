# shellcheck shell=bash
# Resolves the server image for either source.
#
# The build context excludes version-control metadata, so an image built without
# the commit passed in reports an unknown revision and the identity assertion
# would compare nothing.

# Builds the requested ref's contents, not the current working tree stamped
# with that ref's identity. Stamping alone would make the identity assertion
# pass against code the ref does not contain, which is worse than not asserting.
qual_build_image() {
  local repo_root="$1" ref="$2" tag="$3"
  local sha head_sha context worktree=""
  sha="$(git -C "${repo_root}" rev-parse --short=12 "${ref}")" || return 1
  head_sha="$(git -C "${repo_root}" rev-parse --short=12 HEAD)" || return 1

  if [[ "${sha}" == "${head_sha}" ]]; then
    context="${repo_root}"
  else
    if ! git -C "${repo_root}" diff --quiet HEAD 2>/dev/null; then
      echo "error: the working tree has uncommitted changes, so building another ref would silently ignore them" >&2
      return 1
    fi
    worktree="$(mktemp -d)/src"
    if ! git -C "${repo_root}" worktree add --detach --quiet "${worktree}" "${ref}" >&2; then
      echo "error: cannot check out ${ref} in an isolated worktree" >&2
      return 1
    fi
    context="${worktree}"
  fi

  local status=0
  docker build \
    --file "${context}/Dockerfile" \
    --target server-runner \
    --build-arg GUARDIAN_SERVER_FEATURES=postgres \
    --build-arg "GUARDIAN_GIT_SHA=${sha}" \
    --tag "${tag}" \
    "${context}" >&2 || status=1

  if [[ -n "${worktree}" ]]; then
    git -C "${repo_root}" worktree remove --force "${worktree}" >/dev/null 2>&1 || true
    rm -rf "$(dirname "${worktree}")" 2>/dev/null || true
  fi

  (( status == 0 )) || return 1
  echo "${sha}"
}

qual_resolve_digest() {
  local reference="$1"
  local digest
  digest="$(docker buildx imagetools inspect "${reference}" --format '{{.Manifest.Digest}}' 2>/dev/null)" || {
    echo "error: cannot resolve ${reference} to a digest" >&2
    return 1
  }
  if [[ ! "${digest}" =~ ^sha256:[0-9a-f]{64}$ ]]; then
    echo "error: resolved digest ${digest} is malformed" >&2
    return 1
  fi
  echo "${digest}"
}

qual_image_revision() {
  local reference="$1"
  docker image inspect "${reference}" \
    --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' 2>/dev/null \
    || echo ""
}
