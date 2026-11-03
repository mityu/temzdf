# Text editor research

Question (2026-11-02): temzdf may later embed a TUI text editor. Its text
should carry decorations, as in modern editors: text that is drawn but is
not in the document (inlay hints, virtual text, virtual lines, folds,
gutter signs) and styles over ranges. VS Code's text model and decorations
are the main reference. On top of what VS Code has by default, the user wants
these Vim features:

- a tree of undo states;
- persistent undo;
- swap files;
- search and substitute with Vim's regular expressions.

Also asked:

- How to manage Tree-sitter grammars (Zed as an example).
- The cost of Vim-style regex syntax highlighting.
- Other highlighting mechanisms.
- Whether an editor buffer with non-modifiable parts could be the base on
  which users build arbitrary TUI panes, or the reverse.

Status: research only, no decision. **[C]** cited (URL, or a path in the
Vim/Neovim source), **[E]** estimated. Collected by research agents on
2026-11-02; Emacs and Neovim manual pages could not be re-fetched that day,
so claims about them come from the manuals as known and are worth
spot-checking.

Note: this departs from the guiding principle of
`doc/design/scripting.md` ("Vim/Neovim without editing buffers"). An editor
pane would be one more pane kind; the principle would need restating if it is
built.

Licences: temzdf is AGPL-3.0 (`LICENSE`), so GPL-3.0 code (Zed's `rope`,
`editor`) and the Vim licence (convertible to GPL) are compatible in
principle. MIT/Apache crates are still preferred for the parts others might
reuse (the `termpix*` libraries).

## 1. Text and decorations

### VS Code

- **Piece tree** (1.21, 2018): a piece table in a red-black tree. The
  original file buffer is read-only and added text goes to append-only
  buffers. Each piece caches its line breaks, and each node keeps its left
  subtree's length and line count, so lookups by offset and by line are
  O(log N). A 35 MB file took about 600 MB as an array of lines, and about
  its own size as a piece tree. `getLineContent` slows down after many edits.
  [C: code.visualstudio.com/blogs/2018/03/23/text-buffer-reimplementation]
- **Bracket pairs** (1.60, 2021): a (2,3)-tree whose nodes store only
  *lengths* (lines and columns packed into one number), never absolute
  positions. An edit touches one path of nodes. Reparsing is incremental:
  under 1 ms where the old extension took about 10 s.
  [C: code.visualstudio.com/blogs/2021/09/29/bracket-pair-colorization]
- **Decorations** live in an interval tree
  (`src/vs/editor/common/model/intervalTree.ts`) [C: github.com/microsoft/vscode].
  - Each node has `start`, `end`, `delta`, `maxEnd` and packed flags.
    Positions are relative to the deltas of the node's ancestors.
  - An edit removes the overlapping nodes. Nodes after the edit are shifted
    lazily by adding the delta at their subtree root. The removed nodes are
    fixed up and inserted again.
  - **Stickiness** decides what happens to an end at the edit position:
    `AlwaysGrowsWhenTypingAtEdges`, `NeverGrows…`, `GrowsOnlyWhenTypingBefore`,
    `GrowsOnlyWhenTypingAfter`. Further flags: `forceMoveMarkers` and
    `collapseOnReplaceEdit`.
- **Injected text**: `IModelDecorationOptions.before`/`after`
  (`InjectedTextOptions`: one-line `content`, a class, `cursorStops`). It is a
  decoration of zero length that draws text; inlay hints and ghost text use it
  [E].
- **View coordinates**: `IModelLineProjection` maps one model line to zero
  view lines (folded), one, or several. It records where text is injected and
  where the line wraps, measured in text that includes the injections.
  `ViewModelLinesFromProjectedModel` keeps one projection per model line and
  a prefix sum of view line counts. Conversions in both directions take an
  *affinity* that settles ambiguous spots (a wrap point, the edges of injected
  text). Folds are hidden areas, stored as decorations.
  [C: `src/vs/editor/common/viewModel/modelLineProjection.ts`, `viewModelLines.ts`]
- **Tokens**: per line, a `Uint32Array` of `[end offset, metadata]` pairs.
  The metadata packs language, token type, font style, foreground and
  background. Semantic tokens are stored apart and merged when drawing [E].
  [C: `src/vs/editor/common/tokens/lineTokens.ts`, `encodedTokenAttributes.ts`]
- Layers of the code base: base → platform → editor → workbench. The editor
  must not depend on Node or Electron.
  [C: github.com/microsoft/vscode/wiki/Source-Code-Organization]

### Neovim extmarks

- A mark sits between characters. It survives deletion of the text around it,
  and undo restores it.
- Options: `right_gravity` and `end_right_gravity`, `virt_text` with
  `virt_text_pos` (`eol`, `eol_right_align`, `overlay`, `right_align`,
  `inline`), `virt_lines` above or below, `conceal`, `conceal_lines`, signs,
  `hl_group`, `priority`, `invalidate`, `undo_restore`. `ephemeral` marks
  live for one redraw only: decoration providers set them for the visible
  rows as the screen is drawn.
- Storage: the marktree (`src/nvim/marktree.c`), a B-tree whose children
  store positions relative to their parent. `marktree_splice` applies edits.
  Each node lists the ranges spanning it, which answers overlap queries.
  [C: neovim `runtime/doc/api.txt`, `src/nvim/marktree.c`]

### Zed

- The rope is a `SumTree` of chunks of up to 128 bytes: a copy-on-write B+
  tree with `Arc` nodes and user-defined summaries that add up. A cursor
  seeks along one dimension (bytes, points, UTF-16) and sums the others.
  Snapshots cost one reference count, so Tree-sitter parses in the background.
  [C: zed.dev/blog/zed-decoded-rope-sumtree]
- `DisplayMap` stacks transforms: InlayMap → FoldMap → TabMap → WrapMap →
  BlockMap (virtual lines and blocks).
  - Each layer has its own coordinate type (`InlayPoint`, `FoldPoint`, ...)
    and conversions to the layers next to it.
  - Each layer's `sync(snapshot, edits)` returns its new snapshot and the
    edits translated into its own coordinates.
  [C: `crates/editor/src/display_map.rs` in zed-industries/zed]
- Anchors are CRDT positions (insertion id and offset), tied to
  collaboration. [C: zed.dev/blog/crdts]

### Helix

`TextAnnotations` [C: `helix-core/src/text_annotations.rs`]:

- inline virtual text before a grapheme;
- overlays that replace one grapheme;
- `LineAnnotation`, which adds virtual lines.

They are sorted slices handed to the formatter for each render, not a tree
that follows edits. Inlay hints are mapped through change sets [E].

### Xi

Raph Levien's retrospective: the rope was a success. The CRDT was "far more
complex than necessary". Splitting frontend and core into processes with
asynchronous calls "was not a good idea" for wrapping and scrolling.
[C: raphlinus.github.io/xi/2020/06/27/xi-retrospective.html]

For temzdf this means the session server owns the buffer and lays it out.
Clients get finished cells, as they do for terminal panes now.

### Rust crates

| Crate | Version | Licence | Notes |
|---|---|---|---|
| `ropey` | 1.6.1 (2023); 2.0.0-beta.1 (2025-08) | MIT (2.x MIT or Apache-2.0) | B-tree rope, char and line indices; mature (Helix). 2.0 indexes by byte |
| `crop` | 0.4.3 (2025-04) | MIT | B-tree rope, bytes and lines, cheap clones (snapshots) |
| `jumprope` | 1.1.2 (2023) | ISC or Apache-2.0 | skip list, fast edits, few queries |
| `xi-rope` / `lapce-xi-rope` | 0.3.0 (2019) / 0.4.0 (2025-12) | Apache-2.0 | the fork is maintained |
| `zed-sum-tree` | 0.2.0 (2025-10) | Apache-2.0 | Zed's `sum_tree` published |
| Zed `rope`, `editor` | in the Zed repository | GPL-3.0-or-later | |
| `iset`, `rust-lapper` | 0.3.3, 1.3.0 | MIT | interval sets with absolute keys: every edit shifts O(n) |

[C: crates.io]

Ranges that follow edits (checked 2026-11-03; corrects a first pass that
found none). Libraries:

- `xi-rope`'s `Spans<T>` (Apache-2.0): "spans (in an interval tree), useful
  for rich text annotations", moved by an edit with `apply_shape(&Delta)`.
  It is tied to xi-rope's own `Delta` and has no stickiness options; Lapce
  uses it through `lapce-xi-rope`.
  [C: xi-editor `rust/rope/src/spans.rs`]
- `any-rope` 1.2.5 (MIT, 2023): a rope of arbitrary measured items, "for
  example ... a rope of tags" beside a rope of text. Ranges kept as runs, not
  overlapping marks. [C: github.com/AhoyISki/AnyRope]
- Helix (MPL-2.0, `helix-core` not published on crates.io, so a git
  dependency): no tree. Each `ChangeSet` moves sorted positions with
  `update_positions`/`map_pos`, and each position carries an `Assoc`
  (`Before`, `After`, `BeforeWord`, `AfterWord`, `BeforeSticky`,
  `AfterSticky`). Selections, diagnostics and inlay hints are mapped this way.
  [C: `helix-core/src/transaction.rs`]
- Zed (GPL-3.0-or-later):
  - The `text` crate has anchors (CRDT positions with a bias) that are
    resolved through a snapshot.
  - `editor`'s `DisplayMap` (inlays, folds, blocks) and `multi_buffer` build
    on them.
  - These crates are not on crates.io and depend on the rest of Zed.
  [C: zed `crates/text/Cargo.toml`, `crates/multi_buffer/Cargo.toml`]
- `neosh-core` 0.7.0 (MIT, 2026-10): buffers with extmarks, new and
  little used.

Code to port rather than depend on:

- Neovim's `src/nvim/marktree.c` (C, Apache-2.0);
- VS Code's `intervalTree.ts` (TypeScript, MIT);
- Atom's `superstring` marker index (C++, MIT);
- CodeMirror 6's `RangeSet` (JavaScript, MIT);
- Emacs' `itree.c` (C, GPL-3.0).

None of the libraries is a drop-in for marks with VS Code or Neovim
semantics. Options: a small tree of relative offsets of our own (marktree or
VS Code style, ported), `zed-sum-tree` with a `{len, max_end}` summary, or
Helix's approach (sorted positions mapped per edit) for a first version.

### Shape for temzdf [E]

A frontend-independent crate that produces `temzdf-scene` text cells, like
`temzdf-term`:

1. **Buffer**: `crop` (snapshots for background highlighting) or `ropey`.
   Every edit yields a patch (`old range → new length`) that drives
   everything below.
2. **Marks**:
   - `{start, end, start_gravity, end_gravity}`: VS Code's four kinds of
     stickiness as two booleans, as Neovim does.
   - Flags: invalidate when the text is deleted, collapse when it is
     replaced.
   - Namespaces.
   - A payload: a style (highlight group, priority) or virtual content
     (inline text with cursor stops, overlay, end of line, right aligned,
     virtual lines, conceal, fold, sign).
   - Providers that set ephemeral decorations for visible rows only (syntax
     highlighting can use this).
3. **Display transforms**: inlay → fold/conceal → tab → wrap → block, each
   with its own coordinate type, conversions both ways with a bias, and
   `sync(edits) -> edits` (Zed). A first version can be simpler: VS Code's
   projection per model line plus a prefix sum. Widths come from
   `unicode-width` (cells), not from measuring fonts.
4. **Rendering**: rows of cells made of the gutter (numbers, signs), the
   text, and styles resolved by priority. The cursor lives in buffer
   coordinates and is mapped through the layers.

## 2. Vim features

### Undo tree

- **Vim** (`src/undo.c`, `u_header` in `src/structs.h`) [C]:
  - Each change is a header. It is linked to the next and previous change on
    its branch (`uh_next`, `uh_prev`) and to sibling branches (`uh_alt_next`,
    `uh_alt_prev`).
  - A header records a sequence number, the time, the write count
    (`uh_save_nr`), the cursor and the marks.
  - Its entries are whole old lines (`ue_top`, `ue_bot`, `ue_array`). Undo
    swaps them with the current lines, so the same entry then serves as redo.
  - `undo_time()` walks the tree to a target sequence number. `g-`/`g+`,
    `:earlier`/`:later` (by count, time or file writes) and `:undo N` go
    through it.
  - Neovim is the same, plus extmark records.
- **Helix** (`helix-core/src/history.rs`) [C]: revisions in a `Vec`, each with
  `parent`, `last_child`, the transaction, its inversion and a timestamp.
  Earlier and later work by steps or by time; moving between branches goes
  through the common ancestor. The history is in memory only.
- Crates: `undo` 0.52 (a linear `Record` and a tree `History`, serde,
  save points) [C: docs.rs/undo]. VS Code's undo is linear; extensions add
  trees.
- **For us**: Helix's shape, with edits and their inverses on our rope, a
  sequence number, wall-clock time (`SystemTime`, needed for persistence) and
  a write count. 600-1000 lines with tests [E]. Pitfalls: grouping one
  insert into one step, restoring cursor and marks, a memory cap.

#### VS Code's version ids and snapshots (added 2026-11-03)

What VS Code has, from `src/vs/editor/common`:

- **Two version ids** (`model.ts`):
  - `getVersionId()` grows with every change, undo included.
  - `getAlternativeVersionId()` "will return the same values in the case of
    undo-redo": going back to a state gives that state's number again. It
    names the state rather than the change, and is what the dirty flag
    compares with the saved one.
- **`createSnapshot()`** reads out the text of one moment.
  - `PieceTreeSnapshot` copies the list of pieces. The buffers holding the
    text are only appended to, so the snapshot can be read asynchronously
    (to save the file) while edits go on.
  - It is a one-off reader, not a store of past versions.
- **Undo is linear**:
  - `src/vs/platform/undoRedo/common/undoRedoService.ts` keeps `_past` and
    `_future` stacks.
  - Each element (`SingleModelEditStackData` in `model/editStack.ts`) holds
    `TextChange` diffs, the version ids before and after, and cursor
    states.
  [C: github.com/microsoft/vscode]

What carries over to an undo tree [E]:

- **A state number per node**, as the alternative version id (Vim's
  `uh_seq`).
  - One number says which state the buffer is in.
  - Comparing it with the saved node's number gives the modified flag
    directly, instead of Vim's count of writes (`uh_save_nr`).
  - Decorations and display transforms can record the number they were
    computed for, and drop stale results.
- **Snapshots only with a persistent rope.** VS Code's do not help. But
  clones of `crop` or `ropey` share nodes (copy on write), so each tree node
  could hold the whole text cheaply.
  - Jumping far (`:undo N`, `:earlier 10m`) then swaps the text instead of
    replaying every edit on the path.
  - Each snapshot pins the branches of the rope an edit copied.
  - Marks and decorations still have to be moved by the diffs.
  - An undo file stores diffs, not snapshots.
  - A practical mix: diffs as the truth, with snapshots on recent nodes or
    every N nodes as shortcuts.
- Zed's CRDT keeps an undo map (how many times each operation was undone),
  so any operation can be undone selectively. Its commands look linear
  [E, not checked]. This is a different idea from walking Vim's tree.

### Persistent undo

- **Vim** (`src/undo.c`) [C]:
  - The file starts with the magic `"Vim\237UnDo\345"` and a version.
  - Then a SHA-256 of the buffer's lines and the line count, followed by
    headers and entries, with pointers written as sequence numbers.
  - When the file is read, Vim hashes the buffer again. On a mismatch
    ("File contents changed, cannot use undo info") the whole history is
    dropped.
  - Neovim's format (version 3, extmarks) is incompatible with Vim's. Its
    default `'undodir'` is `~/.local/state/nvim/undo//`.
- **Pitfalls**:
  - An edit outside the editor loses everything. Better: keep the hash of
    the last saved text and, on a mismatch, add a node "changed outside"
    holding the diff [E].
  - Changing the encoding or line endings changes the hash.
  - Big files cost a hash on every read and write.
  - Write a temporary file and rename it.
- 300-500 lines (a serialised tree plus a header) [E].

### Swap files

- **Vim** (`src/memline.c`, `src/memfile.c`) [C]:
  - The swap file is the buffer's real storage: a B-tree of fixed-size
    blocks. Pointer blocks lead to data blocks, so large files are paged
    in.
  - Block 0 holds pid, user, host, file name, mtime, inode and a dirty
    flag, plus magic values that detect byte order.
  - It is synced after `'updatecount'` characters (200) or `'updatetime'`
    ms idle (4000).
  - `vim -r` recovers. E325 "ATTENTION" appears when a swap file exists,
    saying whether its process still runs.
  - Neovim skips the prompt when a running Nvim owns the file.
- **VS Code** hot exit backs up dirty buffers and restores them silently,
  with no lock or prompt. **Emacs** auto-saves to `#file#` and locks with a
  `.#file` symlink [E].
- **For us** [E]:
  - The rope stays in memory. Each buffer gets a journal in the state
    directory:
    - a header: magic, version, pid, host, path, mtime, size, a hash of the
      base text;
    - appended edit records, each with a length and a checksum;
    - a periodic compressed snapshot, after which the journal is truncated.
  - Recovery replays records up to the first bad checksum.
  - The session server owns every buffer, so a second client simply
    attaches. The lock only matters across sessions.
  - The journal can be the undo log: a record with a parent sequence number
    is an undo-tree node. The undo file is then the journal (compacted),
    kept after a clean write and keyed by the saved text's hash.
  - 500-900 lines. Pitfalls: fsync cost, partial records, renamed files,
    huge pastes.

### Vim regular expressions

- **Vim** [C: `wc -l`]:
  - The parser and substitution are in `regexp.c` (3409 lines). The
    backtracking engine is `regexp_bt.c` (5763) and the NFA engine is
    `regexp_nfa.c` (7746).
  - `'regexpengine'` picks the engine. The automatic setting falls back from
    the NFA to backtracking when a pattern does not compile or gets too
    expensive.
  - Matching asks for lines one at a time (`reg_getline`), so it suits a
    rope.
  - Neovim has merged them into one `regexp.c` of 16378 lines.
- **Beyond PCRE and Rust's `regex`** (`runtime/doc/pattern.txt`):
  - magic levels `\v \m \M \V` that can change part-way through a pattern;
  - `\zs \ze`;
  - position atoms that need the editor's state: `\%V`, `\%#`, `\%23l`,
    `\%>5c`, `\%>5v`, `\%'m`;
  - `\{-}` (lazy), `\%[]` (optional sequence);
  - lookbehind `\@<=`/`\@<!` of unbounded width, `\@>` (atomic);
  - matches across lines: `\_x`, `\n`;
  - `\< \>`, and `\k \i \f \p`, which depend on options (`'iskeyword'`, ...);
  - `~` (the last replacement), `\z(\)` (syntax);
  - case: `\c`, `\C`, `'ignorecase'`, `'smartcase'`.
- **Substitution**: `&`, `\0`-`\9`, `~`, `\u \U \l \L \e \E`, `\=expr`;
  `\r` inserts a line break, `\n` a NUL.
- **Options**:
  - **(a) Port Vim's engine**: 8-12k lines of Rust, 2-4 person-months, with
    Vim's `test_regexp_*.vim` as the reference [E]. Fully faithful, line by
    line over a rope. Risks: exponential backtracking, many globals to
    untangle.
  - **(b) Translate** Vim syntax to our own syntax tree, then emit
    `regex-cursor` (a fork of `regex-automata` that searches a rope through a
    `Cursor` trait, 0.1.5, used by Helix [C]) for the common subset, and
    `fancy-regex` or `pcre2` when lookaround or backreferences are needed.
    - Not translatable cleanly: unbounded lookbehind, `\zs` inside a
      repetition, `\z(`, and `~`.
    - The position atoms can become filters on the matches. `\k` and the
      like are expanded from the options when the pattern is translated.
    - `fancy-regex` and `pcre2` need contiguous text: copy a line or a
      window of lines.
    - 1.5-3k lines [E].
  - **(c) Existing crates** are partial:
    - `edamame` translates to `fancy-regex` and rejects what it cannot
      handle;
    - `hjkl-engine` only rewrites `\< \>`;
    - `vimlrs` has its own matcher.
    - Zed's vim mode keeps Rust syntax and only swaps the parentheses of
      `:s`; Helix uses Rust syntax.
    [C: docs.rs, zed `crates/vim/src/normal/search.rs`]
- The substitute expander is 300-500 lines on its own; `\=` could call the
  script host [E].

#### Parsing Vim patterns (added 2026-11-03)

How much of Vim's code is parsing (vim master `src/regexp*.c`, 16918 lines,
counted per function) [C]:

| Part | Functions | Lines |
|---|---|---|
| Lexing (depends on the magic level), skipping `[]` | `peekchr`, `skipchr`, `getchr`, `skip_anyof`, `get_coll_element`, ... | about 330 |
| Parsing for the backtracking engine | `reg`, `regbranch`, `regconcat`, `regpiece`, `regatom` | about 1290 |
| Parsing for the NFA engine | `nfa_reg` ... `nfa_regatom` | about 1300 |
| Matching | `regmatch`, `regrepeat`, `nfa_regmatch`, `post2nfa` | about 3950 |
| Substitution | `vim_regsub_both`, `regtilde` | about 610 |
| Class and equivalence tables | `init_class_tab`, `nfa_emit_equi_class` | about 750 |

- Vim builds no syntax tree. Each engine has its own parser that emits
  that engine's code directly, so the parsing is done twice.
- One parser producing a syntax tree would be 1000-1500 lines of
  hand-written Rust [E].

**A parser generator saves little** [E]:

- The structure is four levels:
  - branches separated by `\|`;
  - concats separated by `\&`;
  - pieces: an atom and a multi (`*`, `\+`, `\=`, `\{n,m}`, `\@=`, ...);
  - atoms, a long table.
- That is easy to write by recursive descent.
- The hard parts are outside what LALR or PEG generators handle well:
  - which characters are operators depends on the magic level;
  - inside `[...]`, a leading `]` is literal and a `[` with no closing `]`
    is literal (Vim looks ahead with `skip_anyof`);
  - `\%[...]`, `\_[...]`, `\%d123`, `\%u20AC`;
  - `~` expands to the previous replacement while parsing;
  - errors should carry Vim's numbers.

**The magic level is lexer state, not grammar.**

- A level switch applies to the rest of the pattern, across group
  boundaries. Checked with Vim (`vim -Nu NONE -es`, `matchlist('abc', ...)`)
  [C]:
  - `\(a\vb)` matches `ab` with group 1 `ab`: a group opened by magic `\(`
    is closed by very magic `)`.
  - `\v(a\mb\)` is the same the other way round.
  - In `\(a\vb)c` the `c` after the group is still very magic (`abc`, group
    1 `ab`).
- One grammar per level would need every rule duplicated for each pair of
  levels on entry and exit, up to 16 copies. It is possible in theory (the
  levels are finite), but impractical.
- Instead, define tokens per level in the lexer.
  - `logos` can switch token types with `Lexer::morph` when `\v`, `\m`,
    `\M` or `\V` appears.
  - `[...]` gets a lexer state of its own, since the levels hardly matter
    inside it.
  - Every level emits the same normalised tokens ("group open", "lazy
    repeat", ...), so the parser has one small grammar that knows nothing of
    levels.
  - The parser can be hand-written or generated.
  - The same tree feeds either the translation to `regex-cursor` or
    `fancy-regex`, or a ported engine.
- **Testing**:
  - Generate patterns and texts (`proptest`), and compare with real Vim
    (`vim -es` and `matchstrpos()`).
  - Use Vim's `test_regexp_*.vim` as cases.
  - This finds differences without studying every corner of the
    specification.

### Summary

| Feature | Approach | Effort [E] | Risks |
|---|---|---|---|
| Undo tree, `g-`/`g+`, `:earlier`/`:later` | Helix-style tree of edits and inverses | 600-1000 lines | grouping, cursor and marks, memory |
| Persistent undo | serialised tree + text hash; a "changed outside" node | 300-500 lines | outside edits, encodings, atomic writes |
| Swap and recovery | edit journal + snapshots + lock header, shared with undo | 500-900 lines | fsync, partial records, renames |
| Vim regex, common subset | translate to `regex-cursor`/`fancy-regex`, atoms as filters | 1.5-3k lines | lookbehind, `\zs` in repetitions |
| Vim regex, complete | port the NFA (+ backtracking) over a line callback | 8-12k lines, months | backtracking blow-ups, size |
| `:s` replacement | own expander, `\=` via the script host | 300-500 lines | `\r`/`\n` |

## 3. Syntax highlighting

### Tree-sitter grammars

- **tree-sitter** 0.27.1 (2026-10) [C: crates.io]:
  - Grammars of ABI 13 to 15 load (`TREE_SITTER_MIN_COMPATIBLE_LANGUAGE_VERSION`,
    `TREE_SITTER_LANGUAGE_VERSION`), others are refused.
  - The `wasm` feature loads grammars compiled to WebAssembly through
    wasmtime (`ts_wasm_store_new`).
  - `tree-sitter build --wasm` uses wasi-sdk (downloaded if missing).
  - `tree-sitter-language` (`LanguageFn`) means grammar crates no longer
    depend on one `tree-sitter` version.
  [C: tree-sitter `lib/include/tree_sitter/api.h`,
  tree-sitter.github.io/tree-sitter/cli/build.html]
- **Zed** [C: zed.dev/docs/extensions/languages,
  zed.dev/blog/language-extensions-part-1, zed `crates/extension/src/extension_builder.rs`]:
  - Extensions name grammars in `extension.toml` as `[grammars.NAME]` with a
    `repository` and a commit `rev`. They ship queries such as `highlights`,
    `injections`, `brackets`, `outline`, `indents` and `textobjects` (no
    `locals`).
  - The extension builder compiles grammars to `.wasm` with a pinned wasi-sdk.
    Zed loads them with wasmtime.
  - Built-in languages are statically linked crates, some of them Zed's forks
    pinned by revision.
  - Why WebAssembly: an external scanner is arbitrary C and must not crash
    the editor, one file serves every platform, and users need no C
    compiler.
  - Only the lexer and the external scanner run in WebAssembly. The parse
    tables are copied out, and parsing stays native.
- **Helix** [C: docs.helix-editor.com/languages.html, `helix-loader/src/grammar.rs`]:
  - `languages.toml` pins each grammar's git `rev`. `hx --grammar fetch`
    and `build` compile `.so` files with the user's C compiler. Queries are
    in `runtime/queries`, and highlighting is done by the `tree-house` crate.
  - Known troubles: "incompatible language version", queries out of sync
    with stale parsers, distribution packages of grammars that do not match.
- **Neovim** [C: neovim `runtime/doc/treesitter.txt`, github.com/nvim-treesitter/nvim-treesitter]:
  - It bundles parsers for C, Lua, Markdown, Vimscript, Vimdoc and Diff, and
    finds others as `parser/{lang}.*` on `'runtimepath'` (`.wasm` only in
    builds with wasmtime).
  - nvim-treesitter's `main` branch, an incompatible rewrite for 0.12,
    needs `tree-sitter-cli` and a C compiler. It pins every parser's
    revision in one table, because queries name nodes of particular grammar
    revisions.
  - rocks.nvim ships parsers as luarocks [E].
- **Emacs 29+** builds grammars with the user's compiler
  (`treesit-install-language-grammar`) and checks the ABI version on load.
  [C: GNU Emacs NEWS.29, elisp manual "Language Grammar"]
- **Nix**: nixpkgs pins grammar sources and builds them (`tree-sitter.builtGrammars`,
  `withPlugins`). It gives reproducible `.so` files but no queries.
  [C: nixpkgs `pkgs/by-name/tr/tree-sitter`]
- **Bundles**:
  - `arborium` 2.18 (MIT or Apache-2.0): about 70 grammars as separate
    crates behind features, with queries.
  - `tree-sitter-language-pack`: about 300 parsers, downloaded on demand.
  - `syntastica` 0.6 (MPL-2.0, queries from nvim-treesitter).
  - `lumis` 0.19 (MIT; formerly `autumnus`, while `inkjet` is archived).
  [C: crates.io, GitHub]

**For temzdf** [E]:

1. **First**: link a few grammar crates statically (LaTeX, BibTeX, Markdown,
   and the languages users of a LaTeX previewer edit). Keep our own queries,
   pinned with the grammar's revision and tested in CI. This needs no
   compiler on users' machines and avoids ABI drift. `arborium` can supply
   the crates and queries.
2. **Later, for user-added languages**: Zed's model.
   - A lock file lists each grammar: repository, revision, subdirectory,
     queries.
   - A script or a Nix derivation runs `tree-sitter build --wasm` into a data
     directory, and the session loads the files with wasmtime.
   - One artifact serves Linux and macOS, sandboxed. The price is wasmtime's
     weight (build time, binary size, a large dependency).
3. **Avoid** unmanaged builds of `.so` files on users' machines (Helix,
   Emacs): that is where the ABI and compiler failures come from. Native
   parsers that users add are workable with the management below.

#### User-added native parsers (added 2026-11-03)

What manages `.so`/`.dylib` parsers added by users well, and what does not.
The criteria: where files go, which file types a parser serves, ABI checks,
queries matching the parser revision, prebuilt files, pinning and removal,
safety.

- **`tree-sitter-loader`** 0.27.1 (MIT), the CLI's loader, usable from other
  programs [C: tree-sitter `crates/loader`]:
  - **Finding grammars:** it scans `parser-directories`
    (`~/.config/tree-sitter/config.json`) for `tree-sitter-*` directories.
  - **Building:** it compiles a grammar on demand into
    `$TREE_SITTER_LIBDIR` or `<cache>/tree-sitter/lib`.
    - It rebuilds when the library is older (mtime) than `parser.c`,
      `scanner.c` or `external-files`. Queries and headers are not checked.
    - A build writes a temporary file and renames it, under a lock file.
  - **Symbols:** `tree_sitter_<name>`, with `-` replaced by `_`.
  - **Metadata:** each grammar's `tree-sitter.json` (cli/init.html) gives
    `scope`, `file-types`, `first-line-regex`, `content-regex`,
    `injection-regex`, the query paths, and a semver `version`.
  - **ABI:** the loader itself does not check it. `Parser::set_language`
    returns `LanguageError::Version` outside ABI 13 to 15. ABI 15 parsers
    carry `Language::name()` and `metadata()` (their version).
- **Neovim** [C: neovim `runtime/doc/treesitter.txt`, `src/nvim/lua/treesitter.c`]:
  - **Finding parsers:** `parser/<lang>.*` on `'runtimepath'`, first match
    first (user before plugins before bundled).
  - **Explicit loading:** `vim.treesitter.language.add(lang, {path,
    symbol_name})`, and `language.register(lang, filetypes)`.
  - **ABI errors** are clear: "ABI version mismatch for %s: supported
    between %d and %d, found %d". `language.inspect()` shows the ABI and the
    version.
  - **luarocks** [C: github.com/nvim-neorocks/luarocks-build-treesitter-parser,
    github.com/nvim-neorocks/rocks-treesitter.nvim]:
    `luarocks-build-treesitter-parser` builds `tree-sitter-<lang>` rocks.
    `rocks-treesitter.nvim` installs prebuilt parsers with their queries, so
    no compiler is needed. The only ecosystem found that versions a parser
    and its queries as one package with a real uninstall [E].
- **Kakoune's `kak-tree-sitter`** (`ktsctl` 3.1) [C: man.sr.ht/~hadronized/kak-tree-sitter,
  docs.rs/crate/ktsctl]:
  - The configuration pins each grammar's git URL and revision, and the
    compile and link commands can be changed.
  - Files go to `grammars/<lang>/<pin>.so`, with the queries in a directory
    per pin. Revisions coexist, and a parser always has its own queries.
  - It has `sync` (skips pins already there) and `remove`.
- **Topiary** [C: topiary `topiary-config/languages.ncl`, `src/language.rs`]:
  - The configuration gives git, revision, subdirectory, a Nix hash and an
    optional symbol.
  - Grammars are built through `tree-sitter-loader` into
    `<cache>/<lang>/<rev>`, with queries cached per revision.
- **ast-grep** 0.51 [C: ast-grep.github.io/advanced/custom-language.html,
  ast-grep `crates/dynamic/src/lib.rs`]:
  - `customLanguages` in its configuration gives `libraryPath` and
    `extensions`.
  - It loads with `libloading` and checks the ABI range ("Incompatible
    tree-sitter parser version").
  - Since 0.50 custom languages need `--custom-languages allow|trust`. The
    trust store remembers the configuration's path, not a hash of its
    contents.
- **Helix**:
  - Runtime directories are searched in order (the user's
    `~/.config/helix/runtime` before the installed one).
  - `source = { path = ... }` builds a local grammar in place.
  - A workspace's `.helix/languages.toml` needs `:workspace-trust`.
  [C: helix `helix-loader/src/lib.rs`, `grammar.rs`]
- **Prebuilt downloads**:
  - Lapce downloads `grammars-<os>-<arch>.tar.zst` and `queries.tar.zst`
    from `lapce/tree-sitter-grammars` releases, picked by its own version,
    with no checksum.
  - `tree-sitter-language-pack` (MIT) downloads bundles on demand into a
    versioned cache, checked against a manifest with a size and sha256 per
    platform. The manifest is unsigned: integrity, not authenticity.
  [C: lapce `lapce-app/src/app/grammars.rs`, tree-sitter-language-pack
  `crates/ts-pack-core/src/download.rs`]
- **Emacs, as a warning** [C: github.com/emacs-mirror/emacs/blob/master/doc/lispref/parsing.texi, `lisp/treesit.el`,
  github.com/renzmann/treesit-auto, github.com/emacs-tree-sitter/tree-sitter-langs]:
  - **Loading:** `libtree-sitter-<lang>` from `treesit-extra-load-path` and
    `~/.emacs.d/tree-sitter`, with a clear `version-mismatch` error.
  - **Queries** are written in each major mode's Lisp, so a newer parser can
    break a mode silently.
  - **ABI drift:** `treesit-auto` has an `:abi14-revision` field to pin
    grammars that Emacs 30.1 (ABI 13 and 14) can still load.
  - **No checksums:** `tree-sitter-langs` downloads per-platform bundles
    from GitHub releases without checking them.
- **nixpkgs** builds a grammar into `$out/parser` (or `parser.wasm`) plus
  `tree-sitter.json` and `queries/` when the grammar has them
  [C: nixpkgs `pkgs/by-name/tr/tree-sitter/grammars/build-grammar.nix`].
- **difftastic** links its grammars statically, so users cannot add any.

**For temzdf** [E]:

1. **Layout**, one directory per revision:

   ```
   $XDG_DATA_HOME/temzdf/grammars/<name>/<revision or "local">/
     parser.so | parser.dylib | parser   # the last as nixpkgs builds it
     tree-sitter.json                    # upstream metadata, if any
     queries/{highlights,injections,locals}.scm
     grammar.toml                        # ours: symbol, file types, source, sha256
   ```

   - Search order: `$TEMZDF_GRAMMAR_PATH`, then the user directory, then
     directories from the configuration (Nix store paths). The first match
     per name wins.
   - Removing a revision deletes its directory.
2. **Metadata**: read `tree-sitter.json` and let `grammar.toml` override it.
   The symbol defaults to `tree_sitter_<name>`, and the name can be changed.
3. **Loading**:
   - Load with `libloading` and never unload.
   - Check the ABI range, with Neovim's wording (path, range, found).
   - Compare `Language::name()` and `metadata()` with `grammar.toml`.
   - Compile the queries against the parser when installing and when
     loading, so a stale `highlights.scm` gives a clear error instead of
     broken highlighting.
4. **Commands**: `temzdf grammar add <git URL|path> [--rev]` builds through
   `tree-sitter-loader` and copies the queries from the same checkout (as
   `ktsctl` and Topiary do). Also `grammar ls`, `rm` and `update`.
5. **Nix**: accept nixpkgs grammar outputs as they are. Store paths are
   read-only and pinned, so they need no hash or trust prompt.
6. **Safety**: a native parser is code execution.
   - Load only from the user's directories and the Nix store, never from
     a project.
   - Record a sha256 when installing, and refuse a changed file until it is
     trusted again. That is stronger than ast-grep's trust by path.
   - Download no unverified binaries.
   - Load parsers in the server only.
7. **WebAssembly**: native first, with the loader shaped so that `.wasm`
   files can be added through `WasmStore` later, as Neovim does.
   - Native parsers are fast, work with every grammar, are built by nixpkgs,
     and need no wasmtime. But they are per platform (with quarantine and
     notarisation on macOS), unsandboxed, and can crash the process.
   - WebAssembly is portable and sandboxed, but needs wasi-sdk to build and
     runs somewhat slower.

### Vim-style regex syntax highlighting: cost

- **Vim** [C: `wc -l` on vim master]:
  - `syntax.c` is 7069 lines, `highlight.c` 6310, and the regex engine
    about 16.9k.
  - It tries every applicable pattern at each column, with backtracking.
  - It keeps states at sync points, and re-highlighting after an edit starts
    from the nearest one and stops when line states agree again.
  - Slow cases are bounded by `'synmaxcol'` (3000) and `'redrawtime'`.
- **What an engine needs**:
  - `keyword`, `match`, and `region` with `start`/`skip`/`end`, offsets and
    `matchgroup`;
  - `contains`, `containedin`, `nextgroup`, `transparent`, `oneline`,
    `keepend`, `extend`, `fold`, and clusters;
  - `\z()` (captured text carried to the end of a region), `conceal`;
  - the `syn sync` strategies, `syn include`, `hi def link`.
- **Vim's `runtime/syntax/*.vim`**: 789 files, 158k lines [C, counted].
  - 781 use `if`/`let`/`exe` (at least the `b:current_syntax` guard), and 28
    define functions.
  - 10 are Vim9 script.
  - 93 use `syn include`, 68 use `\z(`, and 21 use `conceal`.
- **Estimates** [E]:
  - **The engine with our own definition format**, patterns in
    `fancy-regex` syntax: 4-6k lines, low to medium risk. But every
    definition is ours to write.
  - **Plus Vim's regex dialect**: 5-8k more lines (section 2). High risk:
    small mismatches mis-highlight silently.
  - **Plus a Vim script subset that runs the runtime files**: 4-8k more
    lines. It needs expressions, `if`, `let` with scopes, `exe`, `exists()`,
    `has()`, `:runtime!`, `cpo` handling and buffer options. Vim9 is out of
    reach.
  - **Total for "compatible enough"**: 15-20k lines and months, for perhaps
    85-95 % of the files.
- **Conclusion** [E]: only the engine alone could be worth it; reusing Vim's
  files costs an order of magnitude more for partial fidelity.

### Other mechanisms

| Mechanism | Format | Rust | Licence | Incremental |
|---|---|---|---|---|
| Tree-sitter (static crates) | C parser + `.scm` queries | `tree-sitter` 0.27, `tree-sitter-highlight` | MIT (grammars vary) | yes (tree edits; `tree-sitter-highlight` itself highlights whole texts, so editors drive `QueryCursor` over changed ranges) |
| Tree-sitter WebAssembly (Zed) | `.wasm` + `.scm` | `wasm` feature | MIT | yes |
| TextMate grammars (VS Code: `vscode-textmate` + Oniguruma) | JSON/plist regexes, begin/end | `syntect` 5.3 (MIT; Oniguruma or `fancy-regex`), `two-face` (bat's extra syntaxes) | MIT | per-line rule stack; the editor caches states |
| Sublime `.sublime-syntax` | YAML context stack | `syntect` | MIT | as above |
| LSP semantic tokens | protocol, delta-encoded | `lsp-types` | n/a | delta requests; an overlay over a syntactic highlighter |
| KDE KSyntaxHighlighting | XML (414 definitions) | none | MIT [E] | per-line state |
| CodeMirror Lezer | `.grammar` → JavaScript | none | MIT | yes (fragment reuse) |
| Pygments / Chroma | Python / Go lexers | none | BSD / MIT | no [E] |

`syntect` highlights about 9200 lines of jQuery in about 600 ms and loads its
syntax set in about 23 ms [C: github.com/trishume/syntect]: fine as a broad
fallback, slower than Tree-sitter.

**Bottom line** [E]:

- Tree-sitter with statically linked grammars and our own pinned queries
  first.
- WebAssembly grammars as the extension path.
- `syntect` with `two-face` as a cheap way to cover many languages.
- LSP semantic tokens on top, once there is an LSP client.

## 4. The editor buffer as a base for script-made panes

### Prior art

- **Emacs**: everything is a buffer.
  - Text properties: `read-only` (and `inhibit-read-only`), `keymap` and
    `local-map` per range, `field`, `invisible`, `display`, `mouse-face`,
    `help-echo`, `cursor-intangible`.
  - Overlays carry the same properties plus `before-string` and
    `after-string`.
  - Buttons and `widget.el` (forms), and `special-mode` for read-only
    buffers (`q`, `g`).
  - Examples: Dired, editable as text with `wdired`, and Magit's foldable
    sections.
  - Overlays were linked lists with O(N) operations until the balanced trees
    of Emacs 29.
  - Problems:
    - the cursor lands inside UI text;
    - undo and edits can break structure unless every insertion binds
      `inhibit-read-only`;
    - a full redraw loses point unless re-anchored (Magit keeps it by
      section identity);
    - layout is done with spaces.
  [C: GNU Emacs Lisp manual "Special Properties", "Overlay Properties",
  "Buttons"; magit.vc/manual/magit/Sections.html; yhetil.org emacs-devel
  threads on noverlay]
- **Vim/Neovim**:
  - `buftype=nofile|prompt|terminal|...` and `modifiable`. A prompt buffer
    edits only its last line and calls a callback on Enter: close to
    temzdf's input pane. Extmarks and floating windows complete the toolkit.
  - Plugins build UIs in buffers: fugitive, neo-tree, telescope (prompt,
    results and preview buffers laid out by hand). nui.nvim adds components
    on top of buffers. oil.nvim edits a directory as text and turns the diff
    into file operations on save.
  - Problems:
    - no widgets: each plugin computes layout, focus and borders;
    - global maps leak into UI buffers (`dd` deletes a tree row), so plugins
      defend with `nomodifiable` and many buffer-local maps;
    - redraw means "modifiable, replace all lines, nomodifiable" and
      restoring cursor and folds by hand.
  [C: neovim `runtime/doc/options.txt` 'buftype', `runtime/doc/channel.txt`, github.com/stevearc/oil.nvim,
  github.com/MunifTanjim/nui.nvim]
- **Widget-first**:
  - Helix's compositor stacks `Component`s (pickers, prompts, popups; the
    editor view is one of them) [C: `helix-term/src/compositor.rs`].
  - VS Code offers tree views, quick picks and webviews rather than the
    editor. Read-only virtual documents come from a
    `TextDocumentContentProvider` [C: code.visualstudio.com/api/ux-guidelines/overview,
    code.visualstudio.com/api/extension-guides/virtual-documents].
  - Lapce's editor is one Floem view.
  - ratatui, Textual and Notcurses treat an editor as one more widget.
- **Mixed (Zed)**: every text input, single-line ones included, is the
  `Editor`; `MultiBuffer` joins excerpts of many buffers (search results)
  into one editable view; everything else is GPUI views.
- **Kakoune** sends clients a few primitives (info boxes, menus, the status
  line). **Acme** makes all UI text: any word in a tag line runs as a
  command, and programs drive windows through 9P files.
  [C: github.com/mawww/kakoune/blob/master/doc/json_ui.asciidoc, plan9.io/sys/doc/acme/acme.html]

### Trade-off [E]

- **Pure "buffer as UI"** (Emacs, Vim, Acme) gives every pane search, copy,
  selection and linear text for free. The costs: layout by hand, fights over
  the cursor and keys, fragile redraws.
- **Pure widgets** (Helix, VS Code) give clean focus, layout and hit testing,
  but the text in a picker usually cannot be searched or copied like a
  buffer.

### For temzdf [E]

One lower layer shared by both: **styled text** made of a rope, marks,
properties over ranges and display transforms (section 1).

- **The editor** uses it as an editable buffer: undo, modes, files.
- **Script-made panes** use it as a read-only buffer, so search and copy
  work everywhere. The layout of regions and anything that is not text
  (PDF pages, images) stays in the pane and widget layer (`termpix-ui`,
  floating panes, bars).

The input pane (`crates/temzdf-server/src/session/input.rs`) would become
such a buffer with one editable line: a prompt buffer.

**Properties a buffer needs**:

- `modifiable`; read-only ranges; a prompt kind (only the tail is editable).
- An action or key map per range, by command name or script callback id,
  resolved in the server, so a keystroke does not cost a round trip to the
  script host. This is the same concern as keymaps' decision 1
  (`doc/design/keymaps.md`).
- Clickable and hover ranges with a pixel → cell → range → action hit test
  (temzdf has pixel mice).
- Highlight groups with priorities; hidden and concealed text; folds.
- Virtual text and virtual lines.
- Atomic ranges that the cursor steps over.
- A stable key per logical row, so redraws keep the cursor, selection, folds
  and scroll: what Magit sections and nui.nvim each reinvent.

**Driving it from scripts**:

- **Declarative by default**: `render(state) => lines` with keyed spans. The
  server diffs by key, applies the smallest edit and keeps the cursor on the
  same key. This avoids Vim's "modifiable, wipe, rewrite, restore" dance.
- **Imperative** for editor-like uses: set marks, insert, set properties.
  Examples: diagnostics or SyncTeX highlights on a real file, or oil.nvim's
  "edit the listing, the diff becomes operations".

**To avoid**:

- User editing maps applying to UI buffers. A UI buffer starts from an empty
  special mode that inherits navigation, search and copy only.
- Undo that can break a generated structure: UI buffers have no undo, or are
  only ever produced by `render`.
- Layout with spaces.
- Per-character property storage.
- Opaque panes for what is really text.
