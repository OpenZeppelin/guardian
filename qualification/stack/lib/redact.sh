# shellcheck shell=bash
# Redaction and the secret scan.
#
# Redaction is field-aware. A blanket rule over long hex runs also destroys the
# image digest, which is the artifact identity a result exists to record, and an
# ECDSA key is the same length as a SHA-256 digest so length alone cannot tell
# them apart.
#
# The scan is the actual control: it looks for the exact secret values this run
# was configured with. It fails the run rather than warning, because an artifact
# that leaked a key has already been written.

QUAL_PUBLIC_FIELDS="image_digest image_revision integrity sdk_integrity"

qual_redact_dir() {
  local dir="$1"
  python3 - "${dir}" <<'PY'
import json
import pathlib
import re
import sys

SECRET_KEY = re.compile(
    r"(?i)(secret|private[_-]?key|passwd|password|token|credential|cookie)"
)
PUBLIC_KEY = re.compile(r"(?i)^(image_digest|image_revision|integrity|.*_integrity)$")
REDACTED = "<redacted>"

TEXT_PATTERNS = [
    (re.compile(r"(?i)\b(secret|private[_-]?key|password|token)\b\s*[:=]\s*\S+"), r"\1=<redacted>"),
    (re.compile(r"(?i)\bset-cookie\s*:\s*\S+"), "set-cookie: <redacted>"),
    # Key material is far longer than any digest this suite records.
    (re.compile(r"\b[0-9a-fA-F]{128,}\b"), "<redacted-key-material>"),
]


def scrub(node, key_hint=""):
    if isinstance(node, dict):
        return {key: scrub(value, key) for key, value in node.items()}
    if isinstance(node, list):
        return [scrub(item, key_hint) for item in node]
    if isinstance(node, str):
        if PUBLIC_KEY.match(key_hint):
            return node
        if SECRET_KEY.search(key_hint):
            return REDACTED
        if node.startswith("sha256:"):
            return node
        if re.fullmatch(r"[0-9a-fA-F]{128,}", node):
            return "<redacted-key-material>"
        return node
    return node


root = pathlib.Path(sys.argv[1])
for path in root.rglob("*"):
    if not path.is_file():
        continue
    try:
        text = path.read_text(errors="replace")
    except OSError:
        continue

    if path.suffix == ".json":
        try:
            document = json.loads(text)
        except json.JSONDecodeError:
            document = None
        if document is not None:
            path.write_text(json.dumps(scrub(document), indent=2) + "\n")
            continue

    for pattern, replacement in TEXT_PATTERNS:
        text = pattern.sub(replacement, text)
    path.write_text(text)
PY
}

qual_scan_for_secrets() {
  local dir="$1"
  shift
  python3 - "${dir}" "$@" <<'PY'
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
needles = [value for value in sys.argv[2:] if value and len(value) >= 8]
if not needles:
    sys.exit(0)

offenders = []
for path in root.rglob("*"):
    if not path.is_file():
        continue
    try:
        text = path.read_text(errors="replace")
    except OSError:
        continue
    for needle in needles:
        if needle in text:
            offenders.append(f"{path}: contains a configured secret value")

if offenders:
    for line in offenders:
        print(line, file=sys.stderr)
    sys.exit(1)
PY
}
