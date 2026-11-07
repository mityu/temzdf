// temzdf's default configuration. The script host loads it before the user's
// configuration (`--clean` loads only this); the user's can map over or
// remove anything here: its mappings come first (they are registered later),
// and these are in the namespace "default" (`keymap.remove(mode, lhs, {
// namespace: "default" })` removes one, `keymap.clear({ namespace: "default" })`
// all of them). Built-in modes: `normal` (operating),
// `terminal` (pass-through, terminal panes), `pdf` (PDF panes), `input`
// and `edit` (input and text panes: typed characters go in) and `visual` (a terminal's visual
// mode: a cursor over the screen and scrollback, selecting and copying).
import { acceptPrompt, action, cancelPrompt, cmd, keymap, on, openPrompt, schedule } from "temzdf";
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

// Searching a PDF pane (/) or a terminal in the visual mode (/ and ?): a
// prompt (openPrompt) one row high below it. As the query is typed the
// pane shows its first match (searchPreview, as Vim's incsearch); <CR>
// searches, <Esc> cancels (back where it began), and either closes the
// prompt, back to the pane (and its visual mode). Closed any other way,
// the search is cancelled.
function openSearch(backward = false): void {
  const pane = cmd.focusedPane();
  if (pane === null) return;
  const visual = cmd.currentMode() === "visual";
  const back = () => {
    if (visual) cmd.switchMode("visual");
  };
  const guard = (f: () => void) => {
    try {
      f();
    } catch {
      // The pane is gone.
    }
  };
  openPrompt({
    prompt: backward ? "?" : "/",
    onChange: (query) => {
      if (visual) cmd.visual.searchPreview(query, pane, backward);
      else cmd.searchPreview(query, pane);
    },
    onLine: (query) =>
      guard(() => {
        back();
        if (visual) cmd.visual.searchFor(query, pane, backward);
        else cmd.searchFor(query, pane);
      }),
    onCancel: () =>
      guard(() => {
        back();
        if (visual) cmd.visual.searchCancel(pane);
        else cmd.searchCancel(pane);
      }),
  });
}

// The input mode (prompts): editing with readline's keys, <CR> runs the
// cursor's line, <Esc> cancels, the prefix. Pasted lines go in as they are.
each("input", ["<BS>", "<C-h>"], action.text.erase("grapheme", -1));
each("input", ["<Del>", "<C-d>"], action.text.erase("grapheme", 1));
// (Not readline's <C-b> and <C-f>: <C-b> is the prefix.)
each("input", ["<Left>"], action.text.move("grapheme", -1));
each("input", ["<Right>"], action.text.move("grapheme", 1));
each("input", ["<Home>", "<C-a>"], action.text.move("lineStart"));
each("input", ["<End>", "<C-e>"], action.text.move("lineEnd"));
each("input", ["<M-b>", "<C-Left>"], action.text.move("wordStart", -1));
each("input", ["<M-f>", "<C-Right>"], action.text.move("wordEnd", 1));
each("input", ["<Up>"], action.text.move("line", -1));
each("input", ["<Down>"], action.text.move("line", 1));
each("input", ["<C-w>"], action.text.erase("wordStart", -1));
each("input", ["<C-u>"], () => {
  const c = cmd.text.cursor();
  cmd.text.delete({ start: { line: c.line, character: 0 }, end: c });
});
each("input", ["<C-k>"], () => {
  const c = cmd.text.cursor();
  cmd.text.delete({ start: c, end: { line: c.line, character: cmd.text.line(c.line).length } });
});
each("input", ["<CR>"], () => acceptPrompt());
each("input", ["<Esc>"], () => cancelPrompt());
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
// A buffer with unsaved changes is not deleted, unless its variable
// forceDelete says so (Vim's :bd!).
on("BufferDeletePre", (buf: number) => {
  const b = cmd.buffers().find((b) => b.id === buf);
  if (b?.modified && cmd.getVar("buffer", "forceDelete", buf) !== true) return { cancel: true, reason: `${b.name} has unsaved changes (write it first)` };
});
on("SessionEndPre", () => {
  const names = cmd.buffers().filter((b) => b.modified).map((b) => b.name);
  if (names.length > 0) {
    return { cancel: true, reason: `unsaved changes in ${names.join(", ")} (write them, or kill-session --force)` };
  }
});

// A text buffer's file changed elsewhere: read again when it has no
// unsaved changes (Vim's autoread); otherwise say so (writing then needs
// write({ force: true }), or reload() to drop the changes).
on("FileChangedOnDisk", (buf: number) => {
  const b = cmd.buffers().find((b) => b.id === buf);
  if (b === undefined) return;
  if (b.modified) cmd.message(`${b.name} changed on disk (reload() drops your changes)`);
  else schedule(() => cmd.reload(buf));
});

// The visual mode (a terminal's output, selected and copied by keys): <C-b>[
// (tmux's copy-mode key) or v in the normal mode enter it, at the program's
// cursor with nothing selected; v, V, <C-v> select characters, rows or a
// block, y copies and leaves, <Esc> drops the selection, then leaves. The
// moves are Vim's (counts too); <C-b> stays the prefix (<PageUp> for a
// screen up).
each("normal", ["[", "v"], [action.exitMode(), action.switchMode("visual")]);
// <C-b>] (tmux's paste-buffer key) or p puts the newest copy (any pane's;
// doc/design/registers.md) into the focused pane.
each("normal", ["]", "p"], [action.exitMode(), action.put()]);
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
