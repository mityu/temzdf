# Vim mode

Vim's editing for text panes, as a script on the text editor framework
(`doc/design/text-editor.md`), the way Evil is in Emacs: the server knows
nothing of Vim. Built on 2026-11-07. Decisions marked **(user)** were the
user's.

## Using it

- Shipped and turned on by the configuration **(user)**: `import
  "temzdf/vim";` in `config.ts` makes text panes start in `vim/normal`
  (`o.basemode`). Without it, text panes edit without modes (the shipped
  `defaults.ts`).
- `import vim from "temzdf/vim"; vim.options.shiftwidth = 2;` changes its
  settings (`shiftwidth`, `expandtab`, `ignorecase`, `smartcase`,
  `hlsearch`, `wrapscan`); `:set` changes them too.
- Its mappings are in the namespace `vim`: a configuration maps over them
  or removes them as any.

## How it is built

- Sources: `crates/temzdf-script/runtime/vim/*.ts`, bundled by
  `crates/temzdf-script/build.rs` with esbuild into the module
  `temzdf/vim` (`doc/design/script-host.md`). No npm package of its own so
  far: none fitted (Vim emulations are tied to their editors).
- Modes: `vim/normal`, `vim/insert` (pass-through: typed text goes in),
  `vim/visual`, and transient ones: `vim/pending` (an operator waiting for
  its motion; its fallback function cancels it), `vim/char` (the character
  of `f`, `t`, `r`), `vim/register` (`"x`), `vim/cmdline` (`/ ? :` over the
  input mode, `keepalive: "always"` so the input mode's editing keys work).
  A transient mode with a fallback function keeps the keys it does not map
  (the framework's rule, section 11 of the editor's specification).
- Vim's cursor is on a character; the framework's sits between them: the
  character after the framework's cursor is Vim's. In the normal mode the
  cursor is kept off a line's end (`text.setCursor(pos, { keepColumn })`
  keeps the column `j` and `k` aim for).
- Motions (`motions.ts`) return a target and how an operator reads it
  (exclusive, inclusive, linewise); text objects (`textobjects.ts`) a range
  of their own. Operators (`operators.ts`) turn that into a span with
  Vim's rules: an exclusive motion ending at a line's start ends at the
  previous line's end, and is linewise from at or before the first
  non-blank; `dw` on a line's last word stops at the line's end; `cw` is
  `ce`.
- Registers (`registers.ts`): `"`, `0`, `1`-`9` (deletes of lines or more),
  `-`, `a`-`z` (`A`-`Z` append), `+` and `*` (written with
  `setClipboard`; reading the system clipboard is not possible yet, so
  they give back what was put there), `_`.
- The visual selection is a decoration (style `Selection`), not the
  framework's selection, so the cursor stays on Vim's character
  (inclusive ends).
- An insert session is one undo step (`text.undoBegin`/`undoEnd`). What was
  typed is the text between a mark set where the session began and the
  cursor at `<Esc>`: `.` positions the cursor (or deletes, for `c`) and
  types it again. A count repeats it (`3ix<Esc>`, `3ox<Esc>`).
- `.` repeats the last change as a closure that runs it again from the
  cursor; a count replaces the original one.
- Searching converts Vim's patterns to Rust's regex (`pattern.ts`): the
  magic levels, `\( \| \+ \= \? \{n,m} \{-}`, `\< \>` (as `\b{start}` and
  `\b{end}`), `\zs \ze` (a group the search counts as the match), Vim's
  ASCII classes (`\s \d \w \a \l \u \x \h`, `\k` as the default word
  characters), `\n`, `\_x`, `\c \C`, back references (through
  fancy-regex). `hlsearch` is `highlightPattern`.
- Ex commands (`ex.ts`): ranges (`%`, `.`, `$`, numbers, `'<,'>`, `+N`
  `-N`), `:w[!] [file]`, `:q[!]`, `:wq`, `:x`, `:e[!] [file]`,
  `:s/pat/rep/[gieI]` (`&`, `\0`-`\9`, `\r`, `\n`, `\t`, `\u \l \U \L \E`),
  `:d`, `:y`, `:noh`, `:set`, a line number. Errors are Vim's (`E486` and
  such), shown as error messages.
- Hooks only read (the framework's rule): the cursor's shape following the
  mode, and closing an insert session or selection a focus change left
  open, are scheduled from a `ModeChanged` hook.

## Differences from Vim, and what is not there yet

- Not yet: marks and the jump list, macros (the user chose to leave them
  for later), `:g`, `:normal`, `:map`, folds, text formatting (`gq`),
  `gJ`, the insert mode's `<C-o>`, `<C-r>{reg}` and completion, `I`/`A` on
  a block, `incsearch`, search offsets (`/pat/e`), `:s` with `c`, `&`
  and `~`, counts on visual operators.
- In a `:s` replacement `\n` is a line break (Vim inserts a NUL), as `\r`.
- `:q` on the last pane ends the session (refused with unsaved changes by
  the shipped hooks); `:q!` there ends it anyway. On another pane it
  closes the pane and its buffer stays, hidden (Vim's `hidden`).
- Block columns count characters, not display columns (tabs and wide
  characters in a block select differently).

## Tests

- `crates/temzdf-script/runtime/vim/test/*.cases`: hand-written cases
  (text with `|` for the cursor, keys, the text and cursor expected,
  optionally the mode or an error), run by
  `crates/temzdf-server/src/session/vim_tests.rs`: a session with the real
  script host (the WebAssembly guest, in process), keys typed one at a
  time. `VIM_CASE=name cargo test -p temzdf-server vim_cases` runs some.
- `crates/temzdf/tests/types.rs` type checks the sources with `tsc`.
- `crates/temzdf/tests/tui.rs`: the real binary with the mode turned on,
  editing and `:w`, `:q`.
