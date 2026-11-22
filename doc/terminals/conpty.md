# ConPTY

What Windows' pseudoconsole (ConPTY) does to the sequences temzdf and the
programs in its terminal panes write and read. On Windows every terminal
talks to its programs through ConPTY (Windows Terminal, WezTerm, and
portable-pty, which temzdf's terminal panes use), so what it drops is lost
in all of them. Why it matters for the port: `doc/survey/windows-port.md`.

## How it was measured

Measured on 2026-11-23 with `experiments/conpty/` in a Windows 11 25H2 VM
(build 26200, the in-box ConPTY of `conhost.exe` 10.0.26100.1; no side-loaded
`conpty.dll`). `Host.cs` opens a pseudoconsole with
`CreatePseudoConsole`, starts `Child.cs` in it and records every byte
ConPTY writes to the output pipe; `Child.cs` writes a fixed list of
sequences, each after a marker `<<N>>`, then reads its console input in
`ENABLE_VIRTUAL_TERMINAL_INPUT` mode while the host writes what a terminal
would send. `run.ps1` compiles both with the .NET Framework's `csc` (in
every Windows install, nothing to download) and runs them with flags 0
(what portable-pty passes) and 8 (`PSEUDOCONSOLE_PASSTHROUGH_MODE`).

**Both flags gave the same bytes** (apart from the `id` ConPTY puts into
OSC 8): the in-box ConPTY accepts the pass-through flag and ignores it.

## Output: what the program writes

| Sequence | Reaches the terminal |
|---|---|
| kitty graphics, `ESC _ G ... ESC \` (transmit and query) | **no**: dropped, no reply either |
| kitty keyboard, `CSI > 1 u`, `CSI ? u` | yes, as written |
| mouse modes `CSI ? 1000 h`, `1006`, `1016` (SGR pixels) | yes |
| bracketed paste `CSI ? 2004 h` | yes |
| synchronized output `CSI ? 2026 h` / `l` | yes |
| `CSI 14 t`, `CSI 16 t` (sizes in pixels) | yes |
| OSC 52 (clipboard), OSC 7 (working directory) | yes |
| OSC 8 (hyperlink) | rewritten: ConPTY adds `id=PID-N` |
| truecolor SGR | yes (the reset comes as `CSI m`) |
| DA1 `CSI c` | **answered by ConPTY**, not forwarded: `CSI ? 61;6;7;21;22;23;24;28;32;42 c` |
| DECRQM `CSI ? 2026 $ p` | **answered by ConPTY**, not forwarded: `CSI ? 2026;0 $ y` (not recognised) |
| XTGETTCAP `DCS + q ... ST` | **no**: dropped, no reply |

ConPTY's own output: `CSI ? 9001 h` (win32-input-mode) and `CSI ? 1004 h`
(focus reports) at the start, `l` for both at the end; a title (OSC 0,
the child's path); text is redrawn by ConPTY (`CSI 2 J`, `CSI H`, cursor
hiding), not copied.

## Input: what the terminal sends

Everything written to the input pipe reached the program unchanged, in
the same chunks: plain keys, kitty keyboard `CSI 97;5 u`, SGR mouse press
and release, bracketed paste, and replies (DA1, `CSI ? 1 u`, the kitty
graphics `ESC _ G i=31;OK ESC \`, `CSI 4;800;1280 t`, DECRPM, an XTGETTCAP
`DCS 1 + r ...`). Only the focus report `CSI I` was consumed (ConPTY asked
for focus reports itself).

## What follows

- Inside a terminal pane on Windows, programs can use the kitty keyboard
  protocol, the SGR-pixel mouse and OSC 52, but **not kitty graphics**:
  APC never leaves ConPTY. Capability probes see ConPTY's DA1, which says
  nothing about the real terminal.
- For temzdf's own terminal frontend on Windows the same holds: drawn with
  kitty graphics, it cannot show anything through an in-box ConPTY,
  whatever the terminal is. The GUI client stays the frontend there.
- Not tried yet: a side-loaded newer `conpty.dll` with `OpenConsole.exe`
  (Windows Terminal's), where the pass-through flag may be implemented;
  and the same probes inside real terminals (Windows Terminal, WezTerm).
