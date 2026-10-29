#!/usr/bin/env bash
# Run the workspace's unit tests under Miri (undefined behaviour, data races,
# leaked threads and memory in our Rust code and the Rust dependencies it
# calls), each test in its own process, in parallel, with terse output.
# Re-executes itself inside the Nix dev shell when needed.
#
# Usage: scripts/miri.sh [PACKAGE...]   (default: every package but tuitest)
#
# What it does not cover: foreign code (MuPDF, libghostty-vt, fontconfig,
# QuickJS's C side), integration tests (crates/*/tests, which drive ptys and
# processes), and tuitest (pty only). Tests Miri cannot run are marked
# `#[cfg_attr(miri, ignore = "why")]` where they are defined; isolation is
# off so tests may read files and the clock. Logs of failed tests go to
# target/miri-logs/. Exits 2 when `cargo miri` is not available.
set -euo pipefail
# Absolute: it re-executes itself after the cd.
self=$(realpath "${BASH_SOURCE[0]}")
cd "$(dirname "$(dirname "$self")")"

if [[ -z "${IN_NIX_SHELL:-}" ]]; then
  # The dirty-tree warning off through nix itself: a process substitution
  # filtering it would leave its pipe open in every child, and a daemon that
  # outlives the run (mozc_server) kept whatever reads our output waiting.
  NIX_CONFIG="warn-dirty = false${NIX_CONFIG:+$'\n'$NIX_CONFIG}" exec nix develop --quiet -c "$self" "$@"
fi

if ! cargo miri --version >/dev/null 2>&1; then
  echo "SKIP miri (cargo miri not available)"
  exit 2
fi
export MIRIFLAGS="-Zmiri-disable-isolation ${MIRIFLAGS:-}"

packages=("$@")
if ((${#packages[@]} == 0)); then
  mapfile -t packages < <(cargo metadata --no-deps --format-version 1 |
    grep -oE '"name":"[^"]+","version"' | sed -E 's/"name":"([^"]+)".*/\1/' | grep -vx tuitest)
fi
args=()
for p in "${packages[@]}"; do args+=(-p "$p"); done

export logs=target/miri-logs
rm -rf "$logs"
mkdir -p "$logs"
if ! out=$(cargo miri test --color never "${args[@]}" --lib --bins --no-run 2>&1); then
  echo "FAIL miri (build)"
  grep -E "^error|-->" <<<"$out" | head -40
  exit 1
fi

# The unit test targets of package $1 (packages live in crates/NAME).
targets() {
  [[ -f crates/$1/src/lib.rs ]] && echo --lib
  [[ -f crates/$1/src/main.rs ]] && echo --bins
  true
}
export -f targets
# One line per test: PACKAGE NAME.
tests=$(for p in "${packages[@]}"; do
  # shellcheck disable=SC2046
  cargo miri test -q -p "$p" $(targets "$p") -- --list --format terse |
    sed -n "s/: test\$//p" | sed "s/^/$p /"
done)
# shellcheck disable=SC2016
results=$(xargs -r -P "$(nproc)" -L 1 bash -c '
  log=$logs/$0.${1//::/-}.log
  if cargo miri test -q --color never -p "$0" $(targets "$0") -- --exact "$1" >"$log" 2>&1; then
    if grep -q "1 ignored" "$log"; then echo IGNORED; else echo PASS; fi
    rm -f "$log"
  else
    echo "FAIL $0 $1: $(grep -m1 -oE "Undefined Behavior: .{0,100}|unsupported operation: .{0,100}|panicked at .{0,100}|memory leaked.{0,60}|terminated without waiting.{0,40}|error: .{0,100}" "$log") (see $log)"
  fi' <<<"$tests")

passed=$(grep -cx PASS <<<"$results" || true)
ignored=$(grep -cx IGNORED <<<"$results" || true)
if grep -q '^FAIL' <<<"$results"; then
  echo "FAIL miri"
  grep '^FAIL' <<<"$results"
  exit 1
fi
echo "OK miri ($passed passed, $ignored ignored)"
