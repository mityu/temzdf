#!/usr/bin/env bash
# Work on the `research/notes` branch (design notes and research, see AGENTS.md)
# without leaving the current branch: a git worktree at a fixed place.
#
# Usage:
#   scripts/notes.sh show FILE    print FILE as committed on research/notes
#                                 (no worktree needed just to read)
#   scripts/notes.sh open         create the worktree if missing; print its path
#   scripts/notes.sh close        remove the worktree (refuses with uncommitted
#                                 changes; commit them first)
#
# Editing: `W=$(scripts/notes.sh open)`, edit files under $W, then
# `git -C "$W" commit -am '...'` and `scripts/notes.sh close`.
#
# The worktree is `target/notes` (ignored; override with $TEMZDF_NOTES_DIR).
# It does not push, and does not look at any other branch.
set -euo pipefail

branch=research/notes
root=$(git rev-parse --show-toplevel)
dir=${TEMZDF_NOTES_DIR:-$root/target/notes}

# Whether $dir is a registered worktree of this repository.
registered() {
  git -C "$root" worktree list --porcelain | grep -qxF "worktree $(realpath -m "$dir")"
}

# Forget the worktree if its directory is gone (e.g. after `cargo clean`).
if [[ ! -d $dir ]] && registered; then
  git -C "$root" worktree prune
fi

case "${1:-}" in
  show)
    [[ $# -eq 2 ]] || { echo "usage: $0 show FILE" >&2; exit 2; }
    git -C "$root" show "$branch:$2"
    ;;
  open)
    if ! registered; then
      mkdir -p "$(dirname "$dir")"
      git -C "$root" worktree add -q "$dir" "$branch" >&2
    fi
    echo "$dir"
    ;;
  close)
    if ! registered; then
      echo "no worktree at $dir" >&2
      exit 0
    fi
    if [[ -n $(git -C "$dir" status --porcelain) ]]; then
      echo "uncommitted changes in $dir:" >&2
      git -C "$dir" status --short >&2
      exit 1
    fi
    git -C "$root" worktree remove "$dir"
    ;;
  *)
    sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'
    [[ "${1:-}" == -h || "${1:-}" == --help ]] || exit 2
    ;;
esac
