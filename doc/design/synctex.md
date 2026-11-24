# SyncTeX

Compile with SyncTeX enabled (`latexmk -pdf -synctex=1`, or `pdflatex
-synctex=1`) so that `name.synctex.gz` sits next to `name.pdf`.

## Forward search (editor -> viewer)

```sh
temzdf --forward LINE[:COLUMN]:FILE doc.pdf
```

If a viewer or a session's pane already has `doc.pdf` open, the request goes to it over a Unix
socket (`$XDG_RUNTIME_DIR/temzdf/<hash>.sock`; on macOS under the per-user
`/var/folders/.../T/`; see `crates/temzdf-server/src/forward.rs`)
and the command exits immediately. The viewer scrolls the matching place 30 % down
the screen (unless it is 10-85 % down already, as for a search hit) and
highlights it in red for 1.5 s (`o.forwardflash`: another length in
milliseconds, 0 for none, or `"input"` to keep it until the next key,
click, wheel step or drag; moving the pointer leaves it).
Otherwise `temzdf` starts a new session showing `doc.pdf` (named as `temzdf
FILE` names it, or `-s NAME`), sends the search once the pane listens, and
attaches. `temzdf standalone pdf --forward ... doc.pdf` starts the standalone
viewer in the current terminal instead.

`FILE` is matched against the input names SyncTeX recorded: the same file
on disk (through symlinks and `..`, ignoring case on macOS), then the same
absolute path, then a name ending with the given path's components, then the
same file name (`choose_input` in `crates/temzdf-pdfcore/src/synctex.rs`).

A request that arrives while a reload is pending or running (the PDF just
changed) waits until the new version is open and its SyncTeX data is re-read,
so a forward search sent right after a compile lands in the new version.

## Inverse search (viewer -> editor)

Ctrl+click on the page. The viewer looks up the source location and hands
it to the configuration's `SynctexInverse` hooks (`doc/config.md`) with the
pane clicked: `{ file, line, column }`, the file's absolute path, the line
(1-based) and the column when TeX recorded one (rarely; else undefined).
The hook does what it likes: start an editor with `temzdf/process`, open the
file in a text pane of temzdf's own. Without a hook, the location is shown in
the pane's status bar. (It replaced the shell command template
`o.synctexeditor` and `--editor`, 2026-11-25: a function may do anything,
and the server runs it, so it needs no shell on the client's side.)

The command runs on the session server's machine, with the server's
environment (where it was started), not the client's.

### Vim (with `+clientserver`)

Start Vim with `vim --servername VIM`, then:

```ts
import { dispatch, on } from "temzdf";
import { run } from "temzdf/process";
on("SynctexInverse", (_pane, { file, line }) => {
  dispatch(() => run(["vim", "--servername", "VIM", "--remote-silent", `+${line}`, file]).then(() => {}));
});
```

### Neovim

Start Neovim with a server socket (`nvim --listen /tmp/nvim-tex.sock`), then:

```ts
import { dispatch, on } from "temzdf";
import { run } from "temzdf/process";
const sock = "/tmp/nvim-tex.sock";
on("SynctexInverse", (_pane, { file, line }) => {
  dispatch(async () => {
    await run(["nvim", "--server", sock, "--remote", file]);
    await run(["nvim", "--server", sock, "--remote-send", `<C-\\><C-n>:${line}<CR>`]);
  });
});
```

### temzdf's own text panes

In a tab page of its own (`action.edit` would replace the PDF pane):

```ts
import { action, on } from "temzdf";
on("SynctexInverse", (_pane, { file, line }) => {
  const pane = action.newTab({ file });
  if (pane !== undefined) action.text.setCursor({ line: line - 1, character: 0 }, { pane });
});
```

### vimtex

`temzdf` needs a terminal, so start it yourself (e.g. in a terminal split) and
let vimtex only send forward searches:

```vim
let g:vimtex_view_method = 'general'
let g:vimtex_view_general_viewer = 'temzdf'
let g:vimtex_view_general_options = '--forward @line:@tex @pdf'
```

When no viewer is running, that command would start a session, which needs a
terminal: inside vimtex's background job it fails ("no terminal") and leaves
nothing behind; open the viewer first.

To follow the cursor after every successful compile (the editor knows where you
are in the *new* source, so this keeps the view on what you are editing even
when text was inserted above it):

```vim
augroup temzdf
  autocmd!
  autocmd User VimtexEventCompileSuccess VimtexView
augroup END
```

vimtex runs the forward search right after latexmk finishes; temzdf holds it
until its auto-reload has picked up the new PDF (see above). Without vimtex,
any compile hook can do the same with `temzdf --forward LINE:FILE doc.pdf`.

## Implementation

- Vendored SyncTeX parser 1.21 (`crates/temzdf-pdfcore/vendor/synctex/`, MIT-style),
  compiled by `crates/temzdf-pdfcore/build.rs` with zlib from `libz-sys`; bindings and a
  safe wrapper in `crates/temzdf-pdfcore/src/synctex.rs`.
- Queries run on a worker thread (`workers::spawn_synctex`); the `.synctex.gz`
  is parsed lazily and again after every reload.
- The parser prints errors to stderr. It runs in the session server, whose
  output goes to `NAME.log` in the log directory (`temzdf ls -v` shows it);
  the standalone viewer redirects stdout/stderr to `temzdf.log` there
  (`doc/config.md`) so nothing scribbles over the screen
  (`termpix::tty::StdioRedirect`).
- Tests: `synctex::tests` (forward and inverse on the fixture) and
  `tests/e2e/synctex.ts` (a second process sends a forward search to a
  running viewer; Ctrl+click runs the editor command) and
  `tests/e2e/synctex-reload.ts` (a forward search sent right after the
  PDF and SyncTeX data were replaced lands in the new version).
