# Terminal compatibility

Snapshot of research done on 2026-10-25 from sources, changelogs and issues.
`?` means unverified. Update this file whenever behaviour is confirmed or
disproved in practice.

Versions at the time: kitty 0.49.2, Ghostty 1.3.1 (tip has 1.4 features),
WezTerm last stable 20240203 (nightlies active), iTerm2 3.7.6, tmux 3.8,
zellij 0.45.1.

## Kitty graphics

| Feature | kitty | Ghostty 1.3.1 | WezTerm (nightly) | Konsole | iTerm2 3.7 | foot |
|---|---|---|---|---|---|---|
| Basic `a=T/t/p`, ids | yes | yes | yes | yes | yes | no (sixel only) |
| `t=d` | yes | yes | yes | yes (only medium) | yes | – |
| `t=f` / `t=t` | yes | yes | yes / ? | no | yes | – |
| `t=s` shm | yes | yes | Linux yes, macOS broken (#7631) | no | yes | – |
| `o=z` | yes | yes | ? | yes | ? | – |
| Crop + `c,r` | yes | yes | glitchy (#5892) | yes | ? | – |
| z-index below text | yes | yes | **not respected** (#7222) | parsed | ? | – |
| Unicode placeholders | yes | yes | no (PR #7924 open) | no | yes | – |

Known problems:

- WezTerm: placement-move glitches (#5892), delete-by-placement fails after
  resize (#2422), aspect ratio (#5402), crash when repositioning with DECSTBM
  scrolling (#7953), no pixel size on WSL (#6781). ratatui-image and yazi avoid
  kitty graphics on WezTerm entirely and use the iTerm2 protocol. We plan the
  `Composited` backend for it.
- Ghostty: older placement/deletion bugs are closed; require >= 1.3.1. No `a=f`
  frame edits in 1.3.
- tmux: no native kitty graphics. Needs `allow-passthrough on` and DCS wrapping;
  direct placements bleed across panes. Only Unicode placeholders are robust.
  Replies are not routed back, so probes fall back to inline.
- zellij 0.45: native kitty graphics per pane, but rejects `U=1` (#5530), no
  `t=s` (#5626), ignores `x/y` without `c/r` (#5567). Use `t=d` and always pass
  `c,r`.

## Input and output modes

| Feature | kitty | Ghostty | WezTerm | foot | Konsole | iTerm2 |
|---|---|---|---|---|---|---|
| SGR-pixel mouse (1016) | yes | yes | yes | yes (>= 1.11) | master | ? |
| Kitty keyboard protocol | full | yes | yes (`enable_kitty_keyboard`, off by default ?) | yes | master | yes (flags ?) |
| Synchronized output (2026) | yes | yes | yes, but DECRQM reports "off": send it unconditionally | yes | master | yes |
| In-band resize (2048) | yes | yes | no | ? | ? | ? |

- Wheel input is discrete everywhere (buttons 64/65, 66/67 horizontal). No
  terminal forwards pixel deltas or pinch gestures to applications.
- kitty: with mouse tracking on, trackpad deltas are accumulated and emitted as
  one button event per step. Ghostty and Konsole behave similarly.
- Shift+click bypasses application mouse reporting in most terminals.
- tmux: 1016 passthrough unverified; fall back to cell coordinates.
