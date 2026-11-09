// temzdf's default configuration. The script host loads it before the user's
// configuration (`--clean` loads only this). It only enables the shipped
// modules; each defines its mappings (in a namespace of its own) and hooks
// when enabled, and takes them away when disabled, so a configuration
// changes the defaults with `import vim from "temzdf/edit/vim";
// vim.disable();` (or maps over them: its mappings come later and are
// tried first). Built-in modes: `normal` (operating), `terminal`
// (pass-through, terminal panes), `pdf` (PDF panes), `markdown` (Markdown
// viewers' graphics mode), `input` (prompts) and
// `edit` (text panes: typed characters go in), `visual` (a terminal's
// visual mode).
import buffers from "temzdf/buffers";
import vim from "temzdf/edit/vim";
import input from "temzdf/mode/input";
import normal from "temzdf/mode/normal";
import pdf from "temzdf/pane/pdf";
import markdown from "temzdf/pane/markdown";
import terminal from "temzdf/pane/terminal";

pdf.enable();
terminal.enable();
normal.enable();
input.enable();
buffers.enable();
// Text panes edit as Vim does (temzdf/edit/notepad: without modes).
vim.enable();
// After vim: its text-mode keys (<CR> on a link) are tried first.
markdown.enable();
