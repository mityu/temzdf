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
