#!/usr/bin/env bash
# Starts built binaries once, as a check that they run: `--help` of temzdf
# and temzdf-gui, then a session (on a pty through script(1)) that splits
# its pane on the shipped key `<C-b>%`, which needs the script host's guest
# and the shipped key maps. Everything lives in a fresh temporary directory
# (runtime, config, state and log directories); on a failure the session's
# logs are printed.
#
# Usage: scripts/smoke-session.sh BINDIR   (e.g. target/debug)
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 BINDIR" >&2
    exit 2
fi
bindir=$(cd "$1" && pwd)
temzdf=$bindir/temzdf

"$temzdf" --help >/dev/null
"$bindir/temzdf-gui" --help >/dev/null
echo "ok: --help" >&2

# Short: socket paths have a length limit.
tmp=$(mktemp -d /tmp/tzs.XXXX)
chmod 700 "$tmp"
mkdir -p "$tmp/run" "$tmp/log" "$tmp/home" "$tmp/config" "$tmp/state"
chmod 700 "$tmp/run"
export XDG_RUNTIME_DIR=$tmp/run TEMZDF_LOG_DIR=$tmp/log HOME=$tmp/home \
    XDG_CONFIG_HOME=$tmp/config XDG_STATE_HOME=$tmp/state
client=

cleanup() {
    "$temzdf" kill-session --force smoke >/dev/null 2>&1 || true
    if [[ -n $client ]]; then
        kill "$client" 2>/dev/null || true
        wait "$client" 2>/dev/null || true
    fi
    rm -rf "$tmp"
}
trap cleanup EXIT

fail() {
    echo "FAIL: $*" >&2
    echo "--- temzdf ls -v" >&2
    "$temzdf" ls -v >&2 || true
    for log in "$tmp"/log/*; do
        [[ -f $log ]] || continue
        echo "--- $log" >&2
        cat "$log" >&2
    done
    exit 1
}

# Waits (up to 30 s) until session smoke has $1 panes.
wait_panes() {
    local i
    for ((i = 0; i < 300; i++)); do
        if [[ "$("$temzdf" eval -s smoke -p -c 'temzdf.action.panes().length' 2>/dev/null)" == "$1" ]]; then
            return 0
        fi
        sleep 0.1
    done
    fail "session smoke never showed $1 panes"
}

script -qfc "$temzdf -s smoke" /dev/null </dev/null >/dev/null 2>&1 &
client=$!
wait_panes 1
echo "ok: session started" >&2

"$temzdf" action -s smoke feedkeys '<C-b>%' || fail "feedkeys failed"
wait_panes 2
echo "ok: <C-b>% split the pane" >&2

"$temzdf" kill-session --force smoke || fail "kill-session failed"
echo "ok: session killed" >&2
