#!/usr/bin/env bash
# Pre-commit gate: rustfmt check, clippy (warnings are errors) and tests for the
# whole workspace, then the unit tests under Miri (scripts/miri.sh) when
# `cargo miri` is available, with terse output. Re-executes itself inside the
# Nix dev shell when needed, so it works from any shell.
#
# Usage: scripts/check.sh [--fix] [--no-miri] [--verbose] [cargo test args...]
#   --fix      run `cargo fmt` instead of only checking formatting
#   --no-miri  skip Miri (about a minute and a half)
#   --verbose  print a failed step's whole output, not its main lines (CI)
#
# The whole output of clippy and the tests goes to target/check/ (Miri's to
# target/miri-logs/; a failed simulated-terminal wait leaves its screen in
# target/tuitest/).
#
# It does not run end-to-end terminal tests or regenerate fixtures.
set -euo pipefail
# Absolute: it re-executes itself after the cd.
self=$(realpath "$0")
cd "$(dirname "$self")/.."

if [[ -z "${IN_NIX_SHELL:-}" ]]; then
  # The dirty-tree warning off through nix itself: a process substitution
  # filtering it would leave its pipe open in every child, and a daemon that
  # outlives the run (mozc_server) kept whatever reads our output waiting.
  NIX_CONFIG="warn-dirty = false${NIX_CONFIG:+$'\n'$NIX_CONFIG}" exec nix develop --quiet -c "$self" "$@"
fi

fix=0 miri=1 verbose=0
while [[ $# -gt 0 ]]; do
  case $1 in
    --fix) fix=1 ;;
    --no-miri) miri=0 ;;
    --verbose) verbose=1 ;;
    *) break ;;
  esac
  shift
done

status=0
if ((fix)); then
  cargo fmt --all
elif ! cargo fmt --all --check >/dev/null; then
  echo "FAIL fmt (run scripts/check.sh --fix)"
  status=1
fi

mkdir -p target/check
# A failed step: its main lines (PATTERN, at most N), or all of it with
# --verbose; the whole output is in LOG either way.
failed() {
  local name=$1 log=$2 pattern=$3 n=$4
  echo "FAIL $name (all of it: $log)"
  if ((verbose)); then cat "$log"; else grep -E "$pattern" "$log" | head -"$n"; fi
}

if ! cargo clippy --workspace --all-targets --color never -- -D warnings >target/check/clippy.log 2>&1; then
  failed clippy target/check/clippy.log "^(error|warning)|-->" 40
  status=1
fi

if ! cargo test --workspace --color never "$@" >target/check/test.log 2>&1; then
  failed tests target/check/test.log "^error|-->|FAILED|panicked|left:|right:" 60
  status=1
fi
out=$(cat target/check/test.log)
passed=$(grep -oE "[0-9]+ passed" <<<"$out" | awk '{s+=$1} END {print s+0}')

if ((status == 0)); then
  echo "OK fmt, clippy, tests ($passed passed)"
fi

if ((miri)); then
  scripts/miri.sh | tee target/check/miri.log && st=0 || st=${PIPESTATUS[0]}
  # Each failed test's own log (named on its FAIL line).
  if ((verbose)); then
    for log in $(grep -oE 'target/miri-logs/[^ )]+\.log' target/check/miri.log); do
      echo "--- $log"
      cat "$log"
    done
  fi
  # 2: no Miri here (e.g. the CI shell); it said so.
  if ((st != 0 && st != 2)); then status=1; fi
fi
exit "$status"
