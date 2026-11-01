#!/usr/bin/env bash
# Check references to documents, so moving or renaming one cannot leave
# stale paths behind. Looks at the files git tracks (committed or added;
# `git add -N` a new file to include it), except the skills under .claude/
# (most are vendored and name files relative to themselves) and this file:
#
#   1. every path under doc/ written anywhere (code comments, scripts,
#      docs: `doc/design/session-server.md`, `doc/survey/`) exists;
#   2. every relative Markdown link, [text](path), in a .md file exists,
#      resolved from that file's directory (anchors are not checked);
#   3. every Markdown file named in backticks in a .md file (`todo.md`,
#      `crates/tuitest/README.md`) exists relative to the repository root,
#      as the conventions want paths written (AGENTS.md);
#   4. every document under doc/ is linked from doc/README.md, the index.
#
# It does not check URLs, headings or anchors, or whether a document's
# content is still true.
#
# Usage: scripts/check-docs.sh   (prints each problem as FILE:LINE: ...;
#                                 exits 1 if there was one)
set -uo pipefail

# shellcheck disable=SC2164
# This cd never fails.
cd "$(dirname "$(dirname "${BASH_SOURCE[0]}")")"

# The files looked at (pathspecs for git grep).
files=(. ':!.claude/skills' ':!scripts/check-docs.sh' ':!Cargo.lock')
md=('*.md' ':!.claude/skills')

bad=0
problem() {
  echo "$1"
  bad=1
}

# 1. Paths under doc/ (globs such as doc/** are not paths).
while IFS=: read -r file line path; do
  path=${path%.}
  [[ $path == *'*'* ]] && continue
  [[ -e $path ]] || problem "$file:$line: $path does not exist"
done < <(git grep -noE '(^|[^A-Za-z0-9_./-])doc/[A-Za-z0-9_./-]*[A-Za-z0-9_/]' -- "${files[@]}" |
  sed -E 's/^([^:]*:[0-9]+:)[^d]*/\1/')

# 2. Relative Markdown links in Markdown files.
while IFS=: read -r file line link; do
  target=${link#*](}
  target=${target%)}
  target=${target%%#*}
  [[ -z $target || $target == *://* || $target == mailto:* ]] && continue
  [[ -e $(dirname "$file")/$target ]] || problem "$file:$line: link to $target, not found from $(dirname "$file")/"
done < <(git grep -noE '\]\([^)[:space:]]+\)' -- "${md[@]}")

# 3. Markdown files named in backticks in Markdown files.
# shellcheck disable=SC2016
# The backticks are the pattern's, not a command substitution.
while IFS=: read -r file line name; do
  name=${name//\`/}
  [[ $name == *'*'* || $name == *'{'* ]] && continue
  [[ -e $name ]] || problem "$file:$line: \`$name\` does not exist (paths are relative to the repository root)"
done < <(git grep -noE '`[A-Za-z0-9_./-]+\.md`' -- "${md[@]}")

# 4. Every document is in the index.
[[ -f doc/README.md ]] || problem "doc/README.md: missing (the index of doc/)"
for doc in $(git ls-files 'doc/*.md' 'doc/**/*.md'); do
  [[ $doc == doc/README.md ]] && continue
  grep -qsF "](${doc#doc/})" doc/README.md || problem "doc/README.md: does not link to $doc"
done

exit "$bad"
