#!/usr/bin/env bash
# Build the test fixtures: every tests/fixtures/*.tex becomes a PDF and a SyncTeX
# file in target/fixtures/ (never committed; see AGENTS.md). Fixtures already
# newer than their source are skipped. The work is done by `cargo xtask
# fixtures` (crates/temzdf-fixtures), which tests also call; this wrapper is
# for the e2e scripts and for people. Needs latexmk/pdflatex on PATH: a global
# TeX installation, or the `alldeps` dev shell (`nix develop .#alldeps`).
# Re-executes inside the default dev shell when not in one (host tools stay on
# PATH there).
#
# Usage: scripts/build-fixtures.sh [--force]
set -euo pipefail

# shellcheck source=SCRIPTDIR/lib/devshell.sh
source "$(dirname "$(realpath "${BASH_SOURCE[0]}")")/lib/devshell.sh" . "$@"

exec cargo xtask fixtures "$@"
