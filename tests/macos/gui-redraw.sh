#!/usr/bin/env bash
# The GUI client keeps drawing after its window was covered (macOS only, see
# "GUI" in doc/macos.md): wgpu hands out no surface texture while the window
# is covered, and the frames that came meanwhile must be drawn once it shows
# again. Uncovering alone, without a resize, used to leave it stale for good.
#
# Usage: tests/macos/gui-redraw.sh [GUI_BINARY]   (default target/debug/temzdf-gui)
#
# Needs `cargo build -p temzdf -p temzdf-gui`, kitty, and what
# scripts/macos-gui.sh needs. A session shows a pane printing a counter,
# attached from the GUI and from temzdf in kitty (two clients, as when the
# problem was found); kitty's window covers the GUI's and moves away again,
# and two screenshots of the GUI a while apart must differ. Exit status 0
# when they do, 1 when the window stayed stale, 2 on a setup problem.
set -euo pipefail
cd "$(dirname "$0")/../.."

gui=${1:-target/debug/temzdf-gui}
[[ -x $gui && -x target/debug/temzdf ]] || { echo "build temzdf and temzdf-gui first" >&2; exit 2; }
g=scripts/macos-gui.sh
ctl=target/macos-gui/termctl
out=target/macos-gui/gui-redraw
mkdir -p "$out"
session=gui-redraw-$$
# temzdf-gui starts the session with the temzdf found next to it or on PATH.
export PATH="$PWD/target/debug:$PATH"

# A virtual display started here is stopped at the end.
own_display=yes
[[ -f target/macos-gui/vdisp.pid ]] && kill -0 "$(<target/macos-gui/vdisp.pid)" 2>/dev/null && own_display=
gpid= kpid=
cleanup() {
  if [[ -n $gpid ]]; then kill "$gpid" 2>/dev/null || true; fi
  if [[ -n $kpid ]]; then $g close "$kpid" || true; fi
  target/debug/temzdf kill-session "$session" >/dev/null 2>&1 || true
  if [[ -n $own_display ]]; then $g display stop; fi
}
trap cleanup EXIT
read -r _ dx dy _ _ < <($g display start) || true
[[ -n ${dy:-} ]] || { echo "no virtual display" >&2; exit 2; }

# The GUI starts the session (a shell); a second pane counts.
read -r gpid _ < <($g launch app -- "$gui" -s "$session")
target/debug/temzdf action -s "$session" new-pane -- \
  sh -c 'i=0; while :; do i=$((i+1)); echo "tick $i"; sleep 0.3; done'
read -r kpid _ < <($g launch kitty -- target/debug/temzdf attach "$session")

# Side by side: the GUI on the left, kitty to its right.
$ctl move "$gpid" $((dx + 20)) $((dy + 20)) 700 450
$ctl move "$kpid" $((dx + 760)) $((dy + 20)) 820 560
sleep 2

# Two screenshots of the GUI 1.5 s apart: "updating" or "stale".
updating() {
  $g shot "$gpid" "$out/$1-a.png"
  sleep 1.5
  $g shot "$gpid" "$out/$1-b.png"
  cmp -s "$out/$1-a.png" "$out/$1-b.png" && echo stale || echo updating
}

before=$(updating before)
[[ $before == updating ]] || { echo "the GUI did not update even before it was covered" >&2; exit 2; }
$ctl move "$kpid" $((dx + 10)) $((dy + 10))
sleep 2
$ctl move "$kpid" $((dx + 760)) $((dy + 20))
sleep 2
after=$(updating after)
echo "after uncovering: $after (screenshots in $out)"
[[ $after == updating ]]
