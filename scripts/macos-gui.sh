#!/usr/bin/env bash
# Drive real GUI terminals on macOS for checks without a human and without
# disturbing the desktop: terminals open in the background on a virtual display
# next to the real screen, keys go only to their process, and screenshots are
# taken per window. The macOS counterpart of scripts/e2e.sh (which needs Xvfb);
# see doc/macos.md.
#
# Usage: scripts/macos-gui.sh COMMAND ...
#   display start|stop         create / remove the virtual display (needed by launch)
#   launch TERM [VAR=VALUE...] -- COMMAND [ARGS...]
#                              run COMMAND in a new kitty, ghostty or wezterm window
#                              on the virtual display; prints "PID STATUSFILE". The
#                              window stays open after COMMAND exits; STATUSFILE then
#                              holds its exit status.
#   shot PID OUT.png           screenshot of PID's window, scaled to points, so
#                              image coordinates are the mouse coordinates below
#   title PID                  the window title (temzdf shows "FILE — PAGE / PAGES")
#   key PID KEY...             e.g. `key $pid j C-f Return` (see termctl.swift)
#   type PID TEXT              printable ASCII text
#   click PID X Y [ctrl|shift|cmd]
#   drag PID X1 Y1 X2 Y2
#   scroll PID X Y STEPS [ctrl]  STEPS > 0 scrolls down
#   close PID                  quit that terminal
#
# Mouse commands bring the terminal to the front for a moment and give the
# front back afterwards; keys reach WezTerm only that way too (it ignores
# per-process events), everything else stays in the background. Wait a little
# after scrolling (kinetic scrolling) and in WezTerm (it redraws lazily while
# unfocused) before taking a screenshot.
#
# Needs the Xcode command line tools (swiftc), and the screen recording and
# accessibility permissions for the terminal this runs in. The tools are built
# into target/macos-gui/. It does not build temzdf.
set -euo pipefail
cd "$(dirname "$0")/.."

[[ $(uname) == Darwin ]] || { echo "macOS only; use scripts/e2e.sh elsewhere" >&2; exit 2; }

out=target/macos-gui
mkdir -p "$out"

# Build a tool when its sources changed. The host compiler, outside the Nix
# environment (whose SDK variables confuse it).
build() {
  local name=$1 src=scripts/macos/$1.swift extra=()
  [[ $name == vdisp ]] && extra=(-import-objc-header scripts/macos/CGVirtualDisplay.h)
  if [[ ! -x $out/$name || $src -nt $out/$name || scripts/macos/CGVirtualDisplay.h -nt $out/$name ]]; then
    env -i HOME="$HOME" PATH=/usr/bin:/bin /usr/bin/swiftc -O "${extra[@]}" "$src" -o "$out/$name"
  fi
}
build termctl
build vdisp
ctl() { "$out/termctl" "$@"; }

usage() { sed -n '2,/^set -euo/{/^set -euo/!p}' "$0"; exit 2; }

# The virtual display's "ID X Y W H", empty when it is not running.
display_bounds() {
  [[ -f $out/vdisp.pid ]] && kill -0 "$(<"$out/vdisp.pid")" 2>/dev/null && cat "$out/vdisp.out"
}

# kitty remote control for `key` and `type`, in termctl's key notation.
kitty_keys() {
  local to=$1 what=$2 k name mods=
  shift 2
  if [[ $what == type ]]; then
    kitty @ --to "$to" send-text -- "$*"
    return
  fi
  for k in "$@"; do
    if [[ ${#k} -eq 1 ]]; then
      # send-key shift+g lacks the shifted key that real typing reports.
      kitty @ --to "$to" send-text -- "$k"
      continue
    fi
    name=$k
    while [[ $name == [CSM]-?* ]]; do
      case ${name:0:1} in C) mods+=ctrl+ ;; S) mods+=shift+ ;; M) mods+=alt+ ;; esac
      name=${name:2}
    done
    case $name in
      Return | Enter) name=enter ;;
      BackSpace | Backspace) name=backspace ;;
      Esc) name=escape ;;
      PageUp) name=page_up ;;
      PageDown) name=page_down ;;
      *) name=$(tr '[:upper:]' '[:lower:]' <<<"$name") ;;
    esac
    kitty @ --to "$to" send-key "${mods:-}$name"
    mods=
  done
}

cmd=${1:-}
[[ -n $cmd ]] || usage
shift
case $cmd in
  display)
    case ${1:-} in
      start)
        if [[ -n $(display_bounds) ]]; then display_bounds; exit 0; fi
        : >"$out/vdisp.out"
        nohup "$out/vdisp" >"$out/vdisp.out" 2>&1 &
        echo $! >"$out/vdisp.pid"
        for _ in $(seq 50); do [[ -s $out/vdisp.out ]] && break; sleep 0.1; done
        cat "$out/vdisp.out"
        ;;
      stop)
        [[ -f $out/vdisp.pid ]] && kill "$(<"$out/vdisp.pid")" 2>/dev/null || true
        rm -f "$out/vdisp.pid" "$out/vdisp.out"
        ;;
      *) usage ;;
    esac
    ;;
  launch)
    term=${1:-}
    shift || true
    env=()
    while [[ $# -gt 0 && $1 != -- ]]; do env+=("$1"); shift; done
    [[ ${1:-} == -- ]] || usage
    shift
    read -r _ dx dy _ _ <<<"$(display_bounds)" || { echo "run 'display start' first" >&2; exit 1; }
    status=$(mktemp "$PWD/$out/status.XXXXXX")
    rm -f "$status"
    # Relative paths stay relative to here: `open` starts apps in /.
    script="cd $(printf '%q' "$PWD") && env ${env[*]+$(printf '%q ' "${env[@]}")}$(printf '%q ' "$@"); echo \$? > $(printf '%q' "$status"); exec sleep 86400"
    case $term in
      kitty)
        app=kitty proc=kitty
        # kitty ignores key events posted to its process: use remote control.
        # Unix socket paths are limited to 104 bytes, hence /tmp.
        sock=$(mktemp -u /tmp/temzdf-gui.XXXXXX)
        targs=(-o allow_remote_control=yes --listen-on "unix:$sock" -o remember_window_size=no -o initial_window_width=1000 -o initial_window_height=650 sh -c "$script") ;;
      ghostty)
        app=Ghostty proc=ghostty
        targs=(--quit-after-last-window-closed=true --window-width=110 --window-height=32 -e sh -c "$script") ;;
      wezterm)
        app=WezTerm proc=wezterm-gui
        targs=(start --always-new-process -- sh -c "$script") ;;
      *) usage ;;
    esac
    before=" $(pgrep -x "$proc" | tr '\n' ' ' || true) "
    # -g: do not bring the new terminal to the front.
    open -g -n -a "$app" --args "${targs[@]}"
    pid=
    for _ in $(seq 100); do
      for p in $(pgrep -x "$proc" || true); do
        [[ $before == *" $p "* ]] && continue
        [[ -n $(ctl windows "$p") ]] && pid=$p
      done
      [[ -n $pid ]] && break
      sleep 0.1
    done
    [[ -n $pid ]] || { echo "$term did not open a window" >&2; exit 1; }
    ctl move "$pid" $((dx + 20)) $((dy + 20))
    # Let the terminal settle at its new place (WezTerm resizes after startup).
    sleep 2
    [[ $term == kitty ]] && echo "$sock" >"$out/kitty-$pid.sock"
    echo "$pid $status"
    ;;
  shot)
    [[ $# -eq 2 ]] || usage
    read -r wid _ _ w _ <<<"$(ctl windows "$1" | head -1)"
    screencapture -x -o -l "$wid" "$2"
    sips --resampleWidth "$w" "$2" >/dev/null
    ;;
  title)
    ctl windows "$1" | head -1 | cut -d' ' -f6-
    ;;
  key | type)
    pid=$1
    shift
    if [[ -f $out/kitty-$pid.sock ]]; then
      kitty_keys "unix:$(<"$out/kitty-$pid.sock")" "$cmd" "$@"
      exit
    fi
    global=()
    [[ $(ps -o comm= -p "$pid") == *wezterm-gui ]] && global=(--global)
    ctl "$cmd" "$pid" "${global[@]}" "$@"
    ;;
  click | drag | scroll)
    ctl "$cmd" "$@"
    ;;
  close)
    kill "$1"
    rm -f "$out/kitty-$1.sock"
    ;;
  *) usage ;;
esac
