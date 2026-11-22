#!/usr/bin/env bash
# What a build on Ubuntu without Nix needs beyond apt and rustup (and Zig,
# esbuild and npm on PATH), installed under PREFIX:
#
#   - the shipped scripts' npm packages, in
#     crates/temzdf-script/runtime/node_modules (`npm ci`, without the
#     devDependencies, tools the build does not use).
#
# Prints the environment the build needs as KEY=VALUE lines on stdout (CI
# appends them to $GITHUB_ENV; none at the moment), and its progress on
# stderr.
#
# Usage: scripts/deps-without-nix.sh PREFIX
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 PREFIX" >&2
    exit 2
fi
root=$(cd "$(dirname "$0")/.." && pwd)

{
    echo "== npm packages of crates/temzdf-script/runtime"
    (cd "$root/crates/temzdf-script/runtime" && npm ci --omit=dev --no-audit --no-fund)
} >&2

