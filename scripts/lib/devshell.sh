# shellcheck shell=bash
# Sourced first thing by a script that needs the Nix dev shell:
#
#   source "$(dirname "$(realpath "${BASH_SOURCE[0]}")")/lib/devshell.sh" . "$@"
#
# The first argument is the dev shell to enter (`.` for the default one,
# `.#enable-miri`, ...), the rest are the caller's arguments, which the
# re-executed script gets again. It sets `self` (the caller's absolute path),
# changes to the repository root and, outside a dev shell (IN_NIX_SHELL
# unset), replaces the caller with itself run inside that shell. Inside one it
# returns and the caller goes on (a shell that lacks a tool is the caller's
# business: no nested entering).

self=$(realpath "${BASH_SOURCE[1]}")
cd "$(dirname "$(dirname "$self")")" || return

if [[ -z "${IN_NIX_SHELL:-}" ]]; then
  # The dirty-tree warning off through nix itself: a process substitution
  # filtering it would leave its pipe open in every child, and a daemon that
  # outlives the run (mozc_server) kept whatever reads our output waiting.
  NIX_CONFIG="warn-dirty = false${NIX_CONFIG:+$'\n'$NIX_CONFIG}" exec nix develop "$1" --quiet -c "$self" "${@:2}"
fi
