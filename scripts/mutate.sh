#!/usr/bin/env bash
# Mutation check: prove a test catches a bug by putting the bug back. Replaces
# OLD with NEW in FILE (exactly one occurrence, else it refuses), runs
# COMMAND, and always restores FILE (also on Ctrl-C or failure). Before the
# mutation it runs COMMAND once to see it pass (skip with -n).
# Re-executes inside the Nix dev shell when needed.
#
# Usage: scripts/mutate.sh [-n] FILE OLD NEW -- COMMAND [ARGS...]
#   FILE is relative to the repository root (the script runs from there,
#   wherever it is started); occurrences of OLD are counted overlapping too.
#   e.g. scripts/mutate.sh crates/termpix/src/input.rs \
#          "(sgr && b[i] == b'-')" "false" -- cargo test -p termpix --lib input
#
# Exit status: 0 the mutation was caught (COMMAND failed), 1 it survived
# (COMMAND passed: the tests do not notice), 2 usage (OLD not found once,
# COMMAND failing before the mutation), 3 the mutated code did not compile
# (no test ran, nothing proven), 4 no test ran (a filter matching nothing).
# It looks at cargo's output only for "could not compile", "running N tests" and
# "test result"; any other command is judged by its exit status alone.
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

usage() {
  sed -n '2,/^set -euo/{/^set -euo/!p}' "$self"
  exit 2
}
first=1
if [[ ${1:-} == -n ]]; then
  first=0
  shift
fi
[[ $# -ge 5 && $4 == -- ]] || usage
file=$1 old=$2 new=$3
shift 4
[[ -f $file && -n $old ]] || usage

# The whole file, trailing newlines included.
IFS= read -r -d '' content <"$file" || true
# Overlapping occurrences count too (`aa` is twice in `aaa`): each search
# starts one character after the previous match began.
count=0
s=$content
while [[ $s == *"$old"* ]]; do
  count=$((count + 1))
  before=${s%%"$old"*}
  s=${s:$((${#before} + 1))}
done
if ((count != 1)); then
  echo "OLD occurs $count times in $file (needs exactly 1)" >&2
  exit 2
fi

# Whether cargo output ran tests: no compile error, and some test counted.
judge() {
  local out=$1
  if grep -q 'error: could not compile' <<<"$out"; then return 3; fi
  if grep -q '^test result' <<<"$out" && ! grep -qE 'running [1-9]' <<<"$out"; then return 4; fi
  return 0
}

log=$(mktemp "${TMPDIR:-/tmp}/mutate.XXXXXX")
if ((first)); then
  if ! "$@" >"$log" 2>&1; then
    echo "COMMAND fails before the mutation (see $log)" >&2
    exit 2
  fi
  judge "$(cat "$log")" || {
    echo "no test ran before the mutation (see $log)" >&2
    exit 4
  }
fi

backup=$(mktemp "${TMPDIR:-/tmp}/mutate-orig.XXXXXX")
cp -p -- "$file" "$backup"
# Restored with a fresh mtime: the old one is older than the mutated build,
# and cargo would keep running that.
trap 'cp -- "$backup" "$file"; touch -- "$file"; rm -f -- "$backup"' EXIT
trap 'exit 130' INT TERM
printf '%s' "${content/"$old"/"$new"}" >"$file"

if "$@" >"$log" 2>&1; then status=0; else status=$?; fi
out=$(cat "$log")
judge "$out" || {
  st=$?
  ((st == 3)) && echo "DID NOT COMPILE: the mutation proves nothing (see $log)"
  ((st == 4)) && echo "NO TEST RAN (see $log)"
  exit "$st"
}
if ((status == 0)); then
  echo "SURVIVED: COMMAND passes with the mutation (see $log)"
  exit 1
fi
echo "CAUGHT:"
grep -E '^test .* FAILED$|panicked at' <<<"$out" | head -5 || true
rm -f -- "$log"
