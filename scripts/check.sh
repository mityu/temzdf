#!/usr/bin/env bash
# The pre-commit gate, `cargo xtask check` (xtask/src/check.rs: rustfmt,
# Biome, the generated declarations, the Node scripts' types, shellcheck,
# references to documents, clippy, the tests and Miri), run inside the Nix
# dev shell: this re-executes itself there when needed, so it works from
# any shell. Inside the dev shell `cargo xtask check` is the same.
#
# Usage: scripts/check.sh [--fix] [--no-miri] [--all] [--verbose] [--stream] [cargo test args...]
#   (`cargo xtask help` says what each option does)
set -euo pipefail

# shellcheck source=SCRIPTDIR/lib/devshell.sh
source "$(dirname "$(realpath "${BASH_SOURCE[0]}")")/lib/devshell.sh" . "$@"

exec cargo xtask check "$@"
