"""Print a hash of every Rust function and RFC section this skill's model mirrors.

Usage: fingerprint.py <repo-root> <mirrored.txt>

mirrored.txt lists "<path relative to repo root> <name>" per line.

For a .rs file, <name> is a function name: its text runs from its
`fn <name>` line to the brace that closes its body, and every definition
with that name in the file is hashed together, so trait impls and wrappers
of the same name count too.

For a .md file, <name> is a section number such as 3.4: the section runs
from the heading whose text starts with that number to the next heading
of the same or a higher level.
"""

import hashlib
import re
import sys


def rust_bodies(source, name):
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


def markdown_sections(source, number):
    lines = source.splitlines(keepends=True)
    heading = re.compile(r"^(#+)\s+(.*)$")
    for start, line in enumerate(lines):
        match = heading.match(line)
        if not match or not match.group(2).startswith(number + " "):
            continue
        level = len(match.group(1))
        end = len(lines)
        for index in range(start + 1, len(lines)):
            other = heading.match(lines[index])
            if other and len(other.group(1)) <= level:
                end = index
                break
        yield "".join(lines[start:end])


def main():
    root, listing = sys.argv[1], sys.argv[2]
    with open(listing) as handle:
        entries = [line.split() for line in handle if line.strip() and not line.startswith("#")]
    for path, name in entries:
        with open(f"{root}/{path}") as handle:
            source = handle.read()
        found = list(markdown_sections(source, name) if path.endswith(".md") else rust_bodies(source, name))
        digest = hashlib.sha256("\n".join(found).encode()).hexdigest()[:16] if found else "MISSING"
        print(f"{path} {name} {digest}")


main()
