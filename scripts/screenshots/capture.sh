#!/usr/bin/env bash
# Regenerate docs/img/tui.png and docs/img/once.png from a synthetic scene.
# Procedure and safety rules: docs/development.md, "Screenshots".
#
# Needs bwrap, tmux, git, python3 and google-chrome (headless; it fetches the
# JetBrains Mono and Noto Color Emoji web fonts, so it needs network access).
#
# Env: CREW_WATCH_BIN (an explicit binary, used as-is; unset, the script runs
#      `cargo build --release` first, a no-op when up to date, and uses the
#      binary cargo reports building, wherever CARGO_TARGET_DIR or
#      build.target-dir put it, so a capture never shows a stale build),
#      OUT_DIR (default docs/img), STAGGER (seconds between launch waves).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd -P)
repo=$(cd "$here/../.." && pwd -P)
bin=${CREW_WATCH_BIN:-}
out=${OUT_DIR:-$repo/docs/img}
work=$(mktemp -d "${TMPDIR:-/tmp}/crew-watch-shots.XXXXXX")
trap 'rm -rf "$work"' EXIT

if [ -z "$bin" ]; then
  # Ask cargo where the binary went: a target-dir override sends it outside
  # target/, where a stale copy may still sit.
  bin=$(cd "$repo" && cargo build --release --bin crew-watch \
      --message-format=json-render-diagnostics |
    python3 -Ic 'import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m["target"]["name"] == "crew-watch" and m.get("executable"):
        print(m["executable"])')
  [ -n "$bin" ] || { echo "capture.sh: cargo reported no crew-watch binary" >&2; exit 1; }
fi
# Fail now, not after the ~3-minute scene, on a bad CREW_WATCH_BIN.
[ -f "$bin" ] && [ -x "$bin" ] || { echo "capture.sh: $bin is not an executable file" >&2; exit 1; }
# Absolute, since scene.sh runs it from other directories.
bin=$(cd "$(dirname "$bin")" && pwd -P)/$(basename "$bin")
mkdir -p "$out"

# A private PID namespace: crew-watch sees only the stand-ins, never a real
# process, and everything started in it dies with it.
bwrap --dev-bind / / --unshare-pid --proc /proc --die-with-parent \
  -- bash "$here/scene.sh" "$bin" "$work"

# Geometry must match CELL_W/CELL_H/PAD in ansi2html.py.
cell_w=9 cell_h=19 pad=16
for shot in tui:150:24 once:150:16; do
  IFS=: read -r name cols rows <<<"$shot"
  python3 -I "$here/ansi2html.py" "$work/$name.ansi" >"$work/$name.html"
  google-chrome --headless=new --disable-gpu --hide-scrollbars \
    --user-data-dir="$work/chrome" --force-device-scale-factor=2 \
    --window-size=$((cols * cell_w + 2 * pad)),$((rows * cell_h + 2 * pad)) \
    --virtual-time-budget=10000 \
    --screenshot="$out/$name.png" "file://$work/$name.html" 2>/dev/null
  echo "wrote $out/$name.png"
done
