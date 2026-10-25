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
#   move X Y           move the pointer (hover)
#   keys KEY...        xdotool key names, e.g. `keys q` or `keys ctrl+f`
#   type TEXT          type literal text
#   pause SECONDS
#   expect NAME [KEYS] save OUTDIR/NAME.png and compare it with the screen
#                      `temzdf --render-frame` draws for the same geometry after
#                      KEYS (headless notation, e.g. "3 G" or "/ n e e d l e
#                      Enter"; the whole sequence since startup). Only for
#                      COMMAND = temzdf [--page N] [--scale S] FILE (possibly
#                      after `env VAR=...`), key-driven scenarios (mouse actions
#                      have no headless equivalent) and terminals drawing from
#                      the window's top-left (kitty, WezTerm). Results go to
#                      OUTDIR/expect.txt, mismatches to NAME.diff.png.
#
# temzdf's log (geometry, warnings) goes to OUTDIR/state/temzdf/temzdf.log.
# It waits 3 s for startup (6 s for Ghostty and WezTerm, 3 s more in zellij) and
# sends `q` at the end. It does not check results; inspect the screenshots (or
# compare pixels) afterwards. The
# command runs without ZELLIJ/TMUX in its environment so a multiplexer around the
# harness does not leak into terminal detection. If the terminal process died,
# it prints "TERMINAL CRASHED" and exits 3; failed `expect`s exit 4. Requires the dev shell (kitty,
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
    *) sed -n '2,/^set -euo/{/^set -euo/!p}' "$0"; exit 2 ;;
  esac
done
shift $((OPTIND - 1))
if [[ $# -lt 3 || $2 != "--" ]]; then
  sed -n '2,/^set -euo/{/^set -euo/!p}' "$0"
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
move() { xdotool mousemove "$1" "$2"; }
keys() { xdotool key "$@"; }
type() { xdotool type -- "$1"; }
pause() { sleep "$1"; }
expect() {
  local name=$1 keys=${2:-} geo cols rows cw ch out
  shot "$name"
  geo=$(grep -oE 'geometry [0-9]+x[0-9]+ cells of [0-9]+x[0-9]+ px' "$outdir/state/temzdf/temzdf.log" | tail -1) || {
    echo "FAIL $name: no geometry in temzdf.log" | tee -a "$outdir/expect.txt"
    return
  }
  read -r cols rows cw ch <<<"$(sed -E 's/geometry ([0-9]+)x([0-9]+) cells of ([0-9]+)x([0-9]+) px/\1 \2 \3 \4/' <<<"$geo")"
  # --keys=...: key lists may start with "-" (zoom out).
  "${expect_cmd[@]}" --render-frame "$outdir/$name.expected.png" --size "${cols}x${rows}" \
    --cell "${cw}x${ch}" "--keys=$keys" "$expect_file" 2>"$outdir/$name.expected.log" || {
    echo "FAIL $name: --render-frame failed (see $name.expected.log)" | tee -a "$outdir/expect.txt"
    return
  }
  # The terminal may have spare pixels right and below the cell grid.
  magick "$outdir/$name.png" -crop "$((cols * cw))x$((rows * ch))+0+0" +repage "$outdir/$name.actual.png"
  out=$(magick compare -metric AE -fuzz 3% "$outdir/$name.actual.png" "$outdir/$name.expected.png" \
    "$outdir/$name.diff.png" 2>&1 || true)
  out=${out%% *}
  if [[ $out == 0 ]]; then
    rm -f "$outdir/$name.diff.png"
    echo "PASS $name" | tee -a "$outdir/expect.txt"
  else
    echo "FAIL $name: $out pixels differ (see $name.diff.png)" | tee -a "$outdir/expect.txt"
  fi
}

# Some terminals (WezTerm) look the program up only in PATH: make it absolute.
if [[ "$1" == */* && -e "$1" ]]; then
  set -- "$(realpath "$1")" "${@:2}"
fi
# For `expect`: the headless renderer with the app's document and view options.
expect_cmd=() expect_file=
args=("$@")
while [[ ${#args[@]} -gt 0 && ( ${args[0]} == env || ${args[0]} == *=* ) ]]; do args=("${args[@]:1}"); done
if ((${#args[@]} > 0)); then
  expect_cmd=("${args[0]}")
  expect_file=${args[-1]}
  for ((i = 1; i < ${#args[@]} - 1; i++)); do
    case ${args[i]} in
      --page | --scale) expect_cmd+=("${args[i]}" "${args[i + 1]}") ;;
    esac
  done
  # kitty blends translucent images itself, in linear light. Inside a
  # multiplexer temzdf composites frames (sRGB) and kitty only shows them.
  if [[ $term == kitty && -z $mux ]]; then expect_cmd+=(--linear-blending); fi
fi
mkdir -p "$outdir/state"
rm -f "$outdir/state/temzdf/temzdf.log" "$outdir/expect.txt"
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
clean_env=(env -u TEMZDF_LOG_DIR -u ZELLIJ -u ZELLIJ_SESSION_NAME -u ZELLIJ_PANE_ID -u TMUX -u TMUX_PANE "XDG_STATE_HOME=$outdir/state")
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
    # WezTerm sizes windows in cells: 10x22 px at its defaults (more rows would
    # run past the bottom of the virtual screen).
    "${clean_env[@]}" wezterm -n --config 'front_end="Software"' --config enable_wayland=false \
      --config 'window_decorations="NONE"' --config enable_tab_bar=false \
      --config 'window_padding={left=0,right=0,top=0,bottom=0}' \
      --config "initial_cols=$((width / 10))" --config "initial_rows=$((height / 22))" \
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
if [[ -f $outdir/expect.txt ]] && grep -q '^FAIL' "$outdir/expect.txt"; then
  echo "EXPECTATIONS FAILED (see $outdir/expect.txt)"
  exit 4
fi
