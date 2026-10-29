#!/usr/bin/env bash
# Headless frame regression check: render a fixed set of screens with
# `temzdf standalone pdf --render-frame` into a directory, or compare two such directories
# pixel by pixel. Used to show that a refactoring leaves the screen unchanged:
# render with the old build, render with the new one, compare.
#
# Usage: scripts/frames.sh render OUTDIR [TEMZDF]   (default TEMZDF: target/debug/temzdf)
#        scripts/frames.sh render-rev REV OUTDIR    (build revision REV, e.g. HEAD, and render)
#        scripts/frames.sh compare DIR_A DIR_B      (exit 1 on any difference)
#
# Typical check of uncommitted changes: render-rev HEAD A; cargo build;
# render B; compare A B. render-rev builds in a temporary worktree with its
# own target directory (target/frames-rev, kept for its dependencies): a
# shared one would leave artifacts of the old sources looking newer than the
# edited files, and cargo would not rebuild them.
#
# Fixtures must exist (scripts/build-fixtures.sh). Re-executes itself inside the
# Nix dev shell when needed (ImageMagick for `compare`).
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
  # The viewer moved to `temzdf standalone pdf`; older revisions take FILE.
  local -a viewer=("$bin")
  "$bin" standalone pdf --help >/dev/null 2>&1 && viewer=("$bin" standalone pdf)
  for s in "${scenarios[@]}"; do
    IFS='|' read -r name opts keys <<<"$s"
    local -a extra=()
    [[ -n "$keys" ]] && extra=("--keys=$keys")
    # shellcheck disable=SC2086
    "${viewer[@]}" target/fixtures/sample.pdf --render-frame "$out/$name.png" $opts "${extra[@]}"
  done
  echo "rendered ${#scenarios[@]} frames into $out"
}

render_rev() {
  local rev=$1 out=$2 root=$PWD
  # Global, for the EXIT trap: a failure under `set -e` leaves the script
  # without returning from here.
  rev_tree=$(mktemp -d "${TMPDIR:-/tmp}/frames-rev.XXXXXX")
  git worktree add --quiet --detach "$rev_tree" "$rev" || { rmdir "$rev_tree"; return 1; }
  trap 'git worktree remove --force "$rev_tree"' EXIT
  local tree=$rev_tree
  (cd "$tree" && CARGO_TARGET_DIR="$root/target/frames-rev" cargo build --quiet -p temzdf) ||
    { echo "building $rev failed" >&2; return 1; }
  render "$out" target/frames-rev/debug/temzdf
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
  render-rev) render_rev "${2:?REV}" "${3:?OUTDIR}" ;;
  compare) compare_dirs "${2:?DIR_A}" "${3:?DIR_B}" ;;
  *)
    echo "usage: $0 render OUTDIR [TEMZDF] | render-rev REV OUTDIR | compare DIR_A DIR_B" >&2
    exit 2
    ;;
esac
