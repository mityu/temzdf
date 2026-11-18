# TUI renderers for script-made panes

Question (2026-11-06, extended 2026-11-19): script-made TUI panes
(`doc/design/tui-panes.md`) need something that turns a described layout
into cells. Which JavaScript/TypeScript and Rust libraries do that, how
does one get cells (characters, colours, attributes) out of them without
a terminal, how do they place images, and could one of them replace
temzdf's own drawing elsewhere?

Status: research only. The decision taken from it (2026-11-06) is in
`doc/design/tui-panes.md`: the API shape is borrowed, the renderer is
temzdf's own. Read from the projects' READMEs, documentation and docs.rs
on the dates above; nothing was built. **[V]** verified there, **[I]**
inference, **[?]** not found in what was read.

## What a renderer must fit

- **The reverse map.** The server must know which span went to which
  cell: clicks, actions on the span under the cursor, the cursor kept on a
  key across updates, editable regions.
- **One width rule.** Terminal panes measure characters with libghostty;
  a renderer measuring with another rule (`unicode-width`, `string-width`)
  misaligns wide characters and emoji next to them.
- **The scene's cell** (`crates/temzdf-scene/src/lib.rs`): a grapheme, a
  width (2 for wide, 0 for the spacer after it), `Color::Default |
  Indexed(u8) | Rgb([u8; 3])` (palette colours stay indices so the user's
  theme applies), and SGR attributes (bold, faint, italic, five underline
  styles, blink, inverse, invisible, strikethrough, overline). No
  underline colour, no hyperlink.
- **The script host** (`doc/design/script-host.md`): QuickJS in wasmtime,
  no WebAssembly inside it and no Node APIs. A JavaScript renderer must be
  plain ES code; anything Rust runs in the server, and the tree crosses
  the protocol as data (`ScriptValue`).

## Candidates

JavaScript and TypeScript:

| Library | Model / layout | Runtime | Output without a terminal |
|---|---|---|---|
| Ink | React, flexbox by Yoga (WASM in current versions) | Node | `renderToString(node, { columns })` (6.8, 7.x): an ANSI string **[V]** |
| OpenTUI (sst, now anomalyco/opentui) | `@opentui/core` imperative, React and Solid reconcilers; drawing, text buffers and Unicode in a Zig core over a C ABI | Bun (its FFI); Node and Deno in progress | `createTestRenderer({ width, height })`, then `captureCharFrame()` (characters) and `captureSpans()` (colours, attributes), in process, no pty **[V]** |
| Silvery (beorn; inkx before) | React, flexbox by Flexily (pure JS, Yoga-compatible) or Yoga | Node 23.6+ or Bun | the test package's `createRenderer({ cols, rows })`: `app.text`, `app.ansi`, `app.cell(col, row)` **[V]**; no documented cell buffer for general use **[?]** |
| Glyph (semos-labs) | React, flexbox, focus | Node | **[?]** |
| blessed, neo-blessed, react-blessed | imperative widgets | Node, a tty | screen buffer bound to a tty; blessed itself unmaintained for years |
| terminal-kit | `ScreenBuffer` and widgets | Node | possibly a detached `ScreenBuffer` **[?]** |

Also: termcn (React components over Ink or OpenTUI) and TermUI
(shadcn-style: component sources copied into the project).

Rust:

| Library | Model / layout | Output without a terminal |
|---|---|---|
| ratatui | immediate-mode widgets; `Layout` with constraints (not flexbox) | widgets draw into a `Buffer` of cells; `TestBackend` **[V]** |
| iocraft | React-like `element!` macro, taffy (flexbox) | `Element::to_string()` (canvas to string); `render()` gives the canvas **[V]** |
| rnk | Ink-like hooks, taffy | `render_to_string(element, width)` **[V]** |
| rxtui | `node!` macro, Elm-like updates | **[?]** |
| reratui | hooks and `rsx!` over ratatui | through ratatui's `Buffer` **[I]** |
| cTUI | declarative, hooks | **[?]** |
| Lip Gloss ports (`lipgloss`, `charmed-lipgloss`, `lacquer`) | styles (borders, padding, alignment) rendered to strings, joined | the output is a string; `charmed-wasm` exposes it to JavaScript **[V]** |
| termwiz (WezTerm) | `Surface`: cells plus a change list; few widgets | read the `Surface` **[V]** |
| cursive | imperative widgets, dialogs | needs a terminal backend |

What comes out falls into three kinds: a grid of cells (ratatui's
`Buffer`, termwiz's `Surface`, OpenTUI's spans, iocraft's canvas), a
string with SGR escapes (Ink, rnk, Lip Gloss, iocraft's `to_string`), which
the pane would have to parse again (libghostty-vt could), or nothing
without a tty (blessed, cursive).

## Getting colours and attributes out

For ratatui, termwiz and Silvery (2026-11-19):

- **ratatui** **[V]**: `buf.content()` or `buf[(x, y)]`. A cell has
  `symbol()`, `fg` and `bg` of `Color` (`Reset`, sixteen named, `Indexed`,
  `Rgb`), a `Modifier` bit set (bold, dim, italic, underlined, two blinks,
  reversed, hidden, crossed out) and `underline_color` behind the
  `underline-color` feature. To the scene: colours map one to one (`Reset`
  to `Default`, the named ones to indices 0 to 15); one underline style
  only, no overline; the cell after a wide character is a filler to turn
  into the scene's spacer.
- **termwiz** **[V]**: `Surface::screen_cells()` or `screen_lines()`. A
  cell has `str()`, `width()` and `attrs()`: `ColorAttribute` (`Default`,
  `PaletteIndex`, true colour with a palette or default fallback, as RGBA
  floats), intensity (normal, bold, half), all five underline styles,
  underline colour, italic, blink, reverse, strikethrough, invisible,
  overline, a hyperlink and an image. The closest to the scene's cell:
  both copy SGR. True colour is rounded to 8 bits.
- **Silvery**: only through the test renderer, one cell at a time:
  `app.cell(col, row).fg` and `.bg` are `{ r, g, b }`, with the palette
  slot as `index` when the colour came from the palette, so `Indexed` and
  `Rgb` can be told apart **[V]**; how the terminal's default colour shows
  there, and the field names of bold, italic, inverse and strikethrough,
  are not documented **[?]**. Its `@silvery/ansi` knows the five underline
  styles, underline colour (SGR 58) and OSC 8 links **[V]**. Parsing
  `app.ansi` with libghostty-vt would avoid relying on a test API **[I]**.

## Images

- temzdf's decorations show images inline (one row high, as wide as the
  aspect asks), in place of a concealed range, or as virtual lines (rows
  of their own) (`doc/design/text-editor.md`, section 7). An image several
  rows high **beside** text, as a laid-out box would be, is none of these:
  it needs an overlay image spanning rows. `DrawnImage` already carries
  position, rows and a skip, and the server already cuts the hole and
  places the layer, so a first version (drawn only while its anchor row is
  shown, shrunk at the pane's edge) is smaller than the virtual lines were
  (`e55d8ef`, about 640 lines with tests); an anchor scrolled above the
  view needs the rows above counted, with wrapping and virtual lines.
- To place images a renderer must reserve a box and say where it ended up,
  or carry images in cells. ratatui gives each widget its `Rect`
  (ratatui-image draws into one with kitty, sixel or half blocks); termwiz
  cells carry image slices (WezTerm's model); iocraft components get their
  layout rectangle when drawing **[V]**. String renderers lose the
  position. Silvery's `<Image>` reserves a box, then writes kitty or sixel
  escapes straight to stdout after rendering **[V]**; whether its cells
  show the image is not documented **[?]**.

## Prior art taken into the design

From the 2026-11-06 discussion; `doc/design/tui-panes.md` keeps the
decisions.

For the API shape (borrowed, not linked):

| Library | What is taken |
|---|---|
| ratatui | `Text` / `Line` / `Span` with a `Style` for the content layer |
| Emacs text properties | properties over ranges: face, keymap, read-only, field |
| FTXUI | the small layout vocabulary: `text`, `hbox`, `vbox`, `border`, `flex`, `frame` |
| Neovim | text and decorations kept apart (lines and extmarks), for the editor-like case |
| Lip Gloss | a warning: joining styled strings loses keys and hit testing |
| Ink, Textual | not taken: flexbox or CSS is more than one pane needs |

As the rendering engine, ratatui (`ratatui-core`'s `Buffer` and widgets)
could draw the cells, but:

- wrapping and widgets lose which span went to which cell, the reverse map
  above; recomputing it means laying out twice;
- it measures width with `unicode-width`, not libghostty's rule;
- it is immediate mode, so scroll, focus and edit state are ours anyway.

What remains would be a few widgets' drawing, not worth the dependency.
termwiz's hyperlink attribute could carry a span's id as a reverse map
**[I]**, but termwiz has almost no layout or widgets, so little would be
reused. Silvery would move layout into the script: the server would only
draw cells, and the reverse map, the width rule and keyed state would be
the script's.

For updates:

- The server-side diff of keyed blocks is a map lookup plus comparing
  matched blocks: small, our own. For unkeyed lines, `similar` (Myers)
  could be used.
- On the script side, a framework could sit on top and produce the
  patches, if plain `render(state)` with `ui.*` builders becomes painful:
  Preact with a minimal fake DOM (small; DOM operations become patches),
  Vue's `createRenderer`, or Solid's universal renderer (fine-grained,
  maps directly onto patches, but normally needs its JSX compiler; check
  whether it is usable without). React with `react-reconciler` (Ink's
  way) is too heavy and Node-bound. Any of them needs:
  - JSX support, or `h()` calls, since the TypeScript stripping
    (`swc_ts_fast_strip`) does not transform JSX;
  - bundling into the script host, as config files cannot import npm
    packages; fetched and pinned through Nix and embedded at build time,
    never committed as built JavaScript;
  - keeping it working on QuickJS across versions.
  All of these are MIT. The protocol stays independent of any framework.
- Ideas, not libraries: Phoenix LiveView (state on the server side,
  re-render and send diffs), the Elm architecture (state, update, view).

Whatever draws in the server, the boundary with scripts stays the same:
`ui.*` builders make plain objects, they cross as `ScriptValue`s and the
server reads them into its own types (as `read_what` in
`crates/temzdf-server/src/session/commands.rs`). The renderer behind it
can change without changing the API.

## A renderer for temzdf's own drawing

Asked on 2026-11-19: could such a library lighten what temzdf draws
itself? Mostly not:

- pages, terminals, pixels and the GUI already sit on libraries (MuPDF,
  Blitz, libghostty-vt, `vello_cpu` and `parley`, wgpu and glyphon);
- the kitty layer compositor (`crates/termpix/src/compositor.rs`,
  `graphics.rs`) has no counterpart in any of these libraries;
- writing cell differences to the terminal (`crates/temzdf-tty/src/text.rs`,
  about 390 lines) is what ratatui with crossterm or termwiz do, but ours
  knows cells under images, synchronized output and tmux placeholders;
- the pane layout (`crates/temzdf-server/src/layout.rs`) is mostly Vim's
  rules (`winfixwidth`, equalizing, resizing), which no layout engine has;
- the editor's drawing (virtual text, concealing, the gutter) and the
  Markdown text mode (each line's source line, anchors, decoration ranges)
  need maps back to their sources that Markdown renderers (termimad,
  tui-markdown) do not give;
- the chrome (status line, tabs, floats, borders, notices) could be ratatui
  widgets converted to `TextGrid`s, but each is 200 to 500 lines, borders
  join between panes, and a second cell model would come with it. Worth
  revisiting only if the kinds of widgets grow a lot.

## Sources

- Ink: `build/render-to-string.d.ts` in ink 6.8.0 and 7.1.1 (unpkg, jsDelivr)
- OpenTUI: opentui.com (testing, rendering diagnostics), github.com/sst/opentui, the `@opentui/core` README
- Silvery: github.com/beorn/silvery; silvery.dev (`api/render`, `reference/ansi`, `guide/testing`, `guide/layout-engine`, `components/Image`); Flexily (github.com/beorn/flexx)
- iocraft, rnk, rxtui, reratui, `lipgloss`, `charmed-lipgloss`, `lacquer`, `charmed-wasm`: docs.rs
- Overviews (blogs, self-reported): OpenReplay "OpenTUI alternative to Ink", Better Stack "OpenTUI: React-Based Terminal UIs"
