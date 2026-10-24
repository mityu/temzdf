#!/usr/bin/env bash
# End-to-end harness: run a command inside a real kitty on a virtual X server,
# drive it with an actions script, and save screenshots.
#
# Usage: scripts/e2e-kitty.sh [-s WxH] [-o OUTDIR] ACTIONS -- COMMAND [ARGS...]
#   -s WxH    window/screen size in pixels (default 900x600)
#   -o OUTDIR where screenshots and kitty.log go (default: target/e2e)
#   ACTIONS   bash file sourced after kitty started; it can use the helpers below.
#
# Helpers available to ACTIONS (coordinates are window pixels):
#   shot NAME          save OUTDIR/NAME.png
#   wheel N [X Y]      N wheel steps (N<0 scrolls up) at X,Y (default: centre)
#   drag X Y1 Y2       left-drag vertically from Y1 to Y2
#   click X Y [BTN]    click (button 1 by default)
#   keys KEY...        xdotool key names, e.g. `keys q` or `keys ctrl+f`
#   type TEXT          type literal text
#   pause SECONDS
#
# It waits 3 s for startup and sends `q` at the end. It does not check results;
# inspect the screenshots (or compare pixels) afterwards. Requires the dev shell
# (kitty, xvfb-run, xdotool, imagemagick); re-executes inside it when needed.
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ -z "${IN_NIX_SHELL:-}" ]]; then
  exec nix develop --quiet -c "$0" "$@" 2> >(grep -v "Git tree .* is dirty" >&2)
fi

size=900x600
outdir=target/e2e
while getopts "s:o:h" opt; do
  case $opt in
    s) size=$OPTARG ;;
    o) outdir=$OPTARG ;;
    *) sed -n '2,25p' "$0"; exit 2 ;;
  esac
done
shift $((OPTIND - 1))
if [[ $# -lt 3 || $2 != "--" ]]; then
  sed -n '2,25p' "$0"
  exit 2
fi
actions=$(realpath "$1")
shift 2
mkdir -p "$outdir"
outdir=$(realpath "$outdir")
width=${size%x*}
height=${size#*x}

if [[ -z "${E2E_INNER:-}" ]]; then
  export E2E_INNER=1
  exec xvfb-run -a -s "-screen 0 ${width}x${height}x24" "$0" -s "$size" -o "$outdir" "$actions" -- "$@"
fi

shot() { import -window root "$outdir/$1.png"; }
wheel() {
  local n=$1 x=${2:-$((width / 2))} y=${3:-$((height / 2))} btn=5
  if ((n < 0)); then btn=4; n=$((-n)); fi
  xdotool mousemove "$x" "$y"
  for _ in $(seq "$n"); do xdotool click "$btn"; sleep 0.03; done
}
drag() {
  local x=$1 y=$2 to=$3 step
  step=$(((to - y) / 10))
  ((step == 0)) && step=1
  xdotool mousemove "$x" "$y" mousedown 1
  while (((step > 0 && y < to) || (step < 0 && y > to))); do
    y=$((y + step))
    if (((step > 0 && y > to) || (step < 0 && y < to))); then y=$to; fi
    xdotool mousemove "$x" "$y"
    sleep 0.03
  done
  xdotool mouseup 1
}
click() { xdotool mousemove "$1" "$2" click "${3:-1}"; }
keys() { xdotool key "$@"; }
type() { xdotool type -- "$1"; }
pause() { sleep "$1"; }

kitty --config NONE -o font_size=12 -o "initial_window_width=$width" -o "initial_window_height=$height" \
  -o window_padding_width=0 -o hide_window_decorations=yes -o remember_window_size=no \
  "$@" >"$outdir/kitty.log" 2>&1 &
kitty_pid=$!
sleep 3
# shellcheck source=/dev/null
source "$actions"
xdotool key q || true
sleep 0.5
kill "$kitty_pid" 2>/dev/null || true
wait "$kitty_pid" 2>/dev/null || true
echo "screenshots in $outdir"
