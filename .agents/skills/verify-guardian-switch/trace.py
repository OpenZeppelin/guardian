"""Print a TLC counterexample one step per line: the action and what it changed.

Usage: trace.py <tlc-output>

`st` is shown as the states added in the step, "<id>=(nonce, key, parent)",
so a chain or a stored value reads as state ids.
"""

import re
import sys

STEP = re.compile(r"^State (\d+): <(\w+)[^>]*>$|^State (\d+): (Stuttering)$")
VAR = re.compile(r"^/\\ (\w+) = (.*)$")
STATE = re.compile(r"\[k \|-> \"(\w+)\", n \|-> (\d+), p \|-> (\d+)\]")


def states(lines):
    current = None
    name = None
    for line in lines:
        step = STEP.match(line)
        if step:
            if current:
                yield current
            current = {
                "n": step.group(1) or step.group(3),
                "action": step.group(2) or step.group(4),
                "vars": {},
            }
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


def show(name, value, previous):
    value = re.sub(r"\s+", " ", value)
    if name == "st":
        before = len(STATE.findall(previous or ""))
        added = STATE.findall(value)[before:]
        return ", ".join(
            f"new {before + i + 1}=(nonce {n}, {k}, parent {p})" for i, (k, n, p) in enumerate(added)
        )
    return f"{name}={value}"


def main():
    with open(sys.argv[1]) as handle:
        lines = handle.read().splitlines()
    start = next((i for i, line in enumerate(lines) if line.startswith(("Error: The behavior", "Error: The following behavior"))), None)
    if start is None:
        return
    previous = {}
    for state in states(lines[start:]):
        changed = [
            show(name, value, previous.get(name))
            for name, value in state["vars"].items()
            if state["n"] != "1" and previous.get(name) != value
        ]
        print(f"{state['n']:>3}. {state['action']:16} {' | '.join(changed)}")
        previous = state["vars"]


main()
