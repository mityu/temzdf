#!/usr/bin/env bash
# Runs the judged e2e scenarios in one or more terminals and sums up:
# scripts/e2e/all.ts (Node, TypeScript; its header lists the options), run
# inside the dev shell.
#
# Usage: scripts/e2e-all.sh [-t TERM]... [-d DISP] [-P PATH] [-o DIR] [-j N] [-R] [SCENARIO...]
set -euo pipefail

# shellcheck source=SCRIPTDIR/lib/devshell.sh
source "$(dirname "$(realpath "${BASH_SOURCE[0]}")")/lib/devshell.sh" . "$@"

exec node scripts/e2e/all.ts "$@"
