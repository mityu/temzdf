# Markdown viewer

A pane that shows a Markdown file, for reading documents and, later, for
the built-in help and LSP hover (decided with the user, 2026-11-09, in
place of a Markdown preview on a webview: `doc/survey/markdown-viewer.md`,
`doc/survey/webview-viewer.md`). Status and the remaining steps are in
`todo.md`.

## Decisions (user)

- Two ways to draw it, chosen per pane by a function and switched in
  place (the pane keeps its id):
  - **text mode**: drawn by temzdf as text on the text-editor pane, so
    the Vim mode's visual selection and copying (into temzdf's
    clipboard) work as in any text pane; what is copied is the drawn
    text. Images (and math as images) where the kitty graphics protocol
    is there, their alt text elsewhere. Math as Unicode text by
    libtexprintf (`crates/texprintf`, utftex's library).
  - **graphics mode**: drawn in pixels like a PDF page, with Blitz
    (`doc/survey/markdown-viewer.md`, "Blitz spike"); math as SVG by
    RaTeX. MathML comes later (Blitz lays out next to none of it).
- comrak parses (`crates/temzdf-markdown`); temzdf draws the text mode
  itself, so source positions stay at hand (SyncTeX-like features
  later).
- One document per file, shared by the panes on it, as PDF documents
  are: the parsed document and what was drawn of it.
- Reloaded when the file changes. A live preview of an edited buffer
  comes later, through LSP-like change notifications.
- Code blocks are highlighted with syntect in the text mode.
- `.md` files still open in the text editor; the viewer is opened with a
  function.
- Images: local files only by default; a function loads the remote ones
  of a document.
- Links: URLs go to the link opener, `#anchor` to the heading in the
  document, other `.md` and `.pdf` files open in the same pane with a
  history to go back.
- The graphics mode's first version has search, an outline and zoom,
  and since 2026-11-18 text selected with the mouse; no dark style yet.

## The document (`crates/temzdf-markdown`)

`Document::parse` reads GitHub-flavoured Markdown with alerts,
footnotes, `$`/`$$`/```` ```math ```` math and front matter (not shown)
into blocks and inlines. Every block keeps the source lines it came
from, which is what keeps the reading position across reloads and mode
switches. Headings get GitHub's anchors (lower case, spaces as `-`,
punctuation dropped, `-1`, `-2` for repeats). Footnotes are numbered as
first referred to and go at the end.

## Math as text (`crates/texprintf`)

libtexprintf keeps its settings and error state in globals: calls go
one at a time through a lock, and its settings are never changed
(out-of-range character widths make it `exit(1)`). Its parser recurses
and slows down badly with nesting (10000 nested `\frac` take over 20 s),
so formulas longer than 4096 bytes or nested deeper than 64 groups are
shown as their source instead. It is vendored in
`crates/texprintf/vendor/libtexprintf/` and built statically with `cc` (GPL-3.0, which the repository's AGPL-3.0 can
combine with); none of its global symbols clash with the other C
libraries linked statically (MuPDF, SyncTeX, QuickJS, zlib), and a clash
would fail the link rather than go unnoticed.

## The text mode (`temzdf_markdown::render`, `session/markdown.rs`)

- The document is drawn at a width into lines and decorations (character
  ranges): highlight groups, links (the target as written) and images
  (over their alt text, `[alt]`). temzdf wraps paragraphs itself, so quote
  bars, list markers and indents carry on to the wrapped lines; code
  blocks are left to the pane's own wrapping. Tables fit the width: the
  widest column narrows first and its cells wrap. Every line drawn knows
  its source line (an empty line belongs to the block before it, so a
  source line finds its block, not the gap before it), and every anchor
  the line it went to.
- A drawing is a text buffer with the attributes `nofile`, `unlisted` and
  `nomodifiable` (new: edits, typing, undo and reload fail, Vim's E21), so
  the pane is an ordinary text pane: Vim's keys, the visual mode and the
  copy history work unchanged. One buffer per (document, width): panes of
  one width share it; a pane that changes width moves to the drawing for
  it (made if needed) at the same source lines, and drawings no pane shows
  go.
- Drawings are internal. A viewer pane shows the same document in either
  mode (the mode only changes how), so the buffer scripts see is the
  document, in both modes: `panes()`, `text.info()`, the focused pane's
  buffer (`deleteBuffer()`, `getVar("buffer", ...)`), `buffers()` (kind
  `markdown`; drawings are never listed, not even with `all`) and the
  buffer events (`BufferCreated` when the document is read,
  `BufferHidden` when its last pane goes, in whichever mode,
  `BufferDeleted` only when `deleteBuffer` deletes it). Its panes are of
  kind `markdown` in either mode too (`panes()`, `PaneOpened`, a
  condition's `paneKind`; `markdown.mode()` tells the modes apart), but a
  text-mode one is still a text pane for its base mode (`o.basemode.edit`:
  Vim's keys). Switching modes or moving to another drawing for a new
  width keeps the number and sends nothing. `deleteBuffer` closes the
  document's panes in both modes (Vim's `:bwipeout`); `showBuffer` opens
  it in the viewer at the pane's width (a viewer pane keeps its mode,
  another takes `markdownmode`).
- A document no pane shows stays, hidden, as a PDF does: the parsed text,
  its images and its file watcher (it is read again on a change) stay, and
  the source line at the top of its last pane (mapped through a reload);
  its drawings, pages and strips go. Shown again (`showBuffer`, a split
  or float of it, opening its file again: documents are found by path),
  it starts at that line, in the mode the pane or `markdownmode` says (the
  mode and zoom are not kept).
- A changed file is read and drawn again, each pane at the same place:
  source lines map from the old text to the new through a line diff
  (`map_line`), then to drawn lines.
- Math goes through libtexprintf always (copied text stays readable);
  only Markdown images become images. A paragraph of one image shows it
  below its line (virtual lines, the alt text concealed), others replace
  their alt text (conceal); both reveal the alt text on the cursor's line.
  Local images are read when the document is (PNG, JPEG, GIF, WebP, SVG);
  remote ones only on `markdown.loadRemoteImages` (ureq, 20 s, 32 MiB),
  in the background, each redrawn as it arrives.

## The graphics mode (`temzdf_markdown::html`, `temzdf-mdview`)

- The document becomes HTML: every block carries `data-line` (its first
  source line), headings their anchors as ids, footnotes `fn-LABEL`, math
  RaTeX's SVG (glyphs as paths, no fonts). Blitz lays it out (style:
  `crates/temzdf-mdview/src/markdown.css`, GitHub-like, light); a page is
  one layout at a width and zoom, shared by the panes that need it, and
  painted in strips 256 pixels high, shared by its panes (64 MiB of
  strips a document, least recently used going first). Pages no pane
  needs go, and their strips.
- Strips are painted as PDF tiles are (`temzdf_scene::tiles`, the same
  cache, schedule and kind of worker pool): a strip's drawing is recorded
  on the session's thread (`blitz_paint::paint_scene` into an
  `anyrender::Scene`, about a tenth of painting it) and painted by worker
  threads (`anyrender_vello_cpu`), the same pixels. Strips in sight come
  first, then those within a screen above and below; until one arrives
  the page's background stands in. Strips are keyed by their page, which
  is new at every layout, so ones painted for a page gone are dropped.
  A frame keeps the strips it still wants queued (`WorkPool::replace_wanted`,
  as PDF tiles do, `Need::Queued`): replacing the queue with new work
  alone dropped work an earlier frame had asked for and nothing asked
  for again while it counted as in flight.
- Positions: `data-line` elements give (y, source line) pairs, both ways;
  `#anchor` is the element with that id; a point's link is the `href` up
  from Blitz's hit test; search hits come from inline roots' text and
  their parley layouts (`Selection::geometry`), placed at the content box.
  Marks are images of their own size (a stretched pixel looked different
  in kitty and WezTerm).
- Selection, as the PDF pane's: a drag from text selects (a press on a
  link still follows it), past the pane's top or bottom it scrolls on,
  the release copies (into the copy history as characters, source
  `markdown`; `y` copies again, `<Esc>` forgets it). Its ends are carets
  of the page: an inline root, by reading order (the layout tree walked
  depth first, anonymous blocks of text included), and a byte offset in
  its text, from Blitz's `find_text_position`; over a margin, the
  nearest root above or below, the point moved into it. Its boxes come
  from each root's part, as search hits do, drawn as images over the
  strips. Blitz's own selection (`set_text_selection`) is not used:
  `blitz_paint` paints it into the page, so the cached strips would be
  painted again at every change. The text joins blocks with a line feed
  (Blitz's `get_selected_text` joins them with a space). A page laid out
  again (zoom, resize, reload) drops the selection, as a PDF's reload
  does.
- Zoom lays the page out again at Blitz's zoom (0.5 to 3, in steps;
  keys, or Ctrl and the wheel); switching modes, zooming, resizing and
  reloading keep the source line at the top (the one the pane is
  scrolling to, if it is).
- A pinch (`MouseEventKind::Pinch`, from `temzdf-gui`) magnifies the page
  without laying it out again, as browsers on macOS do (decided with the
  user on 2026-11-16): separate from the zoom, 1 to 5 times, about the
  pointer, kept until pinched back (zooming, resizing, reloading and
  following links keep it). The viewport counts magnified pixels, so
  fingers and the wheel move the page every way at the same speed; places
  (the source line at the top, links under the pointer, search marks)
  convert. While the fingers pinch, the strips are shown stretched; when
  they leave, the magnification is rounded to hundredths and tiles of
  512 pixels are painted in sight at it (`Page::record_magnified`: the
  page's recording painted larger, so text is sharp and the layout the
  same), the stretched strips under them until they arrive. `blitz_paint`'s
  `scale` argument does not draw larger (the layout's scale does), so the
  magnification is a transform applied when painting. Tiles are not
  painted ahead (a screen of them around would not fit the strips' memory
  at 5 times); a split's new pane starts unmagnified.
- Scrolling goes through `termpix_core::viewport::Viewport`, as the PDF
  pane's does: wheel steps of `o.scrollstep` ease (`o.scrollsmoothness`)
  and speed up when flicked, fingers' pixels are followed exactly, keys
  that scroll and page ease too; jumps (links, headings, search, places
  kept) do not. The viewport is the part of the views worth sharing:
  zoom means different things (a PDF scales, a page lays out again) and
  stays each pane's, and a webview's engine scrolls by itself. Sharing
  the painting schedule (prefetch, workers) is in `todo.md`.
- Differences from the plan, decided while building it: layout runs on
  the session's thread (a page of `tests/fixtures/sample.md` lays out in about 4 ms, 50
  copies of it 33 ms; a worker thread is in `todo.md`), painting on
  workers since 2026-11-09 (a strip took 0.6-0.8 ms there, its recording
  0.03-0.08 ms, release build); the outline is a floating list
  in both modes (`gO`, `o`) rather than the PDF pane's sidebar; ordered
  lists' numbers are written into the items (Blitz puts its markers right
  against the text).
