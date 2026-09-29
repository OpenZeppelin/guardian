# shellcheck shell=bash
# Prints what a finished run actually produced.
#
# A live run that lost scenarios to the network now exits zero, because the
# network is not the product. That is only an improvement while the losses stay
# visible: an unread green is worth no more than an unread red.

qual_print_summary() {
  local report="$1"
  local run_id="${2:-}"
  [[ -f "${report}" ]] || return 0
  python3 - "${report}" "${run_id}" <<'PY'
import collections
import json
import sys

try:
    report = json.load(open(sys.argv[1]))
except (OSError, ValueError):
    sys.exit(0)

# The results directory is shared across runs, so a run that writes no merged
# report (a single-SDK one, or one whose merge failed) leaves the previous
# run's file in place. Printing it announced another run's success directly
# under this run's failure, which is worse than printing nothing.
wanted = sys.argv[2] if len(sys.argv) > 2 else ""


def runs(report):
    """Every run in the file, whichever of the two shapes it is.

    `report` writes a merged document keyed by network, while a single leg
    writes one run with `scenario_results` at the top. Reading only the second
    shape is how this printed nothing at all for every merged run: the caller
    passes `merged/report.json`, which never has that key.
    """
    networks = report.get("networks")
    if isinstance(networks, dict):
        for name, outcome in networks.items():
            for run in outcome.get("runs", []):
                yield name, run
        return
    if report.get("scenario_results") is not None:
        yield report.get("network", {}).get("name", "-"), report


printed = False
for network, run in runs(report):
    results = run.get("scenario_results", [])
    if not results:
        continue
    if wanted and not run.get("run_id", "").startswith(wanted):
        continue
    printed = True
    counts = collections.Counter(
        entry.get("classification") or entry.get("outcome", "unknown") for entry in results
    )
    print()
    print(
        f"{network}: conclusion {run.get('conclusion', 'unknown')}"
        f"    claim {run.get('qualification_claim', 'unknown')}"
        f"    ({len(results)} scenarios)"
    )
    for name in sorted(counts):
        print(f"  {name}: {counts[name]}")

    # Named individually rather than only counted: the point of the
    # classification is that somebody can tell a bad testnet night from a
    # regression, and that needs the reason, not the tally.
    lost = [entry for entry in results if entry.get("classification") == "environment"]
    if lost:
        print()
        print(f"  {len(lost)} scenario(s) lost to the network, not counted against the product:")
        for entry in lost:
            print(f"    {entry['scenario_id']} ({entry.get('sdk', '?')}): {entry.get('reason', '')}")

if wanted and not printed:
    print()
    print(f"no merged report for {wanted}; read the per-leg results files instead")
PY
}
