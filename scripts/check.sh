#!/usr/bin/env bash
# Pre-commit gate: rustfmt check, clippy (warnings are errors) and tests for the
# whole workspace, with terse output. Re-executes itself inside the Nix dev shell
# when needed, so it works from any shell.
#
# Usage: scripts/check.sh [--fix] [cargo test args...]
#   --fix   run `cargo fmt` instead of only checking formatting
#
# It does not run end-to-end terminal tests or regenerate fixtures.
set -euo pipefail
# Absolute: it re-executes itself after the cd.
self=$(realpath "$0")
cd "$(dirname "$self")/.."

if [[ -z "${IN_NIX_SHELL:-}" ]]; then
  exec nix develop --quiet -c "$self" "$@" 2> >(grep -v "Git tree .* is dirty" >&2)
fi

fix=0
if [[ "${1:-}" == "--fix" ]]; then
  fix=1
  shift
fi

status=0
if ((fix)); then
  cargo fmt --all
elif ! cargo fmt --all --check >/dev/null; then
  echo "FAIL fmt (run scripts/check.sh --fix)"
  status=1
fi

if ! out=$(cargo clippy --workspace --all-targets --color never -- -D warnings 2>&1); then
  echo "FAIL clippy"
  grep -E "^(error|warning)|-->" <<<"$out" | head -40
  status=1
fi

if ! out=$(cargo test --workspace --color never "$@" 2>&1); then
  echo "FAIL tests"
  grep -E "^error|-->|FAILED|panicked|left:|right:" <<<"$out" | head -60
  status=1
fi
passed=$(grep -oE "[0-9]+ passed" <<<"$out" | awk '{s+=$1} END {print s+0}')

if ((status == 0)); then
  echo "OK fmt, clippy, tests ($passed passed)"
fi
exit "$status"
