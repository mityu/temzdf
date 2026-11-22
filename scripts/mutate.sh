#!/usr/bin/env bash
# Mutation check, `cargo xtask mutate` (xtask/src/mutate.rs), run inside the
# Nix dev shell: this re-executes itself there when needed, so it works from
# any shell. Inside the dev shell `cargo xtask mutate` is the same.
#
# Usage: scripts/mutate.sh [-n] FILE OLD NEW -- COMMAND [ARGS...]
#   (`cargo xtask help` says what it does)
set -euo pipefail

# shellcheck source=SCRIPTDIR/lib/devshell.sh
source "$(dirname "$(realpath "${BASH_SOURCE[0]}")")/lib/devshell.sh" . "$@"

exec cargo xtask mutate "$@"
