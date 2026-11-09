"""Print a hash of every Rust and SQL function this skill's model mirrors.

Usage: fingerprint.py <repo-root> <mirrored.txt>

mirrored.txt lists "<path relative to repo root> <function name>" per
line. In a Rust file a function's text runs from its `fn <name>` line to
the brace that closes its body; every definition with that name in the
file is hashed together, so trait impls and wrappers of the same name
count too. A SQL function is listed under a migrations directory: its
text runs from `CREATE [OR REPLACE] FUNCTION <name>` to the `$$;` that
closes the body, and every definition in every `*/up.sql` below that
directory is hashed in migration order, so a later migration that
redefines the function counts too.
"""

import hashlib
import os
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


def sql_bodies(source, name):
    pattern = re.compile(r"^CREATE\s+(OR\s+REPLACE\s+)?FUNCTION\s+" + re.escape(name) + r"\s*\(", re.M | re.I)
    for match in pattern.finditer(source):
        end = source.find("$$;", match.end())
        if end != -1:
            yield source[match.start(): end + 3]


def main():
    root, listing = sys.argv[1], sys.argv[2]
    with open(listing) as handle:
        entries = [line.split() for line in handle if line.strip() and not line.startswith("#")]
    for path, name in entries:
        if os.path.isdir(f"{root}/{path}"):
            found = []
            for migration in sorted(os.listdir(f"{root}/{path}")):
                up = f"{root}/{path}/{migration}/up.sql"
                if os.path.isfile(up):
                    with open(up) as handle:
                        found.extend(sql_bodies(handle.read(), name))
        else:
            with open(f"{root}/{path}") as handle:
                found = list(bodies(handle.read(), name))
        digest = hashlib.sha256("\n".join(found).encode()).hexdigest()[:16] if found else "MISSING"
        print(f"{path} {name} {digest}")


main()
