#!/usr/bin/env bash
# End-to-end harness: run a command inside a real terminal (kitty, Ghostty or
# WezTerm) on a virtual X server, drive it with an actions script, and save
# screenshots.
#
# Usage: scripts/e2e.sh [-t kitty|ghostty|wezterm] [-m zellij|tmux] [-s WxH] [-o OUTDIR] ACTIONS -- COMMAND [ARGS...]
#   -t TERM   terminal to run (default kitty)
#   -m MUX    run COMMAND inside a multiplexer in that terminal (zellij: no pane
#             frames, a single pane; its config and layout go to OUTDIR/zellij;
#             tmux: a private server with passthrough and mouse on, no status)
#   -s WxH    window/screen size in pixels (default 900x600)
#   -o OUTDIR where screenshots and terminal.log go (default: target/e2e)
#   ACTIONS   bash file sourced after the terminal started; it can use the helpers below.
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
# It waits 3 s for startup (6 s for Ghostty and WezTerm, 3 s more in zellij) and
# sends `q` at the end. It does not check results; inspect the screenshots (or
# compare pixels) afterwards. The
# command runs without ZELLIJ/TMUX in its environment so a multiplexer around the
# harness does not leak into terminal detection. If the terminal process died,
# it prints "TERMINAL CRASHED" and exits 3. Requires the dev shell (kitty,
# xvfb-run, xdotool, imagemagick; Ghostty from the host); re-executes inside it.
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ -z "${IN_NIX_SHELL:-}" ]]; then
  exec nix develop --quiet -c "$0" "$@" 2> >(grep -v "Git tree .* is dirty" >&2)
fi

size=900x600
term=kitty
mux=
outdir=target/e2e
while getopts "t:m:s:o:h" opt; do
  case $opt in
    t) term=$OPTARG ;;
    m) mux=$OPTARG ;;
    s) size=$OPTARG ;;
    o) outdir=$OPTARG ;;
    *) sed -n '2,32p' "$0"; exit 2 ;;
  esac
done
shift $((OPTIND - 1))
if [[ $# -lt 3 || $2 != "--" ]]; then
  sed -n '2,32p' "$0"
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
  exec xvfb-run -a -s "-screen 0 ${width}x${height}x24" "$0" -t "$term" ${mux:+-m "$mux"} -s "$size" -o "$outdir" "$actions" -- "$@"
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

# Some terminals (WezTerm) look the program up only in PATH: make it absolute.
if [[ "$1" == */* && -e "$1" ]]; then
  set -- "$(realpath "$1")" "${@:2}"
fi
mux_startup=0
case $mux in
  "") ;;
  zellij)
    # A single pane running COMMAND, no frames or tips; quit with the app.
    kdl() { local v=${1//\\/\\\\}; printf '"%s"' "${v//\"/\\\"}"; }
    zdir=$outdir/zellij
    mkdir -p "$zdir"
    printf 'pane_frames false\nshow_startup_tips false\nshow_release_notes false\n' >"$zdir/config.kdl"
    {
      printf 'layout {\n    pane command=%s cwd=%s close_on_exit=true {\n' "$(kdl "$1")" "$(kdl "$PWD")"
      if (($# > 1)); then
        printf '        args'
        for a in "${@:2}"; do printf ' %s' "$(kdl "$a")"; done
        printf '\n'
      fi
      printf '    }\n}\n'
    } >"$zdir/layout.kdl"
    set -- zellij --config "$zdir/config.kdl" --layout "$zdir/layout.kdl"
    mux_startup=3
    ;;
  tmux)
    # A private server, no status line, passthrough and mouse on.
    tdir=$outdir/tmux
    mkdir -p "$tdir"
    printf 'set -g allow-passthrough on\nset -g mouse on\nset -g status off\nset -g escape-time 0\n' >"$tdir/tmux.conf"
    cmd=$(printf '%q ' "$@")
    set -- tmux -f "$tdir/tmux.conf" -L "e2e-$$" new-session -c "$PWD" "$cmd"
    mux_startup=1
    ;;
  *)
    echo "unknown multiplexer: $mux" >&2
    exit 2
    ;;
esac
clean_env=(env -u ZELLIJ -u ZELLIJ_SESSION_NAME -u ZELLIJ_PANE_ID -u TMUX -u TMUX_PANE)
case $term in
  kitty)
    "${clean_env[@]}" kitty --config NONE -o font_size=12 -o "initial_window_width=$width" \
      -o "initial_window_height=$height" -o window_padding_width=0 -o hide_window_decorations=yes \
      -o remember_window_size=no "$@" >"$outdir/terminal.log" 2>&1 &
    startup=3
    ;;
  ghostty)
    # Ghostty sizes windows in cells; 10x21 px cells at its default font size.
    "${clean_env[@]}" ghostty --config-default-files=false --gtk-single-instance=false \
      "--window-width=$((width / 10))" "--window-height=$((height / 21))" -e "$@" >"$outdir/terminal.log" 2>&1 &
    startup=6
    ;;
  wezterm)
    # -n: ignore the user's wezterm config. The software renderer works on Xvfb.
    # WezTerm sizes windows in cells; about 10x20 px cells at its defaults.
    "${clean_env[@]}" wezterm -n --config 'front_end="Software"' --config enable_wayland=false \
      --config 'window_decorations="NONE"' --config enable_tab_bar=false \
      --config 'window_padding={left=0,right=0,top=0,bottom=0}' \
      --config "initial_cols=$((width / 10))" --config "initial_rows=$((height / 20))" \
      start --always-new-process --cwd "$PWD" -- "$@" >"$outdir/terminal.log" 2>&1 &
    startup=6
    ;;
  *)
    echo "unknown terminal: $term" >&2
    exit 2
    ;;
esac
term_pid=$!
sleep "$((startup + mux_startup))"
# Without a window manager WezTerm only gets mouse events once focused.
if [[ $term == wezterm ]]; then
  xdotool search --class wezterm windowfocus 2>/dev/null || true
  sleep 0.3
fi
# shellcheck source=/dev/null
source "$actions"
crashed=0
kill -0 "$term_pid" 2>/dev/null || crashed=1
xdotool key q || true
sleep 0.5
kill "$term_pid" 2>/dev/null || true
wait "$term_pid" 2>/dev/null || true
echo "screenshots in $outdir"
if ((crashed)); then
  echo "TERMINAL CRASHED (see $outdir/terminal.log)"
  exit 3
fi
