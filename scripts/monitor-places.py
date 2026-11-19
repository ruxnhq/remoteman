#!/usr/bin/env python3
"""Where each monitor sits, from the text `gdbus call ... DisplayConfig.GetCurrentState` prints
(a file given as the argument): "Meta-0=0,0 Meta-1=1600,0", by name. For the test scripts."""
import re
import sys

found = re.findall(r"\((-?\d+), (-?\d+), [\d.]+, (?:uint32 )?\d+, (?:true|false), \[\('([^']+)'", open(sys.argv[1]).read())
print(" ".join(f"{name}={x},{y}" for x, y, name in sorted(found, key=lambda m: m[2])))
