#!/usr/bin/env bash
# Pre-commit gate: rustfmt check, Biome (the scripts' formatting and
# lints, biome.jsonc), the generated declarations
# (crates/temzdf-script/ts-types/temzdf-all.d.ts, scripts/ts-types.ts),
# the Node scripts' types (tsc -p scripts), the scripts' shell lint,
# references to documents (scripts/check-docs.ts), clippy (warnings are errors) and tests for the
# whole workspace, then the unit tests under Miri (scripts/miri.sh) when
# `cargo miri` is available, with terse output. Re-executes itself inside the
# Nix dev shell when needed, so it works from any shell.
#
# Usage: scripts/check.sh [--fix] [--no-miri] [--all] [--verbose] [cargo test args...]
#   --fix      run `cargo fmt` and `biome check --write` (formatting and
#              Biome's safe fixes) and write temzdf-all.d.ts again instead
#              of only checking
#   --no-miri  skip Miri (about a minute and a half)
#   --all      run the tests even when only documents changed (see below)
#   --verbose  print a failed step's whole output, not its main lines (CI)
#
# When every uncommitted change (staged, unstaged or untracked) is Markdown
# under doc/, the tests and Miri are skipped: such changes cannot affect
# them. A clean tree (CI), --all or cargo test arguments run them.
#
# The whole output of clippy and the tests goes to target/check/ (Miri's to
# target/miri-logs/; a failed simulated-terminal wait leaves its screen in
# target/tuitest/).
#
# The tests see only the fonts the dev shell names in TEMZDF_TEST_FONTCONFIG
# (JetBrains Mono first; see flake.nix), as in CI.
#
# It does not run end-to-end terminal tests or regenerate fixtures.
set -euo pipefail

# shellcheck source=SCRIPTDIR/lib/devshell.sh
source "$(dirname "$(realpath "${BASH_SOURCE[0]}")")/lib/devshell.sh" . "$@"

fix=0 miri=1 all=0 verbose=0
while [[ $# -gt 0 ]]; do
  case $1 in
    --fix) fix=1 ;;
    --no-miri) miri=0 ;;
    --all) all=1 ;;
    --verbose) verbose=1 ;;
    *) break ;;
  esac
  shift
done

# Paths changed since the last commit; tests are skipped when all of them
# are Markdown under doc/ (and there is at least one).
tests=1
if ((!all)) && [[ $# -eq 0 ]]; then
  changed=$(git diff --name-only --no-renames HEAD && git ls-files --others --exclude-standard)
  if [[ -n $changed ]] && ! grep -qvE '^doc/.+\.md$' <<<"$changed"; then
    tests=0 miri=0
  fi
fi

# The shipped scripts' npm packages, for tsc (crates/temzdf/tests/types.rs)
# and esbuild; build.rs refuses them when they no longer match the lock.
if [[ ! -d crates/temzdf-script/runtime/node_modules ]]; then
  echo "FAIL npm (no crates/temzdf-script/runtime/node_modules: run \`npm ci\` there)"
  exit 1
fi

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

# Biome's safe fixes with --fix; what is left (lints to fix by hand) fails.
if ((fix)); then
  biome check --write --colors=off . >target/check/biome.log 2>&1 || true
fi
if ! biome check --colors=off . >target/check/biome.log 2>&1; then
  failed "biome (scripts/check.sh --fix formats)" target/check/biome.log "━━|^ *×" 20
  status=1
fi

# temzdf-all.d.ts as scripts/ts-types.ts writes it from the declarations
# by hand and the shipped modules.
declarations=crates/temzdf-script/ts-types/temzdf-all.d.ts
if ((fix)); then
  node scripts/ts-types.ts >target/check/ts-types.log 2>&1 || true
fi
if ! node scripts/ts-types.ts target/check/temzdf-all.d.ts >target/check/ts-types.log 2>&1; then
  failed ts-types target/check/ts-types.log "" 20
  status=1
elif ! cmp -s target/check/temzdf-all.d.ts "$declarations"; then
  echo "FAIL ts-types ($declarations is not what scripts/ts-types.ts writes; run scripts/check.sh --fix)"
  status=1
fi

# The Node scripts' types (scripts/tsconfig.json): Node only strips them.
# The TypeScript of runtime/node_modules (the CI shell has no tsc).
if ! node crates/temzdf-script/runtime/node_modules/typescript/bin/tsc -p scripts --pretty false >target/check/scripts.log 2>&1; then
  failed scripts target/check/scripts.log "error" 40
  status=1
fi

if ! shellcheck -x --color=never --format=gcc ./scripts/{*.sh,*/*.sh} ./tests/*/*.sh >target/check/shellcheck.log 2>&1; then
  failed shellcheck target/check/shellcheck.log "^" 100
  status=1
fi

if ! node scripts/check-docs.ts >target/check/docs.log 2>&1; then
  failed docs target/check/docs.log "^" 40
  status=1
fi

if ! cargo clippy --workspace --all-targets --color never -- -D warnings >target/check/clippy.log 2>&1; then
  failed clippy target/check/clippy.log "^(error|warning)|-->" 40
  status=1
fi

if ((tests)); then
  # Text is drawn with the fonts the dev shell fixes for tests (flake.nix,
  # testFontConfig), not the host's, so that it is drawn as in CI.
  if [[ -n "${TEMZDF_TEST_FONTCONFIG:-}" ]]; then
    export FONTCONFIG_FILE=$TEMZDF_TEST_FONTCONFIG
  fi
  if ! cargo test --workspace --color never "$@" >target/check/test.log 2>&1; then
    failed tests target/check/test.log "^error|-->|FAILED|panicked|left:|right:" 60
    status=1
  fi
  passed=$(grep -oE "[0-9]+ passed" target/check/test.log | awk '{s+=$1} END {print s+0}')
  summary="tests ($passed passed)"
else
  summary="tests skipped (only Markdown under doc/ changed; --all runs them)"
fi

if ((status == 0)); then
  echo "OK fmt, biome, ts-types, scripts, shellcheck, docs, clippy, $summary"
fi

if ((miri)); then
  scripts/miri.sh | tee target/check/miri.log && st=0 || st=${PIPESTATUS[0]}
  # Each failed test's own log (named on its FAIL line).
  if ((verbose)); then
    grep -oE 'target/miri-logs/[^ )]+\.log' target/check/miri.log | while read -r log; do
      echo "--- $log"
      cat "$log"
    done
  fi
  # 2: no Miri here (e.g. the CI shell); it said so.
  if ((st != 0 && st != 2)); then status=1; fi
fi
exit "$status"
