#!/usr/bin/env bash
# Headless frame regression check: render a fixed set of screens with
# `temzdf --render-frame` into a directory, or compare two such directories
# pixel by pixel. Used to show that a refactoring leaves the screen unchanged:
# render with the old build, render with the new one, compare.
#
# Usage: scripts/frames.sh render OUTDIR [TEMZDF]   (default TEMZDF: target/debug/temzdf)
#        scripts/frames.sh compare DIR_A DIR_B      (exit 1 on any difference)
#
# Fixtures must exist (scripts/build-fixtures.sh). Re-executes itself inside the
# Nix dev shell when needed (ImageMagick for `compare`).
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ -z "${IN_NIX_SHELL:-}" ]]; then
  exec nix develop --quiet -c "$0" "$@" 2> >(grep -v "Git tree .* is dirty" >&2)
fi

# name | options after the PDF | keys pressed first (Vim feedkeys notation)
scenarios=(
  "start||"
  "page2|--page 2|"
  "zoomed|--scale 2.5|"
  "small-cells|--size 80x24 --cell 8x16|"
  "scroll||jjjjj"
  "half-page||d"
  "last-page||G"
  "zoom-in||++"
  "zoom-out||---"
  "pan-right|--scale 3|lll"
  "sidebar||<Tab>"
  "sidebar-move||<Tab>jj"
  "sidebar-jump||<Tab>jj<CR>"
  "search-prompt||/nee"
  "search||/needle<CR>"
  "search-next||/needle<CR>nn"
  "history||G<C-o>"
)

render() {
  local out=$1 bin=${2:-target/debug/temzdf} name opts keys
  mkdir -p "$out"
  for s in "${scenarios[@]}"; do
    IFS='|' read -r name opts keys <<<"$s"
    local -a extra=()
    [[ -n "$keys" ]] && extra=("--keys=$keys")
    # shellcheck disable=SC2086
    "$bin" target/fixtures/sample.pdf --render-frame "$out/$name.png" $opts "${extra[@]}"
  done
  echo "rendered ${#scenarios[@]} frames into $out"
}

compare_dirs() {
  local a=$1 b=$2 failed=0 name diff
  for s in "${scenarios[@]}"; do
    name=${s%%|*}
    if [[ ! -f "$a/$name.png" || ! -f "$b/$name.png" ]]; then
      echo "MISSING $name"
      failed=1
      continue
    fi
    diff=$(compare -metric AE "$a/$name.png" "$b/$name.png" null: 2>&1 || true)
    if [[ "$diff" != "0" && "$diff" != "0 (0)" ]]; then
      echo "DIFF $name: $diff pixels"
      failed=1
    fi
  done
  ((failed)) && exit 1
  echo "all ${#scenarios[@]} frames identical"
}

case "${1:-}" in
  render) render "${2:?OUTDIR}" "${3:-}" ;;
  compare) compare_dirs "${2:?DIR_A}" "${3:?DIR_B}" ;;
  *)
    echo "usage: $0 render OUTDIR [TEMZDF] | compare DIR_A DIR_B" >&2
    exit 2
    ;;
esac
