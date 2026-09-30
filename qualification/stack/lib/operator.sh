# shellcheck shell=bash
# Seeds the operator allowlist from the server fixtures, so the identities the
# dashboard scenarios sign with and the identities the server accepts cannot
# drift apart.
#
# The allowlist is re-read on every challenge and every authenticated request,
# which is what makes the hot-reload assertion possible without a restart.

qual_write_operator_allowlist() {
  local repo_root="$1" out="$2" restricted_permissions="${3:-}"
  python3 - "${repo_root}" "${out}" "${restricted_permissions}" <<'PY'
import json
import pathlib
import subprocess
import sys

repo_root = pathlib.Path(sys.argv[1])
out = pathlib.Path(sys.argv[2])
restricted = [value for value in sys.argv[3].split(",") if value]

public_keys = json.loads(
    subprocess.run(
        [
            "cargo",
            "run",
            "--quiet",
            "-p",
            "guardian-qualification-driver",
            "--",
            "operator-keys",
        ],
        cwd=repo_root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
)

out.parent.mkdir(parents=True, exist_ok=True)
out.write_text(
    json.dumps(
        [
            # accounts:pause as well as read: the pause scenario needs one
            # identity that can actually pause, and the denial scenario proves
            # the negative through the restricted operator instead.
            {"public_key": public_keys["reader"], "permissions": ["dashboard:read", "accounts:pause"]},
            {"public_key": public_keys["restricted"], "permissions": restricted},
        ],
        indent=2,
    )
    + "\n"
)
PY
}
