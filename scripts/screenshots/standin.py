"""Stand-in agent process for the README screenshots.

Launched as `python3 -I <dir>/<runtime> [--model M]`, where `<runtime>` is a
symlink to this file named after an agent runtime (`claude`, `opencode`, ...).
crew-watch detects it through the interpreter fallback (the basename of the
first non-flag argument after `python3`).
The load it shows comes from the environment, not argv, so no extra positional
argument reads as a prompt:

  STANDIN_CPU  target CPU% of one core (clamped to 0-100), spun in 100 ms
               duty cycles
  STANDIN_MEM  MiB of resident memory to allocate and touch
"""

import os
import time

cpu = min(max(float(os.environ.get("STANDIN_CPU", "0")) / 100.0, 0.0), 1.0)
mem = int(os.environ.get("STANDIN_MEM", "0"))

# Non-zero fill so every page is actually resident.
ballast = b"\x01" * (mem * 1024 * 1024)

period = 0.1
while True:
    if cpu <= 0:
        time.sleep(3600)
        continue
    end = time.monotonic() + period * cpu
    while time.monotonic() < end:
        pass
    time.sleep(period * (1 - cpu))
