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
# Helpers available to ACTIONS (coordinates are window pixels; MODS are
# letters: C Ctrl, S Shift, A Alt):
#   shot NAME          save OUTDIR/NAME.png
#   wheel N [X Y [MODS]] N wheel steps (N<0 scrolls up) at X,Y (default: centre),
#                      30 ms apart (a flick: they accelerate)
#   drag X Y1 Y2       left-drag vertically from Y1 to Y2
#   dragto X1 Y1 X2 Y2 [STEPS]  left-drag in STEPS moves (default 10), 30 ms apart
#   click X Y [BTN [MODS]]  click (button 1 by default)
#   move X Y           move the pointer (hover)
#   keys KEY...        xdotool key names, e.g. `keys q` or `keys ctrl+f`
#   type TEXT          type literal text
#   pause SECONDS
#   expect NAME KEYS   save OUTDIR/NAME.png and compare it with the screen
#                      `temzdf --render-frame` draws for the same geometry after
#                      KEYS (Vim feedkeys notation, e.g. "3G" or
#                      "/needle<CR>"; the whole sequence since startup; "" for
#                      the first screen).
#   expect NAME        the same after all the input temzdf handled since
#                      startup, replayed with its timing (--render-frame
#                      --events of what temzdf recorded through
#                      TEMZDF_RECORD_INPUT, so the terminal's own delivery
#                      times and coordinates count): mouse actions too. Pause
#                      before it until animations end. Mouse actions are not
#                      judged inside multiplexers (they report cells).
#                      Both forms only for COMMAND = temzdf [--page N] [--scale
#                      S] FILE (possibly after `env VAR=...`) and terminals
#                      drawing from the window's top-left (kitty, WezTerm,
#                      Ghostty without decorations). Results go to
#                      OUTDIR/expect.txt, mismatches to NAME.diff.png; the
#                      recorded input to OUTDIR/events.txt.
#
# temzdf's log (geometry, warnings) goes to OUTDIR/state/temzdf/temzdf.log.
# It waits 3 s for startup (6 s for Ghostty and WezTerm, 3 s more in zellij) and
# sends `q` at the end. It does not check results; inspect the screenshots (or
# compare pixels) afterwards. The
# command runs without ZELLIJ/TMUX in its environment so a multiplexer around the
# harness does not leak into terminal detection, and with its own
# XDG_RUNTIME_DIR (also for ACTIONS), so its sessions stay apart from the
# user's and end with the run. If the terminal process died,
# it prints "TERMINAL CRASHED" and exits 3; failed `expect`s exit 4. Requires the dev shell (kitty,
# xvfb-run, xdotool, imagemagick; Ghostty from the host); re-executes inside it.
set -euo pipefail
# Absolute: it re-executes itself after the cd.
self=$(realpath "$0")
cd "$(dirname "$self")/.."

if [[ -z "${IN_NIX_SHELL:-}" ]]; then
  exec nix develop --quiet -c "$self" "$@" 2> >(grep -v "Git tree .* is dirty" >&2)
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
    *) sed -n '2,/^set -euo/{/^set -euo/!p}' "$self"; exit 2 ;;
  esac
done
shift $((OPTIND - 1))
if [[ $# -lt 3 || $2 != "--" ]]; then
  sed -n '2,/^set -euo/{/^set -euo/!p}' "$self"
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
  exec xvfb-run -a -s "-screen 0 ${width}x${height}x24" "$self" -t "$term" ${mux:+-m "$mux"} -s "$size" -o "$outdir" "$actions" -- "$@"
fi

# What the helpers send is recorded with its time (OUTDIR/events.txt, in
# `--render-frame --events` form, window pixels minus the terminal area's
# origin): for checking what was sent against what temzdf recorded
# (OUTDIR/input.txt), and for when the last input went out.
events=$outdir/events.txt
origin_x=0 origin_y=0
t0=0
# Microseconds now, without starting a process.
usec() { local t=$EPOCHREALTIME; echo "${t/./}"; }
# record [-at MS] WHAT...: at the time given (milliseconds since t0) or now.
record() {
  local ms
  if [[ $1 == -at ]]; then ms=$2; shift 2; else ms=$((($(usec) - t0) / 1000)); fi
  echo "$ms $*" >>"$events"
}
# Window pixels to the terminal's.
at() { echo "$(($1 - origin_x)) $(($2 - origin_y))"; }
# xdotool modifier key names for MODS letters.
mod_keys() {
  local m=${1:-} c
  for ((c = 0; c < ${#m}; c++)); do
    case ${m:c:1} in
      C) echo ctrl ;; S) echo shift ;; A) echo alt ;;
      *) echo "bad modifier ${m:c:1}" >&2; exit 2 ;;
    esac
  done
}
button_name() { case $1 in 1) echo left ;; 2) echo middle ;; 3) echo right ;; *) echo "bad button $1" >&2; exit 2 ;; esac; }

shot() { import -window root "$outdir/$1.png"; }
wheel() {
  local n=$1 x=${2:-$((width / 2))} y=${3:-$((height / 2))} mods=${4:-} btn=5 dir=down k
  if ((n < 0)); then btn=4 dir=up n=$((-n)); fi
  xdotool mousemove "$x" "$y"
  record move "$(at "$x" "$y")" $mods
  for k in $(mod_keys "$mods"); do xdotool keydown "$k"; done
  # One xdotool call: separate ones take about 0.1 s each, too slow for a
  # flick. The steps are recorded at the times xdotool spaces them.
  local first=$((($(usec) - t0) / 1000)) i
  xdotool click --repeat "$n" --delay 30 "$btn"
  for ((i = 0; i < n; i++)); do record -at $((first + 30 * i)) wheel $dir "$(at "$x" "$y")" $mods; done
  for k in $(mod_keys "$mods"); do xdotool keyup "$k"; done
}
dragto() {
  local x=$1 y=$2 x2=$3 y2=$4 steps=${5:-10} i px py
  xdotool mousemove "$x" "$y"
  record move "$(at "$x" "$y")"
  xdotool mousedown 1
  record press "$(at "$x" "$y")" left
  for ((i = 1; i <= steps; i++)); do
    px=$((x + (x2 - x) * i / steps)) py=$((y + (y2 - y) * i / steps))
    xdotool mousemove "$px" "$py"
    record drag "$(at "$px" "$py")" left
    sleep 0.03
  done
  xdotool mouseup 1
  record release "$(at "$x2" "$y2")" left
}
drag() { dragto "$1" "$2" "$1" "$3"; }
click() {
  local x=$1 y=$2 btn=${3:-1} mods=${4:-} k b
  b=$(button_name "$btn")
  xdotool mousemove "$x" "$y"
  record move "$(at "$x" "$y")" $mods
  for k in $(mod_keys "$mods"); do xdotool keydown "$k"; done
  xdotool click "$btn"
  record press "$(at "$x" "$y")" "$b" $mods
  record release "$(at "$x" "$y")" "$b" $mods
  for k in $(mod_keys "$mods"); do xdotool keyup "$k"; done
}
move() {
  xdotool mousemove "$1" "$2"
  record move "$(at "$1" "$2")"
}
# An xdotool key name in Vim's notation (the names actions use; others
# stop the run rather than record something else).
key_notation() {
  local k=$1 mods="" name
  while [[ $k == *+* ]]; do
    case ${k%%+*} in
      ctrl) mods+=C- ;; alt) mods+=M- ;; shift) mods+=S- ;;
      *) echo "key_notation: $1" >&2; exit 2 ;;
    esac
    k=${k#*+}
  done
  case $k in
    Return) name="<CR>" ;; Escape) name="<Esc>" ;; Tab) name="<Tab>" ;; BackSpace) name="<BS>" ;;
    space) name="<Space>" ;; Up | Down | Left | Right | Home | End) name="<$k>" ;;
    Page_Up | Prior) name="<PageUp>" ;; Page_Down | Next) name="<PageDown>" ;;
    plus) name="+" ;; minus) name="-" ;; equal) name="=" ;; slash) name="/" ;; question) name="?" ;;
    less) name="<lt>" ;; greater) name=">" ;; colon) name=":" ;; percent) name="%" ;;
    [a-z0-9]) name=$k ;;
    *) echo "key_notation: $1" >&2; exit 2 ;;
  esac
  # Shift with a letter is the capital (as the terminal reports it).
  if [[ $mods == S- && $name == [a-z] ]]; then mods="" name=${name^^}; fi
  if [[ -n $mods ]]; then
    [[ $name == "<"*">" ]] && name=${name:1:-1}
    name="<$mods$name>"
  fi
  echo "$name"
}
keys() {
  local k
  for k in "$@"; do
    xdotool key "$k"
    record key "$(key_notation "$k")"
  done
}
type() {
  local s=$1 i c out=""
  xdotool type -- "$s"
  for ((i = 0; i < ${#s}; i++)); do
    c=${s:i:1}
    case $c in
      "<") out+="<lt>" ;; " ") out+="<Space>" ;; $'\n') out+="<CR>" ;; *) out+=$c ;;
    esac
  done
  record key "$out"
}
pause() { sleep "$1"; }
expect() {
  local name=$1 keys=${2:-} input geo cols rows cw ch out
  if (($# >= 2)); then
    input=("--keys=$keys")
  else
    # Everything sent so far, then the moment of the screenshot.
    if [[ -n $mux ]] && grep -qvE '^[0-9]+ key ' "$events"; then
      echo "FAIL $name: mouse actions are not judged inside $mux" | tee -a "$outdir/expect.txt"
      return
    fi
    # temzdf's own record of what it handled (its clock, the terminal's
    # coordinates: terminals deliver late or offset), ended as long after
    # its last input as the screenshot came after the last one sent.
    if [[ ! -f $outdir/input.txt ]]; then
      echo "FAIL $name: temzdf recorded no input (TEMZDF_RECORD_INPUT)" | tee -a "$outdir/expect.txt"
      return
    fi
    local now sent last
    now=$((($(usec) - t0) / 1000))
    sent=$(tail -1 "$events" | cut -d' ' -f1)
    last=$(tail -1 "$outdir/input.txt" | cut -d' ' -f1)
    cp "$outdir/input.txt" "$outdir/$name.events.txt"
    echo "$((${last:-0} + now - ${sent:-0})) end" >>"$outdir/$name.events.txt"
    input=("--events=$outdir/$name.events.txt")
  fi
  shot "$name"
  geo=$(grep -oE 'geometry [0-9]+x[0-9]+ cells of [0-9]+x[0-9]+ px' "$outdir/state/temzdf/temzdf.log" | tail -1) || {
    echo "FAIL $name: no geometry in temzdf.log" | tee -a "$outdir/expect.txt"
    return
  }
  read -r cols rows cw ch <<<"$(sed -E 's/geometry ([0-9]+)x([0-9]+) cells of ([0-9]+)x([0-9]+) px/\1 \2 \3 \4/' <<<"$geo")"
  # --keys=...: key lists may start with "-" (zoom out).
  "${expect_cmd[@]}" --render-frame "$outdir/$name.expected.png" --size "${cols}x${rows}" \
    --cell "${cw}x${ch}" "${input[@]}" "$expect_file" 2>"$outdir/$name.expected.log" || {
    echo "FAIL $name: --render-frame failed (see $name.expected.log)" | tee -a "$outdir/expect.txt"
    return
  }
  # The terminal may have spare pixels right and below the cell grid.
  magick "$outdir/$name.png" -crop "$((cols * cw))x$((rows * ch))+$origin_x+$origin_y" +repage "$outdir/$name.actual.png"
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
# Sessions get a private runtime directory (short: socket paths are limited),
# exported so the actions' own `temzdf action`/`kill-session` find them.
# Removing it on exit ends any session left behind (servers end once their
# socket is gone), also when an action failed.
XDG_RUNTIME_DIR=$(mktemp -d "${TMPDIR:-/tmp}/temzdf-e2e.XXXXXX")
export XDG_RUNTIME_DIR
term_pid=
cleanup() {
  if [[ -n $term_pid ]]; then
    kill "$term_pid" 2>/dev/null || true
    wait "$term_pid" 2>/dev/null || true
  fi
  if [[ $mux == tmux ]]; then
    # The server outlives the terminal; kill-server leaves its socket file.
    local sock
    sock=$(tmux -L "e2e-$$" display-message -p '#{socket_path}' 2>/dev/null || true)
    tmux -L "e2e-$$" kill-server 2>/dev/null || true
    [[ -n $sock ]] && rm -f -- "$sock"
  fi
  rm -rf -- "$XDG_RUNTIME_DIR"
}
trap cleanup EXIT
clean_env=(env -u TEMZDF_LOG_DIR -u ZELLIJ -u ZELLIJ_SESSION_NAME -u ZELLIJ_PANE_ID -u TMUX -u TMUX_PANE "XDG_STATE_HOME=$outdir/state"
  "TEMZDF_RECORD_INPUT=$outdir/input.txt")
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
# The recorded times start here (the first screen is up).
: >"$events"
t0=$(usec)
# shellcheck source=/dev/null
source "$actions"
crashed=0
kill -0 "$term_pid" 2>/dev/null || crashed=1
xdotool key q || true
sleep 0.5
cleanup
trap - EXIT
echo "screenshots in $outdir"
if ((crashed)); then
  echo "TERMINAL CRASHED (see $outdir/terminal.log)"
  exit 3
fi
if [[ -f $outdir/expect.txt ]] && grep -q '^FAIL' "$outdir/expect.txt"; then
  echo "EXPECTATIONS FAILED (see $outdir/expect.txt)"
  exit 4
fi
