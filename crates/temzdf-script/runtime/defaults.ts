// temzdf's default configuration. The script host loads it before the user's
// configuration (`--clean` loads only this); the user's can map over or
// remove anything here: its mappings come first (they are registered later),
// and these are in the namespace "default" (`keymap.remove(mode, lhs, {
// namespace: "default" })` removes one, `keymap.clear({ namespace: "default" })`
// all of them). Built-in modes: `normal` (operating),
// `terminal` (pass-through, terminal panes), `pdf` (PDF panes), `input`
// and `edit` (input and text panes: typed characters go in) and `visual` (a terminal's visual
// mode: a cursor over the screen and scrollback, selecting and copying).
import { action, cmd, keymap, on, schedule } from "temzdf";
import type { Rhs } from "temzdf";

/** The prefix: enters the normal mode from a pane. */
const prefix = "<C-b>";

function each(mode: string, keys: string[], rhs: Rhs): void {
  for (const k of keys) keymap.add(mode, k, rhs, { namespace: "default" });
}

// Page view.
each("pdf", ["q"], action.quit());
each("pdf", ["<Esc>"], action.cancel());
each("pdf", ["j", "<Down>"], action.scrollDown());
each("pdf", ["k", "<Up>"], action.scrollUp());
each("pdf", ["h", "<Left>"], action.scrollLeft());
each("pdf", ["l", "<Right>"], action.scrollRight());
each("pdf", ["<Space>", "<PageDown>", "<C-f>"], action.pageDown());
each("pdf", ["b", "<PageUp>"], action.pageUp());
each("pdf", ["d"], action.halfPageDown());
each("pdf", ["u"], action.halfPageUp());
each("pdf", ["g", "<Home>"], action.firstPage());
each("pdf", ["G", "<End>"], action.lastPage());
// Backspace too: multiplexers (zellij) take Ctrl-o and Alt+arrows.
each("pdf", ["<C-o>", "<BS>", "<M-Left>"], action.back());
each("pdf", ["<M-Right>"], action.forward());
each("pdf", ["+", "="], action.zoomIn());
each("pdf", ["-"], action.zoomOut());
each("pdf", ["0", "w"], action.fitWidth());
each("pdf", ["/"], () => openSearch());
each("pdf", ["n"], action.nextMatch());
each("pdf", ["N"], action.prevMatch());
each("pdf", ["y"], action.copy());
each("pdf", ["<Tab>"], action.toggleSidebar());

// From a pane to the normal mode.
each("terminal", [prefix], action.switchMode("normal"));
// GUI clients paste the clipboard with these; a terminal client's terminal
// pastes with its own keys before they reach temzdf.
each("terminal", ["<C-S-v>", "<D-v>"], action.paste());
each("pdf", [prefix], action.switchMode("normal"));

// The normal mode: panes. Moving the focus returns to that pane's mode.
each("normal", ["%"], action.split("right"));
each("normal", ['"'], action.split("bottom"));
each("normal", ["h", "<Left>"], action.moveFocus("left"));
each("normal", ["j", "<Down>"], action.moveFocus("down"));
each("normal", ["k", "<Up>"], action.moveFocus("up"));
each("normal", ["l", "<Right>"], action.moveFocus("right"));
// Sizes as Vim's <C-w>+ - < >: the pane taller, shorter, narrower, wider;
// the normal mode stays, so the keys repeat (3>: three cells).
each("normal", ["+"], action.resizePane("taller"));
each("normal", ["-"], action.resizePane("shorter"));
each("normal", ["<lt>"], action.resizePane("narrower"));
each("normal", [">"], action.resizePane("wider"));
// As Vim's <C-w>= | _: every pane the same size; the pane as wide or as
// high as it can be (with a count: that many cells).
each("normal", ["="], action.equalizePanes());
each("normal", ["<Bar>"], action.maximizePane("width"));
each("normal", ["_"], action.maximizePane("height"));
// Places as Vim's <C-w>H J K L r R: the pane to an edge of the screen;
// the panes of its row or column rotated.
each("normal", ["H"], action.movePane("left"));
each("normal", ["J"], action.movePane("bottom"));
each("normal", ["K"], action.movePane("top"));
each("normal", ["L"], action.movePane("right"));
each("normal", ["r"], action.rotatePanes());
each("normal", ["R"], action.rotatePanes(true));
each("normal", ["o"], action.focusNextPane());
// Floating panes (zellij's keys): show or hide them all (with none, open a
// floating shell), open a floating shell, turn the focused pane floating or
// tiled (the focus stays, so back to its mode, as zellij does).
each("normal", ["w"], action.toggleFloats());
each("normal", ["W"], action.openFloat());
each("normal", ["e"], [action.toggleFloating(), action.exitMode()]);
each("normal", ["x"], action.closePane());
// Tab pages as Vim's gt, gT ({N}gt: tab page N) and <C-w>T.
each("normal", ["gt"], action.nextTab());
each("normal", ["gT"], action.prevTab());
each("normal", ["T"], action.breakPane());
each("normal", ["d"], [action.detach(), action.exitMode()]);
each("normal", ["q"], action.killSession());
// The prefix twice sends it to the pane.
each("normal", [prefix], [action.exitMode(), action.feedkeys(prefix, false)]);
each("normal", ["<Esc>"], action.exitMode());

// Status lines: terminal panes go without (PDF panes show their file, page
// and zoom there).
// (Hooks only read: what they change waits for the safe point, schedule.)
on("PaneOpened", (id: number, kind: string) => {
  if (kind === "terminal") schedule(() => cmd.statusLine(false, id));
});

// Searching a PDF pane (/) or a terminal in the visual mode (/ and ?): an
// input pane one row high below it, without a status line. As the query is
// typed the pane shows its first match (searchPreview, as Vim's incsearch);
// <CR> searches, <Esc> cancels (back where it began), and either closes the
// input, back to the pane (and its visual mode). Closed any other way, the
// search is cancelled.
/** A search being typed: the pane searched, a terminal's visual mode, its
 * direction. */
type Searching = { pane: number; visual: boolean; backward: boolean };
/** The searches being typed, by input buffer. */
const searches = new Map<number, Searching>();

function openSearch(backward = false): void {
  const pane = cmd.focusedPane();
  if (pane === null) return;
  const visual = cmd.currentMode() === "visual";
  const buf = cmd.newInput({ prompt: backward ? "?" : "/" });
  searches.set(buf, { pane, visual, backward });
  try {
    const pane = cmd.split({ side: "bottom", buffer: buf, height: 1 });
    if (pane !== null) cmd.statusLine(false, pane);
  } catch (e) {
    searches.delete(buf);
    cmd.deleteBuffer(buf);
    throw e;
  }
}

/** End the search typed into `buf`: search (`confirm`) or cancel, back to
 * its pane (in the visual mode again for a terminal), and the input goes. */
function endSearch(buf: number, confirm: boolean): void {
  const s = searches.get(buf);
  if (s === undefined) return;
  searches.delete(buf);
  const query = cmd.inputText(buf);
  try {
    if (s.visual) {
      cmd.focusPane(s.pane);
      cmd.switchMode("visual");
      if (confirm) cmd.visual.searchFor(query, s.pane, s.backward);
      else cmd.visual.searchCancel(s.pane);
    } else {
      if (confirm) cmd.searchFor(query, s.pane);
      else cmd.searchCancel(s.pane);
      cmd.focusPane(s.pane);
    }
  } catch {
    // The pane is gone.
  }
  cmd.deleteBuffer(buf);
}

/** The input buffer pane `pane` shows. */
function bufferOf(pane: number): number | undefined {
  return cmd.panes().find((p) => p.id === pane)?.buffer;
}

on("InputChanged", (buf: number, text: string) => {
  const s = searches.get(buf);
  if (s === undefined) return;
  schedule(() => {
    if (s.visual) cmd.visual.searchPreview(text, s.pane, s.backward);
    else cmd.searchPreview(text, s.pane);
  });
});
on("BufferHidden", (buf: number) => schedule(() => endSearch(buf, false)));

// The input mode: editing (readline's keys), the search's <CR> and <Esc>,
// the prefix.
each("input", ["<BS>", "<C-h>"], action.input.backspace());
each("input", ["<Del>", "<C-d>"], action.input.delete());
// (Not readline's <C-b> and <C-f>: <C-b> is the prefix.)
each("input", ["<Left>"], action.input.left());
each("input", ["<Right>"], action.input.right());
each("input", ["<Home>", "<C-a>"], action.input.home());
each("input", ["<End>", "<C-e>"], action.input.end());
each("input", ["<M-b>", "<C-Left>"], action.input.wordLeft());
each("input", ["<M-f>", "<C-Right>"], action.input.wordRight());
each("input", ["<C-w>"], action.input.deleteWord());
each("input", ["<C-u>"], action.input.deleteToStart());
each("input", ["<C-k>"], action.input.deleteToEnd());
each("input", ["<CR>"], (ctx) => {
  const buf = bufferOf(ctx.pane);
  if (buf !== undefined) endSearch(buf, true);
});
each("input", ["<Esc>"], (ctx) => {
  const buf = bufferOf(ctx.pane);
  if (buf !== undefined) endSearch(buf, false);
});
each("input", ["<C-S-v>", "<D-v>"], action.paste());
each("input", [prefix], action.switchMode("normal"));

// Text panes (the `edit` mode): editing without modes, as most editors do.
// Typed text goes in (the mode passes it through); arrows move, with Shift
// they select; <C-s> writes, <C-z>/<C-y> undo and redo. A Vim-like editor
// is a script mapping its own modes (o.basemode = { edit: "..." }).
const sel = { select: true };
for (const [keys, unit, n] of [
  [["<Left>"], "grapheme", -1],
  [["<Right>"], "grapheme", 1],
  [["<Up>"], "displayLine", -1],
  [["<Down>"], "displayLine", 1],
  [["<Home>"], "lineStart", 1],
  [["<End>"], "lineEnd", 1],
  [["<C-Left>"], "wordStart", -1],
  [["<C-Right>"], "wordEnd", 1],
  [["<C-Home>"], "documentStart", 1],
  [["<C-End>"], "documentEnd", 1],
  [["<PageUp>"], "page", -1],
  [["<PageDown>"], "page", 1],
] as const) {
  each("edit", [...keys], action.text.move(unit, n));
  each("edit", keys.map((k) => k.replace("<", "<S-").replace("<S-C-", "<C-S-")), action.text.move(unit, n, sel));
}
each("edit", ["<CR>"], action.text.type("\n"));
each("edit", ["<Tab>"], action.text.type("\t"));
each("edit", ["<BS>"], action.text.erase("grapheme", -1));
each("edit", ["<Del>"], action.text.erase("grapheme", 1));
each("edit", ["<C-BS>"], action.text.erase("wordStart", -1));
each("edit", ["<C-Del>"], action.text.erase("wordEnd", 1));
each("edit", ["<Esc>"], action.text.clearSelection());
each("edit", ["<C-s>"], action.write());
each("edit", ["<C-z>"], action.text.undo());
each("edit", ["<C-y>", "<C-S-z>"], action.text.redo());
each("edit", ["<C-S-v>", "<D-v>"], action.paste());
each("edit", [prefix], action.switchMode("normal"));

// Unsaved changes are not lost by accident: deleting a text buffer, or
// ending the session (kill-session, closing the last pane), is refused
// while one has them (`kill-session --force` ends anyway). Closing a pane
// is fine: its buffer stays, hidden.
on("BufferDeletePre", (buf: number) => {
  const b = cmd.buffers().find((b) => b.id === buf);
  if (b?.modified) return { cancel: true, reason: `${b.name} has unsaved changes (write it first)` };
});
on("SessionEndPre", () => {
  const names = cmd.buffers().filter((b) => b.modified).map((b) => b.name);
  if (names.length > 0) {
    return { cancel: true, reason: `unsaved changes in ${names.join(", ")} (write them, or kill-session --force)` };
  }
});

// The visual mode (a terminal's output, selected and copied by keys): <C-b>[
// (tmux's copy-mode key) or v in the normal mode enter it, at the program's
// cursor with nothing selected; v, V, <C-v> select characters, rows or a
// block, y copies and leaves, <Esc> drops the selection, then leaves. The
// moves are Vim's (counts too); <C-b> stays the prefix (<PageUp> for a
// screen up).
each("normal", ["[", "v"], [action.exitMode(), action.switchMode("visual")]);
each("visual", ["h", "<Left>"], action.visual.left());
each("visual", ["l", "<Right>"], action.visual.right());
each("visual", ["k", "<Up>"], action.visual.up());
each("visual", ["j", "<Down>"], action.visual.down());
each("visual", ["0", "<Home>"], action.visual.lineStart());
each("visual", ["^"], action.visual.firstNonBlank());
each("visual", ["$", "<End>"], action.visual.lineEnd());
each("visual", ["w"], action.visual.wordForward());
each("visual", ["b"], action.visual.wordBackward());
each("visual", ["e"], action.visual.wordEnd());
each("visual", ["W"], action.visual.bigWordForward());
each("visual", ["B"], action.visual.bigWordBackward());
each("visual", ["E"], action.visual.bigWordEnd());
each("visual", ["gg"], action.visual.top());
each("visual", ["G"], action.visual.bottom());
each("visual", ["H"], action.visual.screenTop());
each("visual", ["M"], action.visual.screenMiddle());
each("visual", ["L"], action.visual.screenBottom());
each("visual", ["<C-u>"], action.visual.halfPageUp());
each("visual", ["<C-d>"], action.visual.halfPageDown());
each("visual", ["<PageUp>"], action.visual.pageUp());
each("visual", ["<C-f>", "<PageDown>"], action.visual.pageDown());
each("visual", ["<C-y>"], action.visual.scrollUp());
each("visual", ["<C-e>"], action.visual.scrollDown());
each("visual", ["[["], action.visual.prevPrompt());
each("visual", ["]]"], action.visual.nextPrompt());
each("visual", ["v"], action.visual.charwise());
each("visual", ["V"], action.visual.linewise());
each("visual", ["<C-v>"], action.visual.blockwise());
each("visual", ["o"], action.visual.swapEnds());
each("visual", ["y"], action.visual.yank());
each("visual", ["Y"], action.visual.yankLines());
each("visual", ["<Esc>"], action.visual.escape());
each("visual", ["q"], action.exitMode());
each("visual", ["/"], () => openSearch());
each("visual", ["?"], () => openSearch(true));
each("visual", ["n"], action.visual.nextMatch());
each("visual", ["N"], action.visual.prevMatch());
each("visual", [prefix], action.switchMode("normal"));
