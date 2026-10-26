# Scripting and configuration design

Status (2026-10-26): design in progress, nothing implemented. The execution
model is settled; the questions under "to decide later" wait. This records
what was decided with the user and what is still open. It supersedes the
language recommendation of `doc/config-language.md` (Lua): the configuration
is to be written in JavaScript/TypeScript. The session server it builds on is
described in `doc/session-server.md` (with its implementation status).

## Guiding principle

temzdf should feel like **Vim/Neovim without editing buffers**: a
Vim-like editor whose windows show terminals, PDFs (and later web pages)
instead of text being edited. Its configuration works like a vimrc plus
plugins. When a question comes up, the first answer to try is "what does Vim
(or Neovim) do".

The mapping system is the exception: it is meant to improve on Vim's
(mappings run actions rather than produce keys; modes are user-definable), so
there the design departs from Vim on purpose.

The Vim vocabulary maps onto temzdf as follows:

| Vim | temzdf |
|---|---|
| window (`winid`) | pane |
| buffer (`bufnr`) | a PDF document, a terminal (process and screen), later a web page |
| tab page | tabs (later) |
| mode | mode (see "Modes") |
| option scopes (global, window, buffer) | the same three scopes |
| autocmd | hooks |
| Ex commands | the future command line and `temzdf action` |

What Vim cannot answer is everything about several clients on one session;
that part follows tmux and zellij (see "Prior art").

## Decisions

### Where scripts run

- Scripts run **only on the server side** (the session server, or the
  standalone viewer's process). Clients run none.
- Scripts run in a **separate process** and talk to temzdf over RPC, much like
  denops.vim (or Neovim's remote plugins and msgpack-RPC API). A runaway or
  crashing script cannot freeze or kill the session, and the process can be
  restarted.
- The script process is **temzdf itself with rquickjs built in**, started by
  the server as a hidden subcommand (the script host). No outside runtime:
  Node.js needs `node_modules`, Deno's future is uncertain, bun's too, and a
  built-in engine keeps the Nix build and distribution as they are.
- **RPC**: both ends are Rust (JavaScript only meets rquickjs inside the
  host), so the host speaks `temzdf-protocol`'s framing (length prefix and
  postcard, serde types) over pipes to the server that started it. A bridge
  (e.g. msgpack) is only needed if outside runtimes ever connect.
- Key lookup stays in the server (Rust): the script process registers mapping
  tables and hook subscriptions; only a key that is mapped to a script
  function, or an event someone subscribed to, causes an RPC. Typing into a
  terminal pane never waits for the script process.
- How scripts execute (synchronous functions, `dispatch`, `atomic`) is in
  "Execution model" below.
- The language is JavaScript, with TypeScript accepted through type stripping
  (erasable syntax only, as TypeScript's `erasableSyntaxOnly`; positions kept
  so errors point at the source). Type definitions (`.d.ts`) are generated
  from the same registry as the commands, so they cannot drift.
- The TOML configuration (`crates/temzdf/src/config.rs`) is to be
  removed. There are no outside users yet, but each of its settings needs a new
  home (see "Open questions, to decide later").
- Mouse handling is **built in only** and cannot be remapped; scripts can set
  its parameters (scroll speed, kinetic scrolling, hover delay, ...). Besides
  keeping the mouse fast, this keeps a way to operate temzdf when scripts are
  broken.

### Clients: one operator, the others watch

- At any time exactly **one client is the operator**. The others watch: they
  get the same frames, and their input does nothing except make them the
  operator.
- **Control moves implicitly**: the client that sends input (or whose outer
  terminal gains focus) becomes the operator. This fits the common case of one
  person moving between machines. A **read-only attach** (like `tmux attach
  -r`) gives a client that never takes control (e.g. for presenting).
- Consequently focus, the current mode, pending key sequences, counts and
  messages are **session state** (they belong to the operator), and frames
  stay composed once per session as today. The session size, frontend hints
  and side effects (opening a link, copying, running the editor) follow the
  operator. CLI actions act as the operator.
- When control moves, pending keys and counts are dropped; the mode and focus
  carry over.
- Focus gained/lost reports from the outer terminal are to be used: gaining
  focus makes a client the operator (as Vim's `FocusGained`), and hooks get
  the event.

### Commands

- **One registry** of commands (name, typed arguments, which pane kinds they
  apply to) serves key mappings, scripts, `temzdf action` and the future
  command line. The JS functions, the text form and the type definitions are
  generated from it.
- The Rust core provides **primitives** (split a pane, focus a pane, close,
  resize, swap, query the layout, ...); composite operations (split and focus,
  layouts) live in the JavaScript library shipped with temzdf, as Emacs keeps
  `split-window` in C and `split-window-below` in Lisp. Prerequisites, agreed
  in principle but not yet confirmed (see open questions): batched execution,
  and CLI/escape hatches that do not need scripts.
- IDs are **unique within a session and never reused**: panes, documents
  (buffers), clients. Sessions are the namespace across servers.
- A handle to a pane that has closed fails cleanly (the call rejects) instead
  of acting on another pane.

### Key mappings and modes

- A mapping goes from a **key sequence to an action**: a script function, or a
  built-in action given as data (`temzdf.cmd.scroll(1)` returns a description
  the server can run by itself). Built-in actions as data run synchronously in
  the server, without an RPC; this is how the default mappings and every mode
  switch work, so a mode change can never race the next key.
- A mapping may run **several actions in a row**, executed by the server
  without other input in between (so "enter the operating mode, do X, go back"
  behaves like a tmux prefix, while a mapping that only enters the operating
  mode behaves like Vim's Normal mode).
- **No built-in mappings in Rust**: the defaults come from a JavaScript
  configuration shipped with temzdf.
- Key sequences resolve **as in Vim**: ambiguous prefixes wait for
  `timeoutlen`, `<nowait>` opts out. In the pass-through mode keys that time
  out are sent to the pane, as Neovim's terminal mode does with `tmap`. Delays
  caused by one's own mappings are accepted.
- Key notation **follows Neovim** (`<C-h>`, `<S-Tab>`, `<CR>`, ...), and keys
  the terminal tells apart stay apart (revised 2026-10-26): under the kitty
  keyboard protocol `<S-a>` (Shift and a) is not `A`, `<C-i>` is not `<Tab>`,
  `<C-m>` is not `<CR>`, `<C-[>` is not `<Esc>`, `<C-h>` is not `<BS>`.
  `<C-A>` is written the same as `<C-a>` (as in Neovim).
- **Resolving keys with several readings** (decided 2026-10-26, without the
  user): a typed key is matched as typed first; if no mapping (or prefix of
  one) takes it, its fallback is tried: `<S-a>` falls back to `A`, `<C-i>` to
  `<Tab>`, `<C-m>` to `<CR>`, `<C-[>` to `<Esc>`, `<C-h>` to `<BS>`. So a
  configuration written for legacy terminals (`A`, `<Tab>`) works unchanged on
  kitty, while one that maps `<C-i>` gets it there. For a sequence, the
  readings are tried by fewest fallbacks; among equal ones earlier keys stay
  as typed. The longest mapped prefix wins as usual, and keys that no reading
  takes are unmapped (passed through as typed, not as the fallback).
  Alternatives considered: normalising everything to the legacy form (loses
  the distinction the user asked for) and making `<S-a>` and `A` always the
  same (Vim's view; also loses it).
- Mappings take a **count**.
- A mapping whose function throws behaves like a Vim script function failing
  inside a mapping: the error is shown and the rest of the mapping is
  abandoned.
- Modes come in three kinds:
  1. the **operating mode** (like Vim's Normal mode): keys and mouse act on
     temzdf and do not reach panes;
  2. the **pass-through mode** (like Neovim's Terminal mode): keys and mouse go
     to the pane, except mapped ones;
  3. **user-defined modes**.
  Pane kinds are absorbed into modes (e.g. a PDF pane mode; names are
  placeholders), rather than handled by buffer-local mappings: focusing a pane
  switches to that pane's mode.
- How a mode treats **unmapped keys is a property of the mode**, stated when a
  built-in mode is implemented and when a user mode is defined.
- A user-defined mode names its fallback mode explicitly (`inherit: <mode>`):
  an unmapped key is looked up again in that mode, and so on. Proposed but not
  confirmed: make `inherit` one of the unmapped-key behaviours (`drop`,
  `passthrough`, `inherit: <mode>`) so a chain always ends; see open
  questions.
- Showing the current mode is left for later.
- Escape hatches that work whatever the configuration does are required (at
  least detach and quit); their form is open.

### Hooks

- Start with the candidates listed below; frequent events (terminal output,
  pointer motion) are designed when they are needed.
- Reloading the configuration removes every mapping and hook and registers
  them again (no duplicates, the problem Vim's `augroup` solves).
- "Keep a terminal pane open after its program exits" is a terminal option
  (like tmux's `remain-on-exit`), not a hook that cancels the default.

Candidate events: session start, configuration loaded and first pane created
(Vim's `VimEnter`), configuration reloaded, client attached/detached, operator
changed, pane created/closed/focused, PDF loaded/reloaded (success and
failure)/page changed, terminal title or working directory changed (OSC 7),
bell, program exited, resize, mode changed.

### Layout

- Sizes are given **in cells only**; scripts can read the client window size
  and compute proportions themselves.
- A split that leaves no room is an **error**, as in Vim.
- After a split the focus **moves to the new pane by default**; an option
  keeps it.
- Where a split goes (decided 2026-10-26) is two orthogonal choices, so no
  combination is invalid and string literal types catch typos: `side`
  (`"top" | "bottom" | "left" | "right"`: direction and side at once) and
  `scope` (`"pane"`, the default, splits the current pane; `"screen"` splits
  outside the root of the layout tree, giving the full width or height at
  that edge). In Vim's words: top/bottom/left/right with `pane` are
  `:aboveleft split`, `:belowright split`, `:aboveleft vsplit`,
  `:belowright vsplit`; with `screen`, `:topleft`/`:botright` instead.
  Without `side`, options like `'splitbelow'`/`'splitright'` decide. API
  shape: `split({ side, scope, size })`; the Rust primitive takes the target
  (a pane or the root) and the side.

### Pane contents

- SyncTeX forward search belongs to the **document**, not the pane
  (`document.forwardSearch(...)`, reachable from a pane as `pane.document`).
  The result is shown in the focused pane if it shows that document, otherwise
  in the pane closest to the root of the layout tree (like Vim's choice of
  window for `:help`); an optional argument names the pane.
- Pane state (page, zoom, file, a terminal's title and working directory) can
  be read.
- Pane handles are typed by kind (`PdfPane`, `TermPane`) so calling a PDF
  command on a terminal is caught by the type checker.

### Colours

- The pixel UI (status bar, outline, ...) keeps drawing as it does; no themes
  that follow the terminal.
- Palette colours matter only for terminal panes (which pass palette indices
  to the outer terminal).

### Execution model

Decided (the whole model was settled with the user on 2026-10-26):

- **Mapped functions and hook functions are synchronous.** Commands called
  from them are synchronous RPCs (the host waits for the answer). While such a
  function runs, the server serves only that host's requests and holds back
  everything else (keys, other clients, events), exactly like typeahead while
  a Vim function runs. A mapping's or hook's function is therefore atomic by
  construction, and keys pressed meanwhile are handled afterwards.
- **Only synchronous functions** may be mapped or used as hooks; one that
  returns a Promise is an error. Above all an `await` cannot be interrupted
  with `<C-c>` (nothing is executing). A library with an asynchronous API is
  used from `dispatch`, without holding input. Allowing asynchronous
  functions later is sketched, with its pitfalls, in "Future: asynchronous
  mapped and hook functions".
- **`<C-c>` interrupts** a running mapped or hook function, as in Vim. While
  the server holds input for a running function it checks raw input for
  `<C-c>` before queueing it; at other times `<C-c>` is an ordinary key (in the
  pass-through mode it reaches the program). The host keeps a thread that
  reads control messages and sets the flag rquickjs' interrupt handler checks;
  if the function is blocked in a command, the server answers that command
  with an "interrupted" error. Effects of commands already run stay, as in
  Vim. Dispatched tasks are not interrupted (they hold no input); listing and
  cancelling them can come later. The key is **fixed to `<C-c>`, and
  mappings starting with `<C-c>` are not allowed**, so it works whatever the
  configuration (decided 2026-10-26). `<C-c>` also cancels a pending key
  sequence and count, as in Vim's Normal mode (decided).
- **`temzdf.dispatch(async () => ...)`** starts a task without waiting for it
  (like a shell's `cmd & disown`); long work and `await` belong there. Errors
  of dispatched tasks go to the message area and the log.
- **Hooks may be synchronous** (including ones before exit, detach or
  closing a pane), with `dispatch` for anything asynchronous, as for mappings.
- **Errors reach JavaScript as exceptions.** Composite actions (e.g. what
  `temzdf action split right` does today) are implemented in the JavaScript
  shipped with temzdf; the server runs them in the host, also when they are
  asked for from the CLI (an error says so when the host is not running).
  Escape-hatch actions (detach, quit, kill the session) are Rust primitives
  that work without the host.
- **Startup**: the configuration is evaluated with no panes, the server waits
  for the host to report that evaluation finished (with a timeout), then
  creates the panes given on the command line and emits the `VimEnter`-like
  event.

- **Commands are synchronous when they finish quickly; only commands that
  wait for something return Promises** (a PDF finishing to load, a forward
  search result). This revises the earlier "commands return Promises", which
  assumed asynchronous mapped functions.
  Making a PDF pane appear at once in a "loading" state (as Vim's `:e` creates
  the buffer at once) would let `split()` return the pane synchronously; to be
  decided when implementing.
- **`temzdf.atomic(fn)`** for dispatched tasks: `fn` must be synchronous (typed
  `() => T`, and a returned Promise is an error), so the critical section
  ends when `fn` returns and can never be held across an `await`. Only an
  endless loop or a crashed host can hold it, and the timeout below and the
  host's exit release it. Inside mapped and hook functions it is not needed.
  Unlike denops' `batch` and `nvim_call_atomic`, results inside are usable
  right away.
- **Startup in two halves**: the configuration's top level only registers
  (options, modes, mappings, hooks), which is instant; slow work goes into
  `dispatch` or the `VimEnter`-like hook, and Vim's autoload is dynamic
  `import()` on first use (a lazy-loading helper can be shipped). Top-level
  `await` in the configuration is an error (with a hint to use `dispatch`),
  so the first half is always instant.

- **No timeout interrupts a function** (as in Vim): `<C-c>` is the way. A
  function running longer than about a second shows "running; `<C-c>` to
  interrupt" in the message area. The startup wait for the configuration keeps
  its own timeout, so an endless loop at the top level cannot stop startup.
- **CLI requests always time out on the client side**: `temzdf action` (and
  other requests) give up after 10 s by default, adjustable with an option;
  the server's work is not interrupted, the command just fails.
- **Context tokens** (decided 2026-10-26, cheap enough to have from the
  start): each call of a mapped or hook function, and each dispatched task,
  has an id; every command carries the id it was issued under, and the
  server rejects ids whose call has finished or was interrupted (one lookup
  in a set per command). QuickJS cannot carry "the current call" across an
  `await` (no `AsyncLocalStorage`), so: the global functions
  (`temzdf.split()`) work only during a synchronous call and take its id; a
  dispatched task issues commands through the `ctx` it receives, which holds
  its id; a command with no id is rejected. This catches stray continuations
  (`.then` without `await`) and dispatched tasks acting after they were
  cancelled.
- **Deadlock rules.** (1) While the server waits for the host (for a mapped
  or hook function to finish), it keeps answering that host's requests, with
  user input held: a function calling `temzdf.split()` waits for the server's
  answer while the server waits for the function, so ignoring the request
  would stop both. This also covers a dispatched task whose command arrives
  just as the server starts a mapping: answering it first is as if it had
  happened before. (2) While a function runs, the server asks the host to run
  nothing else, except the hooks of events that function caused (below).
- **Hooks run as in Vim's autocmds**: synchronously, at the moment the event
  happens, nested inside the command that caused it (a hook for "pane created"
  runs inside the `split()` of the mapping that split). The host accepts
  such a call while it waits for the answer to that command, and runs it on
  top of the waiting function, as Neovim handles requests while waiting for
  an RPC reply. Events caused inside a hook do not fire hooks, except hooks
  registered with `nested: true` (Vim's `++nested`), with a depth limit (Vim's
  is 10, E218). Hooks for one event run in the order they were registered.
  "Before" events (before closing a pane, before detach, before exit) exist
  because hooks run at the moment; replacing a default action uses the
  `*Cmd`-like replacement functions (open question 16).

## Implementation details (decided 2026-10-26)

Settled while planning the next three stages (modes and key mappings;
buffers separated from panes; the rest of terminal panes). Items marked (u)
were asked of the user, the others are Claude's choices the user approved
with the plan.

**Modes and key mappings**

- (u) The prefix enters the operating mode and stays there (Vim's Normal
  mode): moving to another pane returns to that pane's mode (the history
  rule), `<Esc>` too; no timeout.
- (u) The default prefix is `<C-b>` (configurable).
- (u) With the script host down or the configuration failed, a fixed Rust
  keymap works: `<C-b>d` detach, `<C-b>q` end the session (quit when
  standalone), `<C-b>r` restart the host and reload; every other key goes to
  the pane; the notice lists these three.
- (u) The floating message window: errors stay until the next key (which is
  still handled), information fades after a few seconds, the host-down
  notice stays until the host is back.
- (u) TypeScript joins the dev shells so the shipped defaults are type
  checked with `tsc` against the generated `temzdf.d.ts`.
- Built-in modes: `normal` (operating; unmapped `drop`, counts),
  `terminal` (pass-through, the default of terminal panes; unmapped
  `passthrough`, no counts), `pdf` (the default of PDF panes; inherits
  nothing; unmapped `drop`, counts).
- `timeoutlen` defaults to 1000 ms, for every mode. Counts are digits; `0`
  counts only after another digit (as in Vim).
- While a PDF pane has its search prompt open or the outline focused, keys
  go to the pane directly (fixed keys, as today), until a command-line mode
  covers them.
- Key strings are written as for Vim's `feedkeys` (`jjj++02G/needle<CR>`);
  `--keys`, `scripts/frames.sh` and the judged e2e scenarios switch to it.
- The shipped defaults are a TypeScript module embedded in the binary
  (`crates/temzdf-script/runtime/defaults.ts`, module `temzdf:defaults`),
  loaded before the user's configuration; `--clean` loads only them.
- Default mappings: `pdf` has today's page-view keys (`quit` loses `<C-c>`,
  which is reserved); `terminal` and `pdf` map `<C-b>` to `switch-mode
  normal`; `normal` maps `%`/`"` (split right/down), `h j k l` and arrows
  (focus), `o` (next pane), `x` (close), `d` (detach), `<C-b>` (send C-b to
  the terminal, then exit the mode), `<Esc>` (exit the mode).
- API: `defineMode(name, { inherit, unmapped, count })` (built-in modes'
  properties can be changed too), `map(mode | modes, lhs, rhs, { nowait,
  desc })`, `unmap(mode, lhs)`; `rhs` is a function, an action description
  or an array of them; `action.*` mirrors `cmd.*` as data, plus
  `action.switchMode`, `action.exitMode`, `action.feedkeys(keys, { remap })`,
  `action.sendText`; `feedkeys(keys, { remap = true })`, `currentMode()`;
  options through a Neovim-like `o` (`o.timeoutlen = 500`); a mapping
  starting with `<C-c>` throws.
- Options (global for now, Vim-style names): `timeoutlen`, `splitbelow`,
  `splitright`, `modehistory` (100), `scrollstep` (60), `scrollsmoothness`
  (60), `linkopener`, `linkpreview`, `linkpreviewdelay` (300),
  `reloaddebounce` (200), `reloadanchor`, `synctexeditor`.
- `ModeChanged(old, new)` joins the events.
- `temzdf print-types` (hidden) writes `temzdf.d.ts` from the command
  registry and the static API; generated, never committed.
- The link opener travels with the effect (`OpenUri { uri, command }`) from
  the server's options to the client.
- TOML goes: `--config FILE` names the script; `[terminal]` and `log_dir`
  become environment variables (and options); `doc/config.md` describes the
  script configuration.
- `prefixMap`, the Rust keymaps and `Keymap` go; page-view commands stay as
  registry commands (with counts).

**Buffers and panes**

- (u) Splitting a terminal pane starts a new shell (as today); showing the
  same terminal in two panes is "show buffer". Splitting a PDF pane shows the
  same document.
- (u) Closing the last pane while hidden buffers exist shows the most
  recently shown hidden buffer in it; with no buffers the session ends.
  **Superseded (the user, 2026-10-26):** closing the last shown pane ends
  the session even while buffers are hidden, as if every buffer were Vim's
  `buftype=nofile`. The old rule was implemented in `63ad216` (`last_hidden`
  and the hidden-at clock in `crates/temzdf-server/src/session.rs`) and
  removed in `57ec52f`; that commit's parent has it, should it come back
  (for example as an option).
- Buffer ids are unique within the session and never reused. Closing a pane
  hides its buffer, except that a shown terminal whose program exits takes
  pane and buffer along (`remain-on-exit` keeps the pane); a terminal shown
  nowhere that exits stays as a finished terminal.
- Commands `buffers()`, `showBuffer(id, pane?)` (`:b`), `edit(path)` (`:e`
  for a PDF), `deleteBuffer(id)`; events `BufferCreated(id, kind)`,
  `BufferDeleted(id)`.
- A terminal's scrollback position is per buffer for now (libghostty keeps
  one viewport). Hidden PDFs drop tiles and display lists but keep their
  information and watcher. `temzdf ls` shows the number of buffers.
- Found while implementing (2026-10-26, without the user):
  - Panes and buffers take ids from one counter, so a document's buffer id
    is its document id and no id names both a pane and a buffer.
  - (Until `57ec52f`, see above.) "Most recently shown" is the time a
    buffer was last hidden. The last pane rule skipped the buffer being closed, so with two buffers, closing
    the last pane alternates between them; `killSession` (mapped to `q` in
    the `normal` mode, like the escape keymap's `<C-b>q`) ends the session,
    and `deleteBuffer` removes one.
  - `deleteBuffer` closes the buffer's panes without `PaneClosePre` hooks
    (the buffer goes either way); `PaneClosed` still fires.
  - A hidden terminal that ended goes when it was shown and hidden again.
  - A view smaller than its terminal shows the bottom left part (the prompt
    and cursor are usually at the bottom); mouse reports are shifted to
    match.
  - The view state saved on hiding: the page and the page-space y at the
    top, the doc-space x, and the zoom relative to fitting the width
    (`None` while fitting).

**Terminal panes**

- (u) A mouse selection is copied when the button is released (OSC 52 to the
  client).
- Selecting: drag when the program does not track the mouse, Shift-drag when
  it does, plain drag in the `normal` mode; double click a word, triple click
  a line; dragging past the edge scrolls the history; a click or typing
  clears it.
- The scrollback indicator `[12/340]` (lines above the bottom / lines of
  history) shows inverted at the pane's top right while scrolled back.
- kitty graphics from programs: libghostty's direct placements become pane
  layers (offset, clipped, z kept in order within the session's range),
  images in the session's store; a PNG decoder (the `png` crate) is given to
  libghostty; file, temp-file and shared-memory transfers are allowed as in
  kitty; Unicode-placeholder placements come later.
- OSC 8 links open with `linkopener` on a click (Shift-click when the
  program tracks the mouse), with the pointer shape changing over them.
- Found while implementing (2026-10-26, without the user):
  - Selection is libghostty's gesture state machine as is: the cell under
    the pointer joins once the pointer passes its middle (as in Ghostty);
    clicks within 500 ms and one cell count as double and triple clicks.
  - A link opens only on a plain single click; dragging from it selects.
  - Dragging past an edge scrolls one row per 50 ms.
  - Program images: kitty `z` values are kept as they are, raised above the
    session's background and border layers. Images leave the session's
    store when no placement shows them (shown again, they are uploaded
    again), and when the terminal is hidden or deleted.
- Pane sizes on a screen resize (18) are implemented: a split keeps the size
  its first half got when it was made (half the area), and the last pane
  takes the difference.
- Changing a pane's size (decided 2026-10-26, implementing):
  - Dragging a border with the left button resizes the panes on both sides
    of it, in any mode. Borders belong to no program (decision 14).
  - The pointer shows a resize shape over a border (OSC 22 `ew-resize`,
    `ns-resize`).
  - While dragging, the panes show their new size at once. Terminal
    programs get the new size (their pty is resized) only on release, so
    shells and editors do not redraw at every step.
  - The command is `resize-pane`, in two forms:
    - `resizePane(dir, cells?)` moves one of the focused pane's borders
      `cells` cells (default 1, times a mapping's count) in that direction,
      as tmux's `resize-pane -L/-R/-U/-D` does. The border is the
      right (bottom) one if the pane has one, else the left (top) one.
    - `resizePane({ width?, height? })` sets the size of the focused pane.
  - Sizes are clamped so that every pane keeps the minimum size splits
    already use (10 columns, 4 rows). A move that cannot keep it does as
    much as fits (decision 17).
  - The default mappings are `H` `J` `K` `L` in the `normal` mode, with
    counts. `normal` stays active, so the keys can be repeated.
  - **Changed (the user, 2026-10-26):** sizes take Vim's `+` `-` `<` `>`
    (`resizePane("taller" | "shorter" | "narrower" | "wider")`), and
    `H` `J` `K` `L` move the pane to an edge of the screen as Vim's
    `<C-w>H` ... (`movePane`), with `r` / `R` rotating (`rotatePanes`);
    `dev` `84efed4`. `<C-w>x` (exchange) is left out: `x` closes the pane.

## Future: asynchronous mapped and hook functions

Not planned for the first version (mapped and hook functions are
synchronous, see "Execution model"), but the user may want it later. What was
worked out (2026-10-26):

**Idea.**

- Opt-in per mapping or hook (`map(mode, lhs, fn, { async: true })`); without
  it a returned Promise stays an error, so nobody stumbles into the pitfalls
  below by passing an `async` function by accident.
- The host runs its event loop at the top level until the Promise settles
  and then reports completion. Never a nested loop inside the call (a
  `syncAwait`): that runs other tasks' continuations in the middle of the
  call, which is why Node's `deasync` is considered harmful.
- Until completion the server holds user input (keys, mouse, other clients,
  and `temzdf action` from the CLI, which would otherwise interleave), as for
  a synchronous function. Background work goes on (documents loading,
  forward-search results, terminal output); otherwise awaiting a PDF to load
  inside a mapping would hang until the timeout. Hooks for events that happen
  meanwhile fire after the function finishes.
- `ctx.signal`, an `AbortSignal` aborted by `<C-c>` or the timeout, lets
  libraries that support it (e.g. `fetch`) cancel the outside work too.

**Pitfalls to design for.**

1. *Stale state after an `await`*: a pane may close (its program exited),
   a PDF may reload with another page count, a dispatched task may change
   the layout. Closed panes fail loudly; stale values (page counts, layout)
   silently mislead.
2. *Deferred hooks disagree with what the function sees*: a plugin that
   keeps a table of panes up to date in its "pane closed" hook still lists a
   closed pane while the function continues after its `await`.
3. *Interrupting cannot stop an `await`*: `<C-c>` or the timeout can only
   release the held input and reject further commands (context tokens); the
   continuation still runs later and its commands fail. The work inside the
   library goes on unless it honours `ctx.signal`.
4. *Forgotten `await`* (`lib().then(...)`) runs commands after completion,
   outside the held input; context tokens reject them.
5. *Waiting on what is held*: awaiting the next key (a `getchar()`) would wait
   forever, since keys are held: held input must be handed to the running
   function for that. Awaiting something another hook resolves deadlocks
   until the timeout, since hooks run one at a time.
6. *Long holds*: awaiting the network or a process holds input up to the
   timeout; temzdf looks frozen, and if errors flush typeahead as in Vim,
   everything typed meanwhile is lost.
7. *Drawing during the wait*: drawing shows the function's half-done state
   (a split pane not yet filled); not drawing freezes the screen. A policy
   (and perhaps a busy indicator) is needed.

Guarantee such functions would have: no user input or other client
interleaves; other tasks of the same host may run during an `await`, and
`atomic(fn)` protects what must not interleave with them.

## Open questions, to decide later

The user chose to settle the execution model first (2026-10-26); these stay
open until then; they keep the numbers of the first list (1-7 became
"Execution model"). Recommendations are Claude's, for the user to accept or
change.

### Modes and mappings

8. *Decided (2026-10-26)*: a mode has **properties**: its mappings, what
   unmapped keys do (`drop` or `passthrough`), and whether it takes counts
   (pass-through modes send digits to the pane). **`inherit: <mode>`** takes
   all of another mode's properties, which the mode can then override (like
   a prototype, or an Emacs keymap's parent). Consequences: the mode and its
   ancestors form one table for key sequences, so ambiguity is resolved as in
   Vim (a child's `g` and a parent's `gg` wait for the timeout); a parent's
   mapping runs without changing the mode; cycles and unknown parents are
   errors, checked once the configuration is loaded. Inheritance is **live**:
   keys look up the parent when pressed, so mappings added to the parent
   later count, whatever the order of the configuration. A child can
   **unmap** an inherited key (a marker hiding the parent's mapping, as Vim's
   `:unmap` or zellij's `unbind`).

9. *Decided (2026-10-26)*: **a mode history.** `switch-mode` pushes a mode;
   `exit-current-mode` pops back to the previous one. The history is never
   cleared; it holds at most a configurable number of entries, dropping the
   oldest. A focus change rebuilds it from the new pane's default mode. The
   temzdf operating mode is always below it: `exit-current-mode` with an
   empty history returns there (and does nothing when already there). No
   "run in another mode" action is needed: mappings run actions, which work
   in any mode (sending keys to a terminal writes to its pty), so a
   tmux-like prefix is a multi-key mapping in the pass-through mode
   (`<C-s>x`) or a user mode entered with `switch-mode` whose mappings end
   with `exit-current-mode`. Not for now: a mode property "run one mapping,
   then exit the mode" (as a tmux key table, so that inherited mappings
   return too). A **`ModeChanged`** event (old and new mode) is to be added,
   so scripts can show the current mode.

10. *Decided (2026-10-26)*: **escape hatches**: (1) CLI commands keep working
    without scripts (`temzdf action -s NAME detach`, `temzdf kill-session
    NAME`); (2) a minimal Rust keymap active only when the script host is
    down or the configuration failed: detach, quit, and restarting the host
    (reloading the configuration); when the host goes down, the error window
    (11) says so and lists those keys; (3) `temzdf --clean` (implemented).
    Needed once the Rust keymaps go away (with modes and mappings).

11. *Decided (2026-10-26)*: errors show in a **floating window near the top
    right**, as WezTerm does; the command line is a floating window too
    (eventually something like Neovim's `ui.input`).

12. *Decided (2026-10-26)*: feedkeys comes in three flavours (as Vim):
    through mappings (`feedkeys()`, tmux `send-keys -K`), into temzdf's key
    handling without mappings (`feedkeys(..., 'n')`), and straight to a
    terminal buffer (`chansend()`/`term_sendkeys()`, tmux `send-keys`;
    `cmd.sendText`). Vim's `'x'` (process now) cannot be exact across
    processes; the call returning once the keys have been processed comes
    close.

### Mouse

13. *Decided (2026-10-26)*: the escape-hatch menu (e.g. on a right click) is
    built from two internal pane kinds to come: **non-focusable panes**
    (programs change what they show, the focus never goes there) and
    **floating panes** (proposal: `doc/floating-panes.md`).

14. *Decided (2026-10-26)*: a program that tracks the mouse inside a
    terminal pane gets it; with Shift held temzdf handles the mouse
    (selection, copy, scrollback, URL clicks: the usual terminal convention).
    In the operating mode temzdf handles the mouse without Shift, as for keys.
    A click on an unfocused pane only focuses it and does not reach the
    program (implemented). The built-in mouse duties are: focusing
    panes; the session menu, closing floating windows, later dragging borders
    and tabs; in PDF panes pixel and kinetic scrolling, selection, link hover
    and click, inverse search, the outline, pointer shapes; in terminal panes
    scrollback and, later, selection, copy and OSC 8 links.

15. *Decided (2026-10-26)*: built-in mouse actions raise **semantic events**
    (inverse search requested, link about to be opened) whose default action
    scripts can replace; the mouse itself stays built in.

### Hooks and defaults

16. *Decided (2026-10-26)*: **no general cancellation** (`preventDefault`):
    what must not be stoppable stays so, and behaviour that users want to
    change gets options (or the replacement points of 15).

### Layout

17. *Decided (2026-10-26)*: a requested size larger than the space is
    **rounded down to what fits**. (No room at all is an error.)

18. *Decided (2026-10-26)*: when the window size changes, **the other panes
    keep their sizes and the last (right/bottom) pane takes the change**, as
    Vim does (as far as remembered; when shrinking below the minimum, earlier
    panes give up space too). Besides keeping a sidebar-like pane's width, it
    redraws less: only the last pane re-renders (PDF zoom, terminal reflow and
    its program's redraw), the others stay as they are. Implemented
    2026-10-26.

### Pane contents

19. *Decided (2026-10-26)*: with **no pane showing the document**, forward
    search opens one (as `:help`). Ties among candidates (same depth): left/top
    first, unless the user objects.

### Vim's buffer model

20. *Decided (2026-10-26)*: **hidden buffers as in Vim**: closing a pane does
    not end what it shows (a terminal keeps running), so it can be shown
    again. A way to list buffers (Vim's `:ls`, `getbufinfo()`) is needed.

21. *Decided (2026-10-26)*: **a pane can show another buffer** (`:b`, `:e`),
    which shows hidden buffers again (20). It needs buffers (documents,
    terminals) separated from panes (views); today a terminal pane owns its
    pty and terminal. Runtime cost is small: hidden terminals keep reading
    output but are not drawn, hidden PDFs can drop their tiles, showing a
    terminal again resizes its pty to the pane once. When the program of a
    terminal shown in no pane exits, **the buffer stays** (a finished
    terminal, as in Vim), decided 2026-10-26. A terminal buffer shown in
    several panes is sized **as Neovim does** (decided 2026-10-26; read in
    `terminal_check_size()`, `src/nvim/terminal.c`, neovim master `80ae7e3`):
    the largest width and the largest height among all panes showing it (in
    all tabs, later), taken separately; the focus plays no part, so switching
    between those panes does not resize it; smaller panes show part of it;
    with no pane showing it (or a zero-sized one) the size stays. Neovim
    checks again when windows resize, buffers are entered or left, windows
    or tabs close, and on redraw. A buffer **remembers its last view** (as
    Vim's `'"` mark) and gets it back when shown again (decided 2026-10-26):
    a PDF saves its page, the position within the page as a fraction, and its
    zoom (fit-width or relative to the pane width, since the new pane's size
    may differ); a terminal just returns to the bottom. When the program of a terminal shown in a pane exits, the pane
    closes and **the buffer goes with it**; `remain-on-exit` keeps the pane
    (and so the buffer) instead (decided 2026-10-26).

### Configuration files

22. **Client-side settings** of the TOML file (`[terminal]` backend, medium and
    zlib level; `log_dir`) are needed before a client talks to the server, and
    the server's log file must be known when it is started (its stdout and
    stderr are connected then, before the configuration runs). There are two
    logs because two processes log: the session server (`NAME.log`:
    documents, terminals, scripts' `console.log`) and the client
    (`temzdf.log`, which in standalone mode holds the session too): the
    client redirects its stdout and stderr while it draws, and only it sees
    its terminal detection (the `geometry ...` line, read by
    `scripts/e2e.sh`), the output of effects it runs in the user's
    environment (link opener, inverse-search editor) and its own errors
    (before connecting, failed connections). Recommended: environment
    variables and command-line options only (`TERMPIX_BACKEND`,
    `TERMPIX_MEDIUM`, `TEMZDF_LOG_DIR` exist; perhaps `--log-dir`); the rest
    (`[scroll]`, `[links]`, `[synctex]`, `[reload]`, `[keys]`) moves to the
    script configuration. *Decided (2026-10-26)*: environment variables and
    options only, for now. Choosing the image backend and medium from the
    script configuration would be possible (the client sends what it detected
    in its Hello, the server answers with the choice before the client sets
    up drawing: one more round trip at attach), and can be added later if
    wanted; the overrides stay either way, for broken configurations and
    debugging. The log directory cannot move there (the server's log is
    connected when it starts). Client-side scripting, if ever needed, would be
    a separate client-only file (say `client.ts`, a function from detected
    capabilities to client settings, evaluated with a short time limit before
    connecting, falling back to detection), as Neovim GUIs read `ginit.vim`;
    not branches in `config.ts`, whose top level (registrations, dispatched
    tasks, imported plugins) would then run in both processes.

23. *Decided (2026-10-26)*: **a broken configuration** is reported and temzdf
    starts without it (implemented); once defaults are JavaScript, with the
    shipped defaults.

24. *Decided (2026-10-26)*: **reload** starts a new script host: the
    script's global state is discarded (implemented).

25. *Decided (2026-10-26)*: `console.log` goes to the session log and errors
    point at TypeScript source positions (implemented); easy to change later.

26. *Decided (2026-10-26)*: the configuration can `import` relative paths
    (resolved from the importing file) and `"temzdf"`; no npm packages, no
    Node APIs (as implemented).

### CLI, safety, tests

27. *Decided (2026-10-26)*: **`temzdf eval -s NAME 'js'`** (results as JSON,
    errors as exit codes), implemented last (after the GUI client is fine).
    SyncTeX forward search (`temzdf --forward`) stays built in, without
    scripts.

28. *Decided (2026-10-26)*: **no configuration is loaded automatically from
    project directories** (Vim's `exrc` is off by default too).

29. *Decided (2026-10-26)*: **no `temzdf check-config` for now.** Static
    checks are what tsc gives with the type definitions; what only temzdf can
    do is a dry-run load (syntax type stripping rejects, top-level `await`,
    registrations only wrong at run time, such as unknown or cyclic parent
    modes), and starting temzdf reports those too; the shipped defaults are
    loaded by the tests anyway. Can come later as a dry-run load.

## Prior art: modes and keys in tmux and zellij

Read from the sources (tmux `acee541`, zellij 0.45.1 `efd8fd5`, 2026-10-26),
not tried out.

| | tmux | zellij |
|---|---|---|
| Mode (key table) state | Per client (`struct client.keytable`). The prefix is a switch to the `prefix` table | Per client (`current_input_modes: HashMap<ClientId, InputMode>`) |
| Modes attached to panes | Copy mode and friends are per pane, shared by every client viewing it; any client's keys drive it | None |
| Focus (active pane) | Shared by the session | Per client |
| Unmapped keys | Root table: to the pane. Other tables: back to the default table and retried; still unbound after a switch: dropped (prefix + unbound key is swallowed) | Default mode (e.g. Locked): written to the pane. Rename/search: text input. Other modes: dropped, mode kept |
| Key sequences | None; tables are chained (`switch-client -T`). Timeouts only for `bind -r` (`repeat-time`) and `prefix-timeout`; `escape-time` is byte-level | None; single keys, no timeouts |
| User-defined modes | Any (`bind -T name`, `switch-client -T name`) | No (fixed enum; the "Tmux" mode is plain configuration) |
| feedkeys | `send-keys -K -c client` runs the keys through that client's bindings; plain `send-keys` goes to the pane (or its mode); `send-prefix` | `Write`/`WriteChars` to the pane only |
| Target of CLI commands | The pane the command runs in (`$TMUX_PANE`), else the most recently active client | The last client that sent a key |
| What a binding runs | A command list; conditions with `if-shell -F` (vim-tmux-navigator passes keys with an explicit `send-keys`) | A list of actions |
| Command prompt | Per client | A mode |
| Focus events | `client-focus-in/out` hooks; focus events cannot be bound | - |
| Watch-only clients | `attach -r` | Watcher clients, whose keys are ignored |

Takeaways for temzdf, given the decisions above:

- Both keep modes per client; temzdf instead keeps them per session because
  only one client operates at a time.
- tmux shows how state of a pane (copy mode) is shared while keys are
  interpreted elsewhere; temzdf's pane state (scroll, zoom, a terminal's
  scrollback position) is shared the same way.
- Neither has multi-key sequences; temzdf takes Vim's rules for those.
- tmux's prefix is Neovim's `<C-\><C-o>` (one Normal-mode command, then back to
  Terminal mode) and Vim's Normal mode is `<C-\><C-n>`; with mappings that
  run several actions both are just configurations.
- Like tmux's `$TMUX_PANE`, giving programs in terminal panes a
  `TEMZDF_PANE` (beside `TEMZDF_SESSION_NAME`) would let `temzdf action` run
  inside a pane act on that pane rather than guess from the operator.
