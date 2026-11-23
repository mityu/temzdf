#!/usr/bin/env bash
# End-to-end harness: runs a scenario (tests/e2e/NAME.ts) in a real terminal
# or the GUI on a display server of its own, judges it and saves
# screenshots. The harness is scripts/e2e/main.ts (Node, TypeScript; its
# header lists the options); this runs it inside the dev shell.
#
# Usage: scripts/e2e.sh [-t kitty|ghostty|wezterm|gui] [-d x11|wayland|macos|windows]
#          [-m zellij|tmux] [-I] [-P shared|socket] [-s WxH] [-o OUTDIR] SCENARIO [-- COMMAND...]
set -euo pipefail

# shellcheck source=SCRIPTDIR/lib/devshell.sh
source "$(dirname "$(realpath "${BASH_SOURCE[0]}")")/lib/devshell.sh" . "$@"

exec node scripts/e2e/main.ts "$@"
