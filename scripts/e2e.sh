#!/usr/bin/env bash
# End-to-end harness: run a command inside a real terminal (kitty, Ghostty or
# WezTerm) on a virtual X server (or headless sway, -d wayland; a virtual
# display on macOS, -d macos), drive it with an actions script, and save
# screenshots.
#
# Usage: scripts/e2e.sh [-t kitty|ghostty|wezterm|gui] [-d x11|wayland|macos] [-m zellij|tmux] [-I] [-P shared|socket] [-s WxH] [-o OUTDIR] ACTIONS -- COMMAND [ARGS...]
#   -t TERM   terminal to run (default kitty); gui: COMMAND is temzdf-gui
#             itself (e.g. target/debug/temzdf-gui FILE), run under the xfwm4
#             window manager (X11; sway on Wayland) on lavapipe
#             ($TEMZDF_LAVAPIPE_ICD); on X11 the screen is 100 px larger each
#             way for its decorations
#   -I        (gui) Japanese input: fcitx5 with Mozc (its HOME in OUTDIR,
#             ended with the run), over XIM on X11 (XMODIFIERS), over
#             input-method-v2 and text-input-v3 on Wayland; `ime on` /
#             `ime off` switch it
#   -d DISP   display server: x11 (default; Xvfb, xdotool, import) or wayland
#             (headless sway; keys and pointer through crates/wlinput, built
#             here; screenshots with grim); macos (the default
#             there): the terminal or the GUI on a virtual display at scale 1
#             beside the real screen (in a VM, scripts/macos-vm.sh: on its own
#             screen, at scale 1), keys and the pointer through `termctl
#             serve` (scripts/macos/termctl.swift; the window is in front for
#             the run, as it has the focus on Linux, and the end gives the
#             front and the pointer back),
#             screenshots with screencapture; needs the accessibility and
#             screen recording permissions, and the screen unlocked
#             (doc/macos.md)
#   -m MUX    run COMMAND inside a multiplexer in that terminal (zellij: no pane
#             frames, a single pane; its config and layout go to OUTDIR/zellij;
#             tmux: a private server with passthrough and mouse on, no status)
#   -P PATH   how temzdf's clients take frames' pixels from the session:
#             shared (memory, the default) or socket (inside the messages);
#             sets TEMZDF_PIXELS for COMMAND
#   -s WxH    window/screen size in pixels (default 900x600)
#   -o OUTDIR where screenshots and terminal.log go (default: target/e2e)
#   ACTIONS   bash file sourced after the terminal started; it can use the helpers below.
#
# Helpers available to ACTIONS (coordinates are pixels of the terminal area,
# or of the GUI's drawing area; MODS are letters: C Ctrl, S Shift, A Alt):
#   shot NAME          save OUTDIR/NAME.png
#   wheel N [X Y [MODS]] N wheel steps (N<0 scrolls up) at X,Y (default: centre),
#                      30 ms apart (a flick: they accelerate)
#   drag X Y1 Y2       left-drag vertically from Y1 to Y2
#   dragto X1 Y1 X2 Y2 [STEPS]  left-drag in STEPS moves (default 10), 30 ms apart
#   click X Y [BTN [MODS]]  click (button 1 by default)
#   down X Y [BTN [MODS]], up  press a button there (and MODS), release it;
#                      `move` in between drags
#   move X Y           move the pointer (hover, or drag while `down`)
#   keys KEY...        XKB key names (as xdotool takes them), e.g. `keys q`
#                      or `keys ctrl+f`
#   type TEXT [MS]     type literal text (MS between keys)
#   hold KEY..., release KEY...  keys held across other actions (not recorded)
#   clipboard, set_clipboard TEXT  read or write the clipboard
#   other_window       open a plain window of another program, print its PID
#   focus_window PID   give PID's window the focus ($term_pid: the terminal's)
#   pause SECONDS
#   wait_complete [S]  until temzdf in the terminal has written a complete
#                      frame (every visible page rendered) since it last
#                      showed one that was not, at most S seconds (default
#                      10): for judging a screen whose tiles come slowly
#                      (inline transfers); nothing for the GUI
#   ime on|off         (-I) switch the input method
#   expect NAME KEYS   save OUTDIR/NAME.png and compare it with the screen
#                      `temzdf standalone pdf --render-frame` draws for the
#                      same geometry after
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
#                      Both forms only for COMMAND = temzdf standalone pdf
#                      [--page N] [--scale S] FILE (possibly after
#                      `env VAR=...`) and terminals
#                      drawing from the window's top-left (kitty, WezTerm,
#                      Ghostty without decorations). Results go to
#                      OUTDIR/expect.txt, mismatches to NAME.diff.png; the
#                      recorded input to OUTDIR/events.txt.
#                      With -t gui, `expect NAME` compares the window with
#                      `temzdf-gui --render-frame` attached to the same
#                      session at that moment (the window's drawing, not the
#                      session's input handling).
#                      When COMMAND is not `temzdf standalone pdf` (a
#                      session: `temzdf --clean -s NAME ...`, maybe in `sh
#                      -c`), `expect NAME` compares the screen with `temzdf
#                      render-session` attached to the session at that moment
#                      (the session's only one, or $E2E_SESSION): the images
#                      by pixels outside the cells the terminal draws itself
#                      (NAME.mask.png), and in kitty and WezTerm the text of
#                      the screen (`kitten @ get-text`, `wezterm cli
#                      get-text`) with the frame's
#                      (NAME.expected.txt; NAME.text.diff on a mismatch).
#                      Not inside multiplexers.
#   draw_lines NAME LINE...  type `clear; printf` of the LINEs into the shell
#                      and save OUTDIR/NAME.png: this terminal's own drawing
#                      of them (rows 0, 1, ...), a reference for screen_restored
#                      (it clears the screen: at the end of a scenario)
#   screen_restored SHOT REF K [BEFORE BK]  whether screenshot SHOT, taken
#                      after leaving temzdf, shows REF's row K (the message),
#                      nothing below it but the prompt, and above it what
#                      BEFORE showed up to its row like REF's row BK, or
#                      nothing without BEFORE (temzdf was the first command):
#                      the screen came back and temzdf left nothing behind;
#                      rows compared as images, so in Ghostty too
#   check NAME CMD...  run CMD: PASS NAME when it succeeds, else FAIL
#                      (OUTDIR/expect.txt, as expect), for what a screenshot
#                      cannot judge (files, exit statuses); SKIP when CMD
#                      exits with 77 (it cannot tell in this terminal).
#   screen_contains TEXT  whether the screen shows TEXT (kitty and WezTerm,
#                      through their get-text; 77 elsewhere), e.g. `check
#                      detached screen_contains "[detached from session e2e]"`.
#
# temzdf's log (geometry, warnings) goes to OUTDIR/state/temzdf/temzdf.log;
# when each phase of the run began goes to OUTDIR/timing.txt (milliseconds).
# It waits for the first screen (temzdf's log says a complete frame was
# written, then the screen stands still for 150 ms; at most 3 s, 6 s for
# Ghostty and WezTerm, 5 s for the GUI; inside a multiplexer that long, 3 s
# more in zellij, 1 s in tmux) and sends `q` at the end, waiting up to half a second for the
# terminal to go. E2E_RENDERER (a temzdf) and E2E_GUI_RENDERER (a temzdf-gui)
# draw `expect`'s headless renders instead of COMMAND's builds (release
# builds are quicker; scripts/e2e-all.sh -R); E2E_XVFB_DISPLAY picks the
# Xvfb display number instead of a free one. It does not check results; inspect the screenshots (or
# compare pixels) afterwards. The
# command runs without ZELLIJ/TMUX in its environment so a multiplexer around the
# harness does not leak into terminal detection, without TEMZDF_SESSION_NAME
# (a temzdf session around the harness would refuse to show the run's
# session of the same name inside itself), and with its own
# XDG_RUNTIME_DIR (also for ACTIONS), so its sessions stay apart from the
# user's and end with the run. If the terminal process died, it prints
# "TERMINAL CRASHED" and exits 3; failed `expect`s and `check`s exit 4; a
# command that ended while starting, or no window, exits 1. Requires the dev
# shell (kitty, xvfb-run, xdotool, imagemagick; Ghostty from the host);
# re-executes inside it.
# A COMMAND that runs temzdf must pass --clean (checked), so scenarios see the
# shipped defaults rather than the user's config.ts.
set -euo pipefail
# Absolute: it re-executes itself after the cd.
self=$(realpath "${BASH_SOURCE[0]}")
cd "$(dirname "$(dirname "$self")")"

if [[ -z "${IN_NIX_SHELL:-}" ]]; then
  # The dirty-tree warning off through nix itself: a process substitution
  # filtering it would leave its pipe open in every child, and a daemon that
  # outlives the run (mozc_server) kept whatever reads our output waiting.
  NIX_CONFIG="warn-dirty = false${NIX_CONFIG:+$'\n'$NIX_CONFIG}" exec nix develop --quiet -c "$self" "$@"
fi

size=900x600
term=kitty
display=x11
[[ $(uname) == Darwin ]] && display=macos
mux=
ime=
outdir=target/e2e
while getopts "t:d:m:s:o:P:Ih" opt; do
  case $opt in
    t) term=$OPTARG ;;
    d) display=$OPTARG ;;
    m) mux=$OPTARG ;;
    I) ime=1 ;;
    P)
      [[ $OPTARG == shared || $OPTARG == socket ]] || { echo "e2e.sh: -P shared or socket" >&2; exit 2; }
      # In the environment, so it survives re-executing (Xvfb, sway).
      export TEMZDF_PIXELS=$OPTARG
      ;;
    s) size=$OPTARG ;;
    o) outdir=$OPTARG ;;
    *) sed -n '2,${ /^set -euo/q; p; }' "$self"; exit 2 ;;
  esac
done
shift $((OPTIND - 1))
if [[ $# -lt 3 || $2 != "--" ]]; then
  sed -n '2,${ /^set -euo/q; p; }' "$self"
  exit 2
fi
actions=$(realpath "$1")
shift 2
# Runs of temzdf take the shipped defaults, not the user's config.ts. A
# word naming temzdf or temzdf-gui (also inside `sh -c '...'`), not any
# path with "temzdf" in it.
runs_temzdf() { grep -qE "(^|[ /'\"])temzdf(-gui)?( |\$)" <<<"$*"; }
if runs_temzdf "$@" && [[ "$*" != *--clean* ]]; then
  echo "e2e.sh: COMMAND runs temzdf without --clean (it would load your config.ts)" >&2
  exit 2
fi
case $display in
  x11) ;;
  wayland) ;;
  macos)
    if [[ -n $ime ]]; then
      echo "e2e.sh: -I is X11 and Wayland only so far" >&2
      exit 2
    fi
    ;;
  *)
    echo "e2e.sh: unknown display server $display (x11, wayland or macos)" >&2
    exit 2
    ;;
esac
mkdir -p "$outdir"
outdir=$(realpath "$outdir")
width=${size%x*}
height=${size#*x}

if [[ -z "${E2E_INNER:-}" && $display == wayland ]]; then
  # Headless sway starts below, in the run's own runtime directory. An
  # input method needs a session bus.
  export E2E_INNER=1
  bus=()
  [[ -n $ime ]] && bus=(dbus-run-session --)
  exec "${bus[@]}" "$self" -t "$term" -d wayland ${mux:+-m "$mux"} ${ime:+-I} -s "$size" -o "$outdir" "$actions" -- "$@"
fi
if [[ -z "${E2E_INNER:-}" && $display == x11 ]]; then
  export E2E_INNER=1
  screen="${width}x${height}"
  # The GUI's window has decorations around its drawing area.
  [[ $term == gui ]] && screen="$((width + 100))x$((height + 100))"
  # An input method needs a session bus.
  bus=()
  [[ -n $ime ]] && bus=(dbus-run-session --)
  # A free display number (-a), or E2E_XVFB_DISPLAY's: runs started at the
  # same moment can pick the same free one (scripts/e2e-all.sh -j gives each
  # its own).
  server=(-a)
  [[ -n ${E2E_XVFB_DISPLAY:-} ]] && server=(-n "$E2E_XVFB_DISPLAY")
  exec xvfb-run "${server[@]}" -s "-screen 0 ${screen}x24" "${bus[@]}" "$self" -t "$term" ${mux:+-m "$mux"} ${ime:+-I} \
    -s "$size" -o "$outdir" "$actions" -- "$@"
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
# Coordinates as given (the terminal area's) and on the screen.
at() { echo "$1 $2"; }
root() { echo "$(($1 + origin_x)) $(($2 + origin_y))"; }
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
# MODS down before a button or wheel step, up after it, with a moment in
# between: WezTerm on Wayland took a click that came right after Ctrl as a
# plain one now and then (a person never presses them a millisecond apart).
mods_down() {
  local k
  for k in $(mod_keys "${1:-}"); do be_keydown "$k"; done
  [[ -z ${1:-} ]] || sleep 0.05
}
mods_up() {
  local k
  [[ -z ${1:-} ]] || sleep 0.05
  for k in $(mod_keys "${1:-}"); do be_keyup "$k"; done
}
button_name() { case $1 in 1) echo left ;; 2) echo middle ;; 3) echo right ;; *) echo "bad button $1" >&2; exit 2 ;; esac; }

# What depends on the display server (be_*; X11 here): pointer and keys,
# screenshots, the clipboard, windows. Coordinates are the screen's; key
# names are XKB keysym names (xdotool's), MODS as for the helpers below.
be_move() { xdotool mousemove "$1" "$2"; }
be_button() { xdotool "mouse$1" "$2"; }  # down|up BUTTON
# N presses of BUTTON, DELAY ms apart, in one go (a wheel flick).
be_clicks() { xdotool click --repeat "$2" --delay "$3" "$1"; }
be_key() { xdotool key "$1"; }
be_keydown() { xdotool keydown "$1"; }
be_keyup() { xdotool keyup "$1"; }
be_type() { xdotool type ${2:+--delay "$2"} -- "$1"; }  # TEXT [DELAY_MS]
be_shot() { import -window root "$1"; }
be_clipboard() { xclip -selection clipboard -o 2>/dev/null; }
be_set_clipboard() { printf %s "$1" | xclip -selection clipboard; }
# The first visible window of process PID, waited for up to 10 s.
be_window() { timeout 10 xdotool search --sync --onlyvisible --pid "$1" | tail -1; }
be_focus() { xdotool windowfocus --sync "$(be_window "$1")"; }
# "X Y W H" of PID's window's inside.
be_window_rect() {
  local id
  id=$(be_window "$1")
  [[ -n $id ]] || return 1
  xwininfo -id "$id" | awk '/Absolute upper-left X/ {x = $4} /Absolute upper-left Y/ {y = $4}
    /Width:/ {w = $2} /Height:/ {h = $2} END {print x, y, w, h}'
}
# A plain window of another program, for focus to go to; prints its PID.
be_other_window() { xterm -e sh -c 'sleep 30' >/dev/null 2>&1 & echo $!; }
# Wayland (headless sway, started below): keys and the pointer through
# wlinput, one virtual keyboard and pointer for the whole run (wtype and
# wlrctl make a device per call: clicks and wheel steps got lost, and every
# key was a focus change); screenshots with grim, windows through swaymsg.
if [[ $display == wayland ]]; then
  # A command to wlinput, waiting for its answer.
  wl() {
    local answer
    echo "$*" >&"${WL[1]}"
    read -r answer <&"${WL[0]}"
    [[ $answer == ok ]] || { echo "wlinput: $answer ($*)" >&2; return 1; }
  }
  be_move() { wl move "$1" "$2"; }
  be_button() { wl "$1" "$2"; }
  be_clicks() {
    local i
    case $1 in
      4) wl wheel "-$2" "$3" ;;
      5) wl wheel "$2" "$3" ;;
      *)
        for ((i = 0; i < $2; i++)); do
          ((i == 0)) || sleep "$(printf '%d.%03d' $(($3 / 1000)) $(($3 % 1000)))"
          wl down "$1"
          wl up "$1"
        done
        ;;
    esac
  }
  be_key() { wl key "$1"; }
  be_keydown() { wl keydown "$1"; }
  be_keyup() { wl keyup "$1"; }
  be_type() { wl type "${2:-0}" "$1"; }
  be_shot() { grim "$1"; }
  be_clipboard() { wl-paste -n 2>/dev/null; }
  # wl-copy stays to serve the clipboard: nothing of ours may stay open in it.
  be_set_clipboard() { wl-copy -- "$1" </dev/null >/dev/null 2>&1; }
  be_window_rect() {
    swaymsg -t get_tree | python3 -c '
import json, sys
def walk(n):
    if n.get("pid") == int(sys.argv[1]):
        r = n["rect"]
        print(r["x"], r["y"], r["width"], r["height"])
        sys.exit(0)
    for c in n.get("nodes", []) + n.get("floating_nodes", []):
        walk(c)
walk(json.load(sys.stdin))
sys.exit(1)' "$1"
  }
  be_focus() {
    local i
    for ((i = 0; i < 50; i++)); do be_window_rect "$1" >/dev/null && break; sleep 0.1; done
    swaymsg -q "[pid=$1] focus"
  }
  # Floating (sway.conf), so the window under test keeps its size.
  be_other_window() {
    kitty --config NONE -o linux_display_server=wayland --class e2e-other sh -c 'sleep 30' </dev/null >/dev/null 2>&1 &
    echo $!
  }
fi
# macOS (a virtual display started below; disp_x disp_y: its place among the
# screens, global points, which are its pixels at scale 1): the pointer and
# keys through `termctl serve` (TC, started once the terminal is up), kitty's
# keys through its remote control (it ignores key events posted to its
# process), screenshots of the display, the clipboard with pbcopy/pbpaste.
if [[ $display == macos ]]; then
  ctl=target/macos-gui/termctl
  tc() {
    local answer
    echo "$*" >&"${TC[1]}"
    read -r answer <&"${TC[0]}"
    [[ $answer == ok ]] || { echo "termctl: $answer ($*)" >&2; return 1; }
  }
  be_move() { tc move $((disp_x + $1)) $((disp_y + $2)); }
  be_button() { tc "$1" "$2"; }
  be_clicks() {
    local i
    case $1 in
      4) tc wheel "-$2" "$3" ;;
      5) tc wheel "$2" "$3" ;;
      *)
        for ((i = 0; i < $2; i++)); do
          ((i == 0)) || sleep "$(printf '%d.%03d' $(($3 / 1000)) $(($3 % 1000)))"
          tc down "$1"
          tc up "$1"
        done
        ;;
    esac
  }
  # An XKB name as kitty's remote control takes it: what types a character
  # goes as that text (send-key shift+g would lack the shifted key real
  # typing reports), the rest as a key.
  kitty_key() {
    local k=$1 mods="" name c=
    while [[ $k == *+?* ]]; do mods+="${k%%+*}+"; k=${k#*+}; done
    case $k in
      Return) name=enter ;; Escape) name=escape ;; Tab) name=tab ;; BackSpace) name=backspace ;;
      Delete) name=delete ;; space) name=space c=" " ;; Prior | Page_Up) name=page_up ;; Next | Page_Down) name=page_down ;;
      plus) c=+ ;; minus) c=- ;; equal) c="=" ;; slash) c=/ ;; question) c="?" ;; less) c="<" ;;
      greater) c=">" ;; colon) c=: ;; semicolon) c=";" ;; percent) c=% ;; comma) c=, ;; period) c=. ;;
      ?) c=$k ;;
      *) name=${k,,} ;;
    esac
    if [[ -n $c && ( -z $mods || $mods == shift+ ) ]]; then
      [[ $mods == shift+ ]] && c=${c^^}
      kitten @ --to "unix:$kitty_socket" send-text -- "$c"
    else
      kitten @ --to "unix:$kitty_socket" send-key "$mods${name:-$c}"
    fi
  }
  be_key() { if [[ $term == kitty ]]; then kitty_key "$1"; else tc key "$1"; fi; }
  be_keydown() { tc keydown "$1"; }
  be_keyup() { tc keyup "$1"; }
  be_type() {
    local i
    if [[ $term != kitty ]]; then
      tc type "${2:-0}" "$1"
    elif [[ -z ${2:-} ]]; then
      kitten @ --to "unix:$kitty_socket" send-text -- "$1"
    else
      for ((i = 0; i < ${#1}; i++)); do
        kitten @ --to "unix:$kitty_socket" send-text -- "${1:i:1}"
        sleep "$(printf '%d.%03d' $(($2 / 1000)) $(($2 % 1000)))"
      done
    fi
  }
  be_shot() { screencapture -x -R "$disp_x,$disp_y,$disp_w,$disp_h" "$1"; }
  be_clipboard() { pbpaste; }
  be_set_clipboard() { printf %s "$1" | pbcopy; }
  # The window's inside (without a title bar), on the virtual display.
  be_window_rect() {
    local x y w h
    read -r x y w h <<<"$("$ctl" content "$1")"
    echo "$((x - disp_x)) $((y - disp_y)) $w $h"
  }
  be_focus() { "$ctl" activate "$1" >/dev/null; }
  be_other_window() {
    local before p
    before=" $(pgrep -x TextEdit | tr '\n' ' ' || true) "
    open -g -n -a TextEdit
    for ((i = 0; i < 50; i++)); do
      for p in $(pgrep -x TextEdit); do [[ $before == *" $p "* ]] || { echo "$p"; return; }; done
      sleep 0.1
    done
  }
fi

shot() { be_shot "$outdir/$1.png"; }
wheel() {
  local n=$1 x=${2:-$((width / 2))} y=${3:-$((height / 2))} mods=${4:-} btn=5 dir=down k
  if ((n < 0)); then btn=4 dir=up n=$((-n)); fi
  be_move $(root "$x" "$y")
  record move "$(at "$x" "$y")" $mods
  mods_down "$mods"
  # One call: separate ones take about 0.1 s each, too slow for a flick.
  # The steps are recorded at the times it spaces them.
  local first=$((($(usec) - t0) / 1000)) i
  be_clicks "$btn" "$n" 30
  for ((i = 0; i < n; i++)); do record -at $((first + 30 * i)) wheel $dir "$(at "$x" "$y")" $mods; done
  mods_up "$mods"
}
# The button held by `down` (and its MODS), for `move` and `up`.
held_btn= held_mods= pointer_x=0 pointer_y=0
down() {
  local x=$1 y=$2 btn=${3:-1} mods=${4:-} k
  be_move $(root "$x" "$y")
  record move "$(at "$x" "$y")" $mods
  mods_down "$mods"
  be_button down "$btn"
  record press "$(at "$x" "$y")" "$(button_name "$btn")" $mods
  held_btn=$btn held_mods=$mods pointer_x=$x pointer_y=$y
}
up() {
  local btn=${1:-${held_btn:-1}} k
  be_button up "$btn"
  record release "$(at "$pointer_x" "$pointer_y")" "$(button_name "$btn")" $held_mods
  mods_up "$held_mods"
  held_btn= held_mods=
}
dragto() {
  local x=$1 y=$2 x2=$3 y2=$4 steps=${5:-10} i px py
  down "$x" "$y"
  for ((i = 1; i <= steps; i++)); do
    px=$((x + (x2 - x) * i / steps)) py=$((y + (y2 - y) * i / steps))
    move "$px" "$py"
    sleep 0.03
  done
  up
}
drag() { dragto "$1" "$2" "$1" "$3"; }
click() {
  local x=$1 y=$2 btn=${3:-1} mods=${4:-} k b
  b=$(button_name "$btn")
  be_move $(root "$x" "$y")
  record move "$(at "$x" "$y")" $mods
  mods_down "$mods"
  be_clicks "$btn" 1 0
  record press "$(at "$x" "$y")" "$b" $mods
  record release "$(at "$x" "$y")" "$b" $mods
  mods_up "$mods"
}
# The pointer: a drag while `down` holds a button, else hovering.
move() {
  be_move $(root "$1" "$2")
  pointer_x=$1 pointer_y=$2
  if [[ -n $held_btn ]]; then
    record drag "$(at "$1" "$2")" "$(button_name "$held_btn")" $held_mods
  else
    record move "$(at "$1" "$2")"
  fi
}
# Keys held down across other actions (not recorded: for windows and focus).
hold() { local k; for k in "$@"; do be_keydown "$k"; done; }
release() { local k; for k in "$@"; do be_keyup "$k"; done; }
clipboard() { be_clipboard; }
set_clipboard() { be_set_clipboard "$1"; }
focus_window() { be_focus "$1"; }
other_window() { be_other_window; }
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
    be_key "$k"
    record key "$(key_notation "$k")"
  done
}
type() {
  local s=$1 delay=${2:-} i c out="" line rest
  # Newlines as the Return key: Ghostty ignores those xdotool types.
  rest=$s
  while [[ $rest == *$'\n'* ]]; do
    line=${rest%%$'\n'*}
    [[ -n $line ]] && be_type "$line" "$delay"
    be_key Return
    rest=${rest#*$'\n'}
  done
  [[ -n $rest ]] && be_type "$rest" "$delay"
  for ((i = 0; i < ${#s}; i++)); do
    c=${s:i:1}
    case $c in
      "<") out+="<lt>" ;; " ") out+="<Space>" ;; $'\n') out+="<CR>" ;; *) out+=$c ;;
    esac
  done
  record key "$out"
}
pause() { sleep "$1"; }
ime() {
  case $1 in
    on) fcitx5-remote -o ;;
    off) fcitx5-remote -c ;;
  esac
}
# Ghostty on macOS shows the pixels of images as Display P3 values (no
# conversion from sRGB): saturated colours come out more saturated. The
# expected screen FILE goes through the same, so the comparison stays exact
# (doc/terminal-compat.md).
as_terminal_shows() {
  [[ $display == macos && $term == ghostty ]] || return 0
  magick "$1" -strip -profile "/System/Library/ColorSync/Profiles/Display P3.icc" \
    -profile "/System/Library/ColorSync/Profiles/sRGB Profile.icc" -strip "$1"
}
# macOS draws a 1 px border over the edge of every window and rounds its
# corners, and a title bar's separator line over the first row below it:
# those pixels are not the terminal's (Ghostty's and the GUI's cells reach
# the window's edges). They go black on both sides of NAME's comparison
# (NAME.window-mask.png), the W x H area at the drawing area's origin.
mask_window_edges() {
  [[ $display == macos ]] || return 0
  window_edges_mask "$1" "$2" "$3"
  magick "$outdir/$1.actual.png" "$outdir/$1.window-mask.png" -composite "$outdir/$1.actual.png"
  magick "$outdir/$1.expected.png" "$outdir/$1.window-mask.png" -composite "$outdir/$1.expected.png"
}
# OUTDIR/NAME.window-mask.png: mask_window_edges' mask (black where the
# window's edges show, clear elsewhere) over W x H pixels from the origin.
window_edges_mask() {
  local name=$1 w=$2 h=$3 wid fx fy fw fh ox oy line=()
  read -r wid fx fy fw fh _ <<<"$("$ctl" windows "$term_pid" | head -1)"
  screencapture -x -o -l "$wid" "$outdir/$name.window.png"
  ox=$((disp_x + origin_x - fx)) oy=$((disp_y + origin_y - fy))
  ((oy > 0)) && line=(-draw "line 0,$oy $((fw - 1)),$oy")
  # Corners: where the window's own picture is not opaque, and the border
  # along them one pixel further in.
  magick "$outdir/$name.window.png" -alpha extract -threshold 99% -negate -morphology Dilate Square:1 \
    -fill none -stroke white \
    -draw "rectangle 0,0 $((fw - 1)),$((fh - 1))" "${line[@]}" -crop "${w}x${h}+$ox+$oy" +repage \
    -background black -alpha shape -strip "$outdir/$name.window-mask.png"
  rm -f "$outdir/$name.window.png"
  # Ghostty (1.3) shows a handle for dragging the split (three dots) at the
  # top middle of the terminal when the pointer comes near the top.
  if [[ $term == ghostty ]]; then
    magick "$outdir/$name.window-mask.png" -fill black -draw "rectangle $((w / 2 - 12)),0 $((w / 2 + 12)),7" \
      "$outdir/$name.window-mask.png"
  fi
}
# Wait until the first screen is up, at most LIMIT seconds (what a fixed wait
# took before): temzdf's log saying it wrote a complete frame (when
# the command runs temzdf in the terminal), then the screen unchanged over
# 150 ms and not of one colour (the terminal has drawn it). Returns at
# once when the terminal (or the GUI) ended.
wait_ready() {
  local limit=$1 deadline log=$outdir/state/temzdf/temzdf.log a=$outdir/ready.a.png b=$outdir/ready.b.png
  deadline=$(($(usec) + limit * 1000000))
  if [[ -n $temzdf_in_terminal ]]; then
    until grep -qs 'complete frame written' "$log"; do
      (($(usec) < deadline)) && kill -0 "$term_pid" 2>/dev/null || break
      sleep 0.05
    done
    phase written
  fi
  # On macOS the window only, where it was put: the whole display (the menu
  # bar, the desktop) is never of one colour, and a smaller picture takes
  # less to take and count (a VM counted a whole display in 0.6 s).
  local shoot=(be_shot)
  [[ $display == macos ]] &&
    shoot=(screencapture -x -R "$disp_x,$((disp_y + menu_bar)),$width,$height")
  "${shoot[@]}" "$a"
  while (($(usec) < deadline)) && kill -0 "$term_pid" 2>/dev/null; do
    sleep 0.15
    "${shoot[@]}" "$b"
    # A screen of one colour is a terminal that has drawn nothing yet (kitty
    # took up to 2 s to show what temzdf had written); every first screen
    # has something on it (a status bar, a prompt).
    if cmp -s "$a" "$b" && (($(magick "$b" -format %k info:) > 1)); then
      rm -f "$a" "$b"
      phase ready
      return
    fi
    mv "$b" "$a"
  done
  rm -f "$a" "$b"
  phase ready "(timed out)"
}
wait_complete() {
  local limit=${1:-10} deadline log=$outdir/state/temzdf/temzdf.log
  [[ -n $temzdf_in_terminal ]] || return 0
  deadline=$(($(usec) + limit * 1000000))
  # The last of the two lines temzdf writes as frames turn incomplete and
  # complete again.
  until grep -E 'temzdf: (incomplete frame shown|complete frame written)' "$log" 2>/dev/null | tail -1 |
    grep -q 'complete frame written'; do
    if (($(usec) >= deadline)); then
      echo "e2e.sh: no complete frame written within ${limit}s" >&2
      phase "complete (timed out)"
      return 0
    fi
    sleep 0.05
  done
  phase complete
}
# The GUI's form of expect: the window against an offscreen render of the
# same session at this moment.
expect_gui() {
  local name=$1 session out
  shot "$name"
  session=$("$temzdf" ls | head -1 | cut -d' ' -f1)
  phase "expect $name"
  "${clean_env[@]}" VK_DRIVER_FILES="${TEMZDF_LAVAPIPE_ICD:-}" "${E2E_GUI_RENDERER:-$gui_bin}" --render-frame "$outdir/$name.expected.png" \
    -s "$session" --size "${win_w}x${win_h}" 2>"$outdir/$name.expected.log" || {
    echo "FAIL $name: --render-frame failed (see $name.expected.log)" | tee -a "$outdir/expect.txt"
    return
  }
  phase "expected $name"
  magick "$outdir/$name.png" -crop "${win_w}x${win_h}+$origin_x+$origin_y" +repage "$outdir/$name.actual.png"
  mask_window_edges "$name" "$win_w" "$win_h"
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
# The lines of a screen's text without trailing blanks, or blank lines at
# the end.
screen_lines() {
  sed 's/[[:space:]]*$//' "$1" | awk '{ l[NR] = $0 } END { n = NR; while (n > 0 && l[n] == "") n--; for (i = 1; i <= n; i++) print l[i] }'
}
# The session's form of expect: the screen against `temzdf render-session`
# attached to the session now, as a client like the terminal's.
expect_session() {
  local name=$1 session geo cols rows cw ch backend medium out flags=() failed=
  if [[ -n $mux ]]; then
    echo "FAIL $name: sessions are not judged inside $mux" | tee -a "$outdir/expect.txt"
    return
  fi
  shot "$name"
  session=${E2E_SESSION:-$("$temzdf" ls | head -1 | cut -d' ' -f1)}
  # The backend may come with its settings: "Composited { ... } over Direct".
  geo=$(grep -oE 'geometry [0-9]+x[0-9]+ cells of [0-9]+x[0-9]+ px with [A-Za-z]+.* over [A-Za-z]+' \
    "$outdir/state/temzdf/temzdf.log" | tail -1) || {
    echo "FAIL $name: no geometry in temzdf.log" | tee -a "$outdir/expect.txt"
    return
  }
  read -r cols rows cw ch backend medium <<<"$(sed -E \
    's/geometry ([0-9]+)x([0-9]+) cells of ([0-9]+)x([0-9]+) px with ([A-Za-z]+).* over ([A-Za-z]+)/\1 \2 \3 \4 \5 \6/' <<<"$geo")"
  # The hints the terminal client sent (TtyFrontend::hints).
  [[ $backend != Layered ]] && flags+=(--exact-scaling)
  [[ $medium != Direct ]] && flags+=(--fast-uploads)
  [[ $term == kitty || $term == ghostty ]] && flags+=(--linear-blending)
  phase "expect $name"
  "${E2E_RENDERER:-$temzdf}" render-session "$session" "$outdir/$name.expected.png" --size "${cols}x${rows}" \
    --cell "${cw}x${ch}" --text "$outdir/$name.expected.txt" --mask "$outdir/$name.mask.png" "${flags[@]}" \
    2>"$outdir/$name.expected.log" || {
    echo "FAIL $name: render-session failed (see $name.expected.log)" | tee -a "$outdir/expect.txt"
    return
  }
  phase "expected $name"
  as_terminal_shows "$outdir/$name.expected.png"
  # Pixels where the images show: the cells the terminal draws go black on
  # both sides.
  magick "$outdir/$name.png" -crop "$((cols * cw))x$((rows * ch))+$origin_x+$origin_y" +repage \
    "$outdir/$name.mask.png" -composite "$outdir/$name.actual.png"
  magick "$outdir/$name.expected.png" "$outdir/$name.mask.png" -composite "$outdir/$name.expected.png"
  mask_window_edges "$name" $((cols * cw)) $((rows * ch))
  out=$(magick compare -metric AE -fuzz 3% "$outdir/$name.actual.png" "$outdir/$name.expected.png" \
    "$outdir/$name.diff.png" 2>&1 || true)
  out=${out%% *}
  if [[ $out == 0 ]]; then
    rm -f "$outdir/$name.diff.png"
  else
    failed="$out pixels differ (see $name.diff.png)"
  fi
  # The text, where the terminal can tell it.
  if read_screen >"$outdir/$name.actual.txt"; then
    if diff <(screen_lines "$outdir/$name.expected.txt") <(screen_lines "$outdir/$name.actual.txt") \
      >"$outdir/$name.text.diff"; then
      rm -f "$outdir/$name.text.diff"
    else
      failed+="${failed:+; }the text differs (see $name.text.diff)"
    fi
  fi
  if [[ -z $failed ]]; then
    echo "PASS $name" | tee -a "$outdir/expect.txt"
  else
    echo "FAIL $name: $failed" | tee -a "$outdir/expect.txt"
  fi
}
check() {
  local name=$1 status=0
  shift
  "$@" || status=$?
  case $status in
    0) echo "PASS $name" | tee -a "$outdir/expect.txt" ;;
    77) echo "SKIP $name: $term cannot tell" | tee -a "$outdir/expect.txt" ;;
    *) echo "FAIL $name: $*" | tee -a "$outdir/expect.txt" ;;
  esac
}
# The text the terminal shows (kitty, WezTerm); 77 where it cannot tell.
read_screen() {
  [[ -z $mux ]] || return 77
  case $term in
    kitty) kitten @ --to "unix:$kitty_socket" get-text --extent screen ;;
    # The GUI's own socket: without it the CLI starts a mux server of its own.
    wezterm)
      if [[ $display == macos ]]; then
        # Started through `open`: its socket is in its own runtime directory,
        # and the CLI cannot tell the focused pane of a window in the background.
        local pane
        export WEZTERM_UNIX_SOCKET=$HOME/.local/share/wezterm/gui-sock-$term_pid
        pane=$(wezterm cli list --format json | jq '.[0].pane_id') || return
        wezterm cli get-text --pane-id "$pane"
      else
        WEZTERM_UNIX_SOCKET=$(ls "$XDG_RUNTIME_DIR"/wezterm/gui-sock-* | head -1) wezterm cli get-text
      fi
      ;;
    *) return 77 ;;
  esac
}
# Whether the terminal shows TEXT (77 where it cannot tell).
screen_contains() {
  local text
  text=$(read_screen) || return
  grep -qF -- "$1" <<<"$text"
}
# The terminal's cell grid, as temzdf last logged it: sets cols, rows, cw
# and ch; fails without one.
read_geometry() {
  local geo
  geo=$(grep -oE 'geometry [0-9]+x[0-9]+ cells of [0-9]+x[0-9]+ px' "$outdir/state/temzdf/temzdf.log" 2>/dev/null | tail -1)
  [[ -n $geo ]] || return 1
  read -r cols rows cw ch <<<"$(sed -E 's/geometry ([0-9]+)x([0-9]+) cells of ([0-9]+)x([0-9]+) px/\1 \2 \3 \4/' <<<"$geo")"
}
draw_lines() {
  local name=$1 cmd="clear; printf '%s\n'" line
  shift
  for line in "$@"; do cmd+=" '$line'"; done
  type "$cmd
"
  pause 1
  shot "$name"
}
# Cell row ROW of screenshot NAME (OUTDIR/NAME.png) into FILE. On macOS
# the window's edges are masked (row_mask): its rounded corners are in the
# first and last rows only, and its border is not the terminal's. No colour
# profile (macOS's screenshots have one; on a grey row magick warns).
row_mask=
cell_row() {
  local mask=()
  [[ -n $row_mask ]] && mask=("$row_mask" -composite)
  magick "$outdir/$1.png" -crop "$((cols * cw))x$((rows * ch))+$origin_x+$origin_y" +repage "${mask[@]}" \
    -crop "$((cols * cw))x$ch+0+$(($2 * ch))" +repage -strip "$3"
}
# Whether two images are the same, as expect compares.
same_image() {
  local out
  out=$(magick compare -quiet -metric AE -fuzz 3% "$1" "$2" null: 2>&1 || true)
  [[ ${out%% *} == 0 ]]
}
row_of() {
  local shot=$1 ref=$2 k=$3 r want=$outdir/.row-want.png got=$outdir/.row-got.png
  read_geometry || return 1
  cell_row "$ref" "$k" "$want"
  for ((r = 0; r < rows; r++)); do
    cell_row "$shot" "$r" "$got"
    if same_image "$want" "$got"; then
      echo "$r"
      return 0
    fi
  done
  return 1
}
screen_restored() {
  local shot=$1 ref=$2 k=$3 before=${4:-} bk=${5:-} m mb r
  local a=$outdir/.row-a.png b=$outdir/.row-b.png blank=$outdir/.row-blank.png
  read_geometry || return 1
  if [[ $display == macos && -z $row_mask ]]; then
    # Not .rows: screencapture writes no file whose name starts with a dot.
    window_edges_mask rows $((cols * cw)) $((rows * ch))
    row_mask=$outdir/rows.window-mask.png
  fi
  if ! m=$(row_of "$shot" "$ref" "$k"); then
    echo "screen_restored: no row of $shot.png is $ref.png's row $k" >&2
    return 1
  fi
  # The reference's last row: a row of nothing in this terminal.
  cell_row "$ref" $((rows - 1)) "$blank"
  if [[ -n $before ]]; then
    if ! mb=$(row_of "$before" "$ref" "$bk"); then
      echo "screen_restored: no row of $before.png is $ref.png's row $bk" >&2
      return 1
    fi
    for ((r = 0; r <= mb; r++)); do
      cell_row "$before" "$r" "$a"
      cell_row "$shot" "$r" "$b"
      if ! same_image "$a" "$b"; then
        echo "screen_restored: row $r of $shot.png is not as in $before.png" >&2
        return 1
      fi
    done
  else
    # Empty above the message (temzdf was the first command).
    for ((r = 0; r < m; r++)); do
      cell_row "$shot" "$r" "$b"
      if ! same_image "$blank" "$b"; then
        echo "screen_restored: row $r of $shot.png is not empty" >&2
        return 1
      fi
    done
  fi
  # Between BEFORE's message and this one: the command typed there. Below
  # the message and the prompt after it: nothing.
  for ((r = m + 2; r < rows; r++)); do
    cell_row "$shot" "$r" "$b"
    if ! same_image "$blank" "$b"; then
      echo "screen_restored: row $r of $shot.png is not empty" >&2
      return 1
    fi
  done
}
expect() {
  if [[ $term == gui ]]; then
    expect_gui "$1"
    return
  fi
  if [[ -n $session_mode ]]; then
    expect_session "$1"
    return
  fi
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
  read_geometry || {
    echo "FAIL $name: no geometry in temzdf.log" | tee -a "$outdir/expect.txt"
    return
  }
  # --keys=...: key lists may start with "-" (zoom out).
  phase "expect $name"
  "${expect_cmd[@]}" --render-frame "$outdir/$name.expected.png" --size "${cols}x${rows}" \
    --cell "${cw}x${ch}" "${input[@]}" "$expect_file" 2>"$outdir/$name.expected.log" || {
    echo "FAIL $name: --render-frame failed (see $name.expected.log)" | tee -a "$outdir/expect.txt"
    return
  }
  phase "expected $name"
  # The terminal may have spare pixels right and below the cell grid.
  magick "$outdir/$name.png" -crop "$((cols * cw))x$((rows * ch))+$origin_x+$origin_y" +repage "$outdir/$name.actual.png"
  as_terminal_shows "$outdir/$name.expected.png"
  mask_window_edges "$name" $((cols * cw)) $((rows * ch))
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
  expect_cmd=("${E2E_RENDERER:-${args[0]}}" standalone pdf)
  expect_file=${args[-1]}
  for ((i = 1; i < ${#args[@]} - 1; i++)); do
    case ${args[i]} in
      --page | --scale) expect_cmd+=("${args[i]}" "${args[i + 1]}") ;;
    esac
  done
  # kitty and Ghostty blend translucent images themselves, in linear light.
  # Inside a multiplexer temzdf composites frames (sRGB) and they only show
  # them.
  if [[ ($term == kitty || $term == ghostty) && -z $mux ]]; then expect_cmd+=(--linear-blending); fi
fi
# A session (not the standalone viewer): `expect` attaches to it, with the
# temzdf COMMAND runs (also inside `sh -c '...'`).
session_mode=
if [[ $term != gui && ${args[1]:-} != standalone ]] && runs_temzdf "$@"; then
  session_mode=1
  temzdf=$(grep -oE "[^ '\"]*/temzdf( |$)" <<<"$*" | head -1 | tr -d ' ') || true
  temzdf=$(realpath "${temzdf:-$(command -v temzdf)}")
fi
mkdir -p "$outdir/state"
rm -f "$outdir/state/temzdf/temzdf.log" "$outdir/expect.txt"
# When each phase began, in milliseconds since this point
# (OUTDIR/timing.txt): for seeing where a run spends its time.
timing0=$(usec)
: >"$outdir/timing.txt"
phase() { echo "$((($(usec) - timing0) / 1000)) $*" >>"$outdir/timing.txt"; }
phase launch
# temzdf runs in the terminal (not the GUI): its log says when its first
# screen is shown (wait_ready). Decided before a multiplexer wraps COMMAND.
temzdf_in_terminal=
[[ $term != gui ]] && runs_temzdf "$@" && temzdf_in_terminal=1
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
# macOS: /tmp itself, as $TMPDIR is long there (sockets are limited to 104
# bytes, doc/macos.md).
if [[ $display == macos ]]; then
  XDG_RUNTIME_DIR=$(mktemp -d /tmp/temzdf-e2e.XXXXXX)
else
  XDG_RUNTIME_DIR=$(mktemp -d "${TMPDIR:-/tmp}/temzdf-e2e.XXXXXX")
fi
export XDG_RUNTIME_DIR
# kitty answers `kitten @ get-text` (expect in a session) here.
kitty_socket=$XDG_RUNTIME_DIR/kitty.sock
term_pid= sway_pid= vdisp_pid= user_clipboard=
cleanup() {
  if [[ -n ${TC[1]:-} ]]; then
    # The front and the pointer back to where they were.
    tc done || true
    eval "exec ${TC[1]}>&-"
  fi
  if [[ -n $ime ]]; then
    # fcitx5 and the mozc_server it starts outlive the run (Mozc's server
    # daemonizes): end those of this run, known by their HOME.
    fcitx5-remote -e 2>/dev/null || true
    local p
    for p in /proc/[0-9]*; do
      [[ -O $p/environ ]] || continue
      if tr '\0' '\n' 2>/dev/null <"$p/environ" | grep -qxF "HOME=$outdir/ime"; then
        kill "${p#/proc/}" 2>/dev/null || true
      fi
    done
  fi
  if [[ -n $term_pid ]]; then
    kill "$term_pid" 2>/dev/null || true
    wait "$term_pid" 2>/dev/null || true
    # Started through `open`, not a child: wait for it to go.
    for ((i = 0; i < 100; i++)); do kill -0 "$term_pid" 2>/dev/null || break; sleep 0.05; done
  fi
  if [[ -n $vdisp_pid ]]; then
    kill "$vdisp_pid" 2>/dev/null || true
    vdisp_pid=
  fi
  # The user's clipboard as it was (text only).
  if [[ -n $user_clipboard ]]; then
    pbcopy <"$user_clipboard" || true
    rm -f "$user_clipboard"
    user_clipboard=
  fi
  if [[ -n $sway_pid ]]; then
    # wlinput ends with its input; sway takes its clients with it.
    [[ -n ${WL[1]:-} ]] && eval "exec ${WL[1]}>&-"
    kill "$sway_pid" 2>/dev/null || true
    wait "$sway_pid" 2>/dev/null || true
    sway_pid=
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
if [[ $display == wayland ]]; then
  # One output of the window's size: the only window fills it (no borders),
  # so the terminal area, or the GUI's drawing area, starts at 0,0. The
  # window other_window opens floats over it instead.
  cat >"$XDG_RUNTIME_DIR/sway.conf" <<EOF
output HEADLESS-1 resolution ${width}x${height} position 0 0
default_border none
default_floating_border none
for_window [app_id="e2e-other"] floating enable
EOF
  WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
    sway -c "$XDG_RUNTIME_DIR/sway.conf" >"$outdir/sway.log" 2>&1 &
  sway_pid=$!
  for ((i = 0; i < 50; i++)); do
    ls "$XDG_RUNTIME_DIR"/wayland-[0-9] >/dev/null 2>&1 && ls "$XDG_RUNTIME_DIR"/sway-ipc.* >/dev/null 2>&1 && break
    sleep 0.1
  done
  WAYLAND_DISPLAY=$(basename "$(ls "$XDG_RUNTIME_DIR"/wayland-[0-9] | head -1)")
  SWAYSOCK=$(ls "$XDG_RUNTIME_DIR"/sway-ipc.* | head -1)
  export WAYLAND_DISPLAY SWAYSOCK
  unset DISPLAY
  cargo build -q -p wlinput
  coproc WL { target/debug/wlinput --size "${width}x${height}" 2>>"$outdir/wlinput.log"; }
fi
# macOS: a virtual display at scale 1 (screenshot pixels are points and
# match the terminal's), with room above the window for the menu bar every
# display shows, and for the title bars of WezTerm and the GUI.
menu_bar=40
if [[ $display == macos ]]; then
  scripts/macos-gui.sh build
  # A locked screen takes no input and gives no screenshots, and the
  # display going to sleep locks it: kept awake while this runs.
  caffeinate -d -w $$ &
  screen_w=$width screen_h=$((height + menu_bar))
  [[ $term == gui || $term == wezterm ]] && screen_w=$((width + 100)) screen_h=$((height + 100 + menu_bar))
  user_clipboard=$(mktemp)
  pbpaste >"$user_clipboard" 2>/dev/null || true
  # In a VM (scripts/macos-vm.sh) its own screen is nobody's desktop: no
  # virtual display there. E2E_MACOS_DISPLAY=main|virtual decides otherwise.
  macos_display=${E2E_MACOS_DISPLAY:-$([[ $(sysctl -n kern.hv_vmm_present 2>/dev/null) == 1 ]] && echo main || echo virtual)}
  if [[ $macos_display == main ]]; then
    read -r disp_x disp_y disp_w disp_h scale <<<"$(target/macos-gui/termctl main)"
    if ((scale != 1 || disp_w < screen_w || disp_h < screen_h)); then
      echo "e2e.sh: the main display (${disp_w}x${disp_h} at scale $scale) needs scale 1 and ${screen_w}x${screen_h} at least" >&2
      exit 1
    fi
  else
    target/macos-gui/vdisp "$screen_w" "$screen_h" 1 >"$outdir/vdisp.out" 2>&1 &
    vdisp_pid=$!
    for ((i = 0; i < 50; i++)); do [[ -s $outdir/vdisp.out ]] && break; sleep 0.1; done
    read -r _ disp_x disp_y disp_w disp_h <"$outdir/vdisp.out" || true
    if [[ -z ${disp_h:-} ]]; then
      echo "e2e.sh: no virtual display: $(cat "$outdir/vdisp.out")" >&2
      exit 1
    fi
  fi
fi
clean_env=(env -u TEMZDF_LOG_DIR -u TEMZDF_SESSION_NAME -u ZELLIJ -u ZELLIJ_SESSION_NAME -u ZELLIJ_PANE_ID -u TMUX -u TMUX_PANE "XDG_STATE_HOME=$outdir/state"
  "TEMZDF_RECORD_INPUT=$outdir/input.txt")
# Links are not opened: what temzdf opens them with (`open` on macOS,
# `xdg-open` elsewhere) only notes them in OUTDIR/opened.txt. A browser would
# open them on the desktop of whoever runs this (on Linux through D-Bus, past
# Xvfb and sway).
mkdir -p "$XDG_RUNTIME_DIR/bin"
opener=xdg-open
[[ $display == macos ]] && opener=open
printf '#!/bin/sh\necho "$*" >> %q\n' "$outdir/opened.txt" >"$XDG_RUNTIME_DIR/bin/$opener"
chmod +x "$XDG_RUNTIME_DIR/bin/$opener"
rm -f "$outdir/opened.txt"
clean_env+=("PATH=$XDG_RUNTIME_DIR/bin:$PATH")
if [[ $display == macos ]]; then
  # `open` starts apps in / with launchd's environment: a script gives the
  # command this run's environment (without the variables of the terminal
  # the harness runs in, which would override the new terminal's own) and
  # directory. In the runtime directory, which goes at the end: it holds
  # every variable of the environment. The command starts once the window
  # is on the virtual display (the go file): the terminal opens on the main
  # screen first, and temzdf would start there at its scale and log that
  # geometry.
  launch=$XDG_RUNTIME_DIR/launch.sh go=$XDG_RUNTIME_DIR/go
  {
    echo '#!/usr/bin/env bash'
    echo "export PATH=$(printf %q "$XDG_RUNTIME_DIR/bin:$PATH")"
    # The names from env (compgen is not in a non-interactive bash, as
    # `nix develop -c` runs); a line of a value with newlines is no name.
    for v in $(env | sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\)=.*/\1/p' | sort -u); do
      [[ -n ${!v+x} ]] || continue
      case $v in
        PATH | TERM | TERM_PROGRAM* | COLORTERM | TERMINFO | KITTY_* | WEZTERM_* | GHOSTTY_* | WINDOWID | _ | SHLVL | PWD | OLDPWD) ;;
        *) printf 'export %s=%q\n' "$v" "${!v}" ;;
      esac
    done
    printf 'cd %q\n' "$PWD"
    # Ghostty runs it through login(1), which leaves "Last login: ..." on the
    # screen: empty, as elsewhere (screen_restored).
    echo "printf '\\033[H\\033[2J'"
    printf 'for ((i = 0; i < 600; i++)); do [[ -e %q ]] && break; sleep 0.05; done\n' "$go"
    printf 'exec %s"$@"\n' "$(printf '%q ' "${clean_env[@]}")"
  } >"$launch"
  chmod +x "$launch"
  front=$("$ctl" front)
  case $term in
    kitty)
      # Cells from the top left (kitty centres the grid in the window).
      app=kitty proc=kitty startup=3
      targs=(--config NONE -o font_size=12 -o "initial_window_width=$width" -o "initial_window_height=$height"
        -o window_padding_width=0 -o placement_strategy=top-left -o hide_window_decorations=yes
        -o remember_window_size=no -o allow_remote_control=socket-only --listen-on "unix:$kitty_socket" "$launch")
      ;;
    ghostty)
      # About the window size asked for (cells of 8x17 px here); no restored
      # windows; Option with
      # the arrows not bound to moving by words (its default on macOS sends
      # ESC b and ESC f instead of Alt+Left and Alt+Right). Blending
      # in linear light as on Linux (linear-corrected, the default there): its
      # default on macOS (native) blends in
      # Display P3, which --render-frame cannot draw (doc/terminal-compat.md).
      app=Ghostty proc=ghostty startup=6
      targs=(--config-default-files=false --window-save-state=never --quit-after-last-window-closed=true
        --alpha-blending=linear-corrected --keybind=alt+left=unbind --keybind=alt+right=unbind
        --window-decoration=false --window-padding-x=0 --window-padding-y=0
        --app-notifications=no-clipboard-copy --background=000000
        "--window-width=$((width / 8))" "--window-height=$((height / 17))" -e "$launch")
      ;;
    wezterm)
      # About the window size asked for: cells of 7x16 px here. With a title
      # bar: without one it takes the top of the window for one, and a drag
      # there (selecting the first row) moves the window.
      app=WezTerm proc=wezterm-gui startup=6
      # WebGPU (Metal): a macOS VM has no OpenGL 3.3 (doc/macos.md).
      targs=(-n --config 'front_end="WebGpu"' --config 'window_decorations="TITLE|RESIZE"' --config enable_tab_bar=false
        --config 'window_padding={left=0,right=0,top=0,bottom=0}'
        --config "initial_cols=$((width / 7))" --config "initial_rows=$((height / 16))"
        start --always-new-process --cwd "$PWD" -- "$launch")
      ;;
    gui)
      "${clean_env[@]}" "$@" >"$outdir/terminal.log" 2>&1 &
      term_pid=$! startup=5
      ;;
    *)
      echo "unknown terminal: $term" >&2
      exit 2
      ;;
  esac
  if [[ $term != gui ]]; then
    before=" $(pgrep -x "$proc" | tr '\n' ' ' || true) "
    # -g: not to the front (Ghostty opens its window only once activated).
    open -g -n -a "$app" --stdout "$outdir/terminal.log" --stderr "$outdir/terminal.log" --args "${targs[@]}" "$@"
  fi
  for ((i = 0; i < 150; i++)); do
    if [[ -z $term_pid ]]; then
      for p in $(pgrep -x "$proc" || true); do [[ $before == *" $p "* ]] || term_pid=$p; done
    fi
    if [[ -n $term_pid ]]; then
      [[ -n $("$ctl" windows "$term_pid") ]] && break
      ((i % 20 == 10)) && [[ $term == ghostty ]] && { "$ctl" activate "$term_pid" >/dev/null || true; }
    fi
    sleep 0.1
  done
  if [[ -z $term_pid || -z $("$ctl" windows "$term_pid") ]]; then
    echo "e2e.sh: $term did not open a window" >&2
    exit 1
  fi
  phase window
  # Below the menu bar; at scale 1 it draws again at that density, then the
  # command starts.
  "$ctl" move "$term_pid" "$disp_x" $((disp_y + menu_bar))
  [[ $("$ctl" front) == "$front" ]] || "$ctl" activate "$front" >/dev/null || true
  sleep 0.5
  touch "$go"
else
case $term in
  kitty)
    "${clean_env[@]}" kitty --config NONE -o font_size=12 -o "initial_window_width=$width" \
      -o "initial_window_height=$height" -o window_padding_width=0 -o hide_window_decorations=yes \
      -o remember_window_size=no -o allow_remote_control=socket-only \
      -o "linux_display_server=$display" --listen-on "unix:$kitty_socket" "$@" >"$outdir/terminal.log" 2>&1 &
    startup=3
    ;;
  ghostty)
    # Ghostty sizes windows in cells; 10x21 px cells at its default font size.
    # No decorations or padding: the terminal area starts at the window's
    # top left, as `expect` needs; no "Copied to clipboard" toast over it.
    # A black background, as kitty's and the mock terminal's (expect in a
    # session): its own dark grey showed in blank cells of terminal panes.
    gdk=()
    [[ $display == wayland ]] && gdk=(GDK_BACKEND=wayland)
    "${clean_env[@]}" "${gdk[@]}" ghostty --config-default-files=false --gtk-single-instance=false \
      --window-decoration=false --window-padding-x=0 --window-padding-y=0 \
      --app-notifications=no-clipboard-copy --background=000000 \
      "--window-width=$((width / 10))" "--window-height=$((height / 21))" -e "$@" >"$outdir/terminal.log" 2>&1 &
    startup=6
    ;;
  gui)
    # A window manager, as on a desktop (decorations, focus); sway is one.
    if [[ $display == x11 ]]; then
      xfwm4 >"$outdir/wm.log" 2>&1 &
      sleep 1
    fi
    gui_env=(VK_DRIVER_FILES="$TEMZDF_LAVAPIPE_ICD")
    if [[ -n $ime ]]; then
      mkdir -p "$outdir/ime/.config/fcitx5"
      printf '[Groups/0]\nName=Default\nDefault Layout=us\nDefaultIM=mozc\n\n[Groups/0/Items/0]\nName=keyboard-us\nLayout=\n\n[Groups/0/Items/1]\nName=mozc\nLayout=\n\n[GroupOrder]\n0=Default\n' \
        >"$outdir/ime/.config/fcitx5/profile"
      # On X11 through XIM; on Wayland sway hands it the keys
      # (input-method-v2) and the GUI gets the text (text-input-v3).
      HOME="$outdir/ime" XDG_CONFIG_HOME="$outdir/ime/.config" fcitx5 -d --replace >"$outdir/ime.log" 2>&1
      sleep 2
      [[ $display == x11 ]] && gui_env+=(XMODIFIERS=@im=fcitx)
    fi
    "${clean_env[@]}" "${gui_env[@]}" "$@" >"$outdir/terminal.log" 2>&1 &
    startup=5
    ;;
  wezterm)
    # -n: ignore the user's wezterm config. The software renderer works on Xvfb.
    # WezTerm sizes windows in cells: 10x22 px at its defaults (more rows would
    # run past the bottom of the virtual screen).
    "${clean_env[@]}" wezterm -n --config 'front_end="Software"' \
      --config "enable_wayland=$([[ $display == wayland ]] && echo true || echo false)" \
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
fi
# Until the first screen is up, at most the time startup used to take.
# Inside a multiplexer the whole of it: temzdf's first images can reach tmux
# before its client is attached, and show only later (the first `expect`
# saw a black screen now and then).
if [[ -n $mux ]]; then
  sleep "$((startup + mux_startup))"
  phase ready
else
  wait_ready $((startup + mux_startup))
fi
# A command that ends at once (a missing file, a bad option) leaves no window
# to wait for.
if ! kill -0 "$term_pid" 2>/dev/null; then
  echo "e2e.sh: $term ended while starting; the end of its output:" >&2
  tail -n 20 "$outdir/terminal.log" >&2
  exit 1
fi
# The GUI's drawing area: where its window's inside is on the screen (on
# macOS every window's: below the menu bar).
if [[ $term == gui ]]; then
  for w in "$@"; do [[ $w == */temzdf-gui ]] && gui_bin=$w && break; done
  temzdf=$(dirname "$gui_bin")/temzdf
fi
if [[ $term == gui || $display == macos ]]; then
  read -r origin_x origin_y win_w win_h <<<"$(be_window_rect "$term_pid")" || true
  if [[ -z ${win_h:-} ]]; then
    echo "e2e.sh: no window of $term" >&2
    exit 1
  fi
fi
if [[ $display == macos ]]; then
  # 2 ms after each event (not 20): typing at about 4 ms a character.
  coproc TC {
    TERMCTL_SETTLE_MS=${E2E_SETTLE_MS:-2} "$ctl" serve "$term_pid" "$([[ $term == wezterm ]] && echo global || echo pid)" \
      2>>"$outdir/termctl.log"
  }
  # In front for the run, as on Linux, where it has the focus: unfocused,
  # Ghostty draws a hollow cursor over the images even where the program
  # hides it.
  tc front
fi
# Without a window manager WezTerm only gets mouse events once focused.
if [[ $term == wezterm && $display == x11 ]]; then
  xdotool search --class wezterm windowfocus 2>/dev/null || true
  sleep 0.3
fi
# The recorded times start here (the first screen is up).
: >"$events"
t0=$(usec)
phase actions
# shellcheck source=/dev/null
source "$actions"
phase quit
crashed=0
kill -0 "$term_pid" 2>/dev/null || crashed=1
be_key q || true
# As long as the terminal takes to go, at most half a second.
for ((i = 0; i < 10; i++)); do kill -0 "$term_pid" 2>/dev/null || break; sleep 0.05; done
cleanup
trap - EXIT
phase done
echo "screenshots in $outdir"
if ((crashed)); then
  echo "TERMINAL CRASHED (see $outdir/terminal.log)"
  exit 3
fi
if [[ -f $outdir/expect.txt ]] && grep -q '^FAIL' "$outdir/expect.txt"; then
  echo "EXPECTATIONS FAILED (see $outdir/expect.txt)"
  exit 4
fi
