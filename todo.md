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

- Guard the `nested` flag of `ToHost::Call`/`Eval` (d0f9464): nothing
  checks `Bridge::answering()` (`crates/temzdf-server/src/session/script.rs`).
  If it always said true, calls typed while a task holds an atomic section
  would run inside it again (the race d0f9464 fixed), and only stress runs
  would notice. A session test with a fake host that records what is sent:
  a hook caused while a request is being answered goes `nested: true`; a
  key's call while a dispatched task runs and asks nothing goes `nested:
  false`.
- Replays (`--render-frame --events`, `crates/temzdf/src/headless.rs`)
  open the document in no simulated time, while the real viewer took up
  to ~370 ms (2026-11-23). The recording starts before the open, so
  anything timed from the first screen runs early in a replay by the open
  time. Nothing judged depends on it yet; to line the clocks up, temzdf
  would record when the document opened and the replay start its clock
  there.
- Session timers still on the wall clock (`Instant::now()`) rather than
  the session's (113f4b2 moved notices): the clipboard wait
  (`crates/temzdf-server/src/session/registers.rs`), `updatetime` before
  swap files are written (`swap.rs`) and `busy_since` for the "Script is
  running" notice (`script.rs`). None is judged by a replay now; a replay
  that reaches them depends on how fast it runs.
- `tests/e2e/scroll_demo.actions` in WezTerm on X11: `wheel6` scrolled
  372 px instead of 360 in 2 of 6 runs (2026-11-23), one step accelerated
  as if two arrived within the 80 ms flick window, with the steps sent
  0.34 s apart and with each sent once the screen stood still (1.1 s
  apart). The cause is not known: 6 later runs with `SCROLL_DEMO_LOG=FILE`
  (what scroll_demo receives, with times) all gave 360, each click one
  report 2 to 5 ms after it was sent. `scripts/e2e-all.sh` writes the log
  (OUTDIR/TERM/scroll_demo/scroll_demo.log) until a 372 is caught, then
  stops (the user's decision); to decide then: accept 372 in WezTerm, SKIP the check there, or
  change how e2e sends or how scroll_demo takes its time.
- Simulated-terminal tests that fail under heavy load (seen 2026-11-23 while
  `cargo test --workspace` and many `temzdf test` runs loaded the machine):
  `the_markdown_viewer_copies_follows_links_and_lists_its_headings`,
  `the_markdown_viewers_graphics_mode_is_pixels_or_says_why_not`,
  `a_markdown_text_screen_renders_to_pixels`,
  `inside_a_session_temzdf_means_that_session` and
  `a_pdf_screen_renders_to_pixels`. Most failing screens showed the busy
  notice (then shown after a script ran for a second; now after 8 s, as
  "Script is running.  Type <C-c> to interrupt."), which the tests read
  as part of the screen. Check whether they still fail under load; if so,
  make them wait the notice out or ignore it.
  `cmdline_completion_popup_stays_in_every_frame_while_typing` (also in
  CI) saw partly updated completion lists because frames did not wait for
  scripts started under queued hooks (fixed: `begin_busy` in
  `crates/temzdf-server/src/session/script.rs`); its check now skips
  frames read a second or more after the key, when the session draws
  what the script has done so far, as designed.
  `a_fuzzy_finder_shows_its_prompt_above_its_list_in_a_frame` failed once
  in a full gate (2026-11-23) and passed alone three times; not looked
  into.
- Whether `temzdf action` goes, now that `temzdf eval` runs the
  configuration's language from the command line, or shrinks to what a
  shell needs (opening a file: `new-pane FILE` resolves paths against the
  caller's directory, which `eval` does not): the user decides
  (`doc/design/scripting.md`, "temzdf eval").
- `temzdf eval` whose task the unattended limit stopped outside an atomic
  section answers only when its `--timeout` runs out (the host cannot tell
  which task a stopped promise job was; `doc/design/script-host.md`,
  "temzdf eval").

- Completion and fuzzy finding for scripts (`doc/design/completion.md`,
  a draft; the shared foundation and completion's sources are decided).
  Built: `temzdf/ff`'s types, combinators and engine without a UI, and
  the first shipped matchers, sorter and renderer (`temzdf/ff/builtin`),
  the list buffer, the UI's shared part (`createList`), and completion's
  layer and popup (`temzdf/ff/completion`: registration, groups,
  `firstOf`, inserting on select, the keys `<C-n>` `<C-p>` `<C-y>`
  `<C-e>`), and completing Ex commands in the command line (its marker
  and `isCmdline`, `promptOrigin`, `<Tab>`/`<S-Tab>` there, command names
  and `command.add`'s `complete`: buffers, commands, `:set`'s options),
  and the fuzzy finder's UI (`ff.define`, `ff.start`, `:FF`, the finders
  `buffers`, `lines` and `commands`, the action picker).
  Order: the fuzzy finder and completion framework (`temzdf/ff`) before
  an LSP client; the first use wanted is completing Ex commands in the
  command line, which is a text buffer like Vim's cmdwin, so it is
  insert-mode completion in a text pane and completion's UI (a popup over
  the list buffer) comes before the fuzzy finder's. Still to decide:
  - Removing duplicates across sources, sorting across sources (scores,
    `sortText`; now each source's matches follow the one before).
  - Accepting a completion: snippets (none yet), additional edits, `.`
    repeat (the Vim mode repeats what was typed, candidates included);
    a cancelled completion leaves an undo step that changes nothing.
  - Previewing, resolving details lazily, ghost text, long
    documentation.
  - Debouncing, per-source time limits, keeping the old candidates while
    collecting again; display widths in the popup (labels are padded by
    UTF-16 code units, wide characters misalign the columns); `<C-p>`
    from nothing selected takes the last line written, not necessarily
    the last match.
  - Errors from sources (logged, not shown).
  - Keys typed ahead of the list: `<Tab><Tab><CR>` typed at once in the
    command line runs the line before the selection waiting for the
    candidates is made (the moves wait for lines to be written).
  - Keys typed ahead in a fuzzy finder: the keys acting on the list wait
    for it, but text goes into the prompt at once, so `<C-a>split<CR>`
    typed at once types `split` into the finder's query (the action
    picker is not open yet).
  - The fuzzy finder: a previewer's UI (previewers are types only), a
    files finder (`readDir` of `temzdf/fs` is there now), help topics (no help yet),
    clicking a candidate, closing with `<C-c>` (reserved by the server:
    a Rust change), remembering the last query or resuming the last
    finder.
  - Narrowing only the previous matches when the new query extends the
    old one (as fzf does): measured, one query over 100k items takes
    seconds with npm `fzf` in the script host ("Speed" there).
  - Path completion and a files finder on `temzdf/fs` and `temzdf/path`
    (built 2026-11-23). Still open: a working directory for scripts
    (`cwd()`, `temzdf/path`'s `resolve`, left out until where one is
    comes from is settled: "Absolute paths only" below); an
    `AbortSignal` for long reads; writing files.
  - Floats with `focusable: false` (popups, the list) are also left out
    of `toggleFloats` and of its count. If a float ever should not take
    the focus yet still hide and show with the user's floats (a small
    always-on info window, say), split that into an option of its own.
- Script requests get slower with the number of decorations (found in
  completion's stage 2, 2026-11-22; the user decided to do this right
  after completion's layer): one request costs the server about 0.7 ms
  with no decorations and about 13 ms with 2000 of them, even a request
  on another pane (a list's `last()` over 2000 decorated items took about
  14 s). Profile the server's request path (`crates/temzdf-server`) and
  fix it in Rust.
- A way to turn undo off for a buffer (Vim's `undolevels=-1` for one
  buffer): the ff list's buffer gets an undo step for every write, kept
  for nothing.
- The Windows port (`doc/survey/windows-port.md`, 2026-11-22). Work
  happens on the branch `windows-port`. On Windows the GUI client is the
  frontend; the terminal frontend comes last. Order:
  - The toolchain: MSYS2's UCRT64 (its Rust, `x86_64-pc-windows-gnu`,
    GCC), not MSVC, since 2026-11-23 ("The toolchain" in
    `doc/survey/windows-port.md`). Still to do: check whether the
    executables need MSYS2's DLLs (`libwinpthread-1.dll`,
    `libgcc_s_seh-1.dll`) and link them statically or ship them; once
    the port has settled, build and test with UCRT64's Clang too, besides
    GCC (in CI or the VM): the order of evaluation differs between them
    (libtexprintf's `AddChild`).
  - First steps, no VM. Done: the `windows` step of the gate (clippy for
    `x86_64-pc-windows-gnu` over `WINDOWS_CRATES` in
    `xtask/src/check.rs`, also in the Linux CI job) and a
    `windows-latest` job `check-windows` (MSYS2 through
    `msys2/setup-msys2`; passes, 2026-11-23). texprintf and tuitest only
    cross-check because pkg-config finds the Linux libraries, so they
    are left out of the cross-check; `.gitattributes` with LF; `cfg(unix)` on
    the symlink and mode tests and on `mutate.rs`'s shell tests (not yet
    compiled for Windows); esbuild found as an exe in
    `crates/temzdf-script/build.rs`; `pdf::fixture` builds the fixtures
    in Rust (`crates/temzdf-fixtures`, `cargo xtask fixtures`).
  - The VM: vitro with a local Windows 11 ISO (OVMF firmware).
    `scripts/windows-vm.sh` (a thin wrapper, "Windows in a VM" in
    `doc/dev/testing.md`) starts, stops, resets, runs commands in,
    copies to and from, and takes screenshots of a VM from the golden
    image; `build` (an English ISO, unattended, 20 minutes), `provision`
    (Git, Node, Zig, Python, MSYS2 with UCRT64's Rust, GCC, Clang, make,
    esbuild; promoted to the next golden image) and `check` (the gate on
    a commit in the guest, in UCRT64's shell) too. `check` passes (at
    d14dc45: clippy and 208 tests over `WINDOWS_CRATES`). Earlier, with
    MSVC (at 7ed00b8), `cargo check --workspace` there: `termpix`,
    `temzdf-term` (Unix APIs), `temzdf-pdfcore` (SyncTeX's macro under
    MSVC, and `OsStrExt` in `synctex.rs`) and the script host tests of
    `temzdf-script` and `temzdf-script-runner` (`UnixStream`) failed;
    `tuitest`'s pty tests need `sh`. Since then: libtexprintf's
    `AddChild` fixed (`STATUS_HEAP_CORRUPTION` with MSVC; its 6 tests
    pass), `synctex.rs` fixed, and with UCRT64 `temzdf-pdfcore` builds;
    its tests pass but for 2 that compare paths in SyncTeX files built
    on Linux (the VM has no TeX). Still to do: `cargo check --workspace`
    with UCRT64 against the list above; TeX in the VM, or fixtures built
    there otherwise, for `temzdf-pdfcore`'s tests; `temzdf-pdfcore` into
    `WINDOWS_CRATES`.
    Still to do: fonts, the e2e. The ConPTY
    experiment with the in-box ConPTY is done (`doc/terminals/conpty.md`,
    `experiments/conpty/`): kitty graphics are dropped, the kitty
    keyboard, the SGR-pixel mouse and OSC 52 pass, the pass-through flag
    changes nothing. A side-loaded `conpty.dll` + `OpenConsole.exe`
    (NuGet `Microsoft.Windows.Console.ConPTY` 1.25.261109001) passes kitty
    graphics, XTGETTCAP, DA1 and DECRQM through, with either flag. Still
    to try: the probes inside Windows Terminal and WezTerm, and shipping
    `conpty.dll` with temzdf (portable-pty loading it).
  - The product: compile (gate `rustix`, `signal-hook`, `libc`;
    build tooling), the transport
    (`uds_windows`), directories and processes (the GUI attached to a
    PDF session), terminal panes, SyncTeX paths, shared memory, the
    terminal frontend. Done: the transport, directories and the
    server's start-up for `temzdf-protocol`, `temzdf-client` and
    `temzdf-gui` (cross-checked; their tests not run on Windows yet),
    and the server moved to them (gaps in
    `doc/design/session-server.md`, "Windows"). Terminal panes are off
    on Windows for now (`temzdf-term`'s pty is Unix only; the pane
    tests that start programs are ignored there): ConPTY later. The
    terminal frontend (`termpix`, `temzdf-tty`; `attach`, `standalone`,
    `render-session`) is Unix only: on Windows `temzdf` is the server
    and the client commands, `temzdf FILE` points at `temzdf-gui`,
    inverse search runs `cmd /S /C` and links open through rundll32's
    `url.dll`. With that, `cargo check --workspace --all-targets
    --exclude termpix --exclude temzdf-tty` passes in the VM
    (2026-11-23). Since then the gate on Windows also tests `temzdf`,
    `temzdf-pdfview`, `temzdf-script`, `temzdf-server`, `temzdf-term`,
    `texprintf`, `tuitest` and `xtask` (`WINDOWS_ONLY_CRATES` in
    `xtask/src/check.rs`: not cross-checked from Linux, as they build C
    for the target), with the tests that start programs in panes or on a
    pty, read the TeX fixtures, or have the script guest read files
    ignored there; swap file names on Windows are Neovim's
    (`C%%Users%...`). Still to do: the server run there; the script
    guest reading files on Windows: the runner preopens `/` only, so
    `C:\...` paths are refused (WASI's `EPERM`, os error 63): no user
    configuration, no imports, no `temzdf test` (the shipped defaults
    load); a mapping of Windows paths (drives, UNC) into the guest is to
    be decided. Then `temzdf-script-runner` (3 of its 30 tests pass on
    Windows now) and the js tests join the gate there. `ls-fonts`
    without a session starts a terminal pane for the configuration.
  - The tests: a helper program instead of `sh` in the pty tests; the
    PDF fixtures on Windows (built with TeX; `temzdf-pdfcore` stays out
    of the gate there, and the fixture tests of `temzdf-pdfview` and
    `temzdf-server` are ignored); test fonts without fontconfig; the gate's lock test; `cargo xtask miri`
    (also cross-target from Linux);
    a Windows backend for `scripts/e2e.sh` (WezTerm, Windows Terminal,
    the GUI). Done: the script host tests on `temzdf_protocol::Stream`
    pairs (Unix domain sockets on Windows too, keeping the read timeout
    `std::io::pipe` lacks); the mock terminal tests of `temzdf-pdfview`
    and `temzdf-server` are Unix only (termpix).
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
- Watch the maintenance of JSR's `@std/path` (the planned `temzdf/path`,
  2026-11-23): Deno the company announced the end of its maintenance of
  Deno next year; a community may carry on, but what becomes of `@std/path`
  is unknown. Path handling is mature, so this may never matter.
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
  finder framework for scripts (the user's plan; the design so far:
  `doc/design/completion.md`; what is left to decide: "Completion
  and fuzzy finding" under Deferred). Reading the clipboard
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
- The e2e harness in TypeScript (decided with the user on 2026-11-23; not
  before `windows-port`, whose `-d windows` backend adds about 400 lines
  to `scripts/e2e.sh`, is merged): `scripts/e2e.sh`, `scripts/e2e-all.sh`
  and the action files (`tests/e2e/*.actions`, sourced by the harness, so
  moved together) as Node TypeScript under `scripts/`, type-checked by
  `tsc -p scripts`. The action files become plain TypeScript (`await
  keys("q")`), for type checks of helper names and arguments; the few
  shell lines (line numbers by `grep | cut`, `check ... sh -c`, pipes into
  `magick`) become TypeScript or a small `` sh`...` `` helper. ImageMagick
  stays, behind a thin typed wrapper for the shapes used (crop to the cell
  grid, composite a mask, count differing pixels, a few queries, a
  gradient; anything else as raw arguments), which also tells `compare`'s
  "differ" (exit 1) from its failures (exit 2), both swallowed by `|| true`
  today. The hard part: cleanup (traps, children, Xvfb/sway re-execution);
  Windows' QMP can then be spoken over `node:net` instead of a socat
  coprocess. Linux is checked by `scripts/e2e-all.sh` here; macOS and
  Windows need their VMs.
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
  `select-autoscroll` is judged with `expect` once the autoscroll reached
  the document's end, and `scroll_demo` by how far its image moved
  (2026-11-23); `synctex` and `synctex-reload` judge the forward search's
  highlight with `expect` (the replay sends the recorded searches;
  `o.forwardflash = "input"` keeps the highlight until the screenshot;
  2026-11-23). Each expect's cells (colours, attributes, and Ghostty's
  text) are judged against the terminal's own drawing of them
  (`NAME.cells`, 2026-11-23). Left unjudged: IME composing, sessions
  inside multiplexers, and Ghostty's text outside expects
  (`screen_contains` SKIPs there).
- `screen_contains TEXT` in Ghostty (it cannot report its text): have the
  shell draw TEXT (as `draw_lines` does) and look for that row's pixels in
  the screenshot (an image sub-search, as `screen_restored` compares rows),
  instead of SKIP.
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
