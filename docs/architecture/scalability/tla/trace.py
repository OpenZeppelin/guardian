import re, sys
keys = ("now", "floor", "live", "sent", "honest", "writes", "offRouteWrite", "leakAfter")
state = None
for line in sys.stdin:
    m = re.match(r"State (\d+): <(\w+)", line)
    if m:
        state = f"{m[1]}. {m[2]}"; print(state); continue
    m = re.match(r"/\\ (\w+) = (.*)", line)
    if state and m and m[1] in keys:
        print(f"     {m[1]} = {m[2]}")
    if line.startswith(("Error: Invariant", "Model checking completed")):
        print(line.strip())
