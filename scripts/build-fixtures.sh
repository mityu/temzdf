#!/usr/bin/env bash
# Rebuild the LaTeX fixtures in tests/fixtures/ (PDF + SyncTeX) and remove
# auxiliary files. Uses the host's latexmk/pdflatex (not provided by the flake).
# Commit the regenerated *.pdf and *.synctex.gz together with the .tex change.
#
# SyncTeX records absolute input paths. The fixture directory prefix is rewritten
# to "./" so no machine-specific path is committed; TeX distribution paths
# (/nix/store/...) are left alone because no test refers to them.
set -euo pipefail
cd "$(dirname "$0")/../tests/fixtures"
command -v latexmk >/dev/null || { echo "latexmk not found on PATH" >&2; exit 1; }
here=$(pwd)
# Reproducible output: pdfTeX otherwise embeds the build time (and a time-based
# /ID), so every rebuild would change the committed PDFs.
export SOURCE_DATE_EPOCH=0 FORCE_SOURCE_DATE=1
for tex in *.tex; do
  base=${tex%.tex}
  latexmk -pdf -synctex=1 -interaction=nonstopmode -halt-on-error -quiet "$tex" >/dev/null 2>&1
  latexmk -c -quiet "$tex" >/dev/null 2>&1
  gzip -dc "$base.synctex.gz" | sed "s|^\(Input:[0-9]*:\)$here/\./|\1./|; s|^\(Input:[0-9]*:\)$here/|\1./|" |
    gzip -n >"$base.synctex.gz.tmp"
  mv "$base.synctex.gz.tmp" "$base.synctex.gz"
  if gzip -dc "$base.synctex.gz" | grep -q "$here"; then
    echo "error: $base.synctex.gz still contains $here" >&2
    exit 1
  fi
  echo "built $base.pdf"
done
