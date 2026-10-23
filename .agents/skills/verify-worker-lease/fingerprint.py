"""Print a hash of every Rust function this skill's model mirrors.

Usage: fingerprint.py <repo-root> <mirrored.txt>

mirrored.txt lists "<path relative to repo root> <function name>" per
line. A function's text runs from its `fn <name>` line to the brace that
closes its body; every definition with that name in the file is hashed
together, so trait impls and wrappers of the same name count too.
"""

import hashlib
import re
import sys


def bodies(source, name):
    pattern = re.compile(r"^\s*(pub(\([^)]*\))?\s+)?(async\s+)?fn\s+" + re.escape(name) + r"\b", re.M)
    for match in pattern.finditer(source):
        depth = 0
        opened = False
        for index in range(match.start(), len(source)):
            char = source[index]
            if char == "{":
                depth += 1
                opened = True
            elif char == "}":
                depth -= 1
                if opened and depth == 0:
                    yield source[match.start(): index + 1]
                    break
            elif char == ";" and not opened:
                yield source[match.start(): index + 1]
                break


def main():
    root, listing = sys.argv[1], sys.argv[2]
    with open(listing) as handle:
        entries = [line.split() for line in handle if line.strip() and not line.startswith("#")]
    for path, name in entries:
        with open(f"{root}/{path}") as handle:
            source = handle.read()
        found = list(bodies(source, name))
        digest = hashlib.sha256("\n".join(found).encode()).hexdigest()[:16] if found else "MISSING"
        print(f"{path} {name} {digest}")


main()
