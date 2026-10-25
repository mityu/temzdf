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
# If it cannot start or exits with an error (e.g. xdg-open over SSH without a
# display), the status bar says so.
open_command = ["xdg-open"]
# Preview internal links (references, citations, the table of contents) when
# the pointer rests on one for preview_delay_ms. In the preview the wheel
# scrolls, clicking a link follows it (external ones open and the preview
# stays), and clicking elsewhere jumps there. Hovering an external link shows
# its URI in the status bar.
preview = true
preview_delay_ms = 300

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

## Key bindings

`[keys]` maps a command to a list of keys. Listing a command replaces all of its
default keys; an empty list unbinds it; commands not listed keep their
defaults. A key listed for a command is taken away from whatever command it had
by default. Binding one key to two commands is an error.

```toml
[keys]
quit = ["q", "C-q"]
page_down = ["Space", "C-d"]
copy = []
```

Key notation (shared with `--keys`): a single character (`j`, `G`, `+`, `/`)
or a name (`Tab`, `Enter`, `Esc`, `Space`, `Backspace`, `Delete`, `Insert`,
`Up`, `Down`, `Left`, `Right`, `Home`, `End`, `PageUp`, `PageDown`, `F1`-`F35`),
after any of the prefixes `C-` (Ctrl), `M-` (Alt), `S-` (Shift) and `D-`
(Super). Shifted characters are written as the character (`G`, not `S-g`); `S-`
is for named keys (`S-Tab`).

| Command | Default keys | |
|---|---|---|
| `quit` | `q` `C-c` | |
| `cancel` | `Esc` | clear the selection, else the search, else quit |
| `scroll_down` / `scroll_up` | `j` `Down` / `k` `Up` | takes a count |
| `scroll_left` / `scroll_right` | `h` `Left` / `l` `Right` | |
| `page_down` / `page_up` | `Space` `PageDown` `C-f` / `b` `PageUp` | |
| `half_page_down` / `half_page_up` | `d` / `u` | |
| `first_page` | `g` `Home` | page N with a count (`3g`) |
| `last_page` | `G` `End` | page N with a count (`3G`) |
| `back` / `forward` | `C-o` `Backspace` `M-Left` / `M-Right` | link history |
| `zoom_in` / `zoom_out` / `fit_width` | `+` `=` / `-` / `0` `w` | |
| `search` / `next_match` / `prev_match` | `/` / `n` / `N` | |
| `copy` | `y` | copy the selection |
| `toggle_sidebar` | `Tab` | outline |

Fixed, not configurable: digits before a command are a count (`0` only after
another digit), the outline sidebar's own keys while it has focus, and the
one-line input (search prompt). Mouse bindings are fixed too. Defaults live in
`crates/temzdf/src/keys.rs`; keep this table in sync.
