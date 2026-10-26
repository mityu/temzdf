# Configuration

temzdf is configured with a JavaScript or TypeScript program, as Vim is with
a vimrc: `$XDG_CONFIG_HOME/temzdf/config.ts` or `config.js` (falling back to
`~/.config/temzdf/`; both at once is an error), or the file given with
`--config FILE`. It runs in the script host, a separate process
(`doc/script-host.md` has the execution model). The shipped defaults
(`crates/temzdf-script/runtime/defaults.ts`) run first; the configuration can
map over or unmap anything in them. `--clean` runs only the defaults.

A broken configuration (a syntax error, an exception, a top-level `await`,
taking longer than 5 s) is reported in a floating message and the log, and
temzdf runs with what was registered before the problem (at least the
defaults).

```ts
import { action, cmd, defineMode, map, o, on, unmap } from "temzdf";

o.timeoutlen = 500;
o.synctexeditor = "nvim --server /tmp/nvim.sock --remote-send ':e %{input}<CR>%{line}G'";

// Page view: J scrolls down (a count still multiplies).
map("pdf", "J", action.scrollDown(), { desc: "scroll" });
// A window mode entered from the normal mode.
defineMode("window", { inherit: "normal", unmapped: "drop" });
map("normal", "w", action.switchMode("window"));
map("window", "v", [action.split("right"), action.exitMode()]);
// A script function (synchronous; dispatch() for asynchronous work).
map("terminal", "<C-b>m", (ctx) => cmd.sendText(`make -j${ctx.count ?? 4}\n`));
unmap("normal", "x");
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

Like Vim, temzdf is always in a mode. Built in:

| Mode | Unmapped keys | Counts | |
|---|---|---|---|
| `normal` | dropped | yes | operating mode: panes and the session |
| `terminal` | to the pane | no | terminal panes (pass-through, like Neovim's Terminal mode) |
| `pdf` | dropped | yes | PDF panes |

Focusing a pane puts temzdf in that pane's mode. `defineMode(name, {
inherit, unmapped, count })` makes new modes (or changes built-in ones):
`inherit` takes another mode's mappings and properties, live, and the mode
overrides them; `unmap` hides an inherited key. `switchMode` enters a mode on
the mode history (at most `modehistory` entries), `exitMode` returns to the
previous one, and below the history there is always `normal`.

Mappings go from a key sequence to an action, an array of actions (run in
order by temzdf itself, without the script host, so they are fast and cannot
race the next key), or a synchronous function. `action.<command>(...)`
describes a command as data; `cmd.<command>(...)` runs one now. Sequences
work as in Vim: a key that starts a longer mapping waits `timeoutlen`
milliseconds (unless the shorter mapping has `nowait`); in a pass-through
mode keys that time out go to the pane.

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
| `normal` | `%` / `"` | split right / below (the same document, or a new shell) |
| `normal` | `h` `j` `k` `l`, arrows | move the focus (back to that pane's mode) |
| `normal` | `+` `-` `<` `>` | the pane taller / shorter / narrower / wider (counts; as Vim's `<C-w>+` ...) |
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
| `pdf` | `q` | close the pane (see Buffers) |
| `pdf` | `<Esc>` | clear the selection, else the search, else quit |
| `pdf` | `j` `k` / `<Down>` `<Up>` | scroll (counts) |
| `pdf` | `h` `l` / `<Left>` `<Right>` | scroll sideways |
| `pdf` | `<Space>` `<PageDown>` `<C-f>` / `b` `<PageUp>` | page down / up |
| `pdf` | `d` / `u` | half a page down / up |
| `pdf` | `g` `<Home>` / `G` `<End>` | first page / end (or page N with a count) |
| `pdf` | `<C-o>` `<BS>` `<M-Left>` / `<M-Right>` | link history back / forward |
| `pdf` | `+` `=` / `-` / `0` `w` | zoom in / out / fit width |
| `pdf` | `/` / `n` / `N` | search / next / previous match |
| `pdf` | `y` | copy the selection |
| `pdf` | `<Tab>` | outline |

Inside tmux, `<C-b>` is tmux's own prefix: map another key for temzdf (or
press it twice there).

While a PDF pane's search prompt is open or its outline has the focus, keys
go to them directly (fixed keys). Mouse handling is built in. Dragging a
border between panes resizes them, in any mode (the pointer shows a resize
shape over borders); programs in terminal panes get their new size when the
button is released. Panes stay at least 10 columns by 4 rows. In terminal
panes the left button selects (drag; double click a word, triple click a
line) and releasing copies to the clipboard (OSC 52 through the client's
terminal); where the program uses the mouse, hold Shift, or select from the
`normal` mode. A click on a link a program printed (OSC 8) opens it with
`linkopener`.

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
| `linkopener` | `null` | command opening external links, the URI appended (`["firefox"]`); `null`: `xdg-open` / `open`. Runs in the client; a failure shows as a message. Only `http`, `https`, `mailto`, `ftp` and `file` links open (documents and programs are not trusted) |
| `linkpreview` | `true` | preview internal links (references, citations, the outline) when the pointer rests on one |
| `linkpreviewdelay` | `300` | milliseconds before the preview |
| `reloaddebounce` | `200` | milliseconds after the PDF last changed before reloading |
| `reloadanchor` | `"position"` | `"position"` keeps the page and place on it (as Skim, Okular); `"text"` finds the line read a third down the screen in the new PDF (falling back to the position) |
| `synctexeditor` | `null` | inverse search (Ctrl+click), run with `sh -c`; `%{input}` (shell-quoted path), `%{line}`, `%{column}`; see doc/synctex.md. `--editor` overrides it |
| `remainonexit` | `false` | a shown terminal whose program ended keeps its panes (tmux's `remain-on-exit`) |
| `showtabline` | `1` | when the tab line shows (Vim's): `0` never, `1` with two tab pages or more, `2` always |
| `foreground`, `background` | `null` | default colours (`"#rrggbb"`) of GUI clients and terminal panes (programs asking with OSC 10/11 are told); `null`: the client's own. A terminal client's terminal keeps its own |
| `palette` | `[]` | the first colours of the 256-colour palette (`["#000000", "#cd0000", ...]`), for GUI clients and terminal panes (OSC 4); the rest stay the default |
| `guifont` | `"UDEV Gothic NF:h14"` | the GUI client's font, as Neovim's `guifont` (`Family:hSIZE`, points) |
| `guifontwide` | `null` | the GUI client's font for wide (CJK) characters; `null`: `guifont`, then fallbacks |

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
map("normal", "n", () => {
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

## Commands

`cmd.*` and `action.*` share the command registry
(`crates/temzdf-server/src/session/commands.rs`; `temzdf print-types` lists
them with their signatures): the session's (`split`, `newPane`, `moveFocus`,
`focusNextPane`, `resizePane`, `movePane`, `rotatePanes`, `closePane`,
`openFloat`, `floatConfig`,
`toggleFloats`, `toggleFloating`, `newTab`, `closeTab`, `nextTab`,
`prevTab`, `gotoTab`, `moveTab`, `renameTab`, `breakPane`, `tabs`,
`currentTab`, `buffers`, `showBuffer`, `edit`,
`deleteBuffer`, `killSession`, `detach`, `reloadConfig`, `message`,
`sendText`, `paste` (the client's clipboard, or `"primary"`; GUI clients:
terminal clients paste with the terminal's keys), `feedkeys`, `switchMode`, `exitMode`, `currentMode`, `panes`,
`focusedPane`, `sessionName`) and the page view's (`scrollDown`, `zoomIn`,
`search`, ...: every action of the PDF table above, taking the count).
`temzdf action NAME ARGS...` runs the same commands from a shell (`temzdf
action split right`). `resizePane("left", 5)` moves a border of the focused
pane five cells left (as tmux's `resize-pane -L 5`), `resizePane("wider",
5)` makes it five cells wider (as Vim's `5<C-w>>`), `resizePane({ width:
40 })` sets its size.

## Environment

Settings a client needs before it talks to a session are environment
variables:

| Variable | |
|---|---|
| `TERMPIX_BACKEND` | `layered` or `composited`: how frames reach the terminal (default: detected) |
| `TERMPIX_MEDIUM` | `direct`, `shm` or `file`: how pixels are transferred (default: detected; `direct` mimics SSH) |
| `TEMZDF_ZLIB_LEVEL` | zlib level (0-9) of inline transfers, default 2 (do not use 1 with Ghostty 1.3.1, see doc/terminal-compat.md) |
| `TEMZDF_LOG_DIR` | where logs go (default `$XDG_STATE_HOME/temzdf`): `temzdf.log` for viewers and clients, `NAME.log` for session NAME |
