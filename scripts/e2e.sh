#!/usr/bin/env bash
# End-to-end harness: run a command inside a real terminal (kitty, Ghostty or
# WezTerm) on a virtual X server (or headless sway, -d wayland; a virtual
# display on macOS, -d macos), drive it with an actions script, and save
# screenshots.
#
# Usage: scripts/e2e.sh [-t kitty|ghostty|wezterm|gui] [-d x11|wayland|macos|windows] [-m zellij|tmux] [-I] [-P shared|socket] [-s WxH] [-o OUTDIR] ACTIONS -- COMMAND [ARGS...]
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
#             (doc/terminals/macos.md); windows (-t gui only): temzdf-gui
#             in the Windows VM of scripts/windows-vm.sh
#             ($TEMZDF_WINDOWS_VM, running, with temzdf and temzdf-gui
#             built in it), on its desktop, driven from this host: the
#             pointer and keys through QEMU's QMP (socat), screenshots with
#             QMP's screendump, commands and files over SSH. COMMAND is
#             the guest's temzdf-gui.exe (e.g.
#             C:/t/target/release/temzdf-gui.exe --clean FILE); arguments
#             naming files of this host are copied into the guest first
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
#   wait_still [S]     until the screen stands still (unchanged across three
#                      screenshots 0.3 s apart), at most S seconds (default
#                      20; past that FAIL wait_still in OUTDIR/expect.txt):
#                      for judging a screen once something moving on its own
#                      (autoscroll) has stopped, however long it took
#   forward LINE[:COLUMN]:FILE PDF  a SyncTeX forward search from another
#                      process (`temzdf --forward`, as an editor sends it;
#                      the COMMAND's temzdf), recorded for `expect` like
#                      the input; fails (under set -e: stops the run) when
#                      the command does, so wrap it in `check NAME` to judge
#                      it
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
#                      [--page N] [--scale S] [--config FILE] FILE (the
#                      headless side loads that configuration too; else
#                      --clean; possibly after
#                      `env VAR=...`) and terminals
#                      drawing from the window's top-left (kitty, WezTerm,
#                      Ghostty without decorations). The images are compared
#                      by pixels outside the cells the terminal draws itself
#                      (the status line; NAME.mask.png), and in kitty and
#                      WezTerm the text too (NAME.expected.txt), as for
#                      sessions below. Results go to
#                      OUTDIR/expect.txt, mismatches to NAME.diff.png and
#                      NAME.text.diff; the recorded input to OUTDIR/events.txt.
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
#                      Both (not the GUI's, not inside multiplexers) also
#                      write the cells the terminal draws itself as bytes
#                      (NAME.ansi: positions, SGR, text; --ansi) and which
#                      of them to judge (NAME.cells.png: not the cursor's,
#                      nor default-background ones over images; --cells).
#                      After the actions, temzdf is ended (Esc until the
#                      viewer quits; a session's killed) and the shell
#                      COMMAND leaves behind (`sh -c '...; exec sh'`) draws
#                      each NAME.ansi on a cleared screen: NAME.ref.png,
#                      once it stands still. NAME.png inside NAME.cells.png
#                      against it: PASS or FAIL NAME.cells (the attributes
#                      and colours of those cells, and their text in
#                      Ghostty too; NAME.cells.diff.png), SKIP without such
#                      a shell or when the terminal changed size since.
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
#   same_screen SHOT REF  whether screenshot SHOT shows what `expect REF`
#                      saw, outside the cells the terminal draws itself
#                      (REF's mask: the status line), e.g. `check same
#                      same_screen truncated before`: the images stayed
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
# A COMMAND that runs temzdf must pass --clean or --config FILE (checked), so
# scenarios see the shipped defaults (or a configuration of the
# repository's) rather than the user's config.ts.
set -euo pipefail

# shellcheck source=SCRIPTDIR/lib/devshell.sh
source "$(dirname "$(realpath "${BASH_SOURCE[0]}")")/lib/devshell.sh" . "$@"

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
# Runs of temzdf take the shipped defaults (--clean) or a configuration of
# the repository's (--config FILE), not the user's config.ts. A word naming
# temzdf or temzdf-gui (also inside `sh -c '...'`; Windows' too:
# C:\...\temzdf-gui.exe), not any path with "temzdf" in it.
runs_temzdf() { grep -qE "(^|[ /'\"\\\\])temzdf(-gui)?(\.exe)?( |\$)" <<<"$*"; }
if runs_temzdf "$@" && [[ "$*" != *--clean* && "$*" != *--config\ * ]]; then
  echo "e2e.sh: COMMAND runs temzdf without --clean or --config (it would load your config.ts)" >&2
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
  windows)
    if [[ $term != gui || -n $ime || -n $mux ]]; then
      echo "e2e.sh: -d windows runs -t gui only, without -I or -m" >&2
      exit 2
    fi
    ;;
  *)
    echo "e2e.sh: unknown display server $display (x11, wayland, macos or windows)" >&2
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
# The pointer to (X, Y) of the terminal area.
be_move_at() { be_move "$(($1 + origin_x))" "$(($2 + origin_y))"; }
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
  # A locked screen takes no screenshots ("could not create image from
  # rect"): every run would fail while starting.
  # (Not `ioreg | grep -q`: with pipefail, ioreg's SIGPIPE fails it.)
  if [[ $(ioreg -n Root -d1) == *'"IOConsoleLocked" = Yes'* ]]; then
    echo "e2e.sh: the screen is locked; unlock it (or use scripts/macos-vm.sh)" >&2
    exit 2
  fi
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
      bracketleft) c="[" ;; bracketright) c="]" ;; braceleft) c="{" ;; braceright) c="}" ;;
      parenleft) c="(" ;; parenright) c=")" ;; backslash) c="\\" ;; bar) c="|" ;; apostrophe) c="'" ;;
      quotedbl) c='"' ;; grave) c='`' ;; asciitilde) c="~" ;; exclam) c="!" ;; at) c=@ ;;
      numbersign) c="#" ;; dollar) c='$' ;; asciicircum) c="^" ;; ampersand) c="&" ;; asterisk) c="*" ;;
      underscore) c=_ ;;
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

# Windows (the VM of scripts/windows-vm.sh, connected below): the pointer
# and keys as QEMU's USB tablet and keyboard through QMP (QMP, a socat
# coprocess on the harness's own QMP socket; raw JSON lines, as vitro's
# src/qmp.rs sends them), screenshots with QMP's screendump (QEMU runs
# here, so it writes them here), commands through one PowerShell in the
# guest over SSH (GPS; a PowerShell started per command takes 1.5 s), files
# with scp. What needs the desktop (the GUI, the clipboard) runs as a
# scheduled task on it (on_desktop): SSH's session has none. The guest's
# directory for the run is $gdir.
if [[ $display == windows ]]; then
  gdir=C:/e2e
  # A JSON line to QMP; its answer, skipping the events in between.
  qmp() {
    local line
    printf '%s\n' "$1" >&"${QMP[1]}"
    while IFS= read -r -t 10 line <&"${QMP[0]}"; do
      case $line in
        '{"return"'*) return 0 ;;
        '{"error"'*) echo "e2e.sh: QMP: $line ($1)" >&2; return 1 ;;
      esac
    done
    echo "e2e.sh: no answer over QMP ($1)" >&2
    return 1
  }
  qmp_events() { qmp "{\"execute\":\"input-send-event\",\"arguments\":{\"events\":[$1]}}"; }
  # Each event its own call, the next one 30 ms later: events sent together
  # arrive as one HID report (a press and its release as no press at all,
  # vitro's src/qmp.rs).
  qmp_gap() { sleep 0.03; }
  # A pixel of the screen on the tablet's axis (0 to 0x7fff over the
  # screen): its middle, whichever way the guest rounds.
  tablet() { echo $(((2 * $1 + 1) * 32768 / (2 * $2))); }
  be_move() {
    qmp_events "{\"type\":\"abs\",\"data\":{\"axis\":\"x\",\"value\":$(tablet "$1" "$screen_w")}},{\"type\":\"abs\",\"data\":{\"axis\":\"y\",\"value\":$(tablet "$2" "$screen_h")}}"
    ptr_x=$1 ptr_y=$2
  }
  qmp_button() {
    case $1 in
      1) echo left ;; 2) echo middle ;; 3) echo right ;; 4) echo wheel-up ;; 5) echo wheel-down ;;
      *) echo "bad button $1" >&2; exit 2 ;;
    esac
  }
  qmp_press() { qmp_events "{\"type\":\"btn\",\"data\":{\"down\":$1,\"button\":\"$2\"}}"; }
  be_button() {
    qmp_press "$([[ $1 == down ]] && echo true || echo false)" "$(qmp_button "$2")"
    qmp_gap
  }
  # A wheel notch is a press and a release of a button, too.
  be_clicks() {
    local i b
    b=$(qmp_button "$1")
    for ((i = 0; i < $2; i++)); do
      ((i == 0)) || sleep "$(printf '%d.%03d' $(($3 / 1000)) $(($3 % 1000)))"
      qmp_press true "$b"
      qmp_press false "$b"
    done
    qmp_gap
  }
  # QEMU's key names (qcodes) for an XKB key name with its modifiers
  # (ctrl+shift+a, question: shift slash), on the US layout.
  qcodes() {
    local k=$1 held=() c
    while [[ $k == *+?* ]]; do
      case ${k%%+*} in
        ctrl | Control_L) held+=(ctrl) ;; shift | Shift_L) held+=(shift) ;; alt | Alt_L) held+=(alt) ;;
        super) held+=(meta_l) ;;
        *) echo "e2e.sh: no qcode for ${k%%+*}" >&2; exit 2 ;;
      esac
      k=${k#*+}
    done
    case $k in
      Return) c=ret ;; Escape) c=esc ;; Tab) c=tab ;; BackSpace) c=backspace ;; Delete) c=delete ;;
      space) c=spc ;; Up | Down | Left | Right | Home | End) c=${k,,} ;; Prior | Page_Up) c=pgup ;;
      Next | Page_Down) c=pgdn ;; Insert) c=insert ;; F[0-9] | F1[0-2]) c=${k,,} ;;
      ctrl | Control_L) c=ctrl ;; shift | Shift_L) c="shift" ;; alt | Alt_L) c=alt ;;
      [a-z0-9]) c=$k ;; [A-Z]) held+=(shift) c=${k,,} ;;
      minus) c=minus ;; equal) c=equal ;; slash) c=slash ;; comma) c=comma ;; period) c="dot" ;;
      semicolon) c=semicolon ;; apostrophe) c=apostrophe ;; grave) c=grave_accent ;;
      bracketleft) c=bracket_left ;; bracketright) c=bracket_right ;; backslash) c=backslash ;;
      plus) held+=(shift) c=equal ;; question) held+=(shift) c=slash ;; less) held+=(shift) c=comma ;;
      greater) held+=(shift) c=dot ;; colon) held+=(shift) c=semicolon ;; percent) held+=(shift) c=5 ;;
      underscore) held+=(shift) c=minus ;; exclam) held+=(shift) c=1 ;; at) held+=(shift) c=2 ;;
      numbersign) held+=(shift) c=3 ;; dollar) held+=(shift) c=4 ;; asciicircum) held+=(shift) c=6 ;;
      ampersand) held+=(shift) c=7 ;; asterisk) held+=(shift) c=8 ;; parenleft) held+=(shift) c=9 ;;
      parenright) held+=(shift) c=0 ;; braceleft) held+=(shift) c=bracket_left ;;
      braceright) held+=(shift) c=bracket_right ;; bar) held+=(shift) c=backslash ;;
      quotedbl) held+=(shift) c=apostrophe ;; asciitilde) held+=(shift) c=grave_accent ;;
      *) echo "e2e.sh: no qcode for $k" >&2; exit 2 ;;
    esac
    echo "${held[*]} $c"
  }
  qmp_key() { qmp_events "{\"type\":\"key\",\"data\":{\"down\":$1,\"key\":{\"type\":\"qcode\",\"data\":\"$2\"}}}"; }
  modifier() { [[ $1 == ctrl || $1 == shift || $1 == alt || $1 == meta_l ]]; }
  # A modifier and the key or button after it reach the guest as separate
  # devices' reports: 150 ms apart, or they come in either order (vitro).
  be_keydown() {
    local q
    for q in $(qcodes "$1"); do
      qmp_key true "$q"
      if modifier "$q"; then sleep 0.15; else sleep 0.02; fi
    done
  }
  be_keyup() {
    local q qs
    read -ra qs <<<"$(qcodes "$1")"
    for ((q = ${#qs[@]} - 1; q >= 0; q--)); do
      modifier "${qs[q]}" && sleep 0.15
      qmp_key false "${qs[q]}"
    done
    qmp_gap
  }
  be_key() { be_keydown "$1"; be_keyup "$1"; }
  # The XKB name of a character typed (a letter as itself).
  char_key() {
    case $1 in
      ' ') echo space ;; '-') echo minus ;; '=') echo equal ;; '/') echo slash ;; ',') echo comma ;;
      '.') echo period ;; ';') echo semicolon ;; "'") echo apostrophe ;; '`') echo grave ;;
      '[') echo bracketleft ;; ']') echo bracketright ;; "\\") echo backslash ;; '+') echo plus ;;
      '?') echo question ;; '<') echo less ;; '>') echo greater ;; ':') echo colon ;; '%') echo percent ;;
      '_') echo underscore ;; '!') echo exclam ;; '@') echo at ;; '#') echo numbersign ;; '$') echo dollar ;;
      '^') echo asciicircum ;; '&') echo ampersand ;; '*') echo asterisk ;; '(') echo parenleft ;;
      ')') echo parenright ;; '{') echo braceleft ;; '}') echo braceright ;; '|') echo bar ;;
      '"') echo quotedbl ;; '~') echo asciitilde ;; $'\t') echo Tab ;;
      *) echo "$1" ;;
    esac
  }
  be_type() {
    local i
    for ((i = 0; i < ${#1}; i++)); do
      ((i == 0 || ${2:-0} == 0)) || sleep "$(printf '%d.%03d' $(($2 / 1000)) $(($2 % 1000)))"
      be_key "$(char_key "${1:i:1}")"
    done
  }
  be_shot() { qmp "{\"execute\":\"screendump\",\"arguments\":{\"filename\":\"$1\",\"format\":\"png\"}}"; }
  # One line of PowerShell in the guest (GPS): its output; fails with the
  # exit status of the last native program it ran, or 1 when it threw. The
  # output goes out once the answer is in: a reader that stops early (head)
  # cannot leave the rest of it in the pipe for the next command.
  gps() {
    local line out=''
    printf '%s\n' "\$LASTEXITCODE = 0; try { $1 } catch { [Console]::Error.WriteLine(\$_); \$LASTEXITCODE = 1 }; \"@@done \$LASTEXITCODE\"" >&"${GPS[1]}"
    while IFS= read -r -t 300 line <&"${GPS[0]}"; do
      line=${line%$'\r'}
      if [[ $line == '@@done '* ]]; then
        printf %s "$out"
        return "$((${line#@@done } != 0))"
      fi
      out+=$line$'\n'
    done
    echo "e2e.sh: no answer from the guest ($1)" >&2
    return 1
  }
  # (The command is the guest's PowerShell text, made here.)
  # shellcheck disable=SC2029
  gssh() { ssh "${ssh_opts[@]}" "$ssh_host" "$@"; }
  gcp_in() { scp -q "${ssh_opts[@]}" "$1" "$ssh_host:$2"; }
  gcp_out() { scp -q "${ssh_opts[@]}" "$ssh_host:$1" "$2"; }
  # Run the guest's cmd.exe line CMD on the desktop of the user logged on,
  # without a console window (conhost --headless), as the scheduled task
  # e2e-NAME; returns once it started.
  on_desktop() {
    gps "schtasks /create /tn e2e-$1 /tr 'conhost.exe --headless $2' /sc once /st 00:00 /it /f >\$null 2>&1; schtasks /run /tn e2e-$1 >\$null"
  }
  # Until guest file FILE exists, at most S seconds.
  guest_wait() {
    gps "\$n = 0; while (-not (Test-Path $1) -and \$n -lt $(($2 * 10))) { Start-Sleep -Milliseconds 100; \$n++ }; if (-not (Test-Path $1)) { \$LASTEXITCODE = 1 }"
  }
  # The clipboard through PowerShell's Get-Clipboard and Set-Clipboard on
  # the desktop (clip.ps1, written below), as UTF-8 files.
  be_clipboard() {
    gps "Remove-Item $gdir/clip.txt -ErrorAction SilentlyContinue" &&
      on_desktop clip "powershell.exe -NoProfile -ExecutionPolicy Bypass -File $gdir/clip.ps1 get" &&
      guest_wait "$gdir/clip.txt" 10 &&
      gcp_out "$gdir/clip.txt" "$XDG_RUNTIME_DIR/clip.txt" && cat "$XDG_RUNTIME_DIR/clip.txt"
  }
  be_set_clipboard() {
    printf %s "$1" >"$XDG_RUNTIME_DIR/clip.txt"
    gps "Remove-Item $gdir/clip.done -ErrorAction SilentlyContinue" &&
      gcp_in "$XDG_RUNTIME_DIR/clip.txt" "$gdir/clip-in.txt" &&
      on_desktop clip "powershell.exe -NoProfile -ExecutionPolicy Bypass -File $gdir/clip.ps1 set" &&
      guest_wait "$gdir/clip.done" 10
  }
  # Where the drawing area is: what temzdf-gui wrote (TEMZDF_GUI_GEOMETRY:
  # X Y W H SCALE, in pixels on the screen).
  be_window_rect() {
    local x y w h s
    read -r x y w h s < <(gps "Get-Content $gdir/geometry.txt") || return 1
    [[ -n ${s:-} ]] || return 1
    echo "$x $y $w $h"
  }
  # A screenshot of the drawing area only (the desktop around it has a
  # clock).
  win_shot() {
    be_shot "$1.full.png" && magick "$1.full.png" -crop "${win_w}x${win_h}+$origin_x+$origin_y" +repage -strip "$1" &&
      rm -f "$1.full.png"
  }
  # The guest's logs and what temzdf-gui recorded, into OUTDIR (as on the
  # other displays).
  fetch_logs() {
    gcp_out "$gdir/terminal.log" "$outdir/terminal.log" 2>/dev/null || true
    gcp_out "$gdir/input.txt" "$outdir/input.txt" 2>/dev/null || true
    mkdir -p "$outdir/state"
    scp -q -r "${ssh_opts[@]}" "$ssh_host:$gdir/state/temzdf" "$outdir/state/" 2>/dev/null || true
  }
  be_focus() { echo "e2e.sh: focus_window is not there on Windows yet" >&2; return 1; }
  be_other_window() { echo "e2e.sh: other_window is not there on Windows yet" >&2; return 1; }
fi

shot() { be_shot "$outdir/$1.png"; }
wheel() {
  local n=$1 x=${2:-$((width / 2))} y=${3:-$((height / 2))} mods=${4:-} btn=5 dir=down k
  if ((n < 0)); then btn=4 dir=up n=$((-n)); fi
  be_move_at "$x" "$y"
  record move "$(at "$x" "$y")" ${mods:+"$mods"}
  mods_down "$mods"
  # One call: separate ones take about 0.1 s each, too slow for a flick.
  # The steps are recorded at the times it spaces them.
  local first=$((($(usec) - t0) / 1000)) i
  be_clicks "$btn" "$n" 30
  for ((i = 0; i < n; i++)); do record -at $((first + 30 * i)) wheel "$dir" "$(at "$x" "$y")" ${mods:+"$mods"}; done
  mods_up "$mods"
}
# The button held by `down` (and its MODS), for `move` and `up`.
held_btn='' held_mods='' pointer_x=0 pointer_y=0
down() {
  local x=$1 y=$2 btn=${3:-1} mods=${4:-} k
  be_move_at "$x" "$y"
  record move "$(at "$x" "$y")" ${mods:+"$mods"}
  mods_down "$mods"
  be_button down "$btn"
  record press "$(at "$x" "$y")" "$(button_name "$btn")" ${mods:+"$mods"}
  held_btn=$btn held_mods=$mods pointer_x=$x pointer_y=$y
}
up() {
  local btn=${1:-${held_btn:-1}} k
  be_button up "$btn"
  record release "$(at "$pointer_x" "$pointer_y")" "$(button_name "$btn")" ${held_mods:+"$held_mods"}
  mods_up "$held_mods"
  held_btn='' held_mods=''
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
  be_move_at "$x" "$y"
  record move "$(at "$x" "$y")" ${mods:+"$mods"}
  mods_down "$mods"
  be_clicks "$btn" 1 0
  record press "$(at "$x" "$y")" "$b" ${mods:+"$mods"}
  record release "$(at "$x" "$y")" "$b" ${mods:+"$mods"}
  mods_up "$mods"
}
# The pointer: a drag while `down` holds a button, else hovering.
move() {
  be_move_at "$1" "$2"
  pointer_x=$1 pointer_y=$2
  if [[ -n $held_btn ]]; then
    record drag "$(at "$1" "$2")" "$(button_name "$held_btn")" ${held_mods:+"$held_mods"}
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
forward() {
  local spec=$1 pdf=$2 line col file
  "$forward_temzdf" --forward "$spec" "$pdf" || return
  # Recorded once sent (when temzdf has it), as LINE COLUMN FILE.
  IFS=: read -r line col file <<<"$spec"
  [[ -n $file ]] || { file=$col col=0; }
  record forward "$line" "$col" "$file"
}
ime() {
  case $1 in
    on) fcitx5-remote -o ;;
    off) fcitx5-remote -c ;;
  esac
}
# Ghostty on macOS shows the pixels of images as Display P3 values (no
# conversion from sRGB): saturated colours come out more saturated. The
# expected screen FILE goes through the same, so the comparison stays exact
# (doc/terminals/terminal-compat.md).
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
# Where the screenshots show the pointer and the harness knows where it is
# (ptr_x, ptr_y on the screen: the Windows backend sets them; X11, Wayland
# and macOS take screenshots without it): a square around it goes black on
# both sides of NAME's comparison, 48 px at scale 1 with the hot spot 16 px
# in from its top-left, which holds Windows' standard pointers (the arrow
# is 12x19 from its hot spot down to the right; the hand, the I-beam and
# the resizing arrows reach left of it and above). Moving the pointer off
# the window instead would change the drawing (temzdf sees it leave). A
# transparent pointer (scripts/windows-vm.sh cursor hidden) is masked too.
ptr_x='' ptr_y=''
mask_pointer() {
  [[ -n $ptr_x ]] || return 0
  local name=$1 x=$((ptr_x - origin_x)) y=$((ptr_y - origin_y)) a b f
  read -r a b <<<"$(awk -v s="${win_scale:-1}" 'BEGIN { print int(16 * s + 0.999), int(32 * s + 0.999) }')"
  ((x + b > 0 && y + b > 0 && x - a < win_w && y - a < win_h)) || return 0
  for f in actual expected; do
    magick "$outdir/$name.$f.png" -fill black -draw "rectangle $((x - a)),$((y - a)) $((x + b - 1)),$((y + b - 1))" \
      "$outdir/$name.$f.png"
  done
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
      if (($(usec) >= deadline)) || ! kill -0 "$term_pid" 2>/dev/null; then break; fi
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
  [[ $display == windows ]] && shoot=(win_shot)
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
wait_still() {
  local limit=${1:-20} deadline still=0 a=$outdir/still.a.png b=$outdir/still.b.png
  deadline=$(($(usec) + limit * 1000000))
  local shoot=(be_shot)
  [[ $display == macos ]] &&
    shoot=(screencapture -x -R "$disp_x,$((disp_y + menu_bar)),$width,$height")
  [[ $display == windows ]] && shoot=(win_shot)
  "${shoot[@]}" "$a"
  while (($(usec) < deadline)); do
    sleep 0.3
    "${shoot[@]}" "$b"
    if cmp -s "$a" "$b"; then
      if ((++still == 2)); then
        rm -f "$a" "$b"
        phase still
        return 0
      fi
    else
      still=0
    fi
    mv "$b" "$a"
  done
  rm -f "$a" "$b"
  phase "still (timed out)"
  # (Not a failed status: the actions run under set -e; the FAIL line
  # fails the run at its end.)
  echo "FAIL wait_still: the screen did not stand still within ${limit}s" | tee -a "$outdir/expect.txt"
}
# The expects whose cells draw_references judges: "NAME COLS ROWS CW CH".
refs=()
# Whether temzdf of this run still runs (a viewer or a session server):
# known by its environment.
temzdf_alive() {
  local p comm
  if [[ -d /proc/self ]]; then
    for p in /proc/[0-9]*; do
      # (read, not a process per entry: this runs in loops.)
      [[ -O $p ]] && read -r comm 2>/dev/null <"$p/comm" && [[ $comm == temzdf ]] || continue
      tr '\0' '\n' 2>/dev/null <"$p/environ" | grep -qxF "XDG_STATE_HOME=$outdir/state" && return 0
    done
    return 1
  fi
  # macOS: ps -E adds the environment to the command line.
  ps -Eww -U "$(id -u)" -o comm=,command= | awk '$1 ~ /(^|\/)temzdf$/' | grep -qF "XDG_STATE_HOME=$outdir/state"
}
# Until a file the shell writes appears (it ran what was typed), at most S
# seconds.
wait_file() {
  local i
  for ((i = 0; i < $2 * 20; i++)); do
    [[ -e $1 ]] && return 0
    sleep 0.05
  done
  return 1
}
# Judge each expect's cells (refs) once the actions are done: end temzdf,
# have the shell left behind draw NAME.ansi (the cells the terminal should
# draw itself, cursor hidden) on a cleared screen, take NAME.ref.png once it
# stands still, and compare the screenshot taken at the expect inside
# NAME.cells.png: PASS or FAIL NAME.cells in OUTDIR/expect.txt. The
# terminal's own drawing, so its fonts and colours are no matter; what
# counts is that it showed the right cells with the right attributes during
# the run. Also where the terminal cannot tell its text (Ghostty).
draw_references() {
  local ref name cols rows cw ch was dir i k out cells
  ((${#refs[@]} > 0)) || return 0
  if [[ -z $has_shell ]]; then
    for ref in "${refs[@]}"; do
      echo "SKIP ${ref%% *}.cells: no shell after the command (sh -c '...; exec sh')" | tee -a "$outdir/expect.txt"
    done
    return
  fi
  phase references
  if [[ -n $session_mode ]]; then
    for i in $("$temzdf" ls 2>/dev/null | cut -d' ' -f1); do "$temzdf" kill-session "$i" || true; done
  else
    # Esc closes the input line, the sidebar, the selection and the search,
    # then the pane (the viewer ends).
    for ((i = 0; i < 6; i++)); do
      temzdf_alive || break
      be_key Escape
      for ((k = 0; k < 10; k++)); do temzdf_alive || break 2; sleep 0.05; done
    done
  fi
  for ((i = 0; i < 100; i++)); do temzdf_alive || break; sleep 0.05; done
  if temzdf_alive; then
    for ref in "${refs[@]}"; do
      echo "FAIL ${ref%% *}.cells: temzdf did not end" | tee -a "$outdir/expect.txt"
    done
    return
  fi
  # Short names to type: the shell runs in the run's directory, and `r NAME`
  # draws NAME.ansi on a cleared screen, then says so (NAME.ok), and waits
  # (`read`: no prompt over the cells until the next one).
  dir=$outdir
  [[ $dir == "$PWD"/* ]] && dir=${dir#"$PWD"/}
  rm -f "$outdir/shell.ok"
  type "cd $dir && r() { clear; cat \$1.ansi; : >\$1.ok; read x; } && : >shell.ok
"
  if ! wait_file "$outdir/shell.ok" 10; then
    shot no-shell
    for ref in "${refs[@]}"; do
      echo "FAIL ${ref%% *}.cells: the shell did not answer" | tee -a "$outdir/expect.txt"
    done
    return
  fi
  for ref in "${refs[@]}"; do
    read -r name cols rows cw ch <<<"$ref"
    # The terminal's grid now: another size cannot draw the same cells.
    was="$cols $rows $cw $ch"
    if ! read_geometry || [[ "$cols $rows $cw $ch" != "$was" ]]; then
      echo "SKIP $name.cells: the terminal changed size since" | tee -a "$outdir/expect.txt"
      continue
    fi
    # Pixels of the cells compared (opaque in the mask).
    cells=$(magick "$outdir/$name.cells.png" -alpha extract -format '%[fx:round(mean*w*h)]' info:)
    ((cells > 0)) || continue
    phase "reference $name"
    rm -f "$outdir/$name.ok"
    type "r $name
"
    if ! wait_file "$outdir/$name.ok" 10; then
      echo "FAIL $name.cells: the shell did not draw $name.ansi" | tee -a "$outdir/expect.txt"
      continue
    fi
    wait_still 10
    shot "$name.ref"
    phase "referenced $name"
    be_key Return
    # Both inside the cells compared (elsewhere black), from the grid's
    # top-left, as expect crops.
    for i in "$name" "$name.ref"; do
      magick "$outdir/$i.png" -crop "$((cols * cw))x$((rows * ch))+$origin_x+$origin_y" +repage \
        \( "$outdir/$name.cells.png" -alpha extract \) -compose multiply -composite \
        "$outdir/$i.cells.png.tmp.png"
      [[ -f $outdir/$name.window-mask.png ]] && magick "$outdir/$i.cells.png.tmp.png" \
        "$outdir/$name.window-mask.png" -compose over -composite "$outdir/$i.cells.png.tmp.png"
    done
    mv "$outdir/$name.cells.png.tmp.png" "$outdir/$name.cells.actual.png"
    mv "$outdir/$name.ref.cells.png.tmp.png" "$outdir/$name.cells.ref.png"
    out=$(magick compare -metric AE -fuzz 3% "$outdir/$name.cells.actual.png" "$outdir/$name.cells.ref.png" \
      "$outdir/$name.cells.diff.png" 2>&1 || true)
    out=${out%% *}
    if [[ $out == 0 ]]; then
      rm -f "$outdir/$name.cells.diff.png"
      echo "PASS $name.cells ($((cells / (cw * ch))) cells)" | tee -a "$outdir/expect.txt"
    else
      echo "FAIL $name.cells: $out pixels differ from this terminal's drawing of $name.ansi (see $name.cells.diff.png)" |
        tee -a "$outdir/expect.txt"
    fi
  done
}
# The GUI's form of expect: the window against an offscreen render of the
# same session at this moment.
expect_gui() {
  local name=$1 session out
  shot "$name"
  if [[ $display == windows ]]; then
    # In the guest, at the window's scale; the picture comes back here.
    session=$(gps "& '$temzdf' ls" | head -1 | cut -d' ' -f1)
    phase "expect $name"
    if ! gps "& '${E2E_GUI_RENDERER:-$gui_bin}' --render-frame $gdir/out/$name.expected.png -s '$session' --size ${win_w}x${win_h} --scale $win_scale 2>&1 | Out-File -Encoding utf8 $gdir/out/$name.expected.log" ||
      ! gcp_out "$gdir/out/$name.expected.png" "$outdir/$name.expected.png"; then
      gcp_out "$gdir/out/$name.expected.log" "$outdir/$name.expected.log" || true
      echo "FAIL $name: --render-frame failed (see $name.expected.log)" | tee -a "$outdir/expect.txt"
      return
    fi
  else
    session=$("$temzdf" ls | head -1 | cut -d' ' -f1)
    phase "expect $name"
    "${clean_env[@]}" VK_DRIVER_FILES="${TEMZDF_LAVAPIPE_ICD:-}" "${E2E_GUI_RENDERER:-$gui_bin}" --render-frame "$outdir/$name.expected.png" \
      -s "$session" --size "${win_w}x${win_h}" 2>"$outdir/$name.expected.log" || {
      echo "FAIL $name: --render-frame failed (see $name.expected.log)" | tee -a "$outdir/expect.txt"
      return
    }
  fi
  phase "expected $name"
  magick "$outdir/$name.png" -crop "${win_w}x${win_h}+$origin_x+$origin_y" +repage "$outdir/$name.actual.png"
  mask_window_edges "$name" "$win_w" "$win_h"
  mask_pointer "$name"
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
    --ansi "$outdir/$name.ansi" --cells "$outdir/$name.cells.png" 2>"$outdir/$name.expected.log" || {
    echo "FAIL $name: render-session failed (see $name.expected.log)" | tee -a "$outdir/expect.txt"
    return
  }
  phase "expected $name"
  refs+=("$name $cols $rows $cw $ch")
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
        local socks=("$XDG_RUNTIME_DIR"/wezterm/gui-sock-*)
        WEZTERM_UNIX_SOCKET=${socks[0]} wezterm cli get-text
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
# Whether screenshot SHOT shows what `expect REF` saw (REF.actual.png),
# outside REF's mask (and the window's edges on macOS); the comparison goes
# to SHOT.diff.png.
same_screen() {
  local shot=$1 ref=$2 out
  read_geometry || return 1
  [[ -f $outdir/$ref.actual.png && -f $outdir/$ref.mask.png ]] || return 1
  magick "$outdir/$shot.png" -crop "$((cols * cw))x$((rows * ch))+$origin_x+$origin_y" +repage \
    "$outdir/$ref.mask.png" -composite "$outdir/$shot.actual.png"
  if [[ -f $outdir/$ref.window-mask.png ]]; then
    magick "$outdir/$shot.actual.png" "$outdir/$ref.window-mask.png" -composite "$outdir/$shot.actual.png"
  fi
  out=$(magick compare -quiet -metric AE -fuzz 3% "$outdir/$shot.actual.png" "$outdir/$ref.actual.png" \
    "$outdir/$shot.diff.png" 2>&1 || true)
  [[ ${out%% *} == 0 ]] && rm -f "$outdir/$shot.diff.png"
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
  local name=$1 keys=${2:-} input geo cols rows cw ch out failed=
  if (($# >= 2)); then
    input=("--keys=$keys")
  else
    # Everything sent so far, then the moment of the screenshot.
    if [[ -n $mux ]] && grep -qvE '^[0-9]+ (key|forward) ' "$events"; then
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
    --cell "${cw}x${ch}" --text "$outdir/$name.expected.txt" --mask "$outdir/$name.mask.png" \
    --ansi "$outdir/$name.ansi" --cells "$outdir/$name.cells.png" \
    "${input[@]}" "$expect_file" 2>"$outdir/$name.expected.log" || {
    echo "FAIL $name: --render-frame failed (see $name.expected.log)" | tee -a "$outdir/expect.txt"
    return
  }
  phase "expected $name"
  # Its cells are judged at the end, against this terminal's own drawing of
  # them (draw_references); not inside a multiplexer, which draws them
  # itself.
  [[ -z $mux ]] && refs+=("$name $cols $rows $cw $ch")
  # The terminal may have spare pixels right and below the cell grid. The
  # cells it draws itself (the status line) go black on both sides, as for
  # sessions; their text is compared where the terminal can tell it.
  magick "$outdir/$name.png" -crop "$((cols * cw))x$((rows * ch))+$origin_x+$origin_y" +repage \
    "$outdir/$name.mask.png" -composite "$outdir/$name.actual.png"
  as_terminal_shows "$outdir/$name.expected.png"
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

# Some terminals (WezTerm) look the program up only in PATH: make it absolute.
if [[ "$1" == */* && -e "$1" ]]; then
  set -- "$(realpath "$1")" "${@:2}"
fi
# For `expect`: the headless renderer with the app's document and view options.
# For `forward`: the COMMAND's temzdf (else the one in PATH).
expect_cmd=() expect_file='' forward_temzdf=temzdf
args=("$@")
while [[ ${#args[@]} -gt 0 && ( ${args[0]} == env || ${args[0]} == *=* ) ]]; do args=("${args[@]:1}"); done
# `sh -c 'CMD...; exec sh'`: CMD's words (quotes as the shell reads them),
# and a shell is there after it (has_shell: `expect` has the terminal draw
# its references there).
has_shell=
if ((${#args[@]} >= 3)) && [[ ${args[0]} == sh || ${args[0]} == */sh ]] && [[ ${args[1]} == -c ]]; then
  [[ ${args[2]} =~ \;[[:space:]]*exec\ sh[[:space:]]*$ ]] && has_shell=1
  mapfile -t args < <(printf '%s' "${args[2]%%;*}" | xargs printf '%s\n')
  while [[ ${#args[@]} -gt 0 && ( ${args[0]} == env || ${args[0]} == *=* ) ]]; do args=("${args[@]:1}"); done
fi
if ((${#args[@]} > 0)); then
  [[ ${args[0]} == */temzdf ]] && forward_temzdf=${args[0]}
  expect_cmd=("${E2E_RENDERER:-${args[0]}}" standalone pdf)
  expect_file=${args[-1]}
  for ((i = 1; i < ${#args[@]} - 1; i++)); do
    case ${args[i]} in
      --page | --scale | --config) expect_cmd+=("${args[i]}" "${args[i + 1]}") ;;
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
# bytes, doc/terminals/macos.md).
if [[ $display == macos ]]; then
  XDG_RUNTIME_DIR=$(mktemp -d /tmp/temzdf-e2e.XXXXXX)
else
  XDG_RUNTIME_DIR=$(mktemp -d "${TMPDIR:-/tmp}/temzdf-e2e.XXXXXX")
fi
export XDG_RUNTIME_DIR
# kitty answers `kitten @ get-text` (expect in a session) here.
kitty_socket=$XDG_RUNTIME_DIR/kitty.sock
term_pid='' sway_pid='' vdisp_pid='' user_clipboard='' QMP=() GPS=()
cleanup() {
  if [[ -n ${GPS[1]:-} ]]; then
    # What the run left in the guest: the GUI, its sessions, the tasks.
    gps "taskkill /F /IM temzdf-gui.exe >\$null 2>&1; foreach (\$l in @(& '$temzdf' ls 2>\$null)) { & '$temzdf' kill-session (\$l -split ' ')[0] >\$null 2>&1 }; schtasks /delete /tn e2e-gui /f >\$null 2>&1; schtasks /delete /tn e2e-clip /f >\$null 2>&1" || true
    fetch_logs
    eval "exec ${GPS[1]}>&- ${GPS[0]}<&-"
    GPS=()
  fi
  if [[ -n ${QMP[1]:-} ]]; then
    eval "exec ${QMP[1]}>&- ${QMP[0]}<&-"
    QMP=()
  fi
  [[ -z ${ssh_opts:-} ]] || ssh "${ssh_opts[@]}" -O exit "$ssh_host" 2>/dev/null || true
  if [[ -n ${TC[1]:-} ]]; then
    # The front and the pointer back to where they were.
    tc 'done' || true
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
  displays=("$XDG_RUNTIME_DIR"/wayland-[0-9]) socks=("$XDG_RUNTIME_DIR"/sway-ipc.*)
  WAYLAND_DISPLAY=$(basename "${displays[0]}")
  SWAYSOCK=${socks[0]}
  export WAYLAND_DISPLAY SWAYSOCK
  unset DISPLAY
  # E2E_WLINPUT: one built already (e2e-all.sh builds it once for its runs).
  wlinput=${E2E_WLINPUT:-}
  [[ -n $wlinput ]] || { cargo build -q -p wlinput && wlinput=target/debug/wlinput; }
  coproc WL { "$wlinput" --size "${width}x${height}" 2>>"$outdir/wlinput.log"; }
fi
# Windows: connect to the VM (scripts/windows-vm.sh; QMP and SSH, see the
# be_* above) and make the guest's directory for the run anew.
if [[ $display == windows ]]; then
  # COMMAND's temzdf-gui in the guest, and its temzdf.
  gui_bin=${1//\\//}
  temzdf=$(dirname "$gui_bin")/temzdf.exe
  # A command in the background with a pipe each way (bash keeps one
  # coprocess at a time): NAME=(READ_FD WRITE_FD), as coproc sets them.
  piped() {
    local name=$1 r w
    shift
    mkfifo "$XDG_RUNTIME_DIR/$name.in" "$XDG_RUNTIME_DIR/$name.out"
    "$@" <"$XDG_RUNTIME_DIR/$name.in" >"$XDG_RUNTIME_DIR/$name.out" &
    exec {w}>"$XDG_RUNTIME_DIR/$name.in" {r}<"$XDG_RUNTIME_DIR/$name.out"
    eval "$name=($r $w)"
  }
  if ! scripts/windows-vm.sh ssh-config >"$XDG_RUNTIME_DIR/ssh_config" 2>"$outdir/guest.log"; then
    echo "e2e.sh: no Windows VM ${TEMZDF_WINDOWS_VM:-temzdf-windows} running (scripts/windows-vm.sh start)" >&2
    exit 1
  fi
  ssh_host=$(awk '$1 == "Host" { print $2; exit }' "$XDG_RUNTIME_DIR/ssh_config")
  # One connection for the run (each SSH handshake takes a while).
  ssh_opts=(-F "$XDG_RUNTIME_DIR/ssh_config" -o ControlMaster=auto -o "ControlPath=$XDG_RUNTIME_DIR/ssh"
    -o ControlPersist=yes)
  ssh "${ssh_opts[@]}" -MNf "$ssh_host" </dev/null >/dev/null 2>>"$outdir/guest.log"
  qmp_socket=$(scripts/windows-vm.sh qmp-socket)
  piped QMP socat - "UNIX-CONNECT:$qmp_socket"
  # The greeting, then capabilities negotiation: then commands.
  IFS= read -r -t 10 _ <&"${QMP[0]}" || { echo "e2e.sh: no QMP at $qmp_socket" >&2; exit 1; }
  qmp '{"execute":"qmp_capabilities"}'
  piped GPS ssh -T "${ssh_opts[@]}" "$ssh_host" 'powershell -NoLogo -NoProfile -NonInteractive -Command -' 2>>"$outdir/guest.log"
  # Sessions of the run in its own directories (they end with it, in
  # cleanup), for the GUI (gui.cmd) and the commands here alike.
  gps "[Console]::OutputEncoding = [Text.Encoding]::UTF8; \$env:XDG_RUNTIME_DIR = '$gdir/run'; \$env:XDG_STATE_HOME = '$gdir/state'"
  be_shot "$XDG_RUNTIME_DIR/screen.png"
  read -r screen_w screen_h <<<"$(magick identify -format '%w %h' "$XDG_RUNTIME_DIR/screen.png")"
  # What an earlier run left (it ended without its cleanup).
  gps "taskkill /F /IM temzdf-gui.exe >\$null 2>&1; if (Test-Path $gdir/run) { foreach (\$l in @(& '$temzdf' ls 2>\$null)) { & '$temzdf' kill-session (\$l -split ' ')[0] >\$null 2>&1 } }; Remove-Item -Recurse -Force $gdir -ErrorAction SilentlyContinue; foreach (\$d in 'run', 'state', 'files', 'out') { New-Item -ItemType Directory -Force $gdir/\$d >\$null }; \$LASTEXITCODE = 0"
  cat >"$XDG_RUNTIME_DIR/clip.ps1" <<PS1
# scripts/e2e.sh -d windows: the clipboard of the desktop, run there; get
# writes it to clip.txt, set takes clip-in.txt (UTF-8 without a BOM).
param(\$what)
\$utf8 = New-Object Text.UTF8Encoding \$false
if (\$what -eq 'get') {
  \$t = Get-Clipboard -Raw
  if (\$null -eq \$t) { \$t = '' }
  [IO.File]::WriteAllText('$gdir/clip.tmp', \$t, \$utf8)
  Move-Item -Force $gdir/clip.tmp $gdir/clip.txt
} else {
  Set-Clipboard -Value ([IO.File]::ReadAllText('$gdir/clip-in.txt', \$utf8))
  Set-Content $gdir/clip.done ''
}
PS1
  gcp_in "$XDG_RUNTIME_DIR/clip.ps1" "$gdir/clip.ps1"
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
    printf '%s\n' "printf '\\033[H\\033[2J'"
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
      # Display P3, which --render-frame cannot draw (doc/terminals/terminal-compat.md).
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
      # WebGPU (Metal): a macOS VM has no OpenGL 3.3 (doc/terminals/macos.md).
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
elif [[ $display == windows ]]; then
  # The GUI on the desktop: a batch file with the run's environment
  # (gui.cmd), started by a scheduled task; COMMAND's arguments that name
  # files here are copied in. The batch file waits for the GUI, then
  # writes its exit status (exit.txt); term_pid is an SSH command here
  # waiting for that, so the checks of whether the terminal (the GUI) still
  # runs stay as they are.
  gargs=''
  for a in "${@:2}"; do
    if [[ -f $a ]]; then
      gcp_in "$a" "$gdir/files/$(basename "$a")"
      a=$gdir/files/$(basename "$a")
    fi
    gargs+=" \"$a\""
  done
  win=${gdir//\//\\}
  {
    printf '@echo off\r\n'
    printf 'set XDG_RUNTIME_DIR=%s\\run\r\nset XDG_STATE_HOME=%s\\state\r\n' "$win" "$win"
    printf 'set TEMZDF_RECORD_INPUT=%s\\input.txt\r\nset TEMZDF_GUI_GEOMETRY=%s\\geometry.txt\r\n' "$win" "$win"
    [[ -n ${TEMZDF_PIXELS:-} ]] && printf 'set TEMZDF_PIXELS=%s\r\n' "$TEMZDF_PIXELS"
    printf '"%s"%s > %s\\terminal.log 2>&1\r\n' "${gui_bin//\//\\}" "$gargs" "$win"
    printf 'echo %%ERRORLEVEL%%> %s\\exit.txt\r\n' "$win"
  } >"$XDG_RUNTIME_DIR/gui.cmd"
  gcp_in "$XDG_RUNTIME_DIR/gui.cmd" "$gdir/gui.cmd"
  # The pointer out of the way, where the harness knows it: off the window
  # to come (at the screen's right edge); the actions move it in.
  be_move $((screen_w - 1)) $((screen_h / 2))
  on_desktop gui "cmd.exe /c $win\\gui.cmd"
  gssh "while (-not (Test-Path $gdir/exit.txt)) { Start-Sleep -Milliseconds 200 }" </dev/null &
  term_pid=$!
  # The drawing area, once the window is there (it took more than 5 s).
  startup=5
  if ! guest_wait "$gdir/geometry.txt" 60 ||
    ! read -r origin_x origin_y win_w win_h win_scale < <(gps "Get-Content $gdir/geometry.txt"); then
    fetch_logs
    echo "e2e.sh: no window of temzdf-gui within 60 s; the end of its output:" >&2
    tail -n 20 "$outdir/terminal.log" >&2
    exit 1
  fi
  phase window
  # Windows opens each new window a little lower and further right than
  # the last (the third went under the taskbar), and one a scheduled task
  # opens does not get the focus: its title bar (a little above the drawing
  # area) dragged, as a person would, puts the drawing area in the middle
  # of the screen and gives it the focus; nothing reaches the drawing area.
  tx=$(((screen_w - win_w) / 2)) ty=$(((screen_h - win_h) / 2))
  gx=$((origin_x + win_w / 2)) gy=$((origin_y - $(awk -v s="$win_scale" 'BEGIN { print int(10 * s + 0.5) }')))
  be_move "$gx" "$gy"
  be_button down 1
  for ((i = 1; i <= 10; i++)); do be_move $((gx + (tx - origin_x) * i / 10)) $((gy + (ty - origin_y) * i / 10)); done
  be_button up 1
  # Away again, off the window (it never was over the drawing area).
  be_move $((screen_w - 1)) $((screen_h / 2))
  for ((i = 0; i < 50; i++)); do
    read -r origin_x origin_y _ < <(gps "Get-Content $gdir/geometry.txt")
    [[ "$origin_x $origin_y" == "$tx $ty" ]] && break
    sleep 0.1
  done
  if [[ "$origin_x $origin_y" != "$tx $ty" ]]; then
    echo "e2e.sh: the window did not move to $tx,$ty (it is at $origin_x,$origin_y)" >&2
    exit 1
  fi
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
    # run past the bottom of the virtual screen). No ligatures nor contextual
    # alternates: over temzdf's images WezTerm shapes each cell alone, while
    # NAME.cells' reference (a cat on a plain screen) would be shaped in runs
    # ("1:1"'s colon, "~~", "##", "//"; doc/terminals/terminal-compat.md).
    "${clean_env[@]}" wezterm -n --config 'front_end="Software"' \
      --config "enable_wayland=$([[ $display == wayland ]] && echo true || echo false)" \
      --config 'window_decorations="NONE"' --config enable_tab_bar=false \
      --config 'window_padding={left=0,right=0,top=0,bottom=0}' \
      --config 'harfbuzz_features={"calt=0","clig=0","liga=0"}' \
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
  [[ $display == windows ]] && fetch_logs
  echo "e2e.sh: $term ended while starting; the end of its output:" >&2
  tail -n 20 "$outdir/terminal.log" >&2
  exit 1
fi
# The GUI's drawing area: where its window's inside is on the screen (on
# macOS every window's: below the menu bar).
if [[ $term == gui && $display != windows ]]; then
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
draw_references
phase quit
crashed=0
kill -0 "$term_pid" 2>/dev/null || crashed=1
be_key q || true
# As long as the terminal takes to go, at most half a second.
for ((i = 0; i < 10; i++)); do kill -0 "$term_pid" 2>/dev/null || break; sleep 0.05; done
cleanup
trap - EXIT
phase 'done'
echo "screenshots in $outdir"
if ((crashed)); then
  echo "TERMINAL CRASHED (see $outdir/terminal.log)"
  exit 3
fi
if [[ -f $outdir/expect.txt ]] && grep -q '^FAIL' "$outdir/expect.txt"; then
  echo "EXPECTATIONS FAILED (see $outdir/expect.txt)"
  exit 4
fi
