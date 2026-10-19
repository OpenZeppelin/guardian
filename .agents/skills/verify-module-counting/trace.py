"""Print a TLC counterexample one step per line: the action and what it changed.

Usage: trace.py <tlc-output>

Requests are shown as "<request> <phase> base=<id> pending=<ids>" so a
step reads like push_delta's progress; other variables print as TLC
writes them. Delta ids are indices into `txs`.
"""

import re
import sys

STEP = re.compile(r"^State (\d+): <(\w+)[^>]*>$")
VAR = re.compile(r"^/\\ (\w+) = (.*)$")
REQUEST = re.compile(r"(q\d+) :> \[(.*?)\]")


def states(lines):
    current = None
    for line in lines:
        step = STEP.match(line)
        if step:
            if current:
                yield current
            current = {"n": step.group(1), "action": step.group(2), "vars": {}}
            name = None
            continue
        if current is None:
            continue
        if not line.strip():
            yield current
            current = None
            continue
        var = VAR.match(line)
        if var:
            name = var.group(1)
            current["vars"][name] = var.group(2)
        elif name:
            current["vars"][name] += " " + line.strip()
    if current:
        yield current


def field(record, key):
    match = re.search(key + r" \|-> (\{[^}]*\}|\"\w+\"|\w+)", record)
    return match.group(1).strip('"') if match else "?"


def show(name, value):
    value = re.sub(r"\s+", " ", value)
    if name == "req":
        return "; ".join(
            f"{request} {field(body, 'phase')} base={field(body, 'base')} pending={field(body, 'pend')}"
            for request, body in REQUEST.findall(value)
        )
    if name == "txs":
        rows = re.findall(r"\[([^\]]*)\]", value)
        return "txs: " + ", ".join(
            f"{i + 1}=(parent {field(r, 'parent')}, nonce {field(r, 'nonce')}, amt {field(r, 'amt')})"
            for i, r in enumerate(rows)
        )
    return f"{name}={value}"


def main():
    with open(sys.argv[1]) as handle:
        lines = handle.read().splitlines()
    start = next((i for i, line in enumerate(lines) if line.startswith("Error: The behavior")), None)
    if start is None:
        return
    previous = {}
    for state in states(lines[start:]):
        changed = [
            show(name, value)
            for name, value in state["vars"].items()
            if previous.get(name) != value and (state["n"] != "1" or name in ("holder", "believes"))
        ]
        action = state["action"]
        print(f"{state['n']:>3}. {action:16} {' | '.join(changed)}")
        previous = state["vars"]


main()
