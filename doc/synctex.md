# SyncTeX

Compile with SyncTeX enabled (`latexmk -pdf -synctex=1`, or `pdflatex
-synctex=1`) so that `name.synctex.gz` sits next to `name.pdf`.

## Forward search (editor -> viewer)

```sh
temzdf --forward LINE[:COLUMN]:FILE doc.pdf
```

If a viewer already has `doc.pdf` open, the request goes to it over a Unix
socket (`$XDG_RUNTIME_DIR/temzdf/<hash>.sock`, see `crates/temzdf/src/ipc.rs`)
and the command exits immediately. The viewer scrolls to the matching place
(unless it is already comfortably visible) and highlights it in red for 1.5 s.
Otherwise `temzdf` starts a viewer in the current terminal and does the search
once the document is shown.

`FILE` is matched the way SyncTeX does it: as given, then by its file name.

A request that arrives while a reload is pending or running (the PDF just
changed) waits until the new version is open and its SyncTeX data is re-read,
so a forward search sent right after a compile lands in the new version.

## Inverse search (viewer -> editor)

Ctrl+click on the page. The viewer looks up the source location and runs the
configured command with `sh -c`, replacing `%{input}` (shell-quoted absolute
path), `%{line}` and `%{column}` (1-based; usually 0, TeX rarely records
columns). Configure it in `config.toml`:

```toml
[synctex]
editor = "..."
```

or per invocation with `--editor '...'`. Without a command, the location is
shown in the status bar.

### Vim (with `+clientserver`)

```toml
[synctex]
editor = "vim --servername VIM --remote-silent +%{line} %{input}"
```

Start Vim with `vim --servername VIM`.

### Neovim

Start Neovim with a server socket (`nvim --listen /tmp/nvim-tex.sock`), then:

```toml
[synctex]
editor = "nvim --server /tmp/nvim-tex.sock --remote %{input} && nvim --server /tmp/nvim-tex.sock --remote-send '<C-\\><C-n>:%{line}<CR>'"
```

### vimtex

`temzdf` needs a terminal, so start it yourself (e.g. in a terminal split) and
let vimtex only send forward searches:

```vim
let g:vimtex_view_method = 'general'
let g:vimtex_view_general_viewer = 'temzdf'
let g:vimtex_view_general_options = '--forward @line:@tex @pdf'
```

When no viewer is running, that command would try to open one inside vimtex's
background job, which has no terminal and fails; open the viewer first.

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

- Vendored SyncTeX parser 1.21 (`crates/temzdf/vendor/synctex/`, MIT-style),
  compiled by `crates/temzdf/build.rs` with zlib from `libz-sys`; bindings and a
  safe wrapper in `crates/temzdf/src/synctex.rs`.
- Queries run on a worker thread (`workers::spawn_synctex`); the `.synctex.gz`
  is parsed lazily and again after every reload.
- The parser prints errors to stderr; in interactive mode stdout/stderr are
  redirected to `$XDG_STATE_HOME/temzdf/temzdf.log` so nothing scribbles over the
  screen (`termpix::tty::StdioRedirect`).
- Tests: `synctex::tests` (forward and inverse on the fixture) and
  `tests/e2e/synctex.actions` (a second process sends a forward search to a
  running viewer; Ctrl+click runs the editor command) and
  `tests/e2e/synctex-reload.actions` (a forward search sent right after the
  PDF and SyncTeX data were replaced lands in the new version).
