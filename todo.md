# TODO

Long-running and deferred work. Milestones M0–M6 are the current plan; the items
below them are deferred.

## Milestones

- [x] M0: scaffolding (flake, workspace, docs, this file)
- [x] M1: `termpix` (tty, probe, input, graphics, compositor Layered/Composited, scroll, mock, `scroll_demo` example)
- [x] M2: minimal viewer (open PDF, continuous layout, tile cache, smooth scroll, zoom, page navigation); the window title stands in for the status bar
- [x] M3: `termpix-ui` and viewer features (status bar, outline sidebar, search, links + history, text selection + OSC 52 copy, config file)
- [x] M4: auto-reload (see `doc/design/reload.md`)
- [x] M5: SyncTeX (vendored parser, IPC socket, forward/inverse search). SyncTeX anchoring on reload was dropped, see `doc/design/reload.md`
- [x] M6: compatibility (Composited backend verified on WezTerm and in zellij; zellij/tmux recognised from XTVERSION; inline frames fenced for slow links)

## Session server (`doc/design/session-server.md`)

Steps 1-3 are done (PDF and terminal panes, standalone viewer and session
server on one `Session`). Next and open:

- [ ] UI chrome as cell grids in the scene instead of pixels.
- [ ] Per-client pixel density (every client now gets frames for the latest
  client's geometry and cell size, programs' images included) and zoom
  stored relative to the pane width.
- [ ] Shared memory for local clients' pixels: done (`doc/design/session-server.md`,
  "Pixels"; `TEMZDF_PIXELS=socket` for the socket, which no longer copies
  pixels through the encoder either). Left: why the GUI's
  uploads at scale 2 got slower on lavapipe (`doc/dev/performance.md`, "With
  shared memory"); measure on a real GPU and on macOS.
- [ ] Deep recursion in a script exhausts the guest's WebAssembly stack
  and ends the script host (a trap, not a JavaScript error): QuickJS has
  no stack limit set below it. Found with `temzdf/command`'s depth limit
  (200 nested commands trap, 150 fit; 2026-11-19).
- [ ] What pane status lines say, from the configuration (a string or a
  function, as Vim's `statusline`; `doc/design/floating-panes.md`); today a PDF
  pane's says what its old status bar said and a terminal's is blank. To
  settle with the user first: the markup for colours and clickable
  elements, when a function is evaluated again, whether clicking an element
  focuses the pane (proposed: no); then top bars and the window's bars.
  The window's status line exists with built-in content (the mode, the
  session, the tab page; 2026-11-06): the same settlement makes it
  configurable, its content then the default.
- [ ] Terminals without graphics (text only, done 2026-11-02): the server
  could skip rendering PDF tiles while no attached client draws images (the
  Hello would say); a PDF pane could show its text instead (the page's text
  lines) in such terminals.
- [ ] Prompts (input panes were replaced by text panes on 2026-11-07:
  `openPrompt`, the Vim mode's command line on it): a history
  (`<Up>`/`<Down>` move between the lines now); choosing how the search's
  prompt shows (the user: a configuration's own action opening it,
  mapped).
- [ ] A log level (or verbosity) for temzdf's logs: today some lines are
  there for scripts/e2e.sh (`complete frame written`, `incomplete frame
  shown`, geometry) and others only with variables (`TEMZDF_TIMING`); the
  frame lines could go below the default level (user, 2026-10-30).
- [ ] User report (2026-10-26, deferred): after resizing the terminal window
  with split panes, the bottom-right pane's status bar (file name) stayed where
  it was. Not reproduced in kitty or WezTerm (standalone and session) nor
  through the mock terminal (`resizing_split_panes_redraws_like_a_fresh_start`).
  Suspect: the cell size derived from the kernel's pixel size after a resize
  (`Winsize::geometry` divides by rows/cols; padding would skew it, worst at the
  bottom). Needs the terminal, mode and the `resized to geometry` log line.
- [ ] PDF panes' fixed keys (the outline) as mappings: deferred by the user
  (decision 8 of `doc/design/keymaps.md`, which weighs the ways).
- [ ] Naming and crate layout once temzdf is more than a PDF viewer (open
  since the session server design, 2026-10-25).
- [ ] Terminal panes, what is left: kitty graphics through Unicode
  placeholders (tmux inside a pane) and animation frames.
- [ ] Scripting and configuration (JavaScript/TypeScript in the script host,
  QuickJS in WebAssembly; `doc/design/scripting.md`, `doc/design/script-host.md`, `doc/config.md`): the
  host, modes and mappings, options, hooks, buffers, floating panes and tab
  pages are implemented, and every question in `doc/design/scripting.md` is
  decided. Decided but not built: `temzdf eval` (27 there), the
  escape-hatch menu on non-focusable panes (13) and a read-only attach
  ("Clients" there). Worked out but not planned: asynchronous mapped and
  hook functions ("Future" there). Next: default mappings for
  switching buffers; status and title bars on panes and the window
  (direction in `doc/design/floating-panes.md`).
- [ ] Step 4, the GUI client (`temzdf-gui`, `doc/design/gui-client.md`): works on
  X11 (checked on Xvfb with xfwm4, Japanese input through fcitx5 and Mozc).
  Left: a real Wayland desktop (headless sway passes) and macOS checks,
  HiDPI and fractional scales (`--scale` and `ScaleFactorChanged` exist,
  untested), Option as Alt, cursor blinking, underline colours (SGR 58, not
  in the scene's cells yet), the configurable action that ends the session
  when the window closes (closing detaches), texture reuse for tiles and
  partial redraws when only text changed (measure first). Step 5: web panes (headless Chromium).
  (Step 6, programmable configuration, is the script host.)

## Deferred

- A Windows port of the tests. The gate is `cargo xtask check` and the
  JavaScript tooling Node TypeScript (`scripts/ts-types.ts`,
  `scripts/check-docs.ts`), neither assuming Nix or bash; what is left:
  `crates/tuitest` runs programs on a Unix pty (ConPTY instead), tests
  and the terminal pane start `/bin/sh`, the tmux tests need tmux (skip
  them there), the session protocol uses Unix sockets (named pipes or
  AF_UNIX on Windows), and Miri runs through `scripts/miri.sh` (bash;
  the gate says `SKIP miri` elsewhere), as does shellcheck. Never tried
  on Windows: whether `cargo xtask check` itself builds there, and
  whether cargo leaves the running `xtask.exe` alone (it did not rebuild
  `target/debug/xtask` during the gate on Linux).
- Absolute paths only (the user's direction, 2026-11-22): actions that
  take a path accept only absolute ones, as the session's working
  directory (where it started) is not obvious to anyone; relative paths
  are resolved by whoever knows what they are relative to. The command
  line already sends absolute paths. To settle first:
  - the scripts resolve what users type (the Vim mode's `:e foo.txt`,
    `~/`), so they need a base to resolve against: the buffer's
    directory, a terminal pane's working directory (OSC 7), where the
    session started, or a session-wide one set by `:cd`;
  - `temzdf action ...` sends words as typed: the client would have to
    know which arguments are paths (from
    `crates/temzdf-script/ts-types/temzdf-actions.d.ts`) to make them
    absolute;
  - the other paths taken: `newBuffer({ path })`, `write({ path })`,
    `new-pane`'s cwd, the `directory` option (relative to where the
    session started today);
  - an absolute path still does not identify a file (symbolic links,
    `..`): telling a file opened twice keeps needing canonical paths.
- Copy notices from the scripts (decided with the user on 2026-11-22):
  every copy raises an event and the shipped scripts show the notice
  (`action.message`), instead of the server. For now (review 6, R10) the
  server shows one floating notice for every pane's copy. `Copied` would
  need what copied it (its buffer; a Vim yank raises it too) and, for a
  PDF pane, how many pages were left out (next item).
- Copying a PDF selection leaves out the pages that are not loaded (far
  or evicted pages have no text; `PdfPane::copy_selection` in
  `crates/temzdf-pdfview/src/pane.rs` only says how many). Load their text
  for the copy instead.
- Actions handled but not declared in
  `crates/temzdf-script/ts-types/temzdf-actions.d.ts` (an arm of
  `run_command`'s `match` with no declaration) are not found by any test;
  turning the `match` into a table (name -> function) would let a test
  compare both ways.
- Script tests (`temzdf/test`, `temzdf test`; `doc/dev/testing.md`),
  left for later when the first version was built (2026-11-22):
  - chai-style `expect(x).to.equal(y)` beside QUnit's `assert`;
  - a diff of two strings in a failure, rather than both shown whole;
  - `scenario`: steps sharing state, with cleanups that run when the
    scenario ends rather than after each step;
  - `t.newSession()`: a fresh session for one test, driven through a
    remote API, for tests that cannot share the file's session;
  - helpers reading the screen's text (what tui.rs checks), so that more
    tests can move from `crates/temzdf/tests/tui.rs`;
  - moving tui.rs' tests that only embed a configuration string and
    check what scripts can see, and the rest of
    `crates/temzdf-server/src/session/vim_tests.rs`, to `*.test.ts`;
  - user-facing documentation of `temzdf test` (`doc/config.md` only
    points to `doc/dev/testing.md`);
  - stack paths in failures relative to the repository (today the
    absolute paths the host loaded the file from);
  - an action to write files from a test (`t.openText` makes buffers
    without one; the Ex test writes beside the session's first file).
- `:w` with a range (Vim's `:5w`, `:'<,'>w FILE`: those lines only) is
  not built; a range on `:w` is `E481` (`crates/temzdf-script/runtime/vim/ex.ts`).
- A Nix cache for CI. Magic Nix Cache (in GitHub's cache) was turned off
  on 2026-11-20: it asks the cache API once per store path, ran into its
  rate limit (HTTP 429), and rust-cache's restores in the same run failed
  with it. Without one, `nix build` builds crane's `.#deps` and the guest
  every run, and the dev shells build libghostty-vt. Cachix would keep the
  store outside GitHub's cache (an account and a signing key needed).
- libtexprintf as a crate of its own (like a `libtexprintf-sys`) compiling
  its C with `cc`: builds without Nix would need neither autotools nor a
  pkg-config file of ours (`scripts/deps-without-nix.sh`). Not now.
- Terminal panes: wait for a program's output to pause before a frame.
  htop writes one screen update as about 150 small writes over 1.5 ms
  (measured with `strace` at 160x50, 6-8 KB, at most 0.05 ms apart), so the
  server, which handles the queued events first (`DRAIN_LIMIT` in
  `crates/temzdf-server/src/daemon.rs`), finds the queue empty after a few
  lines and shows a half-updated screen for one frame: a little more
  flicker than in zellij. The fix: after output, make the frame once no
  output came for about 1 ms, for at most about 4 ms (as Ghostty gathers a
  saturated pty for up to 3 ms); input alone never waits. Not urgent (the
  user, 2026-11-16).
- SyncTeX: what to do with a result, in scripts. With the script host in
  the server process a call costs about 10 µs, so the old reason to keep it
  all in Rust (the cost of talking to a separate JS process) is gone. The
  parser and queries stay in Rust: a large `.synctex.gz` has hundreds of
  thousands of records (estimated), slow to parse in QuickJS inside wasm and
  too big for the script host's memory cap. Expose the queries as
  primitives (`document.synctex.forward(file, line)` returning positions,
  `document.synctex.inverse(page, x, y)` returning file and line) and let
  scripts decide which pane shows a result, how it is marked, and how the
  editor is called (a function instead of `o.synctexeditor`).
  `temzdf --forward` keeps working without scripts (decision 27 in
  `doc/design/scripting.md`). Not urgent (the user, 2026-11-17).
- How `temzdf --forward` finds its viewer (2026-11-19). Today it connects
  to the per-PDF socket of whichever session opened the file first
  (`crates/temzdf-server/src/forward.rs`). That socket serves one
  connection at a time and waits up to 2 s for each request, and the
  client too waits 2 s for its answer: a connection that sends nothing
  (a stray `nc`, a crashed client) makes a forward search behind it fail.
  Open question: lengthen the client's wait or serve each connection on
  its own, or rather name the session to search in (the user's idea)
  instead of finding one by the file.
- Terminal panes' scrollback limit in lines, as a setting (like Neovim's
  `'scrollback'`). Now it is a byte limit, `SCROLLBACK_BYTES` in
  `crates/temzdf-term/src/lib.rs` (10 MB: about 13,600 rows at 80 columns,
  5,500 at 200), because the libghostty-vt we build against has only the
  byte limit at creation. Newer Ghostty (C API since late July 2026) has
  `GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES` and `_MAX_BYTES` through
  `ghostty_terminal_set`; when the Rust crate `libghostty-vt` exposes them,
  bump it and the commit in `nix/libghostty-vt.nix`, remove the byte limit,
  set the line limit, and add the setting (user's decision, 2026-11-02).
  Neovim's switch to libghostty-vt (neovim/neovim#39773) does the same.
  libghostty 1.0 is out (the user, 2026-11-07: its API may now stay
  stable): see whether the `libghostty-vt` crate has followed.
- The terminal visual mode (`crates/temzdf-term/src/visual.rs`), left for
  later (2026-11-06):
  - text objects (`iw`, `aw`, `iW`, `aW`), the user chose to wait;
  - searches matching across soft wraps (rows are searched one by one);
  - autoscrolling while a visual-mode drag is past the top or bottom (the
    mouse's own selection has it);
  - a view smaller than its terminal (it shows the bottom part) can leave
    the cursor in rows it does not show;
  - a visual mode for PDF panes (keyboard selection of text).
- The text editor framework, specified with the user on 2026-11-07
  (`doc/design/text-editor.md`, "Implementation" for the order). Built:
  `crates/temzdf-texteditor` (text, positions, edits, marks, the undo
  tree, motions, layout and drawing, files), the `edit` pane with
  `text.*`, `openFile`/`newBuffer`/`write`, `fallback` (with functions),
  `basemode`, mode-less editing in `defaults.ts` and the mouse (click,
  drag, wheel). Also built: the events and hooks of sections 9 and 10
  (invocations, `TextChanged`, `KeyTyped`, `schedule`, hooks that only
  read, cancelling "before" hooks and unsaved changes,
  `FileChangedOnDisk` and `reload`), search and `highlightPattern`,
  decorations and highlight groups for scripts, named encodings,
  `wordchars`, `findPair`, variables. Registers and the copy history
  (`doc/design/registers.md`): every pane's copies, the Vim mode's
  registers on them, `readClipboard`, `temzdf copy`/`paste`; picking from
  the history and cycling after a put wait for a completion and fuzzy
  finder framework for scripts (the user's plan). Reading the clipboard
  (OSC 52 queries) is untried in real terminals and inside tmux: check
  kitty, Ghostty, WezTerm, tmux and record it in
  doc/terminals/terminal-compat.md. The Vim mode
  is built (`doc/design/vim-mode.md`), with marks, the jump list,
  macros, incsearch, `:s///c`, Ex commands for panes, buffers and tab
  pages, block `I`/`A`/`c` and `<C-r>{reg}` (2026-11-07); what that
  document lists as not there yet is left. Deferred there:
  - `getchar()`/`input()`-like reading (`readKey`, `await` in
    asynchronous mappings; the user wants `input()`-driven operations
    such as sandwich.vim's function surround later);
  - function values in buffer, pane and tab variables;
  - signs, folding, `listchars` (decorations have highlights, virtual
    text, images, conceal and virtual lines, 2026-11-09; math previews
    are the scripts' to build on them);
  - backups, and swap files of buffers without a file (swap files,
    `o.swapfile`, and undo files, `o.undofile`, are built, 2026-11-08);
  - customising the editor's status line;
  - not discussed yet: SyncTeX with editor panes, syntax highlighting;
  - editable marks: ranges of a `nomodifiable` buffer the user may edit,
    and `text.edit(..., { force })` (specified 2026-11-19,
    `doc/design/text-editor.md`, section 7, "Editable marks"; for a JS
    REPL and TUI panes' input boxes).
- zellij delays keyboard input (even its own prefix commands) while it
  processes a frame, so `q` waits there; sending less data per frame is the
  only lever we have (user agreed to defer, 2026-10-25).
- Multiplexers: zellij (Composited strips) and tmux 3.8 (Unicode placeholders
  through passthrough) work; see `doc/terminals/terminal-compat.md`. Left: zellij over SSH
  is still slow (zellij 0.45.1 forwards images uncompressed; a half-resolution
  mode could help), and tmux master's native kitty support (z=0 only) is untested.
- e2e beyond X11 (Wayland, macOS, maybe Windows) without rewriting the
  action files (discussed 2026-10-29). Done: 1. action files use helpers
  only, the display server behind `be_*` functions in `scripts/e2e.sh`;
  2. `-d wayland` on headless sway with `crates/wlinput` (wtype and
  wlrctl did not do: a virtual device per call), kitty and the GUI
  (`doc/dev/testing.md`, "X11, Wayland and macOS"), every terminal and the
  GUI, `-I` (fcitx5) too; 3. `-d macos` (`termctl serve` and a virtual
  display at scale 1; `doc/terminals/macos.md`), every terminal and the GUI. Left:
  kitty's `viewer` zoom on macOS (three wheel reports per notch make it
  deeper than its pause allows), and:
  4. Windows: no bash, so then one small Rust driver for every platform
     (`crates/wlinput` is its Wayland part), `enigo` (input; Wayland and
     libei still experimental behind feature flags) with `xcap` (screen and
     window capture on X11, Wayland, macOS, Windows), and the action files
     as one action per line read by a Rust runner.
  Considered and set aside: VNC as the one layer (vncdotool; Xvnc, wayvnc,
  macOS screen sharing, a Windows server; tends to need a logged-in
  desktop on macOS and Windows), PyAutoGUI and nut.js (no Wayland), openQA
  and Appium (whole VMs or accessibility APIs, a poor fit for pixel-drawn
  terminals). The `expect` comparisons themselves are platform-neutral.
- Vector-heavy pages: MuPDF's anti-aliased rasteriser is slow there and
  `vello_cpu` about 20x faster (`doc/survey/gpu.md`; GPU rendering does not pay off).
  Measure real TikZ/pgfplots documents first; then maybe render such pages
  through a MuPDF-device-to-`vello_cpu` translation.
- Two-page (spread) layout.
- Markdown viewer (`doc/design/markdown-viewer.md`): built (2026-11-09:
  text and graphics modes, links, images, search, zoom, the outline; in the
  graphics mode pinch zoom and, since 2026-11-18, text selection and copy).
  Later:
  - math as MathML once Blitz lays it out;
  - a live preview of an edited buffer, through LSP-like change
    notifications;
  - the built-in help and LSP hover drawn by it;
  - a dark style for the graphics mode;
  - mermaid diagrams;
  - layout of large documents on a worker thread (now on the session's
    thread, a few milliseconds for usual documents; strips are painted on
    workers, ahead of the pane, through the tile schedule PDF tiles use).
- Showing PDF annotations.
- Check crate names (`termpix`, `termpix-ui`, `temzdf`) for collisions before publishing.
- Judged e2e (`expect` in `scripts/e2e.sh`, `doc/dev/testing.md`) covers keys and
  mouse input in kitty, WezTerm and Ghostty (keys also in tmux and zellij);
  sessions are judged against `temzdf render-session` (the text too in kitty
  and WezTerm; all pass in both, 2026-10-29), reload against fresh renders,
  SyncTeX with `check`, the GUI's focus and Japanese input with `check`
  on what the shell wrote, the screens after detaching and killing
  against the terminal's own drawing of the messages, the text
  `select-autoscroll` copies, the truncated moment of `reload` (the screen
  unchanged outside the status line, which says "reloading…") and the
  page a forward search lands on (the status line's text; 2026-11-02).
  Left unjudged: SyncTeX's highlight, IME composing,
  how far `select-autoscroll` scrolls (it follows the frames drawn, which
  a replay does not; judging it needs autoscroll driven by time alone),
  sessions inside multiplexers, and Ghostty's text.
- MuPDF's allocation lock makes text rendering slower with more threads (see
  `doc/dev/performance.md`). `fz_malloc`/`fz_free` take `FZ_LOCK_ALLOC` around the
  allocator itself, and `mupdf-sys` backs every context with the same static
  mutexes, so neither a custom allocator nor separate base contexts help. Only
  matters if rasterising ever becomes the bottleneck; then: patch the
  `mupdf-sys` lock wrapper, or render text-heavy tiles on fewer threads.
- Measure frame pacing over a real slow SSH link (fences and the writer thread
  are in place but were only tested locally, with a throttling pty proxy).
- The page thumbnail is misplaced by up to half a cell with the Layered
  backend (terminals scale images to whole cells). It only shows briefly; the
  `StandIn::Composed` path could compose it too.
