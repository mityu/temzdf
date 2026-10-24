# Auto-reload

LaTeX rebuilds rewrite the PDF repeatedly. The viewer must reload automatically
and keep the reader where they were. The design borrows ideas (not code) from
Skim (BSD-3, `SKFileUpdateChecker.m`, `SKMainDocument.m`,
`SKMainWindowController.m`) and Okular (GPL-2.0+, `part/part.cpp`,
`core/document.h`), whose behaviour is the reference for "correct".

## What they do

| Aspect | Skim | Okular |
|---|---|---|
| Watcher | GCD vnode source on the file (DELETE/RENAME/WRITE); after delete, polls the path every 2 s and re-arms | `KDirWatch` on the file (and symlink target); parent-dir events detect recreation; re-arms the watch |
| Debounce | none | trailing 750 ms single-shot timer |
| Completeness | `%%EOF` within the last 1024 bytes | none, just try to open |
| Failure | do nothing; next write event retries | retry every 750 ms, unbounded; no error box while reloading |
| Swap | parse new document first, old one stays visible | close, then open ("Reloading..." and a blank view in between) |
| Position | top-left of the visible area as a point in page coordinates | page nearest the centre + centre in normalized page coords (`DocumentViewport::rePos`) |
| Page count shrinks | clamp to last page, drop the point | clamp to last page, keep normalized offset |
| Other state | keeps outline expansion, snapshots; clears search results and history | keeps rotation, sidebar, TOC expansion; clears search text |

## Our design

1. **Watch path, not inode.** Use `notify` on the file and its parent directory.
   On delete, create, or rename-over (latexmk), re-arm by path. Also watch the
   symlink target if the path is a symlink.
2. **Debounce + pre-check + retry.** Trailing debounce (default ~200 ms,
   configurable). Before opening, require `%%EOF` in the last 1024 bytes. If
   opening still fails, retry with a timer; never show an error popup while
   retrying, only a "Reloading..." status.
3. **Open, then swap.** The new `Document` is opened on the doc thread while the
   old one stays on screen. Swap only after success.
4. **Viewport.** Save page index + normalized (0..1) top-left of the viewport within
   that page + zoom + horizontal offset. Restore using the *new* page geometry so
   small page-size changes still land correctly. If the page count shrinks, clamp
   to the last page and keep the normalized offset.
5. **Other state.** Keep sidebar visibility/selection, outline expansion (matched
   by title path) and navigation history (clamped). Keep the search query and
   re-run it on the new document. Clear the text selection.
6. **Optional SyncTeX anchoring** (`reload.anchor = "synctex"`, default off):
   before reload, map the viewport anchor back to a source location; after reload,
   forward-search it. Off by default so the default behaviour matches Skim/Okular.

## Tests to keep

Truncated PDF (no `%%EOF`), rename-over, delete-then-recreate, page count
shrinking. Each must keep the viewport and never show an empty view.
