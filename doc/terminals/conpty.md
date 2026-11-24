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
- Inside real terminals (Windows Terminal, WezTerm) the same holds:
  "Inside Windows Terminal and WezTerm" below.

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

## Inside Windows Terminal and WezTerm

What a console program sees when a real terminal runs it, measured on
2026-11-24 in the VM (Windows 11 build 26300) with Windows Terminal
1.24.11911.0 (in the image; the default terminal) and WezTerm
20240203-110809-5046fc22 (winget `wez.wezterm`, the latest stable; it
ships its own `conpty.dll` and `OpenConsole.exe`). The probe, a C#
console program like `Child.cs` started on the desktop by a scheduled
task (`wt.exe -w new PROBE`, `wezterm-gui start -- PROBE`, and PROBE
alone, which the default terminal takes over), set
`ENABLE_VIRTUAL_TERMINAL_INPUT`, wrote each query 500 ms after the last
and logged every byte it read with the time; then a 96x96 RGB image with
`a=T,t=d` (chunked), then mouse modes 1000, 1002, 1006 and 1016 and kitty
keyboard flags 9 (`CSI > 9 u`), and logged the input QMP sent (a click,
a drag, a wheel notch, `a`, Ctrl+A, Escape, Up). WezTerm needed
`--config "front_end='WebGpu'"` in the VM (with the default OpenGL and
with `Software` it failed: "The OpenGL implementation is too old to work
with glium"). Raw logs and screenshots: `target/conpty-terminals/` (not
committed).

| Item | Windows Terminal 1.24 | WezTerm 20240203 |
|---|---|---|
| kitty graphics `a=T,t=d` | **not shown** (screenshot: blank lines) | **not shown** |
| kitty graphics `a=q` | no answer | no answer |
| DA1 `CSI c` | `CSI ? 61;4;6;7;14;21;22;23;24;28;32;42;52 c`, within 2 ms | `CSI ? 61;6;7;22;23;24;28;32;42 c`, within 1 ms (ConPTY's; WezTerm's own is never seen) |
| XTVERSION `CSI > 0 q` | no answer | `DCS > \| WezTerm 20240203-110809-5046fc22 ST`, after 25 to 35 ms |
| XTGETTCAP `TN` | no answer | no answer |
| DECRQM 2026 | `CSI ? 2026;2 $ y` (reset: known) | `CSI ? 2026;0 $ y` (not recognised; ConPTY's answer, at once) |
| DECRQM 1016 | `CSI ? 1016;0 $ y` | `CSI ? 1016;0 $ y` (but 1016 works, below) |
| kitty keyboard `CSI ? u` | no answer | no answer; with `enable_kitty_keyboard=true`: `CSI ? 0 u`, and `CSI ? 9 u` after `CSI > 9 u` |
| `CSI 14 t` | `CSI 4;600;1200 t` | `CSI 4;528;800 t`, after about 30 ms |
| `CSI 16 t` | `CSI 6;20;10 t` | `CSI 6;22;10 t`, after about 30 ms |
| `CSI 18 t` | `CSI 8;30;120 t` (the grid) | `CSI 8;24;80 t` (the grid), at once |
| SGR mouse with 1016 | **cells**: `CSI < 0;44;20 M` (column 44, row 20 of 9x19 cells) | **pixels**: `CSI < 0;332;330 M`; drag `32`, wheel `65` |
| keys with kitty flags 9 | legacy: `a`, `0x01`, `ESC`, `CSI A` | legacy as WT; with `enable_kitty_keyboard=true`: `a`, `CSI 57442;5 u` (Ctrl alone), `CSI 97;5 u`, `CSI 27;1 u`, `CSI 1;1 A` |

- Windows Terminal answers `CSI 14 t` and `CSI 16 t` at once with a cell
  of 10x20 that is not the one drawn: the screenshots show 9x19 cells
  (and the mouse's cells fit 9x19), and after three Ctrl+= zooms the
  glyphs grew while `16 t` still said 10x20 and `14 t` the grid times
  10x20 (`CSI 8;24;90 t`, `CSI 4;480;900 t`). There is no usable cell
  size in pixels in Windows Terminal. WezTerm's are the drawn ones
  (after two zooms `CSI 6;26;11 t`).
- The default terminal (a console program started alone) is Windows
  Terminal: same answers, but `WT_SESSION` is not set there. WezTerm sets
  `TERM=xterm-256color` and `TERM_PROGRAM=WezTerm`.
- WezTerm's `conpty.dll` with `Host.cs` (`run.ps1 -ConptyDir`, flags 0)
  behaves as the in-box ConPTY above: kitty graphics (transmit and query)
  and XTGETTCAP dropped, DA1 and DECRQM answered by ConPTY, the text
  redrawn, 14t and 16t and the rest passed, input unchanged. So WezTerm's
  missing image is its ConPTY's doing; with a newer `conpty.dll` WezTerm
  draws it ("WezTerm with the newer ConPTY" below). Windows
  Terminal's package has `OpenConsole.exe` but no `conpty.dll` to load
  that way.
- termpix's probe (`crates/termpix/src/probe.rs`) writes its queries in
  one write and stops at the DA1 answer. Sent that way: in Windows
  Terminal DECRQM 1016, 2026, `16 t` and DA1 came at once, nothing else;
  in WezTerm DECRQM 1016 and 2026 and DA1 came within 2 ms and XTVERSION
  and `16 t` 32 ms later, after DA1: the probe would miss WezTerm's
  name and cell size.

What follows: from a console program on Windows, neither terminal shows
kitty graphics, so temzdf's terminal frontend shows no page in either;
the GUI client is the frontend on Windows. WezTerm passes the SGR-pixel
mouse and, when enabled, the kitty keyboard; Windows Terminal gives cell
mouse reports, legacy keys and a made-up pixel size.

### WezTerm with the newer ConPTY

Measured on 2026-11-24 in the same VM: a copy of WezTerm's install
directory with its `conpty.dll` and `OpenConsole.exe` replaced by the
NuGet 1.25.261109001 ones (as in "Side-loaded conpty.dll"; WezTerm
loads both from its own directory, and the pane's `OpenConsole.exe` was
the copy's), the same probe, `--config "front_end='WebGpu'" --config
enable_kitty_keyboard=true`. Two runs gave the same answers. The guest
was busy (orphaned `OpenConsole.exe` processes of an earlier run used
the CPU), so WezTerm's window took about a minute to appear and the
probe's first write blocked until then; the times below are from after
that. Raw logs and screenshots: `target/conpty-terminals/conpty125/`
(not committed).

| Item | WezTerm's `conpty.dll` | NuGet ConPTY 1.25 |
|---|---|---|
| kitty graphics `a=T,t=d` | not shown | **shown** (the four quadrants, 96x96 px) |
| kitty graphics `a=q` | no answer | `APC G i=31;OK ST`, after 35 to 50 ms |
| DA1 `CSI c` | ConPTY's `CSI ? 61;6;7;22;23;24;28;32;42 c`, at once | **WezTerm's** `CSI ? 65;4;6;18;22 c` (4: sixel), after 15 to 50 ms (once 1.4 s, while busy); the only answer |
| XTVERSION | WezTerm's, after about 30 ms | WezTerm's, after 15 ms |
| XTGETTCAP `TN` | no answer | `DCS 1 + r 544E=57657A5465726D ST` ("WezTerm"), after 15 ms |
| DECRQM 2026 | `0` (ConPTY's) | `2` (WezTerm's) |
| DECRQM 1016 | `0` (ConPTY's) | `2` (WezTerm's) |
| kitty keyboard `CSI ? u` | `CSI ? 0 u`; `9` after `CSI > 9 u` | the same |
| `CSI 14 t` / `16 t` / `18 t` | `4;528;800` / `6;22;10` / `8;24;80`, after about 30 ms (18t at once) | the same, all three after about 33 ms |
| SGR mouse with 1016 | pixels | pixels: click `CSI < 0;257;205 M`/`m`, drag `32`, wheel `65` |
| keys with kitty flags 9 | `a`, `CSI 57442;5 u`, `CSI 97;5 u`, `CSI 27;1 u`, `CSI 1;1 A` | the same |

- termpix's one-write probe (`crates/termpix/src/probe.rs`'s queries):
  every answer came within 3 ms (14 ms in the other run), in the order asked: XTVERSION in one
  read, then the kitty graphics `OK`, DECRPM 1016 and 2026 (`2`),
  `CSI ? 0 u`, `16 t` and DA1 last in the next. Nothing arrives before
  the terminal's DA1 any more, so stopping at DA1 sees all of it.
- So the NuGet ConPTY makes WezTerm on Windows a kitty-graphics terminal
  for console programs, with everything termpix probes for. Shipping
  `conpty.dll` and `OpenConsole.exe` with temzdf is not decided.
- portable-pty 0.9.0 opens `conpty.dll` by name (`LoadLibrary`'s search
  order, `PATH` included) before falling back to kernel32's functions. In
  this VM WezTerm's directory is on the machine `PATH` (`where.exe
  conpty.dll` finds WezTerm's), so a program using portable-pty there
  gets WezTerm's `conpty.dll` and `OpenConsole.exe`, not the in-box
  ConPTY. 59 orphaned `OpenConsole.exe` from WezTerm's directory (flags
  `--inheritcursor --win32input --resizeQuirk`, portable-pty's), their
  parents gone, were each using CPU.

## In temzdf's terminal panes

Since 2026-11-23 terminal panes on Windows run their programs in the
in-box ConPTY through portable-pty 0.9.0 (no `conpty.dll` shipped, the
user's choice; design: `doc/design/session-server.md`, "Windows"), so
images from programs in panes are lost, as above. What portable-pty does
and what was seen in the VM (Windows 11 build 26300, cmd.exe and Windows
PowerShell 5.1 in `temzdf-gui`):

- portable-pty passes `PSEUDOCONSOLE_INHERIT_CURSOR | RESIZE_QUIRK |
  WIN32_INPUT_MODE`. With the inherited cursor ConPTY starts by asking
  `CSI 6 n` and does nothing else until it is answered: see "The startup
  cursor query" below. ConPTY also asks for win32-input-mode
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
- Closing: deleting a pane closes ConPTY only, as Windows Terminal does;
  the programs on its console get `CTRL_CLOSE_EVENT` and those off it
  go on ("Closing a pane" in `doc/survey/windows-port.md`).
- portable-pty loads a `conpty.dll` it finds on the DLL search path (the
  executable's directory first) in place of kernel32's functions, so
  one placed beside `temzdf.exe` would be used (not tried with temzdf).
- Not tried yet: the kitty keyboard protocol and the mouse from a
  program in a pane (measured through ConPTY with flags 0 and 8 above,
  not with portable-pty's).
- Seen in the pane tests run on Windows (2026-11-24, `temzdf-testhelper`
  as the program, same VM): kitty graphics from the program never reach
  the pane (as above); a program's text written after `CSI ? 2026 h`
  without the `l` did not reach the pane within 10 s, so an update left
  on is held back by ConPTY before temzdf's own timeout can show it; the
  program got no answer to the queries `OSC 4 ; 1 ; ?` and `OSC 11 ; ?`
  (not measured above: whether ConPTY drops the query or the answer);
  ConPTY titles the console with the program's path, so a terminal's
  tab is named after it, not `shell`. The focus reports a program asks
  for (`CSI ? 1004 h`), typed keys in raw mode (`hi`, Enter as CR,
  Ctrl+B), the wheel's arrows on the alternate screen, the mouse
  tracking modes, OSC 8, the kitty keyboard protocol's `CSI 104;5u`,
  sizes after a resize and a background job left on the console all
  behaved as on Unix.

### Which ConPTY

portable-pty 0.9.0 (`src/win/psuedocon.rs`, `load_conpty`) loads
`conpty.dll` by name (`LoadLibraryW`) and prefers it to kernel32's
`CreatePseudoConsole`; a `conpty.dll` starts the `OpenConsole.exe` beside
it. System32 has no `conpty.dll`, so Windows' default search order finds
only someone else's copy: beside the executable, in the current directory
(the server inherits the client's, often a document's directory: a
`conpty.dll` there ran in the server, tried in the VM) or on `PATH`.
WezTerm's installer puts its directory on `PATH`; from then on the VM's
panes and tests ran on WezTerm's 2023 ConPTY (2026-11-24, found by 118
spinning `OpenConsole.exe`). Since then `dll_search_restricted` in
`crates/temzdf-term/src/pty.rs` (and tuitest) sets
`SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_APPLICATION_DIR |
LOAD_LIBRARY_SEARCH_SYSTEM32)` before the first ConPTY: a `conpty.dll`
beside `temzdf.exe` is still used (whether to ship one is open), any
other is not, and otherwise kernel32's is. Rio loads only its own copy by
full path, psmux and Go's and Python's libraries use kernel32's; WezTerm
and Zed ship theirs beside the executable, which comes first.

### The startup cursor query

With `PSEUDOCONSOLE_INHERIT_CURSOR` the console host asks `CSI 6 n` first
and waits for the answer before anything else, closing included
(`VtIo::StartIfNeeded` before microsoft/terminal#17574's 3 s timeout;
microsoft/terminal#11213). A pane closed, or a server that ended, before
the terminal emulator saw the query left the host and its program
running for good: the in-box conhost of build 26300 waits, WezTerm's
2023 OpenConsole spins a core. Measured in the VM with 20 panes of a
program that never ends (2026-11-24): closed unread, 3 stayed; left open
while the process exited, 14 stayed; answered, none. portable-pty's
users answer it themselves (turborepo, deno-pty, distant and others);
Windows Terminal, Alacritty, Zed, Rio, psmux and VS Code do not pass the
flag at all, and wezterm#8256 (open) would let portable-pty's users drop
it.

temzdf answers it in the pty's reader thread (`StartupQuery` in
`crates/temzdf-term/src/pty.rs`), until portable-pty can open ConPTY
without the flag: when the query comes within the first 256 bytes of the
output (possibly split between reads) it writes `CSI 1;1 R` (a new pane's
cursor is at the top left) and takes the query out of the output, so the
pane does not answer it again; a program's own query later reaches the
pane. It answers only after the query: an answer sent ahead takes the
program's first key under win32-input-mode (psmux#313). The reader
writes the answer itself, also after the pane was deleted. With it, of
20 panes none stayed when closed, and 1 or 2 when the process ended at
once after opening or closing them; none when it ended 3 s later.

A host whose pseudoconsole is closed within milliseconds of its start
can still miss that and stay, the query answered or not: the session
tests, which split and delete panes at once, left 55 hosts with their
programs in a run with one thread and 89 with several. Neovim met the
same on Windows 11 with no flags at all (neovim#38244, root cause
unknown) and ends the program first when the host wrote nothing yet
(`pty_proc_close_console`); temzdf's `Pty` does the same on `Drop`
(nothing besides the startup query read yet: `TerminateProcess`, then
`ClosePseudoConsole`). With it the session tests left none, in both
runs, and the tests of `temzdf-term`, `temzdf-server` and tuitest
together none (2026-11-24). Not covered: a process that ends within
milliseconds of opening a pane without dropping it (a crash), as with
Neovim (1 of 20 such panes stayed). Both workarounds can go once
conhost no longer needs them; the query's once portable-pty can drop
the flag.

