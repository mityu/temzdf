# ConPTY

What Windows' pseudoconsole (ConPTY) does to the sequences temzdf and the
programs in its terminal panes write and read. On Windows every terminal
talks to its programs through ConPTY (Windows Terminal, WezTerm, and
portable-pty, which temzdf's terminal panes use), so what it drops is lost
in all of them. Why it matters for the port: `doc/survey/windows-port.md`.
How temzdf's terminal panes use it: "In temzdf's terminal panes" below.

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
- A side-loaded `conpty.dll` with its `OpenConsole.exe` changes this, see
  the next section.
- Not tried yet: the same probes inside real terminals (Windows Terminal,
  WezTerm).

## Side-loaded conpty.dll

Measured on 2026-11-23 in the same VM image, with the NuGet package
`Microsoft.Windows.Console.ConPTY` 1.25.261109001 (MIT, latest stable; the
x64 `runtimes/win-x64/native/conpty.dll` and
`build/native/runtimes/x64/OpenConsole.exe`, both in one directory).
`Host.cs` takes the DLL's path as an optional fifth argument and calls its
`CreatePseudoConsole` / `ClosePseudoConsole` (LoadLibrary, GetProcAddress);
`run.ps1 -ConptyDir DIR` runs in-box and side-loaded, each with flags 0
and 8. Raw logs of that run: `target/conpty/` (not committed).

Flags 0 and 8 gave the same bytes again, but this time the stream
differs from the in-box one in every flag:

| What | In-box (`conhost.exe`) | Side-loaded (`OpenConsole.exe`) |
|---|---|---|
| kitty graphics (transmit and query) | dropped | **passed as written** |
| XTGETTCAP `DCS + q` | dropped | **passed as written** |
| DA1 `CSI c` | answered by ConPTY, not forwarded | **forwarded**, not answered by ConPTY (the terminal's own answer is the program's only one) |
| DECRQM `CSI ? 2026 $ p` | answered by ConPTY | **forwarded** |
| kitty keyboard, mouse (1016 too), 2004, 2026, `14t`, `16t`, OSC 52, OSC 7, truecolor, OSC 8 | passed (OSC 8 gets an `id`) | passed; OSC 8 as written |
| the program's text | redrawn by ConPTY (`CSI 2 J`, `CSI H`, cursor hiding, a title) | **copied** with the program's own line endings, no redraw, no title |
| input: keys, kitty keyboard, SGR mouse, paste, replies (DA1, `CSI ? 1 u`, kitty graphics, `14t`, DECRPM, XTGETTCAP) | unchanged | unchanged |
| focus report `CSI I` | consumed | consumed |

ConPTY's own start-up output: `CSI 1 t`, `CSI c` (it asks the terminal,
and the host here never answered), `CSI ? 1004 h`, `CSI ? 9001 h`,
`CSI ? 7 l`; `l` for 1004 and 9001 at the end.

What follows: with a side-loaded `conpty.dll` (as Windows Terminal ships
it) kitty graphics can reach the real terminal, so the terminal
frontend and graphics programs in terminal panes are possible there.
It needs `conpty.dll` and `OpenConsole.exe` shipped beside temzdf, and
portable-pty (or temzdf's own wrapper) to load `conpty.dll` instead of
kernel32's functions. Not checked: whether a real terminal then shows the
image, and what an unmatched `conpty.dll` / `OpenConsole.exe` pair does.

## In temzdf's terminal panes

Since 2026-11-23 terminal panes on Windows run their programs in the
in-box ConPTY through portable-pty 0.9.0 (no `conpty.dll` shipped, the
user's choice; design: `doc/design/session-server.md`, "Windows"), so
images from programs in panes are lost, as above. What portable-pty does
and what was seen in the VM (Windows 11 build 26300, cmd.exe and Windows
PowerShell 5.1 in `temzdf-gui`):

- portable-pty passes `PSEUDOCONSOLE_INHERIT_CURSOR | RESIZE_QUIRK |
  WIN32_INPUT_MODE`. With the inherited cursor ConPTY starts by asking
  `CSI 6 n`, which portable-pty's users must answer (it waits for the
  reply; not tried here without one); the pane's terminal
  (libghostty) answers it as any query (`tests::conpty` in
  `crates/temzdf-term/src/lib.rs`). ConPTY also asks for win32-input-mode
  (`CSI ? 9001 h`); the pane's keys still went as plain VT bytes, and
  ConPTY took them. Typing, Backspace, the arrows (cmd's and
  PowerShell's history), Tab completion, PowerShell's colours, resizing
  (`mode con`, `$Host.UI.RawUI.WindowSize`) and `exit` worked.
- Ctrl+C: ConPTY turns the byte 0x03 into a Ctrl+C event for the
  console, but a process ignores it when its parent did and it inherited
  that. A server a client starts with `CREATE_NEW_PROCESS_GROUP` ignores
  Ctrl+C (and so did the tests' processes under OpenSSH in the VM), so `ping -t`
  in a pane went on. The server clears the flag before starting a pane's
  program (`SetConsoleCtrlHandler(NULL, FALSE)`, `pty::before_spawn`);
  since then Ctrl+C ends it.
- portable-pty loads a `conpty.dll` it finds on the DLL search path (the
  executable's directory first) in place of kernel32's functions, so
  one placed beside `temzdf.exe` would be used (not tried with temzdf).
- Not tried yet: the kitty keyboard protocol and the mouse from a
  program in a pane (measured through ConPTY with flags 0 and 8 above,
  not with portable-pty's).
