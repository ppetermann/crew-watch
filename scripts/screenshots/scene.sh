#!/usr/bin/env bash
# Synthetic scene for the README screenshots. Run ONLY by capture.sh, inside
# its private PID namespace: builds a scratch firstmate home, starts stand-in
# agent processes, runs crew-watch on a dedicated tmux socket and writes the
# pane captures (with escape sequences) to <work>/tui.ansi and <work>/once.ansi.
#
# Usage: scene.sh <crew-watch binary> <work dir>
set -euo pipefail

bin=$1
work=$2
here=$(cd "$(dirname "$0")" && pwd -P)
# Seconds between the three launch waves (and before the capture), so ELAPSED
# differs across rows and reads as minutes.
stagger=${STAGGER:-60}

fm=$work/fm
mkdir -p "$fm/state" "$fm/data" "$work/bin" "$work/wt" "$work/home" "$work/config/crew-watch"
export HOME=$work/home XDG_CONFIG_HOME=$work/config
echo "quota_providers=claude,zai" >"$XDG_CONFIG_HOME/crew-watch/config"
cp "$here/quota-axi" "$work/bin/quota-axi"
chmod +x "$work/bin/quota-axi"
export PATH=$work/bin:$PATH
for runtime in claude opencode codex kimi grok; do
  ln -s "$here/standin.py" "$work/bin/$runtime"
done

# wave|task id|runtime|model|cpu%|MiB|status verb|turn state|project|title
scene='
0|cw-release|opencode|glm-5.3|82|880|working|busy|crew-watch|prepare the public release: README, screenshots, packaging
2|api-cursor|claude|opus-5|58|640|working|busy|web-api|add cursor pagination to the /orders endpoint
1|infra-pin|codex|gpt-5-codex|28|260|working|-|infra|pin every terraform provider to an exact version
1|docs-start|claude|sonnet-5|3|410|working|idle|docs-site|rewrite the getting-started page around the new CLI
0|cw-glyphs|claude|haiku-4.5|0|150|done|-|crew-watch|document the STATE column glyphs in the README
1|pay-refund|opencode|glm-5.3|0|320|needs-decision|-|payments-svc|partial refunds: which ledger entry wins?
0|etl-schema|kimi|kimi-k2|0|180|blocked|-|etl-jobs|nightly import fails on the new orders schema
0|cw-tag|grok|grok-5|0|120|paused|-|crew-watch|cut the 0.2 tag once the release PR lands
'

{
  echo "# Backlog"
  while IFS='|' read -r _ id _ _ _ _ _ _ _ title; do
    if [ -n "$id" ]; then echo "- [ ] $id - $title (repo: x)"; fi
  done <<<"$scene"
} >"$fm/data/backlog.md"

while IFS='|' read -r _ id runtime _ _ _ verb turn project _; do
  [ -n "$id" ] || continue
  mkdir -p "$work/wt/$id"
  printf 'endpoint_task_id=%s\nworktree=%s\nproject=/src/%s\nharness=%s\n' \
    "$id" "$work/wt/$id" "$project" "$runtime" >"$fm/state/$id.meta"
  echo "$verb [at=1]: synthetic" >"$fm/state/$id.status"
  if [ "$turn" != - ]; then
    echo g1 >"$fm/state/$id.busy-gen"
    echo "v1 gen=g1 seq=1 state=$turn source=standin event=x ts=1" >"$fm/state/$id.busy-state"
  fi
done <<<"$scene"

# An interactive session: no fleet record, cwd in a git checkout.
mkdir -p "$work/repos/crew-watch"
git -C "$work/repos/crew-watch" init -q

launch() { # cwd runtime model cpu mib
  local args=()
  if [ "$3" != - ]; then args=(--model "$3"); fi
  (cd "$1" && STANDIN_CPU=$4 STANDIN_MEM=$5 exec python3 -I "$work/bin/$2" "${args[@]}") &
}
for wave in 0 1 2; do
  while IFS='|' read -r w id runtime model cpu mib _; do
    if [ -n "$id" ] && [ "$w" = "$wave" ]; then
      launch "$work/wt/$id" "$runtime" "$model" "$cpu" "$mib"
    fi
  done <<<"$scene"
  if [ "$wave" = 2 ]; then
    launch "$work/repos/crew-watch" claude - 6 220
  fi
  sleep "$stagger"
done

# A tmux server of our own on a socket inside the work dir (not -L: the
# default socket dir plus a long TMPDIR can overflow the socket path limit).
tmux() { command tmux -S "$work/tmux.sock" -f /dev/null "$@"; }
trap 'tmux kill-server 2>/dev/null || true' EXIT

# Capture <session> to <file> once its pane shows <pattern> (up to 60s), so
# a slow host never yields a half-drawn frame.
capture() { # session file pattern
  for _ in $(seq 60); do
    sleep 1
    tmux capture-pane -e -p -t "$1" >"$2"
    if grep -q -- "$3" "$2"; then return 0; fi
  done
  echo "scene.sh: $1 never showed '$3'" >&2
  return 1
}

tmux new-session -d -s tui -x 150 -y 24 "$bin --fm-home $fm"
# The quota row arrives from a background poll; wait for it plus one more
# tick so CPU% is measured over a full interval.
capture tui "$work/tui.ansi" 'zai'
sleep 2
tmux capture-pane -e -p -t tui >"$work/tui.ansi"

cat >"$work/once.sh" <<EOF
printf '\$ crew-watch --once\n'
$bin --once --fm-home $fm
printf '\$ '
exec sleep 600
EOF
tmux new-session -d -s once -x 150 -y 16 "sh $work/once.sh"
capture once "$work/once.ansi" 'quota zai'
sleep 1
tmux capture-pane -e -p -t once >"$work/once.ansi"
