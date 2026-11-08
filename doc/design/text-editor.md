# Text editor framework: specification

The text editing pane, as specified with the user on 2026-11-07. **This is
a specification, built in part** (what is, and the order of the rest:
`todo.md`). The research behind it (VS Code,
Neovim, Zed, Vim's undo, swap files and regular expressions, Tree-sitter) is
in `doc/survey/text-editor.md`. Decisions marked **(user)** were the user's
own call or correction; the others were proposals the user accepted. The
script host's round trips are measured ("Script host round trips"
below); how it is built is in "Implementation" at the end.

## 1. What it is

- The Rust part is **a framework for building text editors**, not an
  editor: the relation of Emacs' C core to Emacs Lisp **(user)**. It holds
  what every editor needs: positions and text, motions, the selection,
  decorations (and marks), the undo tree, search, events, key-input
  pieces, and drawing (wrapping, widths, the cursor).
- Editors are scripts. Vim-like modal editing is a script built on the
  primitives, as Evil is in Emacs **(user)**; temzdf does not reimplement
  Vim. The default mode-less editing (typing inserts, arrows and Home/End
  move, Shift with arrows selects, `<C-s>` saves, `<C-z>`/`<C-y>` undo and
  redo, `<C-b>` the prefix as in other panes) is the shipped
  `crates/temzdf-script/runtime/defaults.ts`, in the same position as a Vim
  plugin.
- Policies (the default undo grouping, where the cursor goes after undo)
  are framework defaults scripts can change.

## 2. Uses and scope

- Two uses **(user)**: a component for command-input panes (prompts:
  done 2026-11-07, `openPrompt`, a `nofile`, `unlisted` buffer with its
  prompt as inline virtual text; not limited to one line, `<CR>` runs the
  cursor's line, as Vim's command-line window **(user)**), and editing
  arbitrary files.
- Read-only views (help, command output) are not considered now **(user)**.
- Encodings: UTF-8 by default, a BOM kept on save. A file that is not
  valid UTF-8 is not opened (with a message); the user can reopen it naming
  an encoding (Shift_JIS, EUC-JP, ...; `encoding_rs`), and it is saved in
  that encoding. No guessing **(user)**: a wrong guess corrupts the file on
  save.
- Size: files of tens of MB open and edit without stalls; a 1 MB line does
  not freeze the pane (only what is visible is laid out); binary files are
  not opened.

## 3. Positions

Following VS Code **(user)**:

- Text is UTF-8 inside (`ropey`). Scripts see positions as a zero-based
  line and a **UTF-16** column within it **(user)**: JavaScript strings index
  in UTF-16 (`line.slice(a, b)` works), and it is LSP's default. Columns
  exclude the line ending; ranges are half-open.
- Out-of-range positions are clamped, not errors (last line, end of line);
  a reversed range is swapped. A position inside a surrogate pair moves to
  before the pair; a position inside a grapheme cluster is accepted.
  Emoji do not favour UTF-8 or UTF-16: any unit can point into a character
  a user sees as one, so the clamping rules matter, not the unit.
- Cursor motions step by grapheme.
- One line ending per document (LF or CRLF); mixed files are normalised
  when opened.

## 4. Cursor and selection

- The cursor sits **between** characters, as in VS Code, and may be at the
  end of a line. Its shape (bar, block, underline) is set per pane; a block
  is drawn over the next character. Vim's constraints (not past the last
  character in Normal mode) are the script's job.
- **One selection with a kind**: character, line or block **(user)**; no
  multiple cursors **(user)**. It is an anchor, an active end (the cursor)
  and a kind.
- A character selection excludes the active end (half-open, as VS Code);
  Vim's inclusive Visual mode moves the end by one in the script.
- A block selection is bounded by **display columns** **(user)**; a wide
  character or tab partly inside is included whole; it can extend to each
  line's end (Vim's `$`); operations split it into per-line ranges.
- Cursor and selection are per pane and shift with edits made elsewhere.
- The desired column for vertical moves is kept by the server, in display
  columns.

## 5. Primitives

- **Edits**, as VS Code **(user)**: `edit([{ range, text }, ...])` applies
  non-overlapping replacements given in the original document's positions,
  at once. `insert`, `delete` and `replace` are shorthands. Cursors and
  selection ends shift with edits; an insertion exactly at a cursor leaves
  that cursor after the inserted text.
- **Queries**: `lineCount`, `line(n)`, `text(range?)`, `cursor`,
  `selection`, `offsetAt`/`positionAt` (UTF-16 offsets), `info` (path,
  modified, encoding, line ending).
- **Motions are the server's** **(user)**: `positionOf(from, unit, count)`
  returns a target without moving; `move(unit, count, { select })` moves.
  Units: grapheme, word starts and ends, lines (keeping the desired
  column), display lines, line start, first non-blank, line end, document
  start and end, page. Graphemes, widths and wrapping are known only to the
  server, so computing these in scripts would be slower and error-prone.
  Vim's operators are `positionOf` plus `delete` and friends in a script.
- **Selection and cursor**: `setCursor`, `setSelection(anchor, active,
  kind)`, `clearSelection`, `setCursorStyle`.
- **View**: `scroll`, `reveal(pos, "center" | "top" | "minimal")`,
  `visibleRange`.

## 6. Search

- `search(pattern, ...)` takes Rust `regex` syntax: start position,
  direction, wrap-around, a line range, which group counts as the match,
  all capture ranges, and an option to check word boundaries against the
  buffer's word characters.
- Converting Vim patterns to Rust syntax is the Vim script's job **(user)**,
  so the server knows nothing of Vim. Most everyday patterns convert
  (magic levels, quantifiers including non-greedy ones, groups,
  alternation, `\%[]`, classes rewritten to Vim's ASCII meaning, `\k` built
  from the current settings, anchors, `\c`). `\<`/`\>` map to regex 1.10's,
  with an optional post-check against `iskeyword`; `\zs`/`\ze` become a
  capture group; `\%23l` narrows the searched lines; column, mark and
  cursor assertions are post-checks. Back references, look-around, atomic
  groups and `\&` need `fancy-regex`; the engine is picked per pattern.
- Large buffers are searched in place with `regex-cursor` (as Helix);
  `fancy-regex` works on lines or nearby ranges.
- Building replacements (`\1`, `&`, `\u`, `\=` expressions) is the script's.

## 7. Decorations and marks

- A mark is a zero-width decoration without a style **(user)**, as in
  Neovim's extmarks; there is one mechanism.
- A decoration has a start and an end, an optional style (a highlight group
  or explicit colours and attributes), a gravity per end (stay before or
  move after an insertion exactly there), and is queried, moved and
  deleted by id. When the text containing it is deleted it collapses to the
  start and is flagged deleted (a Vim script treats such a mark as unset);
  undo restores its position. It has a namespace (cleared together), a
  priority, an optional pane (shown only there) and an optional `ttl` in
  milliseconds (a yank flash).
- The selection and the cursor are drawn above decorations.
- Per-window lists (the jump list) are scripts holding decoration ids.
- `highlightPattern(ns, pattern, style)` paints matches of a pattern found
  while drawing, only in what is drawn: `hlsearch`, trailing blanks,
  `TODO`, cheap on large files.
- First version: highlights only **(user)**. Then virtual text (2026-11-07,
  the user asking for Neovim's three places): a decoration's `virt_text`
  at its start, `inline` (layout places it before the character there:
  it takes columns, has no characters, so the cursor and offsets skip it;
  it goes with that character when rows wrap, so a row still depends only
  on its start, and one too wide for a row is cut), `eol` and `overlay`
  (drawn over the rows, layout unchanged). View methods take the buffer
  (its marks) and a view knows its pane (decorations shown only there).
  Later: images before or after a range, replacement (conceal, math
  previews), signs, virtual lines.

## 8. Undo

- Per buffer, a **tree** from the start **(user)**; `undo` and `redo`
  follow the current branch; querying the tree and jumping in it come
  later.
- Default grouping: one step per invocation (see "Events"), consecutive
  typing merged. `undo.begin()`/`undo.end()` group across invocations (a
  Vim Insert session), nestable, closed by `undo()` if left open;
  `undo.boundary()` splits inside one.
- After undo the cursor and selection go back to where they were before the
  change (in the pane that ran it), and `undo()` returns the changed ranges
  (a Vim script moves to their start).
- The saved state is remembered: undoing back to it clears "modified".
- Kept in files (`o.undofile`, 2026-11-08; off by default **(user)**:
  deleted text, a removed password too, stays in them): written when the
  buffer is written, only then **(user)**, as Vim's undofile, read back
  when its file is opened again (`undofile.rs`: JSON, versioned; every
  node, its changes and cursors, the saved state; refused for another path
  or text other than what was written, a FNV-1a of it; it starts at the
  saved state). The marks a step collapsed and the panes are not kept.
  `$XDG_STATE_HOME/temzdf/undo/<FNV-1a of the path>.json`, 0600 in a 0700
  directory, written to a temporary file and renamed.

## 9. Events

All events are **synchronous** **(user)**: the next key waits for the hooks.
Notifications that do not wait would let `feedkeys` or mode changes in a
hook land at a time that depends on typing speed. Asynchronous work is the
hook's own `dispatch`. Events are sent only while someone subscribes.

- **`InvokePre` / `InvokePost`**: once per invocation started by user input
  (a key or key sequence that runs a mapping, the mouse, a CLI action), the
  equivalent of Emacs' `pre-command-hook`/`post-command-hook`. Not per
  command: `[action.a(), action.b()]` or a function issuing five `cmd.*`
  calls is one invocation; `cmd.*` from hooks or timers is none. They carry
  a sequence number, what was invoked (an action value or a mapped
  function's id), the keys, the count, the mode, the pane and the source
  (key, mouse, CLI, replay); `InvokePost` adds the cursor and selection
  after. `invoke(target, { count })` runs it again.
- Unmapped keys: a key that types into an editor buffer (a prompt too) is an
  invocation ("text input", with the text; IME commits and pastes too); keys
  passed to a terminal program or dropped are not **(user asked; agreed)**.
- **`TextChanged`**: once per editing primitive call, with the changes
  (range in pre-change positions, deleted length, inserted text; not the
  deleted text), a version number, the cause (script, typing, undo, redo,
  reload) and the invocation it happened in (none for hooks and timers).
  It reaches hooks **before the editing `cmd.*` returns**, also for the
  script's own edits, as Emacs' `after-change-functions` (the host serves
  nested hook calls while waiting for a reply).
- **`KeyTyped`**: when a key arrives, before matching; observe only (it
  cannot drop or change keys, or it would be a second mapping layer); the
  key as typed and its source (typed, `feedkeys`, replay). Macros are
  recorded by scripts with it **(user)**, subscribing only while recording;
  `showcmd` and key hints use it too. There is no server recording API;
  replay is `feedkeys`.
- **`FileChangedOnDisk(buffer)`**: see "Files and buffers".

## 10. What hooks may do

- **The synchronous part of a hook does not run state-changing actions**
  **(user)**: only queries and the script's own state (recording a repeat).
  Changes (mode, focus, layout, edits, `feedkeys`) go to `defer(fn)`
  (named `schedule(fn)` when built, see Implementation), which
  runs at the **safe point**: after the current invocation and its hooks,
  before the next key is matched, with input held (like Vim's `SafeState`),
  so the order is fixed. `InvokePost` is itself at the safe point and may act
  directly.
- Hooks answer through return values, e.g. a pre-hook returning
  `{ cancel: true, reason }`.
- Hooks whose purpose is to act declare their exception in their
  definition, e.g. a future "before saving" hook may edit that buffer.
- `dispatch` stays available; it does not hold input, so what it does can
  interleave with typing: the script's choice.
- Enforced at run time through the context tokens
  (`doc/design/scripting.md`, "Context tokens").
- How it was reached: listing every event for whether `switchMode` is safe
  there left only mapped functions, `fallback`, `InvokePost`,
  `SessionReady` and `dispatch`, so the user asked whether hooks should act
  at all. Per-event locks (as Vim's `textlock`) and an allow-list were the
  alternatives.
- `defaults.ts` changes accordingly: hiding terminals' status lines in
  `PaneOpened`, ending a search in `BufferHidden` and previewing it on
  each change (now a prompt's `onChange`) move to `defer` (or, for the status line, a per-kind
  setting).

## 11. Key input

- A mode's `unmapped` becomes **`fallback`** **(user named it)**: `"drop"`,
  `"passthrough"` or a function `(ctx, key) => void` given the unmatched
  key. "Leave and swallow the key" (an invalid key after an operator) is
  `fallback: () => cmd.exitMode()`. This replaces the third `keepalive`
  value noted in `todo.md`.
- Reading one key (`f{char}`, `r{char}`, `m{mark}`, `"{reg}`) is a
  transient mode with a `fallback` function; counts and such pass through
  script variables.
- `getchar()`- and `input()`-like reading (`readKey`, `await` in
  asynchronous mappings) is **deferred** **(user)**: asynchronous mappings
  need care ("Future: asynchronous mapped and hook functions" in
  `doc/design/scripting.md`). The user wants `input()`-driven operations
  later (sandwich.vim's surround with a function name).

## 12. A pane's base mode

- Today `pane_mode()` in `crates/temzdf-server/src/session.rs` fixes it to
  the pane kind's name. It is built in because focus changes rebuild the
  mode history synchronously in the server; a hook calling `switchMode`
  would need the host, run in the middle of a mapping
  (`[moveFocus(), exitMode()]` pops what the hook pushed) and have to fire
  on every focus path (`doc/design/keymaps.md`, "Focus changes").
- Scripts choose it **as data**: **`basemode`**, the bottom of the mode
  history **(user)**. Per pane kind it is set only while the configuration
  loads (`o.basemode = { edit: "vim/normal" }`); per buffer only as an
  argument when the buffer is created (`newBuffer({ basemode })`), never
  changed afterwards **(user)**, so it cannot drift from what is shown. A
  command-input buffer and a file buffer differ this way.
- Rejected: asking a function on focus changes, setting it from hooks (the
  first focus can come before the hook), an `initialMode` pushed once
  **(user)**, and switching from a focus hook with `defer`: the order is
  fixed but the hook's mode still lands on top of whatever the mapping
  meant (`[moveFocus(), switchMode("Y")]` ends in the hook's mode), the
  bottom stays the kind's mode and `ModeChanged` fires twice.

## 13. Files and buffers

- `newBuffer({ basemode, text?, path?, attrs? })`: buffers without a file;
  with only `path`, a new file. Attributes as Vim's `buftype`, but a list
  **(user)** so that several hold at once: `nofile` (never modified,
  written only to a path given), `unlisted` (a script's own: not listed);
  unknown names are errors, more come as needed. `openFile(path, { encoding?, basemode? })`:
  errors when unreadable; returns the existing buffer for the same file.
  Choosing PDF or text by extension is `defaults.ts`'s policy.
- Swap files (2026-11-08; on by default **(user)**, as Neovim;
  `session/swap.rs`): the mechanism is the server's, the asking the
  scripts'. Named as Neovim's in `$XDG_STATE_HOME/temzdf/swap/`, JSON
  with the owner and the whole text, 0600, replaced whole; written when
  the file is opened, `o.updatetime` after the last change, when it is
  written; removed with the buffer and at a normal end (explicitly, not
  in `Drop`: a panic must leave them). Opening behaves as Neovim's
  **(user)**: a running owner (its session answers on its socket with
  the swap file's pid) is passed over (W325) and the next name taken; a
  gone one is `SwapExists`, answered by `temzdf/buffers` with Neovim's
  E325 prompt (a mode reading one key; the message an error notice, so it
  stays until the key). Not yet: buffers without a file. `o.directory`
  and `o.undodir` (Vim's names, one path each, `~/` expanded) choose
  where swap and undo files go.
- `write(buffer, { path?, encoding?, force? })` writes a temporary file and
  renames it over the target, copying permissions, replacing a symlink's
  target, writing in place when the directory is not writable. It keeps the
  encoding, BOM, line endings and whether the file ends with a newline
  (adding one is a script's "before saving" hook). A file changed on disk
  since it was read is an error unless `force`.
- "Modified" comes from the undo tree's saved state (`info().modified`).
- The server watches files (the PDF reload watcher) and emits
  `FileChangedOnDisk`. `reload(buffer)` applies the difference (`similar`)
  as one edit, so marks and decorations survive and it can be undone;
  `TextChanged` gives "reload" as the cause. Whether to reload by itself is
  the script's policy.
- Closing or ending with unsaved changes: the server does not stop it; the
  pre-hooks (`PaneClosePre`, a new `BufferDeletePre`, one before the session
  ends) return `{ cancel: true, reason }`, and the server returns the reason
  to **whoever asked** (the CLI's output and a non-zero exit, or the message
  area for a key), so it works with no client attached **(user raised it)**.
  `kill-session --force` ends anyway. No recovery files **(user)**: until
  swap files exist, a forced end (`--force`, signals) loses unsaved changes.
- Relative paths resolve against the session's working directory; using a
  terminal's directory instead is a script's policy.

## 14. Drawing

- Wrapping per pane, with word boundaries (`linebreak`), indent
  (`breakindent`) and a marker (`showbreak`). Vertical scrolling is by
  display rows: the top is a buffer line plus a row within it, so long
  wrapped lines (LaTeX paragraphs) scroll smoothly. Without wrapping,
  horizontal scrolling. `scrolloff`.
- `tabstop` per buffer (display width); `expandtab` is a script policy.
  Widths follow the terminal panes' rule (per grapheme); control
  characters show as `^X`.
- Line numbers (off, absolute, relative, both), `cursorline` and a hollow
  cursor in unfocused panes are drawing settings in the server (computed on
  every draw, so not script round trips).
- The pane's status line has fixed contents for now: file name, modified,
  line and column, position in the file, mode, encoding, line ending.
  Customising it comes later, all at once **(user)**.
- Colours come from highlight groups; their definitions and the default
  scheme are the scripts'.
- Later: folding, conceal, `listchars`, signs, virtual lines.

## 15. Smaller primitives

- `wordchars` per buffer, a Rust regex character class (`[\p{L}\p{N}_]`),
  used by word motions, word-boundary checks and double-click selection.
  Word characters are further split by script (hiragana, katakana, kanji,
  ...) as Vim does, so unspaced Japanese is not one word; no dictionary
  segmentation.
- `findPair(pos, { open, close, direction })` counts nesting to find the
  matching (or enclosing) bracket; strings and comments are not special
  (as Vim's `%`).
- Variables per buffer, pane and tab page (Vim's `b:`, `w:`, `t:`; `t:`
  added by the user), held by the server, JSON values only, gone with their
  owner, surviving host restarts. Function values are deferred **(user)**
  (a split, JSON in the server and functions in the host, was considered).
- `copy(text, { primary? })` writes the clipboard. Reading it is deferred
  with asynchronous mappings **(user)**: terminals usually refuse OSC 52
  reads and the GUI's answer is a round trip; until then `paste` inserts
  what arrives at the cursor.

## 16. Guidance for scripts

**Dot-repeat** is a Vim script built on the events (`doc/design/dot-repeat.md`
predates this and is superseded where they differ). Evil, read on
2026-11-07: a unit is the commands from Normal (or Visual) state back to
Normal; each command declares `:repeat` (`t` keys, the default; `change`,
buffer changes relative to point from `after-change-functions`; `motion`;
`ignore`; `abort`, e.g. `delete-window`, `save-buffer`); replay types keys
with `execute-kbd-macro` and applies changes with a function.

- Inside an Insert session, classify by effect, overridden by declarations
  **(user)**: edits to the target buffer are recorded relative to the
  cursor; operations that do not touch the buffer (closing another pane)
  are neither recorded nor split the unit; other buffers' edits are not
  recorded; losing focus ends the session.
- Replay **(user)**: invoke the entry again, apply the recorded edits,
  invoke the exit again; a session that ended without its exit (a focus
  change) is dropped and `.` keeps its previous content, as in Evil.
- Declarations attach to a **region of execution** **(user's idea)**:
  ```ts
  keymap.add("ed/insert", "<Left>", () => {
    plugin.ed.dotrepeat("call", () => { cmd.moveCursor(/* ... */); });
  });
  ```
  The kind holds while `fn` runs: `call` records `fn` itself and calls it
  again on replay (an anonymous closure is fine; it receives the count),
  `change` records its edits, `ignore` neither records nor splits, `abort`
  drops the unit (names provisional). A record is a sequence of regions,
  so a function that edits, calls a wrapped region and edits again records
  all three; the outermost region wins. This replaced naming user commands
  or attaching `meta` to mappings. (In temzdf `cmd.x()` runs a command now
  and `action.x()` is the same command as data the server runs itself, as
  a mapping's right-hand side; mapped functions have no name to attach a
  declaration to.)
- Open, for when the Vim script is written: the default for an undeclared
  cursor move inside an Insert session (Vim splits there; Evil records it).

**Reading one key**:
```ts
defineMode("vim/find-char", {
  transient: true, keepalive: "always",
  fallback: (ctx, key) => { findChar(key.text, pendingCount); cmd.exitMode(); },
});
keymap.add("vim/normal", "f", (ctx) => { pendingCount = ctx.count; cmd.switchMode("vim/find-char"); });
```

## 17. Script host round trips

In this design one key can issue many primitives, each a round trip to the
script host, and synchronous events add more. Before designing,
the user asked to measure three forms with one benchmark (a mapped
function calling a trivial command many times), in `experiments/` with the
results in `doc/survey/`:

- today's pipe to a separate process;
- `rquickjs` in the server process;
- QuickJS inside `wasmtime` in the server process (in-process isolation:
  traps instead of crashes, epoch interruption, memory limits).

The primitives do not depend on the transport. Measured on 2026-11-07
(`doc/survey/script-host-transport.md`): per command about 11-30 µs over
the pipe, 2-6 µs in process, 4.5-15 µs in wasmtime; a key issuing 50
commands stays under about 1.5 ms at the median in every form and
condition, so the transport does not rule the design out. The user chose
wasmtime, precompiled at build time (`doc/design/scripting.md`); through
the server's runner and its thread a command costs about 10 µs
(`doc/survey/script-host-transport.md`, "In the server").

## 18. Implementation

Decided with the user on 2026-11-07, before building:

- **The crate is `crates/temzdf-texteditor`** **(user named it)**: a
  library with no terminal code, depending on `temzdf-scene` only
  (`temzdf-scene <- temzdf-texteditor <- temzdf-server`). It holds the
  text (`ropey`, LF inside), positions, edits, decorations and marks, the
  undo tree, motions, layout (wrapping, display rows, widths) and drawing
  into `temzdf-scene` text cells, search, and reading and writing files.
  Widths follow the GUI's rule (per grapheme with `unicode-width`, 1 or 2).
- The server side is `crates/temzdf-server/src/session/text.rs`: a buffer
  store beside the PDF, terminal and input ones, and a pane kind whose
  name (and default base mode) is `edit`.
- **Scripts reach the primitives under `text.*`** **(user)**:
  `cmd.text.edit([...])`, `cmd.text.move("word", 1)`,
  `action.text.move(...)` (prompts are edited with them too). The existing `edit(path)`
  (open a PDF in the focused pane, Vim's `:edit`) stays. The names in the
  sections above (`edit`, `move`, `positionOf`, ...) are these.
- **A position is `{ line, character }`** **(user)**, LSP's names: zero
  based, `character` in UTF-16 code units (section 3); a range is
  `{ start, end }`.
- A primitive acts on the focused pane (and its buffer) unless given a
  pane or buffer.
- The hollow cursor of panes without the focus (section 14) is, for now,
  the highlight group `CursorUnfocused` drawn in the cell (underlined by
  default): frames carry one cursor, and terminals draw one.
- `defer(fn)` of section 10 is **`schedule(fn)`** **(user renamed it)**:
  Go's `defer` runs in reverse order; these run in the order scheduled.
  At the safe point the scheduled functions run before `InvokePost`.
- `copy(text, { primary })` of section 15 is `copyText(text)` (`copy`
  is the PDF pane's command), which records it in the copy history too
  (`doc/design/registers.md`); the primary selection needs the protocol's
  `Effect::Copy` to carry it, and waits.
- Searching, `^` and `$` match at lines (multi-line mode); `\A` and `\z`
  at the text's ends.
- `KeyTyped` hooks run before the key is matched: the key waits for them.
  `invoke` runs a target as part of what runs now, without invocation
  events of its own; the mouse is not an invocation yet.
- Wrapping is greedy, so a row depends only on where it starts: a view
  keeps the offset its top row starts at and lays out only the rows it
  shows. Tabs in continuation rows count from the row's start.
- Order of work: the crate's core (positions, edits, marks, undo,
  motions), then layout and drawing, then the pane in the server with
  files, `fallback`, `basemode` and the mode-less defaults (where the user
  tries it), then the events and hooks of sections 9 and 10, then search
  and the smaller primitives.

## 19. Deferred and open

- `readKey`/`input()` and asynchronous mappings; reading the clipboard;
  function values in variables.
- Decorations beyond highlights and virtual text; folding; `listchars`.
- Backups, and swap files of buffers without a file (swap files and undo
  files are built: `o.swapfile`, `o.undofile`).
- Customising the status line.
- Not discussed yet: SyncTeX between this pane and PDF panes (forward and
  inverse search), syntax highlighting (`doc/survey/text-editor.md`,
  section 3), whether the TUI panes' styled text
  (`doc/design/tui-panes.md`) shares this layer.
