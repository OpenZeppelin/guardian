# shellcheck shell=bash
# Provisions the acknowledgement identity.
#
# Under the development default the keypair is regenerated on every boot, which
# breaks the profile twice over: registration binds the server's acknowledgement
# commitment, so a fixture prepared against one boot is rejected by the next,
# and the restart assertion would fail for a configuration reason rather than a
# durability defect.

QUAL_ACK_FALCON_FILE="ack-falcon-secret-key"
QUAL_ACK_ECDSA_FILE="ack-ecdsa-secret-key"

# The deterministic profile registers the committed fixture account, whose
# stored state binds one specific guardian commitment. A generated identity is
# rejected, so that profile must run against the fixture's own key.
qual_provision_fixture_ack_keys() {
  local dir="$1" repo_root="$2"
  mkdir -p "${dir}"
  chmod 700 "${dir}"

  python3 -c 'import json,pathlib,sys; keys=json.loads((pathlib.Path(sys.argv[1])/"crates/server/src/testing/fixtures/keys.json").read_text()); secret=keys.get("guardian_secret_key"); sys.exit("keys.json has no guardian_secret_key") if not secret else pathlib.Path(sys.argv[2]).write_text(secret)' \
    "${repo_root}" "${dir}/${QUAL_ACK_FALCON_FILE}" || return 1

  # The fixture account binds only the Falcon identity, but the file provider
  # still requires an ECDSA key to be present.
  if [[ ! -s "${dir}/${QUAL_ACK_ECDSA_FILE}" ]]; then
    head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "${dir}/${QUAL_ACK_ECDSA_FILE}"
  fi

  chmod 600 "${dir}/${QUAL_ACK_FALCON_FILE}" "${dir}/${QUAL_ACK_ECDSA_FILE}"
}

qual_provision_ack_keys() {
  local dir="$1" image="$2"
  mkdir -p "${dir}"
  chmod 700 "${dir}"

  if [[ -s "${dir}/${QUAL_ACK_FALCON_FILE}" && -s "${dir}/${QUAL_ACK_ECDSA_FILE}" ]]; then
    return 0
  fi

  # ack-keygen refuses to overwrite, so a partial directory is cleared first.
  rm -f "${dir}/${QUAL_ACK_FALCON_FILE}" "${dir}/${QUAL_ACK_ECDSA_FILE}"

  docker run --rm --user "$(id -u):$(id -g)" \
    -v "$(cd "${dir}" && pwd):/out" \
    --entrypoint /app/ack-keygen \
    "${image}" --out-dir /out >/dev/null || {
      echo "error: ack-keygen failed" >&2
      return 1
    }

  if [[ ! -s "${dir}/${QUAL_ACK_FALCON_FILE}" || ! -s "${dir}/${QUAL_ACK_ECDSA_FILE}" ]]; then
    echo "error: ack-keygen produced no key files in ${dir}" >&2
    return 1
  fi

  chmod 600 "${dir}/${QUAL_ACK_FALCON_FILE}" "${dir}/${QUAL_ACK_ECDSA_FILE}"
}
