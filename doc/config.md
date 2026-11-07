# Configuration

temzdf is configured with a JavaScript or TypeScript program, as Vim is with
a vimrc: `$XDG_CONFIG_HOME/temzdf/config.ts` or `config.js` (falling back to
`~/.config/temzdf/`; both at once is an error), or the file given with
`--config FILE`. It runs in the script host, isolated from the session (WebAssembly)
(`doc/design/script-host.md` has the execution model). The shipped defaults
(`crates/temzdf-script/runtime/defaults.ts`) run first; the configuration can
map over or remove anything in them. `--clean` runs only the defaults.

A broken configuration (a syntax error, an exception, a top-level `await`,
taking longer than 5 s) is reported in a floating message and the log, and
temzdf runs with what was registered before the problem (at least the
defaults).

```ts
import { action, cmd, defineMode, keymap, o, on } from "temzdf";

o.timeoutlen = 500;
o.synctexeditor = "nvim --server /tmp/nvim.sock --remote-send ':e %{input}<CR>%{line}G'";

// Page view: J scrolls down (a count still multiplies).
keymap.add("pdf", "J", action.scrollDown(), { desc: "scroll" });
// A window submode entered from the normal mode; the normal mode's keys
// work in it too.
defineMode("window", { transient: true, keepalive: "always" });
keymap.add("normal", "w", action.switchMode("window"));
keymap.add("window", "v", [action.split("right"), action.exitMode()]);
// A script function (synchronous; dispatch() for asynchronous work).
keymap.add("terminal", "<C-b>m", (ctx) => cmd.sendText(`make -j${ctx.count ?? 4}\n`));
// Only in floating terminal panes.
keymap.add("terminal", "<C-b>q", action.toggleFloating(), { when: (ctx) => ctx.floating });
keymap.remove("normal", "x", { namespace: "default" });
on("ModeChanged", (old, now) => console.log(`${old} -> ${now}`));
```

`temzdf print-types > temzdf.d.ts` writes the API's TypeScript declarations
(generated from the command registry, so always current); point the editor's
TypeScript at it (`"paths": { "temzdf": ["./temzdf.d.ts"] }`). TypeScript is
type-stripped, so only erasable syntax works (no `enum`, no `namespace` with
values); errors report positions in the source as written. Imports: relative
paths and `"temzdf"`; no npm packages or Node APIs. Relative imports resolve
as TypeScript's do: `./util` finds `util.ts` or `util.js`, `./util.js` finds
`util.ts`, a directory its `index.ts` or `index.js`. `console.log` goes to the
log, as do promise rejections nothing handled.

## Modes and keys

Like Vim, temzdf is always in a mode; the window's status line (the bottom
row, `showstatusline`) names it at its left, with the session's name (and,
with several tab pages, the current one's number) at its right, as
Neovim's global status line and tmux's status line do. What it says is
built in for now. Built in:

| Mode | Unmapped keys | Counts | |
|---|---|---|---|
| `normal` | dropped | yes | operating mode: panes and the session |
| `terminal` | to the pane | no | terminal panes (pass-through, like Neovim's Terminal mode) |
| `pdf` | dropped | yes | PDF panes |
| `input` | typed characters go in | no | input panes (the search's line) |
| `visual` | dropped | yes | a terminal's output selected and copied by keys (see "The visual mode") |

Focusing a pane puts temzdf in that pane's mode. `switchMode` enters a
mode on the mode history (at most `modehistory` entries), `exitMode` returns
to the previous one, and below the history there is always `normal`. The
design is in doc/design/keymaps.md.

Mappings go from a key sequence to an action, an array of actions (run in
order by temzdf itself, without the script host, so they are fast and cannot
race the next key), or a synchronous function. `action.<command>(...)`
describes a command as data; `cmd.<command>(...)` runs one now. Sequences
work as in Vim: a key that starts a longer mapping waits `timeoutlen`
milliseconds (unless the shorter mapping has `nowait`); in a pass-through
mode keys that time out go to the pane.

`keymap.add(mode, lhs, rhs, { nowait, desc, when, namespace, priority })`:

- **`when`**: a condition, `(ctx) => boolean`: the mapping exists only
  while it holds; otherwise the key is looked up as if it were not there
  (another mapping, else the mode's `fallback`). `ctx` is the context of
  the sequence's first key: `pane`, `paneKind` (`"terminal"`, `"pdf"`),
  `floating`, `mode`, `keys` (typed before). Conditions run in the script
  host, synchronously, and may only read (`cmd.panes()`, `o.x`, ...); one
  that throws counts as false. Only keys some conditional mapping could
  take ask the host. Use conditions for what happens to you (a pane's
  kind, a state of the program), modes for what you enter on purpose.
- **Several mappings of a key** are tried by `priority` (higher first),
  then the last registered first, so the configuration's come before the
  shipped defaults; the first whose condition holds runs. A disabled
  longer mapping does not make a key wait.
- **`namespace`**: mapping the same mode, keys and namespace again
  replaces that mapping; without a namespace mappings are added. The
  shipped defaults are in the namespace `"default"`: `keymap.remove(mode,
  lhs, { namespace: "default" })` removes one, `keymap.remove(mode, lhs)`
  every mapping of the keys.
- `keymap.clear({ mode, namespace })` removes every mapping of the mode and
  the namespace (as Vim's `:mapclear`); each one left out means all of
  them: `keymap.clear({ namespace: "default" })` removes the shipped
  defaults, `keymap.clear()` every mapping.
- `keymap.get({ mode, namespace, lhs })` lists the mappings of the mode,
  the namespace and the keys (each one left out means all of them) as
  `keymap.add` takes them (functions included): to copy some into another
  mode, or to show them. It is a snapshot.

`defineMode(name, { fallback, count, transient, keepalive, timeout })`
makes a mode (or changes one). Properties left out are the mode below's
(then `drop` and no counts). **`fallback`** says what keys no mapping
takes do: `"drop"`, `"passthrough"` (to the focused pane: typed into a
terminal, an input or a text pane), or a function `(ctx, key) => void`
given the key (`key.keys` in notation, `key.text` what it types or
null): reading one key (`f{char}`) is a transient mode with a fallback
function (a transient mode's own fallback function takes the keys it
does not map instead of leaving, whatever its `keepalive`), and leaving a
mode on a key it does not know is `fallback: () => cmd.exitMode()`. A **transient** mode (a submode, as
kana/vim-submode or Emacs' transient keymaps) sits over the mode it was
entered from:

- `keepalive: "mapped"` (the default): it stays while its own keys are
  typed; another key leaves it and is handled again below.
- `keepalive: "always"`: it stays; keys it does not map go to the modes
  below it.
- `keepalive: (ctx) => boolean`: it stays while the condition holds.
- `timeout`: it is left after that many milliseconds without keys.

A focus change leaves submodes, as it leaves the `normal` mode.

```ts
defineMode("resize", { transient: true, timeout: 1500 });
keymap.add("resize", "+", action.resizePane("taller"));
keymap.add("resize", "-", action.resizePane("shorter"));
keymap.add("normal", "s", action.switchMode("resize")); // <C-b>s++-
```

Key notation follows Neovim: `j`, `G`, `%`, `<C-h>`, `<M-x>`, `<D-k>`,
`<S-Tab>`, `<CR>`, `<Esc>`, `<Space>`, `<BS>`, `<Tab>`, `<lt>`, `<Bar>`,
`<Bslash>`, `<F1>`..`<F35>`, `<Up>`, `<PageDown>`, ... Keys the kitty keyboard
protocol tells apart can be mapped apart: `<S-a>` (Shift and a) and `A`
(Caps Lock), `<C-i>` and `<Tab>`, `<C-m>` and `<CR>`, `<C-[>` and `<Esc>`,
`<C-h>` and `<BS>`. A typed key matches its own form first and then the form
legacy terminals send, so mapping `A` or `<Tab>` covers both everywhere.
`<C-c>` cannot be mapped: it interrupts a running script function and
cancels pending keys and counts.

`feedkeys(keys, { remap })` feeds keys as if typed (after the running
mapping); `cmd.sendText(text)` writes straight to the terminal pane.

### Default mappings

| Mode | Keys | Action |
|---|---|---|
| `terminal`, `pdf` | `<C-b>` | enter the `normal` mode (the prefix) |
| `terminal` | `<C-S-v>`, `<D-v>` | paste the clipboard (GUI clients; a terminal client's terminal pastes with its own keys) |
| `normal` | `%` / `"` | split right / below (the same document, or a new shell) |
| `normal` | `h` `j` `k` `l`, arrows | move the focus (back to that pane's mode) |
| `normal` | `+` `-` `<` `>` | the pane taller / shorter / narrower / wider (counts; as Vim's `<C-w>+` ...) |
| `normal` | `=` | every tiled pane the same size (Vim's `<C-w>=`) |
| `normal` | `\|` / `_` | the pane as wide / as high as the others allow; `{N}\|` N cells wide (Vim's `<C-w>\|` / `<C-w>_`) |
| `normal` | `H` `J` `K` `L` | move the pane to the left / bottom / top / right edge, the full height or width there (Vim's `<C-w>H` ...) |
| `normal` | `r` / `R` | rotate the panes in the pane's row or column (Vim's `<C-w>r` / `<C-w>R`; counts) |
| `normal` | `o` | next pane |
| `normal` | `w` | show or hide the floating panes (with none, open a floating shell) |
| `normal` | `W` | open a floating shell |
| `normal` | `e` | turn the pane floating, or tiled again |
| `normal` | `x` | close the pane (its buffer stays, hidden) |
| `normal` | `gt` / `gT` | next / previous tab page; `{N}gt` goes to tab page N, `{N}gT` N back (Vim) |
| `normal` | `T` | move the pane to a new tab page (Vim's `<C-w>T`) |
| `normal` | `d` | detach (`temzdf -s`) |
| `normal` | `q` | end the session |
| `normal` | `<C-b>` | send `<C-b>` to the pane |
| `normal` | `<Esc>` | back to the pane's mode |
| `normal` | `[` `v` | the `visual` mode (a terminal pane; so `<C-b>[` from it, as tmux's copy mode) |
| `pdf` | `q` | close the pane (see Buffers) |
| `pdf` | `<Esc>` | clear the selection, else the search, else quit |
| `pdf` | `j` `k` / `<Down>` `<Up>` | scroll (counts) |
| `pdf` | `h` `l` / `<Left>` `<Right>` | scroll sideways (counts) |
| `pdf` | `<Space>` `<PageDown>` `<C-f>` / `b` `<PageUp>` | page down / up (counts) |
| `pdf` | `d` / `u` | half a page down / up (counts) |
| `pdf` | `g` `<Home>` / `G` `<End>` | first page / end (or page N with a count) |
| `pdf` | `<C-o>` `<BS>` `<M-Left>` / `<M-Right>` | link history back / forward (counts) |
| `pdf` | `+` `=` / `-` / `0` `w` | zoom in / out (counts) / fit width |
| `pdf` | `/` | search: an input line below the pane (typing shows the first hit; `<CR>` searches, `<Esc>` cancels) |
| `pdf` | `n` / `N` | next / previous match of the search (`searchFor`; counts) |
| `pdf` | `y` | copy the selection |
| `pdf` | `<Tab>` | outline |
| `input` | `<BS>` `<C-h>` / `<Del>` `<C-d>` | delete the character before / after the caret |
| `input` | `<Left>` / `<Right>` / `<Home>` `<C-a>` / `<End>` `<C-e>` | caret: a character left / right, the start / end |
| `input` | `<M-b>` `<C-Left>` / `<M-f>` `<C-Right>` | caret: a word left / right |
| `input` | `<C-w>` / `<C-u>` / `<C-k>` | delete the word before the caret / to the start / to the end |
| `input` | `<CR>` / `<Esc>` | a search: search / cancel (the input closes) |
| `input` | `<C-b>` | the normal mode |
| `visual` | `h` `j` `k` `l`, arrows | a character / row (counts) |
| `visual` | `0` `<Home>` / `^` / `$` `<End>` | the first column / the first character / the last character of the row |
| `visual` | `w` `b` `e` / `W` `B` `E` | words / blank-separated words (counts) |
| `visual` | `gg` / `G` | the first row of the scrollback / the last row with text (row N with a count) |
| `visual` | `H` `M` `L` | the top / middle / bottom row shown |
| `visual` | `<C-u>` `<C-d>` / `<PageUp>` / `<C-f>` `<PageDown>` | half a screen up / down, a screen up / down (the view with it) |
| `visual` | `<C-y>` / `<C-e>` | the view a row up / down |
| `visual` | `[[` / `]]` | the shell prompt above / below (shells that mark prompts, OSC 133) |
| `visual` | `v` / `V` / `<C-v>` | select characters / rows / a block from the cursor; the same key again stops selecting |
| `visual` | `o` | the cursor to the other end of the selection |
| `visual` | `y` / `Y` | copy the selection / the selected rows (else the cursor's, counts) and leave |
| `visual` | `/` / `?` / `n` `N` | search the scrollback forwards / backwards (an input line, as the PDF search) / next, previous match |
| `visual` | `<Esc>` / `q` | drop the selection, else leave / leave |
| `visual` | `<C-b>` | the normal mode (`<Esc>` there comes back) |

Inside tmux, `<C-b>` is tmux's own prefix: map another key for temzdf (or
press it twice there).

While a PDF pane's outline has the focus, keys go to it directly (fixed
keys). The search (`/`) is the shipped configuration's: a function mapped
in `defaults.ts` opens an input buffer (`newInput`) in a one-row pane split
below the PDF pane (`split({ height: 1 })`, its status line off), an
`InputChanged` hook previews the query (`searchPreview`), `<CR>` and
`<Esc>` in the `input` mode search or cancel and delete the input buffer;
closed any other way (`BufferHidden`), the search is cancelled. A
configuration can replace it with its own (`keymap.add("pdf", "/", ...)`).
Mouse handling is built in. Dragging a
border between panes resizes them, in any mode (the pointer shows a resize
shape over borders); programs in terminal panes get their new size when the
button is released. Panes stay at least 10 columns by 4 rows. In terminal
panes the left button selects (drag; double click a word, triple click a
line) and releasing copies to the clipboard (OSC 52 through the client's
terminal); where the program uses the mouse, hold Shift, or select from the
`normal` mode. A click on a link a program printed (OSC 8) opens it with
`linkopener`.

### The visual mode

Keys select and copy a terminal's output, its scrollback included (Vim's
Visual mode on a terminal buffer, tmux's copy mode). `<C-b>[` (or `v` in
the `normal` mode) enters it in the focused terminal pane: a cursor of its
own at the program's cursor, nothing selected; the window's status line says
`VISUAL` (`VISUAL CHAR`, `VISUAL LINE`, `VISUAL BLOCK` with a selection), and
the pane's top right how far the view is back. `v`, `V`
and `<C-v>` start a selection of characters, rows or a block; `y` copies it
(OSC 52, as the mouse's: soft-wrapped rows joined, trailing blanks dropped,
no line break after the last row) and leaves. Keys do not reach the
program meanwhile; its output goes on, and the cursor and the selection stay
on their characters. Leaving returns the view to the live screen.

Searching (`/`, `?`) is plain text, case-insensitive, matches shown black on
yellow; a match across a soft wrap is not found. While the search's input
line has the focus the visual mode waits, and `<CR>` or `<Esc>` there comes
back to it; so does `<Esc>` from the `normal` mode entered with `<C-b>`.
Focusing another pane ends nothing either: the selection stays until keys
reach that terminal again. In the visual mode the mouse is its own: a click
moves the cursor, a drag selects characters (double click a word, triple
click a row) without copying, the wheel scrolls with the cursor kept in
view. `switchMode("visual")` refuses panes that are not terminals.

When the script host is down (it exited, or the configuration never finished
loading), mappings are off and a fixed keymap works: `<C-b>d` detaches,
`<C-b>q` ends the session, `<C-b>r` restarts the host; other keys go to the
pane. A floating message says so.

## Options

Set with `o.<name> = value` (read with `o.<name>`); they apply at once.

| Option | Default | |
|---|---|---|
| `timeoutlen` | `1000` | milliseconds a key that starts a longer mapping waits |
| `splitright` | `true` | `split()` without a side goes right (else left) |
| `splitbelow` | `true` | splits towards the bottom go below (else above) |
| `modehistory` | `100` | modes the history keeps |
| `scrollstep` | `60` | pixels per wheel step, before flick acceleration |
| `scrollsmoothness` | `60` | easing time constant in milliseconds (smaller is snappier) |
| `linkopener` | `null` | command opening external links, the URI appended (`["firefox"]`); `null`: `xdg-open` / `open`. Runs in the client; a failure shows as a message. Only `http`, `https`, `mailto` and `ftp` links open, and `file` links to existing documents: regular files, not executable, not of a kind openers launch (`.desktop`, `.command`, scripts, ...): documents and programs are not trusted |
| `linkpreview` | `true` | preview internal links (references, citations, the outline) when the pointer rests on one |
| `linkpreviewdelay` | `300` | milliseconds before the preview |
| `reloaddebounce` | `200` | milliseconds after the PDF last changed before reloading |
| `reloadanchor` | `"position"` | `"position"` keeps the page and place on it (as Skim, Okular); `"text"` finds the line read a third down the screen in the new PDF (falling back to the position) |
| `synctexeditor` | `null` | inverse search (Ctrl+click), run with `sh -c`; `%{input}` (shell-quoted path), `%{line}`, `%{column}`; see doc/design/synctex.md. `--editor` overrides it |
| `remainonexit` | `false` | a shown terminal whose program ended keeps its panes (tmux's `remain-on-exit`) |
| `showtabline` | `"auto"` | when the tab line shows (Vim's): `"never"`, `"auto"` (with two tab pages or more), `"always"` |
| `showstatusline` | `"always"` | whether the window's status line shows (its bottom row: the mode, the session, the tab page): `"always"`, `"never"` |
| `foreground`, `background` | `null` | default colours (`"#rrggbb"`) of GUI clients and terminal panes (programs asking with OSC 10/11 are told); `null`: the client's own. A terminal client's terminal keeps its own |
| `palette` | `[]` | the first colours of the 256-colour palette (`["#000000", "#cd0000", ...]`), for GUI clients and terminal panes (OSC 4); the rest stay the default |
| `guifont` | `[{ font: "JetBrains Mono", size: 14 }]` | the GUI client's fonts, `{ font, size? }` (size in points, 4 to 200; left out: the first font's, else 14). The first installed one sets the cell size; a character goes to the first font of the list that has it, at that font's size, then (as WezTerm) to the built-in JetBrains Mono, Noto Color Emoji and Symbols Nerd Font Mono, then the system's fonts for its script. A font that is not installed is skipped: `temzdf ls-fonts` shows what the list resolves to |
| `guifontwide` | `[]` | fonts tried first for wide (CJK) characters, as `guifont` |
| `basemode` | `{}` | the base mode (the bottom of the mode history) of panes of each kind by its name, e.g. `{ edit: "vim/normal" }`; a kind left out (or a mode not defined) has its own mode. Set while the configuration loads only. A text buffer made with `newBuffer({ basemode })` or `openFile(path, { basemode })` uses its own |
| `wrap` | `true` | text panes wrap long lines (else they scroll sideways) |
| `linebreak` | `false` | wrap after a blank rather than anywhere |
| `breakindent` | `false` | continuation rows start at the line's indent |
| `showbreak` | `""` | shown at the start of continuation rows |
| `tabstop` | `8` | display columns between tab stops |
| `number` | `"off"` | line numbers in text panes: `"off"`, `"absolute"`, `"relative"`, `"both"` (relative, the cursor's line absolute) |
| `cursorline` | `false` | highlight the cursor's line |
| `scrolloff` | `0` | rows kept between the cursor and the top or bottom of text panes |

## Buffers

What a pane shows is a buffer, as in Vim: a document or a terminal. Several
panes can show one buffer (a split PDF pane shows the same document; a split
terminal pane starts a new shell, `showBuffer` shows the same terminal).

- Closing a pane hides its buffer: a terminal keeps running, a document
  stays open (reloading as it changes) and keeps its position for the next
  pane that shows it. Closing the last pane ends the session, hidden
  buffers or not (as if every buffer were Vim's `buftype=nofile`); so does
  the last pane's program ending. Buffers belong to no tab page: any tab's
  panes can show them.
- When a shown terminal's program ends, its panes close (unless
  `remainonexit`); a hidden one stays, marked as ended, until it was shown
  and hidden again.
- `buffers()` lists them (`id`, `kind`, `name`, the `panes` showing it,
  `exited`); `showBuffer(id, pane?)` shows one in a pane (Vim's `:buffer`);
  `edit(path)` opens a PDF in the focused pane (`:edit`); `deleteBuffer(id?)`
  ends a terminal's program or closes a document and closes its panes.
- A terminal shown in several panes is as large as the largest (as in
  Neovim); smaller panes show its bottom left part.

For example, a mapping that shows the next hidden buffer:

```ts
keymap.add("normal", "n", () => {
  const hidden = cmd.buffers().filter((b) => b.panes.length === 0);
  if (hidden.length > 0) cmd.showBuffer(hidden[0].id);
});
```

## Floating panes

A floating pane shows a buffer, as any pane, in a frame over the tiled panes
(nothing below shows through it). `openFloat(what?, opts?)` opens one:
`what` is `{ buffer: id }`, `{ file: path }` or `{ command: [...] }` (a new
shell by default); `opts` take `width`, `height` (the content's, in cells,
or `"N%"` of the screen for the whole float), `col`, `row` (centred when
left out), `title` and `focus: false`. The default is a centred float of
80% by 80%. `temzdf action open-float FILE.pdf` and `temzdf action
open-float -- htop` work as `new-pane`. `tabpage` says where it shows, as
for Vim's popups: `0` (default) the tab page it opens on, `-1` every tab
page, `N` tab page N.

- Focusing a float raises it. `moveFocus` moves among the floats (or among
  the tiled panes, where the focus is); `focusNextPane` goes through the
  floats, then the tiled panes. A click on a tiled pane focuses it and the
  floats stay.
- `toggleFloats()` shows or hides them all; hidden, their programs keep
  running and the focus goes back to the tiled pane focused last. Each tab
  page shows or hides its own; floats on every tab page follow the current
  one's. The stacking is one for all tab pages.
- Splitting, or `newPane`, while a float has the focus opens another float,
  two cells right and down.
- `resizePane("right", 5)` widens a float (its right or bottom edge moves);
  `floatConfig(pane, opts)` moves or resizes one, or changes its title.
  With the mouse, its title edge moves it and its other edges and corners
  resize it.
- `toggleFloating(pane?)` turns a tiled pane floating and back (beside the
  tiled pane focused last; a float on every tab page becomes a pane of the
  current one); the last tiled pane cannot float.
- Closing a tab page's last tiled pane puts the topmost shown float of that
  tab page in its place (not one on every tab page); with none, the tab
  page closes (see Tab pages).
- A float's program exiting closes it (unless `remainonexit`).

## Tab pages

As in Vim, a tab page is a layout of panes with its own focus; buffers are
shared, and a terminal shown in several tab pages is as large as its
largest pane in any of them. All clients show the same tab page.

- With two tab pages or more (`showtabline`), the top row is the tab line:
  each tab page's number and name (`renameTab`) or focused buffer, the
  current one highlighted. A click there goes to that tab page; a middle
  click closes it.
- `newTab(what?)` opens a tab page after the current one (`what` as for
  `openFloat`; a shell by default; `temzdf action new-tab FILE.pdf` or
  `new-tab -- htop`), `closeTab(n?)` closes one with its panes (their
  buffers stay; the last tab page stays: `killSession` ends the session),
  `nextTab()`, `prevTab()` and `gotoTab(n)` go to one (counted from 1;
  `gt`, `gT`), `moveTab(to?)` moves the current one as Vim's `:tabmove`
  (after tab page `to`, `0` first, none last, or `"+1"` / `"-1"`),
  `breakPane(pane?)` moves a pane to a new tab page (`T`), `tabs()` lists
  them, `currentTab()` gives the current one's number.
- Closing a tab page's last pane (or its program ending) closes the tab
  page, unless a float of its own shows (it takes the place); the tab page
  after it becomes current. With the last tab page, the session ends.
- `panes()` gives each pane's `tab` (`null` for a float on every tab page).
- Hooks: `TabEntered(n)`, `TabClosed(n)`.

## Status lines

Each pane's bottom row is its status line, drawn by the session as text: a
PDF pane's says its file, the search, the page and the zoom, and its
messages (errors, "Copied ..."); a terminal's is blank. The shipped
configuration hides the terminals' (a `PaneOpened` hook calling
`cmd.statusLine(false, id)`); without the script host every pane has one.
A pane one row high shows its status line only. Changing what they say
comes later (`doc/design/floating-panes.md`).

## Input buffers

An input buffer is one line of text with a prompt, as Vim's command line,
shown in input panes. `newInput({ prompt: "/", text })` makes one and
returns it; a pane shows it like any buffer (`split({ buffer, height: 1
})`, `openFloat({ buffer })`, `showBuffer`). Focused, an input pane puts
the session in the `input` mode: keys no mapping takes that type a character
(no modifier but Shift), committed input-method text and pastes go in at
its caret (the terminal cursor, where input methods show their preedit);
the line scrolls to keep the caret in sight. Editing is commands in the
`input` namespace, for mappings: `input.backspace`, `input.delete`,
`input.left`, `input.right`, `input.home`, `input.end`, `input.wordLeft`,
`input.wordRight`, `input.deleteWord`, `input.deleteToStart`,
`input.deleteToEnd`, `input.insert(text)`. `inputText(buffer?)` and
`setInputText(text, buffer?)` read and set the text (the focused input
pane's buffer by default); the hook `InputChanged(buffer, text)` hears every
change of it. An input buffer stays when its last pane closes, as others do;
`BufferHidden(buffer, kind)` says so (delete it there if it is done with).
Panes showing one buffer share its text, each with its own caret.

## Text panes

A file opens as text unless its extension is `.pdf` (`temzdf FILE`,
`newPane`, `split({ file })`, ...; a file that does not exist yet opens
empty), or with `openFile(path, { basemode? })`, which returns the pane.
`newBuffer({ text?, path?, basemode? })` makes a text buffer without a
pane (returns it; show it as any buffer). Files are UTF-8 (a BOM kept);
binary files and others are refused. Line endings stay as read (CRLF
files are LF inside), and `write({ path?, force?, buffer? })` writes the
focused pane's buffer to a temporary file renamed over the file (its
permissions kept, a symlink's target written); a file changed on disk
since it was read needs `force`.

Focused, a text pane puts the session in its base mode, `edit` unless
`o.basemode` or the buffer says otherwise (see Options): pass-through,
so typed text, input-method text and pastes go in at the cursor,
replacing the selection. The shipped mappings edit without modes: arrows,
`<Home>`, `<End>`, `<C-Left>`/`<C-Right>` (words), `<C-Home>`/`<C-End>`,
`<PageUp>`/`<PageDown>` move, with Shift they select; `<CR>`, `<Tab>`,
`<BS>`, `<Del>`, `<C-BS>`/`<C-Del>` (words), `<Esc>` drops the selection,
`<C-s>` writes, `<C-z>`/`<C-y>` (or `<C-S-z>`) undo and redo, `<C-b>` is
the prefix. Unsaved changes are kept: deleting the buffer or ending the
session (`kill-session`, closing the last pane) is refused while a text
buffer has them (the shipped `BufferDeletePre` and `SessionEndPre` hooks
cancel it, saying why); `temzdf kill-session --force` ends anyway. Closing
a pane is fine: its buffer stays, hidden (`buffers()` lists it,
`modified` true). A file changed elsewhere is read again when its buffer
has no unsaved changes (`FileChangedOnDisk`, `reload(buffer?)`: one
undoable step); otherwise a message says so, and writing it then needs
`write({ force: true })`. The mouse: a click puts the cursor, dragging selects, the
wheel scrolls.

Editors are scripts on the primitives in the `text` namespace (the
focused pane's unless `{ pane }` is given last). Positions are
`{ line, character }` (zero based, `character` in UTF-16 units, as
JavaScript strings index; out of range is clamped), ranges
`{ start, end }`:

- queries: `text.lineCount()`, `text.line(n)`, `text.getText(range?)`,
  `text.cursor()`, `text.selection()`, `text.offsetAt(pos)`,
  `text.positionAt(offset)`, `text.info()`, `text.visibleRange()`;
- edits: `text.edit([{ range, text }, ...])` (ranges in the text before
  the edit, not overlapping), `text.insert(text, at?)`,
  `text.delete(range)`, `text.replace(range, text)`, `text.type(text)`
  (as typed: replaces the selection, consecutive typing is one undo
  step), `text.erase(unit?, count?)` (the selection, else from the
  cursor);
- motions: `text.positionOf(from, unit, count?)` (where, without moving)
  and `text.move(unit, count?, { select? })`, by `"grapheme"`,
  `"wordStart"`, `"wordEnd"` (just after a word; words split at
  punctuation and between scripts, so unspaced Japanese is several
  words), `"lineStart"`, `"firstNonBlank"`, `"lineEnd"`, `"line"` (keeping
  the column), `"displayLine"`, `"page"`, `"documentStart"`,
  `"documentEnd"`; negative counts go backwards, and a mapping's count
  multiplies;
- the cursor and view: `text.setCursor(pos, { select? })`,
  `text.setSelection(anchor, active, kind?)` (`"char"`, `"line"`,
  `"block"`), `text.clearSelection()`, `text.setCursorStyle(style)`,
  `text.scroll(rows)`, `text.reveal(pos?, "minimal" | "center" | "top")`;
- undo: `text.undo()` and `text.redo()` (return the changed ranges; the
  cursor goes back where the step started), `text.undoBegin()` /
  `text.undoEnd()` (one step across mappings, e.g. a Vim Insert session),
  `text.undoBoundary()`. A step is one mapping (or key, paste, `temzdf
  action`), typing merged; moving the cursor ends a typing step.

Options for text panes: `wrap`, `linebreak`, `breakindent`, `showbreak`,
`tabstop`, `number`, `cursorline`, `scrolloff` (see Options). The status
line shows the file, `[+]` when modified, the mode, the encoding, the line
ending, the line and column and the position in the file.

## Commands

`cmd.*` and `action.*` share the command registry
(`crates/temzdf-server/src/session/commands.rs`; `temzdf print-types` lists
them with their signatures): the session's (`split`, `newPane`, `moveFocus`,
`focusNextPane`, `focusPane`, `resizePane`, `equalizePanes`, `maximizePane`, `movePane`, `rotatePanes`, `closePane`,
`openFloat`, `floatConfig`,
`toggleFloats`, `toggleFloating`, `newTab`, `closeTab`, `nextTab`,
`prevTab`, `gotoTab`, `moveTab`, `renameTab`, `breakPane`, `tabs`,
`currentTab`, `buffers`, `showBuffer`, `edit`,
`deleteBuffer`, `killSession`, `detach`, `reloadConfig`, `message`,
the visual mode's (`visual.left`, `visual.wordForward`, `visual.top`,
`visual.charwise`, `visual.yank`, `visual.escape`, ...: one for each key of
the table above, counts where it says so; `visual.searchFor(query, pane?,
backward?)`, `visual.searchPreview`, `visual.searchCancel(pane?)` take the
terminal's pane, as the PDF search does),
`statusLine` (shows or hides a pane's status line, its bottom row:
`statusLine(false)` hides the focused pane's, `statusLine(true, id)` shows
pane `id`'s, `statusLine()` turns it the other way; the content gets or
gives back the row; `temzdf action status-line off`), `sendText`, `paste` (the client's clipboard, or `"primary"`; GUI clients:
terminal clients paste with the terminal's keys), `fontSize` (GUI
clients' font: `fontSize(16)` sets the first font of `guifont` to 16 points
and scales the other sizes given with it, `fontSize("+1")` / `fontSize("-1")`
one point more or less for each, times a mapping's count, `fontSize()` back to the
configured sizes; terminal clients keep their terminal's font), `feedkeys`, `switchMode`, `exitMode`, `currentMode`, `panes`,
`focusedPane`, `sessionName`) and the page view's (`scrollDown`, `zoomIn`,
`nextMatch`, ...: every action of the PDF table above, taking the count where it says
"counts"), and the searches of a PDF pane (the focused one, else `pane`):
`searchFor(query, pane?)` searches for `query` (the place it starts from
goes on the back history, the first hit is shown once known; `""` clears
the search; `temzdf action search-for needle`); `searchPreview(query,
pane?)` shows the first hit as a query is typed (Vim's incsearch: from where
the preview began; nothing found or empty, back there), and `searchFor` of
it then keeps that hit and puts where the preview began on the back
history; `searchCancel(pane?)` drops the search, a preview's view back
where it began.
Commands named `ns.name` are in a namespace: `cmd.ns.name()` and
`action.ns.name()` (`temzdf action ns.name`), the name camelCased as the
others (`input.delete-word` is `cmd.input.deleteWord`).
`temzdf action NAME ARGS...` runs the same commands from a shell (`temzdf
action split right`). `resizePane("left", 5)` moves a border of the focused
pane five cells left (as tmux's `resize-pane -L 5`), `resizePane("wider",
5)` makes it five cells wider (as Vim's `5<C-w>>`), `resizePane({ width:
40 })` sets its size.
`split` returns the new pane and also takes an object: `split({ side:
"bottom", buffer: b, height: 1, pane: p })` splits pane `p` (the focused
one by default) and shows buffer `b` there (else what the split pane shows);
`width` (a split to the side) or `height` (up or down), in cells or `"N%"`
of the screen, makes the new pane that size and keeps it so (as Vim's
`winfixwidth` / `winfixheight`: resizing the screen and `equalizePanes`
leave it, `resizePane` and dragging its border change it; it may be smaller
than other panes can be). `focusPane(id)` focuses a pane, going to its tab
page.

## Terminals without graphics

In a terminal without the kitty graphics protocol (or inside tmux without
passthrough, or with an outer terminal tmux cannot pass images to, or with
`TERMPIX_GRAPHICS=off`) the client runs as text only: terminal panes, the
status lines, borders and messages work; a PDF pane shows "PDF viewer
disabled (no kitty graphics protocol support)" in its middle, and a
message at the start (until the next key) says why. When a pane gets
images later (a program in a terminal pane shows one, a PDF pane opens),
a message names it, once until its images go and come back, at most one
message every five seconds. Only that client is affected: a client in a
terminal with graphics attached to the same session sees everything.

## Environment

Settings a client needs before it talks to a session are environment
variables:

| Variable | |
|---|---|
| `TERMPIX_BACKEND` | `layered` or `composited`: how frames reach the terminal (default: detected) |
| `TERMPIX_MEDIUM` | `direct`, `shm` or `file`: how pixels are transferred (default: detected; `direct` mimics SSH) |
| `TERMPIX_GRAPHICS` | `off`: run as in a terminal without the kitty graphics protocol (text only; see below) |
| `TEMZDF_ZLIB_LEVEL` | zlib level (0-9) of inline transfers, default 2 (do not use 1 with Ghostty 1.3.1, see doc/terminals/terminal-compat.md) |
| `TEMZDF_LOG_DIR` | where logs go (default `$XDG_STATE_HOME/temzdf`): `temzdf.log` for viewers and clients, `NAME.log` for session NAME. Logs are the user's alone (0600; they name the files opened and the sessions' sockets); the directory is made 0700, and the default one is made so if it is not |
| `TEMZDF_PIXELS` | `shared` (default) or `socket`: how a client takes frames' pixels from the session, in shared memory or inside the messages (`doc/design/session-server.md`) |
| `TEMZDF_TIMING` | set: the session server and clients log how long frames take (`timing:` lines; `scripts/timing.sh`, `doc/dev/performance.md`) |
| `TEMZDF_GUI` | the `temzdf-gui` that `temzdf ls-fonts` runs (default: next to `temzdf`, else on `PATH`) |

`temzdf ls-fonts` shows the fonts GUI clients use, as `wezterm ls-fonts`
does: each font of `guifont` and `guifontwide` with its file (`<built-in>`
for the bundled ones) or `not found`, and the fallbacks after them. It
takes the lists of the configuration, of `-s NAME` (or of the session it
runs in, as changed there), or `--font NAME[:SIZE]` given (repeated;
`--font-wide` likewise). `--text TEXT` shows the font of each character of
TEXT, `--list-system` every font available. It needs `temzdf-gui`, which
has the fonts built in.
