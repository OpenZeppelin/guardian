"""Print a hash of every Rust or TypeScript function this skill's model mirrors.

Usage: fingerprint.py <repo-root> <mirrored.txt>

mirrored.txt lists "<path relative to repo root> <function name>" per
line. A function's text runs from its definition line to the brace that
closes its body (not a brace in its parameters, generics or return type); every definition with that name in the file is hashed
together, so trait impls, overloads and wrappers of the same name count
too. A `.rs` path matches `fn <name>`; a `.ts` path matches a function
declaration, a class method or a `const <name> =` arrow function. A file
or function that is not found prints MISSING.
"""

import hashlib
import re
import sys

RUST = r"^\s*(pub(\([^)]*\))?\s+)?(async\s+)?fn\s+{name}\b"
TYPESCRIPT = (
    r"^\s*(export\s+)?(default\s+)?(async\s+)?function\s*\*?\s*{name}\b"
    r"|^\s*((public|private|protected|static|readonly|override)\s+)*(async\s+)?{name}\s*(<[^>\n]*>)?\s*\("
    r"|^\s*(export\s+)?const\s+{name}\s*(:[^=\n]*)?=\s*(async\s+)?"
)


def definition(path, name):
    template = TYPESCRIPT if path.endswith(".ts") else RUST
    return re.compile(template.format(name=re.escape(name)), re.M)


def closing(source, index):
    depth = 0
    for position in range(index, len(source)):
        if source[position] == "{":
            depth += 1
        elif source[position] == "}":
            depth -= 1
            if depth == 0:
                return position
    return len(source) - 1


def body_end(source, start):
    """Index just past the definition starting at `start`: its body's closing
    brace, or the `;` of a bodiless declaration. Braces inside the parameter
    list (`options = {}`), inside generics (`Promise<{ ... }>`) or opening a
    type literal after `:` (`): { ... } {`) are not the body."""
    paren = 0
    angle = 0
    seen_params = False
    index = start
    while index < len(source):
        char = source[index]
        previous = source[index - 1] if index > 0 else ""
        if char == "(":
            paren += 1
            seen_params = True
        elif char == ")":
            paren -= 1
        elif paren == 0 and char == ";" and angle == 0:
            return index + 1
        elif paren == 0 and seen_params:
            if char == "<":
                angle += 1
            elif char == ">" and previous not in "-=":
                angle -= 1
            elif char == "{" and angle == 0:
                end = closing(source, index)
                if source[:index].rstrip()[-1:] in (":", "|", "&"):
                    index = end + 1
                    continue
                return end + 1
        index += 1
    return len(source)


def bodies(source, pattern):
    for match in pattern.finditer(source):
        yield source[match.start(): body_end(source, match.start())]


def main():
    root, listing = sys.argv[1], sys.argv[2]
    with open(listing) as handle:
        entries = [line.split() for line in handle if line.strip() and not line.startswith("#")]
    for path, name in entries:
        try:
            with open(f"{root}/{path}") as handle:
                source = handle.read()
        except FileNotFoundError:
            print(f"{path} {name} MISSING")
            continue
        found = list(bodies(source, definition(path, name)))
        digest = hashlib.sha256("\n".join(found).encode()).hexdigest()[:16] if found else "MISSING"
        print(f"{path} {name} {digest}")


main()
