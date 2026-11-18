# Script-made TUI panes: design draft

Panes whose content a script builds: lists, trees, forms, dashboards, a
help or picker UI. Discussed with the user on 2026-11-06. **Nothing is
built yet**; this records the direction agreed so far and the questions
left open. The research behind it is in `doc/survey/tui-renderers.md`
(renderers and prior art) and `doc/survey/text-editor.md` (section 4,
"The editor buffer as a base for script-made panes"), and the scripting
model it plugs into is `doc/design/scripting.md`.

## The idea

Building styled text is building a screen layout, so the script describes
the pane in a small format, and the server acts as the rendering engine
for that format. The format has two layers:

1. **Content**: styled text.
2. **Layout**: a deliberately small set of boxes around that text.

The server owns layout, text measurement and everything per-keystroke
(cursor, scrolling, editing); the script owns the state and describes what
to show. The script host is isolated from the session (`doc/design/script-host.md`),
so everything crosses the protocol as data.

## Layer 1: styled text

The shape follows ratatui's `Text` / `Line` / `Span` (a text is lines, a
line is spans, a span is a string with a style), with Emacs-style
properties on ranges added:

- **Style**: a highlight group by name (resolved by the server's theme),
  or explicit colours and attributes (bold, italic, underline, reverse).
  Groups are preferred so themes and the window's chrome stay consistent.
- **Action**: a command name or a script callback id, run on click or on
  a key when the cursor is on the span. Resolved in the server, so moving
  over and activating items costs no round trip to the script host (the
  same concern as decision 1 in `doc/design/keymaps.md`).
- **Key**: a stable identity for a line or block (see "Updates").
- **Editable**: a range the user can type into (see "Editable regions").
  Everything else is read-only.
- Later: hover text, concealed text, folds, virtual text.

Because the result is text, search and copy (the visual mode of terminal
panes, `doc/config.md`) can work on these panes too.

## Layer 2: layout

Only what a text UI in one pane needs; anything with several independent
regions is several panes (splits, floats and bars already exist,
`doc/design/floating-panes.md`). The vocabulary follows FTXUI's
(`vbox`, `hbox`, `border`, `flex`, `frame`):

- **Inside a line**: alignment (left, centre, right) and table columns
  whose widths the server computes from the content (as a table widget
  does), with truncation and an ellipsis when too narrow.
- **Splitting the pane**: a fixed header and footer around one body.
  Only the body scrolls. Horizontal splits (`hbox`) with fixed or flexible
  widths, nested at most a little; no general flexbox.
- **Borders and separators**: a box around a part, a rule between parts,
  a title in the border.
- **Scrolling**: the body scrolls as a whole (vertical; horizontal for
  long lines), by the server. The scroll position is anchored to a key,
  not a row number, so an update above it does not move the view.

A general layout engine (flexbox, as `taffy`) is not needed for this; if
the layout grows later, it can be reconsidered then.

## Updates

Three ways were compared:

- **A. Re-render**: the script calls `render(state)` and sends the whole
  tree; the server diffs it against the previous one and redraws what
  changed. Simple to write (as React or Elm), but the whole tree crosses
  the protocol each time.
- **B. Patches**: the script sends operations: append, replace a keyed
  block, remove, set properties. Cheap for logs and growing lists, but
  the script keeps track of what is on screen.
- **C. Pull**: the server asks for the rows it is about to show (as VS
  Code's `TreeDataProvider`), for very large or lazy content.

Chosen direction: **keyed blocks**, which unify A and B. The content is a
list of blocks, each with a key. A full render (A) is the default; the
server matches blocks by key and compares the matched ones. Patches (B)
address the same keys, for logs and partial updates. C is deferred.

Common rules:

- **Coalescing**: renders requested several times before the next frame
  are sent once.
- **Identity by key**: the cursor, the selection, folds and the scroll
  anchor stay on the same key across updates, which Magit sections and
  nui.nvim each reinvent.
- **Editable regions are uncontrolled**: the server owns their text while
  the user types and reports changes to the script (as prompts do with
  `onChange`, `doc/config.md`). A re-render does not overwrite
  the text being edited unless the script asks for it.

## Existing libraries

The comparison is in `doc/survey/tui-renderers.md`. Decided:

- **API shape** borrowed, not linked: ratatui's `Text` / `Line` / `Span`
  for content, Emacs text properties for ranges, FTXUI's small layout
  vocabulary, Neovim's split of text and decorations. Not flexbox or CSS
  (Ink, Textual): more than one pane needs.
- **Rendering engine**: our own, not ratatui's `Buffer` and widgets. They
  lose which span went to which cell (the reverse map clicks, actions,
  the cursor on a key and editable regions need), measure width with
  another rule than terminal panes, and leave scroll, focus and edit
  state to us anyway.
- **Updates**: the keyed-block diff is ours. A script-side framework
  (Preact, Vue's `createRenderer`, Solid's universal renderer) may sit on
  top later; the protocol stays independent of any.

## First version (proposed)

- Protocol: keyed blocks of styled text with actions, the minimal layout,
  full renders and block patches.
- Server: the renderer with the reverse map, the keyed diff, the cursor
  and scroll anchored to keys, actions resolved in the server.
- Script side: small `ui.*` builders and `render(state)`; no framework.

## Open questions

1. **Callbacks**: actions that call back into the script are synchronous
   for now (as mapped functions, "Execution model" in
   `doc/design/scripting.md`); whether a render may be requested from an
   asynchronous callback, and how that interacts with "Future:
   asynchronous mapped and hook functions" there.
2. **The first consumer**: which pane to build first (the built-in help,
   a buffer list, a picker) decides which parts come first.
3. Whether the content layer becomes the read-only mode of a future editor
   buffer (`doc/survey/text-editor.md`, section 4) or stays separate and
   converges later.
4. Whether to prototype Preact and Solid on QuickJS without a compiler
   before committing to the `ui.*` builders' shape.
5. **Images beside text** (2026-11-19): decorations show images inline
   (one row), in place of a concealed range, or as rows of their own; an
   image several rows high next to text needs an overlay image spanning
   rows (cost in `doc/survey/tui-renderers.md`, "Images"). Not built until
   a pane needs it; the first panes show images in rows of their own.
