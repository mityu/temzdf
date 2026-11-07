# Vim mode

Vim's editing for text panes, as a script on the text editor framework
(`doc/design/text-editor.md`), the way Evil is in Emacs: the server knows
nothing of Vim. Built on 2026-11-07. Decisions marked **(user)** were the
user's.

## Using it

- The module `temzdf/edit/vim`; importing it does nothing **(user,
  2026-11-08)**: `vim.enable()` defines its modes, mappings and hook and
  makes text panes start in `vim/normal` (`o.basemode`, the focused pane at
  once), `vim.disable()` takes them away (text panes back to `edit`, what
  was open closed). The defaults enable it: text panes edit as Vim does
  **(user, 2026-11-08)**; `temzdf/edit/notepad` edits without modes.
- `vim.options.shiftwidth = 2` changes its settings (`shiftwidth`,
  `expandtab`, `ignorecase`, `smartcase`, `hlsearch`, `incsearch`,
  `wrapscan`); `:set` changes them too.
- Its mappings are in the namespace `builtin-edit-vim`: a configuration
  maps over them or removes them as any.

## How it is built

- Sources: `crates/temzdf-script/runtime/vim/*.ts` (entry
  `runtime/edit/vim.ts`), bundled by `crates/temzdf-script/build.rs` with
  esbuild into the module `temzdf/edit/vim` (`doc/design/script-host.md`);
  `runtime/lib/keyset.ts` makes enable/disable. No npm package of its own so
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
- Registers (`registers.ts`) are the session's copy history and named
  registers (`doc/design/registers.md`), so what other panes copy is in
  them: `"` the newest entry, `0` the newest copy, `1`-`9` and `-` the
  newest deletes, `a`-`z` (`A`-`Z` append) named, `+` and `*` the client's
  clipboard (written with `copyText(text, { clipboard: true })`, read with
  `readClipboard()` in a dispatched task: `"+p` puts once the client
  answered), `_`. Yanks and deletes go to the clipboard too, as Vim's
  `clipboard=unnamedplus` (`o.clipboard`); `"_` or a named register keeps
  it.
- Marks (`marks.ts`) are the framework's marks (empty decorations without
  a style: they follow edits, and a mark whose text was deleted is unset,
  `E20`), named in the buffer variable `vim.marks`, so they outlive a
  restarted script host and go with their buffer; in memory only, as the
  copy history. `a`-`z` are the buffer's; `A`-`Z` one in the session (kept
  in the buffer it was set in: going there shows that buffer in the pane);
  `'` (where the last jump began), `[` `]` (the last change or yank), `.`,
  `^` (where the insert mode was left) and `<` `>` (the last visual
  selection) are set by the editing commands. `'x` (linewise) and `` `x ``
  (exclusive) are motions; Ex ranges take `'x`; `:marks`, `:delm[arks][!]`,
  `:ma[rk]`/`:k`.
- The jump list is per pane (the pane variable `vim.jumps`, at most 100
  entries, each a mark): `G`, `gg`, `%`, `(`, `)`, `{`, `}`, `H`, `M`, `L`,
  marks, searches (`/ ? n N * #`) and `:N` leave an entry where they began
  (others on that line go); `<C-o>` goes back (taking the cursor's place
  along first), `<C-i>` and `<Tab>` forward, showing another buffer's entry
  in the pane.
- Macros (`macros.ts`): `q{a-z0-9}` records the keys typed (`A`-`Z`
  append) with a `KeyTyped` hook, hooked only while recording (it holds
  every key); `q` stops (that `q` is not kept). The keys go into the
  register as key notation (`<Esc>`), so `"ap` puts them so. `@{reg}`
  feeds them back (`feedkeys`, through the mappings; a count repeats
  them), `@@` the last, `@:` the last Ex command, `@+` the clipboard. An
  error, or a motion that fails (`beep()`), drops the rest of what was
  fed: `100@a` stops at the last line. Recording is said by a message
  when it starts, not on the status line (that waits for configurable
  status lines).
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
  `:s/pat/rep/[gieIc]` (`&`, `\0`-`\9`, `\r`, `\n`, `\t`, `\u \l \U \L \E`),
  `:d`, `:y`, `:noh`, `:set`, a line number. Errors are Vim's (`E486` and
  such), shown as error messages.
- Panes, buffers and tab pages, as Vim's windows, buffers and tabs:
  `:sp[lit]`/`:vs[plit] [file]` (the same buffer, or the file, in a pane
  beside: `o.splitbelow`, `o.splitright`), `:new`/`:vne[w] [file]` (a new
  buffer), `:ene[w]`, `:clo[se]` (`E444` on the last pane), `:on[ly]`
  (the tab page's other panes close, with `closePane(pane)`; their buffers
  stay hidden), `:bn`, `:bp`/`:bN`, `:b {N|name}`, `:ls`/`:buffers`/
  `:files`, `:bd[!] [N|name]` (its panes show another buffer first, a new
  empty one when it was the last; `!` sets the buffer variable
  `forceDelete` the shipped `BufferDeletePre` hook honours), `:tabnew`/
  `:tabe [file]`, `:tabn [N]`, `:tabp`, `:tabc`, `:tabo`, `:wa`, `:qa[!]`,
  `:wqa`/`:xa`.
- A block's `I` types on its first line and, at `<Esc>`, puts the text
  (when it has no line break) into the block's other lines that reach the
  block's start; `A` after the block, padding shorter lines with spaces;
  after `$` (the block reaches every line's end, until a motion across)
  `A` appends at each line's end; `c` deletes the block, then the same as
  `I` on the lines that had text in it. One undo step; `.` does it again
  from the cursor on as many lines. `I` and `A` on characters or lines
  insert at the selection's start or after its end. `r{char}` in the
  visual mode replaces every selected character (line breaks stay).
- `<C-r>{reg}` in the insert mode (the transient mode `vim/insreg` reads
  the name) types the register's text, so `.` and counts repeat it; `+`
  and `*` read the client's clipboard first.
- `:s` with `c` asks about each match in the transient-like mode
  `vim/confirm` (a fallback function reading one key): the match shown
  with `IncSearch`, the question as a message; `y`, `n`, `a`, `l`, `q`
  (`<Esc>`), `<C-e>`/`<C-y>`. A mark follows the range's end through the
  replacements; all of them are one undo step.
- `incsearch` (on by default): while `/` or `?` is typed, an
  `onChange` of its prompt runs a search from where the command line
  opened; the text pane's cursor goes to the match (`IncSearch`), every
  match is highlighted with `hlsearch`, and the cursor goes back when
  nothing matches, on `<Esc>`, and before `<CR>` searches. Not for `:s`
  (Neovim's `inccommand`).
- Hooks only read (the framework's rule): the cursor's shape following the
  mode, and closing an insert session or selection a focus change left
  open, are scheduled from a `ModeChanged` hook.

## Differences from Vim, and what is not there yet

- A mark at a line's start survives deleting that line (`dd` deletes from
  the line's start: the mark is at the edge, not inside); it moves to the
  next line. Vim deletes it.
- Not yet: `:g`, `:normal`, `:map`, folds, text formatting (`gq`),
  `gJ`, the insert mode's completion, search offsets (`/pat/e`), `&` and
  `~`, counts on visual operators, `.` after a visual `r`. Not wanted
  (the user, 2026-11-07): the insert mode's `<C-o>`.
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
