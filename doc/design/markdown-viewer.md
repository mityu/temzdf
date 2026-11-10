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
- The graphics mode's first version has search, an outline and zoom; no
  dark style yet, and no text selection.

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
shown as their source instead. It is linked statically from
`nix/libtexprintf.nix` (GPL-3.0, which the repository's AGPL-3.0 can
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
  go (the document too, once no pane shows it).
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
- Zoom lays the page out again at Blitz's zoom (0.5 to 3, in steps;
  keys, or Ctrl and the wheel); switching modes, zooming, resizing and
  reloading keep the source line at the top (the one the pane is
  scrolling to, if it is).
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
