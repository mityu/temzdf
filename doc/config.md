# Configuration

`temzdf` reads `$XDG_CONFIG_HOME/temzdf/config.toml` (falling back to
`~/.config/temzdf/config.toml`), or the file given with `--config FILE`. A
missing default file is fine; an explicitly given missing file is an error.
Every key is optional. Unknown keys are errors, so typos do not go unnoticed.
The parser lives in `crates/temzdf/src/config.rs`; keep this file in sync.

```toml
[terminal]
# How frames reach the terminal: "auto" (detect), "layered" (upload images once,
# re-place them; kitty/Ghostty) or "composited" (send whole frames; WezTerm/zellij).
backend = "auto"
# How pixels are transferred: "auto", "direct" (inline, works over SSH),
# "shm" (POSIX shared memory) or "file" (temporary files).
medium = "auto"
# zlib level (0-9) for inline transfers. Do not use 1 with Ghostty 1.3.1: it
# crashes on fixed-Huffman streams (see doc/terminal-compat.md).
zlib_level = 2

[scroll]
# Pixels per wheel step, before flick acceleration.
step = 60
# Easing time constant in milliseconds (smaller = snappier).
smoothness_ms = 60

[links]
# Command for external links; the URI is appended. Default: xdg-open / open.
open_command = ["xdg-open"]

[reload]
# Wait this long after the PDF last changed before reloading.
debounce_ms = 200
# How the reading position survives a reload: "position" keeps the same page
# and place on it (like Skim and Okular); "text" finds the line a third of the
# way down the screen again in the new PDF and keeps it at the same height, so
# text inserted above it does not shift the view. "text" falls back to the
# position when the line is gone (it searches two pages either side).
anchor = "position"

[synctex]
# Inverse search (Ctrl+click), run with sh -c. %{input} (shell-quoted path),
# %{line} and %{column} are substituted. See doc/synctex.md for editor setups.
editor = "vim --servername VIM --remote-silent +%{line} %{input}"
```

Precedence: environment variables (`TERMPIX_BACKEND`, `TERMPIX_MEDIUM`) override
the file, which overrides detection.
