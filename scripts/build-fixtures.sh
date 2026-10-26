#!/usr/bin/env bash
# Build the test fixtures: every tests/fixtures/*.tex becomes a PDF and a SyncTeX
# file in target/fixtures/ (never committed; see AGENTS.md). Fixtures already
# newer than their source and this script are skipped, so tests can call this
# cheaply. Each is built in a scratch directory and renamed into place, so a
# reader (a test, a viewer watching it, a concurrent run) never sees a partly
# written file. Needs latexmk/pdflatex on PATH: a global TeX installation, or
# the `alldeps` dev shell (`nix develop .#alldeps`). Re-executes inside the
# default dev shell when not in one (host tools stay on PATH there).
#
# Usage: scripts/build-fixtures.sh [--force]
set -euo pipefail
# Absolute: it re-executes itself after the cd.
self=$(realpath "$0")
cd "$(dirname "$self")/.."

if [[ -z "${IN_NIX_SHELL:-}" ]]; then
  exec nix develop --quiet -c "$self" "$@" 2> >(grep -v "Git tree .* is dirty" >&2)
fi

command -v latexmk >/dev/null || {
  echo "error: latexmk not found; install TeX or use 'nix develop .#alldeps'" >&2
  exit 1
}
force=0
[[ "${1:-}" == "--force" ]] && force=1
out=target/fixtures
mkdir -p "$out"
# Reproducible output: pdfTeX otherwise embeds the build time (and a time-based /ID).
export SOURCE_DATE_EPOCH=0 FORCE_SOURCE_DATE=1
fresh() { [[ $1 -nt $2 && $1 -nt $self ]]; }
work=
trap 'if [[ -n $work ]]; then rm -rf -- "$work"; fi' EXIT
for tex in tests/fixtures/*.tex; do
  base=$(basename "${tex%.tex}")
  if ((!force)) && fresh "$out/$base.pdf" "$tex" && fresh "$out/$base.synctex.gz" "$tex"; then
    continue
  fi
  work=$(mktemp -d "$out/.build-$base.XXXXXX")
  latexmk -pdf -synctex=1 -interaction=nonstopmode -halt-on-error -quiet -outdir="$work" "$tex" >/dev/null 2>&1 || {
    cp -f "$work/$base.log" "$out/$base.log" 2>/dev/null || true
    echo "error: building $tex failed; see $out/$base.log" >&2
    exit 1
  }
  # The PDF last: its time tells the next run both are there.
  mv -f "$work/$base.synctex.gz" "$out/$base.synctex.gz"
  mv -f "$work/$base.pdf" "$out/$base.pdf"
  rm -rf -- "$work"
  work=
  echo "built $out/$base.pdf"
done
