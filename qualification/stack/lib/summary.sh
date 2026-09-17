# shellcheck shell=bash
# Prints what a finished run actually produced.
#
# A live run that lost scenarios to the network now exits zero, because the
# network is not the product. That is only an improvement while the losses stay
# visible: an unread green is worth no more than an unread red.

qual_print_summary() {
  local report="$1"
  [[ -f "${report}" ]] || return 0
  python3 - "${report}" <<'PY'
import collections
import json
import sys

try:
    report = json.load(open(sys.argv[1]))
except (OSError, ValueError):
    sys.exit(0)

results = report.get("scenario_results", [])
if not results:
    sys.exit(0)

counts = collections.Counter(
    entry.get("classification") or entry.get("outcome", "unknown") for entry in results
)
print()
print(f"conclusion: {report.get('conclusion', 'unknown')}", end="")
print(f"    claim: {report.get('qualification_claim', 'unknown')}")
for name in sorted(counts):
    print(f"  {name}: {counts[name]}")

# Named individually rather than only counted: the point of the classification
# is that somebody can tell a bad testnet night from a regression, and that
# needs the reason, not the tally.
lost = [entry for entry in results if entry.get("classification") == "environment"]
if lost:
    print()
    print(f"{len(lost)} scenario(s) lost to the network, not counted against the product:")
    for entry in lost:
        print(f"  {entry['scenario_id']} ({entry['sdk']}): {entry.get('reason', '')}")
PY
}
