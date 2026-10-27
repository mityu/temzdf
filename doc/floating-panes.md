# Floating panes: design

Status (2026-10-26): implemented (`2d4bfc0` clipping, `a10166d`
floats, `74a3f32` mouse, `b55690a` keys and docs, `46ee6fb` text over images
in WezTerm); user documentation in `doc/config.md` ("Floating panes"). See
"Found while implementing" at the end for where it differs from the
proposal. Status bars (below) are not started.
Decision 13 in `doc/scripting.md` asks for floating panes (and non-focusable
panes) as the parts of the escape-hatch menu; users also want them on their
own, as in zellij and Neovim. Items marked **(decide)** were put to the user;
the others are my defaults, open to change.

## Decisions (2026-10-26)

1. Floats are not modal by default, as proposed. Modal floats must stay
   possible later: keep the model and the input path such that a float can
   take all keys (a flag or a mode bound to it), not only by scripts.
2. Floats show and hide together, as proposed. But if notifications (or the
   message window) are ever built on floats, `toggleFloats` must not hide
   them: they belong to a separate class (as zellij's pinned floats) that the
   toggle leaves alone.
3. `focusable: false` as a flag in the model, not exposed in the first step,
   as proposed. Later the flag should not be limited to floats: a tiled pane
   that never takes the focus (as Emacs side windows with `no-other-window`)
   could then carry a status bar or similar. So the flag lives on the pane
   entry, not on the float geometry.
4. The topmost float is embedded when the last tiled pane closes, as
   proposed.
5. Default mappings `w`, `W`, `e` in the `normal` mode, as proposed.

Second round (the user, 2026-10-26), all as proposed:

6. Floats hidden and the last tiled pane closes: the session ends (hidden
   floats are not shown panes, as for hidden buffers since `57ec52f`).
7. `toggleFloats` with no float opens a floating shell (zellij's Alt f).
8. Splitting or `new-pane` while a float has the focus opens a new float,
   2 cells right and down of the focused one (zellij).
9. `resizePane` (`H` `J` `K` `L`) on a float moves its right or bottom edge;
   no keys to move a float for now (the mouse does).
10. `toggleFloating` on the last tiled pane is refused with a message.
11. A `modal` flag sits in the model, unused, for modal floats later.

Direction for status bars (the user, 2026-10-26, tentative): rather than
non-focusable tiled panes, decoration bars at the top and bottom of each pane
and of the whole window (as Emacs's header and mode lines and tab bar,
Neovim's global statusline, tmux's status line). They take rows from the
pane's or the session's area and are never focusable.

Content (the user, 2026-10-26, tentative): as Vim's `statusline`, a string
or a function returning one. Open: the markup inside the string for colours
and clickable elements (Vim's `%#Group#` and `%@func@...%X`, or segments as
objects), and when a function is evaluated again. Vim evaluates on every
redraw; ours run in the script host, a round trip away, so on events and on
an explicit request rather than per frame.

Moving the PDF pane's own status bar onto bars: later (the user).

Clicks on bars (the user, 2026-10-26), after Vim:

- A bar can hold clickable elements; a click on one calls its callback
  (with the button, the click count, the modifiers and the pane, as
  Neovim's `%@` handlers get `minwid, clicks, button, mods`).
- A click elsewhere on a pane's bar focuses that pane.
- A drag that starts on a border stays a border drag (resizing), as now.
- Open: whether a click on a clickable element also focuses the pane.
  Neovim (`src/nvim/mouse.c`, `do_mouse` and `jump_to_mouse`): on the
  statusline the `%@` function runs on the press and the window is entered
  on the release when nothing was dragged, so yes; on the winbar a click
  only runs the function and never changes the focus. Vim has no clickable
  statusline items (only `%T`/`%X` in the tabline). Proposed: no focus
  change (as the winbar and GUI buttons; a close button should not focus
  the pane it closes); a callback can focus the pane itself.

## What others do

- **zellij**: floating panes are ordinary panes outside the tiled layout.
  Alt f shows or hides all of them at once (and opens one if there is none);
  hidden ones keep running. A pane can be pinned (always on top), moved and
  resized with the mouse or keys, and turned from floating to tiled and back
  ("embed"). Clicking a tiled pane leaves the floats visible.
- **tmux** (`display-popup`, 3.2+): a popup is not a pane. It is modal (keys
  go to it), cannot be split, and closes when its command exits (`-E`); a
  persistent popup needs a nested session.
- **Neovim** (`nvim_open_win`): a float shows any buffer at a position
  relative to the editor, a window or the cursor, with an anchor, size,
  `zindex`, border, title, and `focusable`. Floats are windows: the same
  commands work in them.

temzdf already separates buffers from panes, so the zellij/Neovim model
fits: a float is a pane that shows a buffer, outside the layout tree.

## Proposal

### Model

- A floating pane is a `PaneEntry` like any other (PDF or terminal, any
  buffer), kept in a stack (`floats: Vec<Float>`, last on top) instead of the
  layout tree. Pane commands work on it (`showBuffer`, `edit`, `closePane`,
  `resizePane`, `deleteBuffer`).
- Its place is stored as given: `width`, `height`, `row`, `col`, each either
  cells or a fraction of the screen (`"50%"`), plus an `anchor` (default:
  centred, 80% by 80%, as zellij's default size is close to that). It is
  recomputed when the screen changes size and clamped to fit; dragging it
  stores cells.
- A float has a one-cell frame: a border line (as the tiled borders, accent
  colour when focused) with a title on its top edge (the buffer's name, or a
  `title` given when opening). The frame is part of the float's rectangle;
  the content is inside it.
- **(decide)** Not modal (as zellij and Neovim): keys go to the focused pane,
  floating or not. A modal popup (tmux) can be built on this later by a
  script (a mode whose mappings only talk to the float) if wanted.

### Visibility and focus

- **(decide)** Floats show and hide together (zellij's Alt f):
  `toggleFloats()`. Hidden floats keep their buffers running, as hidden
  buffers do. A single float can also be closed (its buffer goes hidden, as
  for tiled panes). Showing them focuses the topmost; hiding them returns the
  focus to the tiled pane focused before.
- Focusing a float raises it to the top. A click on a float focuses and
  raises it; a click on a tiled pane focuses that pane and leaves the floats
  shown (as zellij).
- `moveFocus(dir)` moves within the layer that has the focus (among floats
  by their rectangles, or among tiled panes); `focusNextPane()` cycles
  through floats then tiled panes. The mode follows the focus as it does now
  (the focused pane's mode, `normal` stays a stack).
- **(decide)** `focusable: false` floats (Neovim) never take the focus and
  let clicks through. They are what decision 13 calls non-focusable panes;
  first used for the menu and possibly the message window. Proposed to
  include in the model now (a flag) but not to expose in the first step.
- `pinned` (zellij): stays shown when floats are hidden. Later.

### Closing, exiting, the last pane

- Closing a float hides its buffer, as for tiled panes.
- A terminal float whose program exits closes (as tmux `display-popup -E`),
  unless `remainonexit`.
- The last-pane rule stays about the tiled layout: closing the last tiled
  pane shows the most recently hidden buffer there; with none, and floats
  open, **(decide)** the topmost float becomes the tiled pane (embedded)
  instead of the session ending. Proposed: embed; quit only when no pane is
  left at all. (Since `57ec52f` hidden buffers no longer keep the session:
  closing the last tiled pane embeds the topmost float if one is open, and
  otherwise ends the session. Floats are shown panes, so decision 4 stands.)
- `toggleFloating()` (zellij's embed): a tiled pane becomes a float and back.

### Drawing (the hard part)

All images are drawn below text (every z is below kitty's
`-1073741824`), and terminal panes' text is real text in the outer terminal.
So a float over a terminal pane would show that pane's text through it, and
a float over a PDF page would show the page through the float's
transparent (default background) cells. Z order alone cannot fix this, and
with tmux's Unicode placeholders a cell shows one image only, so z order does
not exist there.

Proposal: **clip what is under a float**. While composing a frame, the
session:

1. cuts every layer of the panes below a float by the float's rectangle (a
   rectangle minus a rectangle is up to four rectangles: the layer becomes up
   to four layers of the same image with smaller `src`/`dst`; keys stay
   unique by deriving them from the piece),
2. drops the text cells of lower panes inside the float (and writes blank
   cells there so the outer terminal clears them),
3. draws the float (frame, then content) as a tiled pane is drawn.

This works the same in every backend (Layered, Composited, Placeholders)
and keeps z ranges as they are. The cost is more placements where tiles
cross a float's edge (a few per frame); only those pieces change while a
float moves. Program images (kitty graphics in a terminal pane under a float)
are clipped the same way. The message window (notices) stays above floats.

### Terminal sizes

The float's content is its terminal's view: the Neovim rule applies
unchanged (a terminal shown in a float and a tiled pane is as large as the
larger). While a float is dragged or resized, programs get their new size on
release, as with borders.

### Mouse

- Dragging the title edge moves the float; dragging its other edges or
  corners resizes it (pointer shapes `move`, `ew-resize`, `ns-resize`,
  `nwse-resize`, `nesw-resize`). The drag machinery of border dragging
  (session-level capture, programs resized on release) is reused.
- Wheel, selection, links: to the float under the pointer, as for tiled
  panes. Floats are hit-tested first, top down.

### Commands and API

| Command | JavaScript | Notes |
|---|---|---|
| `open-float` | `openFloat(what?, opts?)` | `what`: `{ buffer }`, `{ file }`, `{ command: [...] }`, default a new shell; `opts`: `width`, `height`, `row`, `col`, `anchor`, `title`, `focus` (default true). Returns the pane id |
| `float-config` | `floatConfig(pane, opts)` | move or resize (same `opts`) |
| `toggle-floats` | `toggleFloats()` | show or hide all floats |
| `toggle-floating` | `toggleFloating(pane?)` | tiled to floating and back; a pane of the current tab page only |
| (existing) | `panes()` | gains `floating: boolean` and the rectangle |

`temzdf action open-float -- htop` and `temzdf action open-float FILE.pdf`
follow `new-pane`'s words. Events: `PaneCreated` and `PaneClosed` as now,
with the pane's `floating` flag in `panes()`.

**(decide)** Default mappings in the `normal` mode, after zellij: `w`
toggles floats, `W` opens a floating shell, `e` turns the focused pane
floating or tiled. (`f` is left free for a future hint/jump mode.)

### Not now

Pinned floats, floats relative to a pane or the cursor (Neovim
`relative`), per-float z order beyond "focus raises", rounded or styled
borders, animations.

## Plan

- **F1. Clipping** (no visible change): compose lower panes clipped by a list
  of rectangles; unit tests on layer cutting (pieces, keys, `src` scaling at
  zoom, partial cells) and text dropping; `scripts/frames.sh compare` shows
  no change with no floats.
- **F2. Floats**: the stack, open/close/focus/raise, the frame and title,
  placement from the spec and on screen resize, terminal sizing, the
  last-pane rule, `open-float`, `float-config`, `toggle-floats`,
  `toggle-floating`, `panes()`; session tests; tuitest: a floating shell over
  a shell (no text from below inside it), over a PDF pane, inside tmux.
- **F3. Mouse**: hit testing top down, focus and raise on click, moving and
  resizing by the frame, pointer shapes; session and tuitest drags.
- **F4. Defaults and docs**: mappings, `doc/config.md`, `doc/session-server.md`,
  `print-types`; e2e in kitty (a float over a PDF page and over images from a
  program) and in WezTerm (Composited).

## Problems and risks

- Clipping PDF tiles at fractional positions: tiles are placed in pixels,
  floats in cells; cuts happen on cell boundaries in pixels, which divide
  tiles exactly, so `src` rectangles stay integral at scale 1 but need care
  when a layer is scaled (`src` and `dst` sizes differ).
- More placements while floats move over PDF pages: measure with the frame
  pacing in place; zellij's Composited strips are the worst case.
- Programs that track the mouse under a float must not get events meant for
  the float (hit testing top down covers it).
- The last-pane rule and `toggleFloats` interact: hiding floats when the
  focus is in one, closing the last tiled pane while floats are hidden.
  Session tests must cover these.

## Found while implementing (2026-10-26, without the user)

- No `anchor`: `col` and `row` place the top left corner (cells or
  `"N%"`), and a float without them is centred. Sizes in cells are the
  content's (as Neovim's), shares of the screen the whole float's (the
  default 80% by 80% leaves even margins).
- `focusNextPane` goes through the floats in the order they were opened,
  not as stacked: focusing raises, so a stacking order would bounce between
  the top two.
- `e` (`toggleFloating`) is mapped with `exitMode`: the focus stays, so
  nothing else returns to the pane's mode (zellij does the same).
- `toggleFloating` back to tiled splits the tiled pane focused last
  (`splitright` decides the side).
- WezTerm draws images over text whatever their z (#7222), so text the
  session draws over images (floating messages, a float's title) was
  hidden there. Such text now counts as a cover, as a float does: the
  images below are cut out. This also fixed the floating messages over PDF
  pages in WezTerm, hidden since they exist.
- A click on a float's frame never reaches the pane below; frames lie over
  the tiled borders.

## Tab pages (2026-10-27, with the user)

With tab pages, a float shows on the tab page it opened on, or on all of
them, as Vim's popup `tabpage` option (`openFloat(what, { tabpage: -1 })`;
`floatConfig` changes it). The stack is one for all tab pages (raising a
float on one raises it everywhere); showing and hiding is per tab page, and
floats on every tab page follow the current one's. Closing a tab page's
last tiled pane puts only a float of that tab page in its place: one on
every tab page would leave the others. `toggleFloating` makes a float on
every tab page a tiled pane of the current one.
