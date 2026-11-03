"""Print a hash of every Rust or TypeScript function the proposal model mirrors.

Usage: fingerprint.py <repo-root> <mirrored.txt>

mirrored.txt lists "<path relative to repo root> <function name>" per
line. A function's text runs from its definition line to the brace that
closes its body; every definition with that name in the file is hashed
together. Rust definitions are `fn name`; TypeScript definitions are
`function name(` or a class method `[modifiers] name(`.
"""

import hashlib
import re
import sys


def pattern_for(path, name):
    escaped = re.escape(name)
    if path.endswith(".rs"):
        return re.compile(r"^\s*(pub(\([^)]*\))?\s+)?(async\s+)?fn\s+" + escaped + r"\b", re.M)
    return re.compile(
        r"^\s*(export\s+)?((public|private|protected|static|async|readonly)\s+)*"
        r"(function\s+)?" + escaped + r"\s*(<[^>]*>)?\(",
        re.M,
    )


def body_start(source, match, typescript):
    if not typescript:
        return match.start()
    index = match.end()
    parens = 1
    while index < len(source) and parens:
        parens += {"(": 1, ")": -1}.get(source[index], 0)
        index += 1
    angles = 0
    while index < len(source):
        char = source[index]
        if char == "<":
            angles += 1
        elif char == ">" and source[index - 1] != "=":
            angles -= 1
        elif char == "{" and angles == 0:
            return index
        index += 1
    return match.start()


def bodies(source, pattern, typescript):
    for match in pattern.finditer(source):
        depth = 0
        opened = False
        start = body_start(source, match, typescript)
        for index in range(start, len(source)):
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
        found = list(bodies(source, pattern_for(path, name), not path.endswith(".rs")))
        digest = hashlib.sha256("\n".join(found).encode()).hexdigest()[:16] if found else "MISSING"
        print(f"{path} {name} {digest}")


main()
