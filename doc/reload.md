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
6. **No SyncTeX anchoring.** Planned as an option (inverse-search the viewport
   before the reload, forward-search the result after it) and tried on
   2026-10-25 with a real latexmk rebuild. It was dropped. When text is inserted
   above the anchor, which is exactly when position-based restore drifts, the
   recorded line number points at different text in the edited source. Also,
   pdfTeX finishes the PDF before it writes `.synctex.gz`, so the reload can see
   stale SyncTeX data. Editor-driven forward search after each successful
   compile (e.g. a vimtex compiler callback running `temzdf --forward`) solves
   the real problem, because the editor knows the cursor in the *new* source. A
   content-based anchor (find the text line at the anchor point again in the new
   PDF) would work without SyncTeX; see `todo.md`.

## Tests to keep

Truncated PDF (no `%%EOF`), rename-over, delete-then-recreate, page count
shrinking. Each must keep the viewport and never show an empty view.

## Implementation

| Piece | Where |
|---|---|
| Watcher (parent dir + symlink target, filtered by file name) | `workers::spawn_watcher` |
| `%%EOF` pre-check | `pdf::looks_complete` |
| Debounce 200 ms, retry every 300 ms, give up after 100 attempts until the next change | `ReloadState` in `app.rs` |
| Open-then-swap on the document thread (`DocCmd::Reload` -> `Reloaded` / `ReloadFailed`) | `workers::spawn_document` |
| Position restore (page + fraction of page height, clamp to last page, keep zoom/fit mode/horizontal scroll) | `Viewer::replace_pages` |
| Outline collapse state by title path, selected entry by title, search re-run (current hit restored), selection dropped, history clamped | `Session::reload` |

Generations: the document thread and the viewer both count successful
reloads. Display lists, links and tiles carry the generation they belong to
(`TileKey::generation`, `AppEvent::PageLoaded`), so results computed from the
old file can never be shown after a swap. The search worker reopens the file
when a request carries a newer document generation.

Tests: unit tests in `session.rs` (`reload_*`) and the end-to-end scenario
`tests/e2e/reload.actions` (in-place truncation, rename-over with fewer pages,
delete and recreate).
