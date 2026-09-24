# shellcheck shell=bash
# Pairing binds image source to SDK source. Mixing them produces
# contract-mismatch failures that read as product defects, so an inconsistent
# combination is rejected rather than reconciled.

qual_infer_pairing() {
  local image_source="$1" explicit="$2"
  if [[ -n "${explicit}" ]]; then
    echo "${explicit}"
    return 0
  fi
  case "${image_source}" in
    built) echo "branch" ;;
    pulled) echo "release" ;;
    *) echo "error: unknown image source ${image_source}" >&2; return 1 ;;
  esac
}

qual_check_pairing() {
  local pairing="$1" image_source="$2"
  case "${pairing}" in
    branch)
      [[ "${image_source}" == "built" ]] && return 0
      echo "error: the branch pairing builds its image; got image source '${image_source}'" >&2
      return 1
      ;;
    release)
      [[ "${image_source}" == "pulled" ]] && return 0
      echo "error: the release pairing pulls a published image; got image source '${image_source}'" >&2
      return 1
      ;;
    published)
      # Refused rather than accepted on the image alone. This pairing is meant
      # to prove a consumer can install the published SDKs and drive the
      # published image; nothing installs them yet, so the SDKs would still come
      # from this workspace and a pass would claim coverage nobody has. A
      # missing pairing is recoverable; a pairing that lies is not.
      echo "error: the published pairing needs the SDKs installed from the registry outside this workspace, which is not implemented; use release to qualify a pulled image against the in-repo SDKs" >&2
      return 1
      ;;
    *)
      echo "error: unknown pairing '${pairing}'" >&2
      return 1
      ;;
  esac
}
