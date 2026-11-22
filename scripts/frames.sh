#!/usr/bin/env bash
# Headless frame regression check: render a fixed set of screens into a
# directory, or compare two such directories pixel by pixel. Used to show
# that a refactoring leaves the screen unchanged: render with the old build,
# render with the new one, compare.
#
# The screens: PDF scenes by `temzdf standalone pdf --render-frame`
# (OUTDIR/NAME.png), and the scenes of the `shots` test in
# crates/temzdf/tests/tui.rs (sessions, Markdown, terminal and floating
# panes, tabs, the command line) drawn by tuitest (OUTDIR/tui/NAME.png).
# The test's code is always the working tree's; TEMZDF_SHOTS_BIN gives it
# the binary to run, so an older revision's binary plays today's scenes. A
# scene that binary cannot play is named and left out (compare says
# MISSING).
#
# Usage: scripts/frames.sh render OUTDIR [TEMZDF]   (default TEMZDF: target/debug/temzdf)
#        scripts/frames.sh render-rev REV OUTDIR    (build revision REV, e.g. HEAD, and render)
#        scripts/frames.sh compare DIR_A DIR_B      (exit 1 on any difference)
#
# Typical check of uncommitted changes: render-rev HEAD A; cargo build;
# render B; compare A B. compare checks every PNG in either directory and
# writes an image of each difference to DIR_B/diff/NAME.png. render-rev
# builds in a temporary worktree with its own target directory
# (target/frames-rev, kept for its dependencies): a shared one would leave
# artifacts of the old sources looking newer than the edited files, and
# cargo would not rebuild them.
#
# Fixtures must exist (scripts/build-fixtures.sh). Re-executes itself inside the
# Nix dev shell when needed (ImageMagick for `compare`, the tests' fonts in
# TEMZDF_TEST_FONTCONFIG).
set -euo pipefail

# shellcheck source=SCRIPTDIR/lib/devshell.sh
source "$(dirname "$(realpath "${BASH_SOURCE[0]}")")/lib/devshell.sh" . "$@"

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
  render_tui "$out/tui" "$bin"
}

# The `shots` test's scenes, played by temzdf binary $2, into directory $1.
render_tui() {
  local out=$1 bin=$2 log=$1.log
  rm -rf "$out"
  mkdir -p "$out"
  if FONTCONFIG_FILE=${TEMZDF_TEST_FONTCONFIG:?the dev shell sets it} \
    TEMZDF_SHOTS_DIR=$(realpath "$out") TEMZDF_SHOTS_BIN=$(realpath "$bin") \
    cargo test --quiet -p temzdf --test tui -- --ignored --exact shots::shots >"$log" 2>&1; then
    echo "rendered $(find "$out" -name '*.png' | wc -l) tuitest scenes into $out"
  elif grep -q '^shots: scene' "$log"; then
    # Scenes this binary cannot play (an older revision's): named, the
    # others rendered.
    sed -n 's|^shots: scene \([^ ]*\) failed.*|not rendered: tui/\1|p' "$log" >&2
    echo "(why: $log)" >&2
    echo "rendered $(find "$out" -name '*.png' | wc -l) tuitest scenes into $out"
  else
    tail -n 30 "$log" >&2
    echo "the tuitest scenes failed (whole output in $log)" >&2
    return 1
  fi
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

# The PNGs under directory $1 (relative paths), but its diff/.
list_pngs() {
  (cd "$1" && find . -path ./diff -prune -o -name '*.png' -print | sed 's|^\./||')
}

compare_dirs() {
  local a=$1 b=$2 failed=0 name diff
  local -a names
  mapfile -t names < <({ list_pngs "$a"; list_pngs "$b"; } | sort -u)
  ((${#names[@]})) || { echo "no frames in $a or $b" >&2; exit 1; }
  rm -rf "$b/diff"
  for name in "${names[@]}"; do
    if [[ ! -f "$a/$name" || ! -f "$b/$name" ]]; then
      echo "MISSING ${name%.png}"
      failed=1
      continue
    fi
    diff=$(compare -metric AE "$a/$name" "$b/$name" null: 2>&1 || true)
    if [[ "$diff" != "0" && "$diff" != "0 (0)" ]]; then
      mkdir -p "$(dirname "$b/diff/$name")"
      compare "$a/$name" "$b/$name" "$b/diff/$name" 2>/dev/null || true
      echo "DIFF ${name%.png}: $diff pixels ($b/diff/$name)"
      failed=1
    fi
  done
  ((failed)) && exit 1
  echo "all ${#names[@]} frames identical"
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
