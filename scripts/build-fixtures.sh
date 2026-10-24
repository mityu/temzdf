#!/usr/bin/env bash
# Build the test fixtures: every tests/fixtures/*.tex becomes a PDF and a SyncTeX
# file in target/fixtures/ (never committed; see AGENTS.md). Fixtures already
# newer than their source are skipped, so tests can call this cheaply. Needs
# latexmk/pdflatex on PATH: a global TeX installation, or the `alldeps` dev
# shell (`nix develop .#alldeps`). Re-executes inside the default dev shell when
# not in one (host tools stay on PATH there).
#
# Usage: scripts/build-fixtures.sh [--force]
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ -z "${IN_NIX_SHELL:-}" ]]; then
  exec nix develop --quiet -c "$0" "$@" 2> >(grep -v "Git tree .* is dirty" >&2)
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
for tex in tests/fixtures/*.tex; do
  base=$(basename "${tex%.tex}")
  if ((!force)) && [[ "$out/$base.pdf" -nt "$tex" && "$out/$base.synctex.gz" -nt "$tex" ]]; then
    continue
  fi
  latexmk -pdf -synctex=1 -interaction=nonstopmode -halt-on-error -quiet -outdir="$out" "$tex" >/dev/null 2>&1 ||
    { echo "error: building $tex failed; see $out/$base.log" >&2; exit 1; }
  echo "built $out/$base.pdf"
done
