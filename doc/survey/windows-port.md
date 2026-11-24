# Porting to Windows: a survey

Research done on 2026-11-22, before any Windows work: what in temzdf
does not work on Windows and what could replace it, what the tests and
tooling need, and how Windows end-to-end tests could run in a VM on the
Linux host. Nothing here was tried on Windows yet; claims about Windows
behaviour come from documentation, issue trackers and general knowledge
unless they say they were checked. The plan and its status are in
`todo.md`.

## The main conclusions

- On Windows the GUI client (`temzdf-gui`) should be the frontend. The
  terminal frontend (`termpix`, `temzdf-tty`) comes last, or stays
  optional: on Windows a terminal talks to programs through ConPTY,
  which renders their output again, and nothing shows that kitty
  graphics (APC), the SGR-pixel mouse or the kitty keyboard protocol
  get through it ([microsoft/terminal#1985](https://github.com/microsoft/terminal/issues/1985)
  asks for a pass-through mode;
  [wez/wezterm#2902](https://github.com/wez/wezterm/issues/2902)).
  Windows Terminal and mintty draw sixel, not kitty graphics. The same
  holds inside terminal panes: programs in them run behind ConPTY too.
- The gate (`cargo xtask check`), xtask and the Node tooling are nearly
  portable already; the workspace does not compile for Windows mostly
  because of `temzdf-protocol` (Unix sockets, fd passing) and `termpix`
  (termios).
- Testing first needs a few small changes that need no VM (see "First
  steps"); end-to-end tests run in a Windows VM on the Linux host, built
  on vitro.

## The product code

Fallbacks that already exist and make a first port smaller: pixels in
the socket's messages (`TEMZDF_PIXELS=socket`), `Medium::Direct` and
`TERMPIX_GRAPHICS=off` in termpix, a prebuilt script guest
(`TEMZDF_SCRIPT_GUEST_WASM`), `$ESBUILD`.

| Area | What breaks on Windows | Alternative | Effort |
|---|---|---|---|
| Session protocol (`temzdf-protocol` `session.rs`, `shared.rs`, `discovery.rs`; `temzdf-server` `daemon.rs`, `forward.rs`; `temzdf-client` `connection.rs`) | `std::os::unix::net`; socket identity by `(dev, ino)`; ownership and mode 0700 checks with `getuid`; the 108-byte path limit | A `Stream`/`Listener` type in `temzdf-protocol`: std on Unix, `uds_windows` (AF_UNIX, Windows 10 1803+, socket files obey ACLs) on Windows. Named pipes would need a new "socket gone" design, an explicit DACL and overlapped I/O (a blocking reader serialises a synchronous pipe). Sessions under `%LOCALAPPDATA%\temzdf`; `same-file` for identity | L |
| Shared-memory pixels (`shared.rs`) | memfd/`shm_open`, `SCM_RIGHTS` | Off on Windows (`shared_memory: false`, pixels in the messages). An unnamed file mapping per image handed over with `DuplicateHandle` was measured slower than the socket, and reused mappings were not taken (2026-11-25, "Windows, and shared memory kept and reused" in `doc/dev/performance.md`) | S |
| Terminal frontend (`termpix` `tty.rs`, `output.rs`, `graphics.rs`) | `/dev/tty`, termios, `select`, `tcgetwinsize`, SIGWINCH/SIGTERM/SIGHUP, `dup2`, POSIX shm transfers | `CONIN$`/`CONOUT$` with `SetConsoleMode` (VT input and output), blocking reader and writer threads, `WINDOW_BUFFER_SIZE_EVENT`, `SetConsoleCtrlHandler`; or crossterm's backend. Direct transfers only. Text only first | L |
| Session server start-up (`temzdf` `main.rs`, `temzdf-client`) | `setsid`; `/dev/tty` check | `DETACHED_PROCESS \| CREATE_NEW_PROCESS_GROUP \| CREATE_NO_WINDOW` (maybe `CREATE_BREAKAWAY_FROM_JOB`); `IsTerminal` | S |
| Terminal panes (`temzdf-term` `pty.rs`, `lib.rs`) | `master.as_raw_fd()` (Unix-only in portable-pty), `poll`, process-group signals, `$SHELL`/`/bin/sh`, OSC 7 paths | portable-pty's ConPTY with blocking reader and writer threads, a Job Object with `KILL_ON_JOB_CLOSE`, `%COMSPEC%` or PowerShell, `file:///C:/` paths. ConPTY asks `ESC[6n` at start and needs Windows 10 1809+ | M |
| Paths and directories (`config.rs`, `session.rs`, `paths.rs`, `text.rs`, `temzdf-client`) | XDG and `HOME`; `canonicalize` gives `\\?\C:\`; `mode` and `PermissionsExt` calls not gated; `file:` URIs; the executable check by mode | Known folders (`%APPDATA%`, `%LOCALAPPDATA%`) unless XDG variables are set; `dunce::canonicalize` and case-insensitive comparison; `cfg(unix)`; `PATHEXT` | M |
| SyncTeX (`temzdf-pdfcore` `synctex.rs`) | `OsStrExt` and narrow `char*` paths | UTF-8 paths into a SyncTeX built with UTF-8 file APIs, or files opened in Rust | S |
| External programs (`temzdf-client` `run_shell`, `default_opener`; `temzdf-pdfview` `shell_quote`) | `sh -c`, `xdg-open`/`open` | `synctexeditor` as an argument list without a shell (better on Unix too); `ShellExecuteW`; `CREATE_NO_WINDOW` | S |
| Script guest's files (`temzdf-script-runner` `lib.rs`, `temzdf-script` resolver) | `preopened_dir("/", "/")`, `/` as the only absolute prefix | A preopen per drive and path translation at the `Link` | S |
| Native libraries | libtexprintf through pkg-config and autotools; MuPDF, libghostty-vt (Zig) and SyncTeX untried on MSVC | libtexprintf built with the `cc` crate from its sources, as SyncTeX is (also removes autotools on Unix); `mupdf-sys` and `libghostty-vt-sys` have Windows branches | M |
| Build tooling | `TEMZDF_BUNDLED_FONTS` split on `:`; `esbuild.cmd` not found by `Command`; WASI clang for the guest | `std::env::split_paths`; resolve the exe; wasi-sdk or a guest built on Linux | S |
| GUI client | little: X11 and primary selection are gated already; IME goes through winit | Decide `windows_subsystem = "windows"` versus console output (`ls-fonts`, `--render-frame`) | S |

Suggested order: (1) compile (gate `rustix`, `signal-hook`, `libc`;
libtexprintf; build tooling), (2) the transport, (3) directories and
processes — the GUI attached to a PDF session on Windows —, (4) terminal
panes, (5) SyncTeX, (6) shared memory, (7) the terminal frontend.

## The server on Windows

Read from the code on 2026-11-23, once the server ran on Windows with the
GUI and PDF panes and before it had terminal panes: what it still lacked
or did differently there. "Since" notes what was done afterwards.

| What | Where | State |
|---|---|---|
| Terminal panes | `crates/temzdf-term/src/pty.rs` | Off (`Unsupported`). Since: in the in-box ConPTY (below) |
| `ls-fonts` without a session | `crates/temzdf/src/lsfonts.rs` | Starts a pane running `true` so that the configuration is read (it is read before the first pane opens); Windows has no `true`. A way to read the configuration without a pane would fix it; postponed, as the user may stop resolving the configuration's fonts there |
| A console window for inverse search | `run_shell` in `crates/temzdf-client/src/lib.rs` | The server (`DETACHED_PROCESS`) and `temzdf-gui` have no console, so the `cmd` that runs the command opens a window of its own. Since: `CREATE_NO_WINDOW` |
| The default shell | `default_shell` in `crates/temzdf-term/src/lib.rs` | `%COMSPEC%` (cmd.exe) already; it stays the default (the user, 2026-11-23) |
| An external editor | none | Nothing opens `$EDITOR` yet. If something does: `CreateProcess` completes only `.exe`, so `.cmd` and `.bat` editors need cmd |
| The clipboard | `session/registers.rs` | The server has none: clients get OSC 52 or `ClientMsg::Clipboard`; the GUI's works on Windows (`gui.actions` copies) |
| Signals | `pty.rs` (the panes' hang-up and kill) | Nothing else in the server uses signals; `kill-session` goes over the socket. Ctrl+C and Ctrl+Break of a server were not looked at (it runs detached) |
| OSC 7 working directories | `path_of_url` in `crates/temzdf-term/src/lib.rs` | `file:///C:/x` became `/C:/x`. Since: `C:/x`. cmd and PowerShell do not send OSC 7 unless configured to |
| Images from programs by file, temporary file or shared memory | `TermBuffer::spawn` | Moot with the in-box ConPTY (APC is dropped). With a shipped `conpty.dll`: Windows paths in `t=f` and `t=t` are untried, `t=s` names POSIX shared memory |
| Shared-memory pixels | `crates/temzdf-protocol/src/shared.rs` | None on Windows: every image travels on the socket (known; only speed; decided to stay so, `doc/dev/performance.md`) |
| The host name of swap files | `host` in `crates/temzdf-server/src/session/swap.rs` | Reads `/proc/sys/kernel/hostname`, `/etc/hostname` and `HOSTNAME`, none of which Windows has (`COMPUTERNAME` is), so the host is empty there (on macOS, which has no `/etc/hostname`, mostly too). Since: the `hostname` crate, gethostname on Unix and GetComputerNameExW on Windows (uname's node name first, which Miri found rustix reading past what libc writes) |
| Owner and ACL checks of the directories | `crates/temzdf-client/src/lib.rs` | None on Windows (decided, `doc/design/session-server.md`, "Windows") |
| `file:` links, path comparisons | `crates/temzdf-client/src/lib.rs`, `paths.rs` | Links do not open; comparisons are case-sensitive (known). Comparisons are to follow the host's file system (the user, 2026-11-24; how: `todo.md`). Since: comparisons do; a `file:` link to a document (by its extension) opens, another file is shown in Explorer, and anything but a plain drive path (UNC, devices, streams, names Windows changes) is refused (the user chose this over a list of what runs, which `ShellExecute`'s associations outgrow) |
| Tests | `session/tests.rs`, `host_tests.rs`, `temzdf-term` | The pane tests that run Unix programs are ignored there; a helper program in their place comes next (`todo.md`). Since: they start `temzdf-testhelper`, and 70 of the 80 run on Windows (2026-11-24); the rest are ignored with their reasons |
| The terminal frontend | `termpix`, `temzdf-tty` | Unix only (decided: last). Since: Windows' console (2026-11-24, "temzdf in a terminal on Windows" in `doc/terminals/conpty.md`); `standalone` stays Unix only |

Nothing else was found that keeps the server from running.

### Terminal panes

Only `pty.rs` was Unix-specific: portable-pty 0.9.0 opens a ConPTY on
Windows (`openpty`, `spawn_command`, `resize`), so `TermBuffer::spawn`
stays as it is. On Unix `pty.rs` duplicates the master's descriptor,
makes it non-blocking and waits with `poll`; on Windows the master has no
descriptor, only blocking pipes (`try_clone_reader`, `take_writer`)
without a timeout, which end only when ConPTY is closed
(`ClosePseudoConsole`, which on older Windows can wait until its last
output was read). Unix hangs the program's process group up and kills it
a second later; Windows has no process groups, and portable-pty's `kill`
ends only the program (what ends a pane's programs instead: "Closing a
pane" below).

The options weighed:

| Option | Images from programs | Cost |
|---|---|---|
| portable-pty with the in-box ConPTY | dropped (`doc/terminals/conpty.md`) | M: `pty.rs` only |
| the same with `conpty.dll` and `OpenConsole.exe` beside the executable (portable-pty loads a `conpty.dll` it finds first) | passed | the above, and shipping the two from NuGet |
| ConPTY through `windows-sys`, without portable-pty | either | L: the pipes, `STARTUPINFOEX`, the command line and environment block by hand |
| other crates (`conpty`, `conpty-oxide`) | either | small, little-used crates |

The in-box ConPTY through portable-pty was chosen (the user,
2026-11-23); shipping `conpty.dll` waits (`todo.md`). Done the same
day: `pty.rs` on Windows, `supported` asking kernel32 for
`CreatePseudoConsole` first (portable-pty panics without it), and tests
with cmd in a pane (`tests::conpty` in `crates/temzdf-term/src/lib.rs`):
output after ConPTY's start-up query for the cursor (answered by the
pane's terminal), input, resizing, and which programs end with the pane.

### Closing a pane

At first the pane's program ran in a Job Object with
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, ended a second after ConPTY was
closed: everything the pane started ended, programs off its console too,
and console programs had a second (none when the server crashed) for
`CTRL_CLOSE_EVENT`. The user chose Windows Terminal's behaviour instead
(2026-11-23), so it was looked up and measured:

- Windows Terminal (`src/cascadia/TerminalConnection/ConptyConnection.cpp`,
  `ConptyConnection::Close`, the same in v1.24.11911.0, v1.25.3131.0 and
  main at cf5e6bf) only closes the pseudoconsole, for a tab, a pane or a
  window: no job, no `TerminateProcess`. When the terminal dies, the
  console host sees its control pipe break
  (`PtySignalInputThread::_Shutdown`) and does the same. The console
  host then sends `CTRL_CLOSE_EVENT` to each process attached to the
  console, newest first (`CloseConsoleProcessState`,
  `ProcessCtrlEvents` in `src/host`); Windows ends one that has not
  ended after about five seconds. Processes not attached to the console
  get nothing.
- Measured in the VM (Windows 11 build 26300, the in-box console host;
  a process "stays" when it still ran 22 s later):

| Process | Job Object (before) | ConPTY closed only (now): pane deleted, `kill-session`, server killed | Windows Terminal 1.24: window closed, killed |
|---|---|---|---|
| the program in front (ping) and the cmd that started it | ends | ends | ends |
| `start /b ping` (same console, background) | ends | ends | ends |
| a program slow on `CTRL_CLOSE_EVENT` | killed after 1 s; no event when the server died | gets it, ended after about 5 s | the same |
| charmap (a GUI program) | ends | stays | stays |
| cmd on a console of its own | ends | stays | stays |

  Windows 11's Notepad stayed in every case, the job's included (not
  understood); `start "" cmd` went to Windows Terminal, the VM's default
  terminal.
- How long a console program lives after the close depends on the
  order the programs attached, not on how the pane was closed (measured
  2026-11-24 with a probe that logs a heartbeat, its console writes and
  its control events every 50 ms; `delete-buffer`, `kill-session` and a
  killed server, debug and release builds). The console host sends
  `CTRL_CLOSE_EVENT` one process at a time, newest attached first, so a
  program slow on it holds up every program attached before it for its
  five seconds, whichever way the pane closed. With no such program,
  everything on the console ended within 0.3 s; a background ping
  started before a slow program ended after 5.1 to 5.3 s on all three
  (fast in 2 of 23 runs on `delete-buffer`: chance, Fisher's p about
  0.5), one started after it within 0.3 s on all three. `start /b`
  returns before its child attaches to the console, so programs started
  back to back can attach in either order: that, not the way of closing,
  was the "about 1.3 s on `delete-buffer`" seen at first (2 runs
  against 4, times of snapshots). Console writes kept succeeding until
  the event in every case, whether the pane's output was still read or
  not.
- So the job went: closing ConPTY alone matches Windows Terminal, when
  the pane is deleted and however the server ends. Not tried: Windows
  before 24H2, where `ClosePseudoConsole` waits until its output was
  read (the reader thread keeps reading, and the close runs on a thread
  of its own) and the five-second end may not be there.

## Tests and tooling

- **Line endings**: there is no `.gitattributes`, and Git for Windows
  checks out CRLF by default; Biome, the `temzdf-all.d.ts` comparison
  and `include_str!` expectations would all fail. Add `* text=auto
  eol=lf`.
- **The gate**: fmt, clippy, tests and the Node steps are portable;
  `find_tool` already tries `.exe`/`.cmd`. shellcheck stays Unix-only
  (it says SKIP). Miri (`scripts/miri.sh`, bash) could become `cargo
  xtask miri`, which could also interpret `--target
  x86_64-pc-windows-msvc` from Linux and so check Windows `cfg` paths of
  pure Rust code without a VM. `FONTCONFIG_FILE` does nothing on
  Windows: deterministic text needs a test-only font collection with
  system fonts off. The lock test reads the locked file through another
  handle, which Windows refuses.
- **Test code**: some test code does not compile on Windows (symlink and
  mode tests in `temzdf-texteditor`, `temzdf-pdfcore`; `mutate.rs`
  gates pure tests with the shell ones). The script host tests use
  `UnixStream::pair` only as a byte pipe (`std::io::pipe` instead).
  `pdf::fixture` runs `scripts/build-fixtures.sh`; its logic belongs in
  Rust. `std::env::temp_dir()` reads `TMP`/`TEMP`, not `TMPDIR`.
- **`sh` everywhere**: about 59 `sh` and 28 `sleep` uses in the session
  tests, about 13 tests in `temzdf-term`, about 113 typed shell lines in
  `crates/temzdf/tests/tui.rs`. A small helper program (print bytes, raw
  mode, echo input in hex, sleep) would replace them mechanically;
  `tui.rs` can start as `cfg(unix)`.
- **ConPTY is the first experiment**: portable-pty 0.9 defines
  `PSEUDOCONSOLE_PASSTHROUGH_MODE` but does not pass it, and prefers a
  side-loaded `conpty.dll` with OpenConsole. Whether kitty graphics,
  kitty keyboard, the SGR-pixel mouse and capability queries reach
  libghostty-vt decides what tuitest and the terminal panes can do.
- **Scripts**: `check.sh`, `mutate.sh`, `devshell.sh`, `timing.sh`, the
  macOS scripts stay Unix-only; `build-fixtures.sh` moves into Rust;
  `deps-without-nix.sh` goes once libtexprintf builds itself;
  `smoke-session.sh` could become `cargo xtask smoke`; `e2e.sh` stays
  bash on the Linux host with a Windows backend.
- **CI**: a `windows-latest` job (LF checkouts, npm, Biome, the Node
  steps, clippy and tests on the crates that build), and meanwhile
  `cargo check --target x86_64-pc-windows-msvc` on those crates in the
  Linux job.

## The toolchain: MSYS2 UCRT64, not MSVC

Decided with the user on 2026-11-23, after the first builds in the VM:
Windows builds with MSYS2's UCRT64 environment (its Rust package, the
`x86_64-pc-windows-gnu` target, GCC for C), not with MSVC; the VM, CI
and the Linux gate's cross-check follow. Not MINGW64, which MSYS2 is
retiring. What led to it:

- With MSVC the vendored C needed fixes of its own: SyncTeX's
  `synctex_parser_utils.h` expands `SYNCTEX_ATTRIBUTE_FORMAT_PRINTF` to
  `ATTRIBUTE_FORMAT_PRINTF` under `_MSC_VER`, which only TeX Live's
  headers define (syntax errors at `synctex_parser.c` 8449 and 8486),
  and `cl`'s `/D` cannot define a macro with parameters (a header forced
  in with `/FI` worked). libtexprintf's `AddChild` wrote one element
  past its array (`STATUS_HEAP_CORRUPTION`), MSVC reading the index
  after the call (fixed in the vendored copy,
  `crates/texprintf/vendor/libtexprintf/README.md`).
- With UCRT64's GCC 16.2 and Clang 23.1 both libraries built unmodified
  and their tests passed (`temzdf-pdfcore` after a Rust-side fix in
  `synctex.rs`, `OsStrExt` being Unix-only).
- The order of evaluation in `a[i] = f()` (with `f` incrementing `i`)
  differs: GCC reads the index first; Clang reads it after the call
  when `f` returns an `int` but first when it returns an 80-byte struct
  (as `InitBox` does); MSVC reads it after the call in both. So the
  libtexprintf fix stays with GCC too.
- Rust's `x86_64-pc-windows-gnu` with UCRT64's C linked and ran without
  trouble (tried with rustup's toolchain; MSYS2's own Rust package is
  the one used now). bindgen (`mupdf-sys`) needs `LIBCLANG_PATH` set to
  UCRT64's `bin`, or it takes another libclang that does not match
  UCRT64's headers. MSYS2's `usr/bin` stays off the machine's `PATH`
  (its `link.exe` and others shadow tools), but `make`, which `mupdf-sys`
  runs, is only there: the gate runs in UCRT64's login shell (CI's
  `shell: msys2`, `scripts/windows-vm.sh check`), which puts `usr/bin` on
  `PATH` after UCRT64's `bin`.

The executables need none of MSYS2's DLLs (checked 2026-11-24 with
`objdump -p` and by starting them with only `System32` on `PATH`): Rust's
`windows-gnu` links `libgcc_eh` and winpthread statically, the C
libraries use no C++, and the rest are Windows' own DLLs, the UCRT
(`api-ms-win-crt-*`, in Windows 10 and later) included.

Open: MSYS2's Rust following its own release schedule, not the dev
shell's.

## End-to-end tests in a VM

- `/dev/kvm` is usable without setup on the maintainer's machine;
  nixpkgs has QEMU, OVMF, swtpm, quickemu, cargo-xwin.
- The build runs in the guest (`cargo xtask check` has to anyway; cross
  builds hit the same C dependencies).
- `scripts/e2e.sh` can stay on the Linux host with a `-d windows`
  backend: QMP `screendump` for screenshots, QMP `input-send-event`
  (USB tablet and keyboard) for input, SSH for commands and files.
  Input and screenshots inside the guest work only in the logged-in
  desktop session, not over SSH (as on macOS): auto-logon, and programs
  started on the desktop through an interactive scheduled task.
- Terminals to judge: WezTerm (kitty graphics, `wezterm cli get-text`),
  behind the same ConPTY question; Windows Terminal (sixel) for the
  text-only scenario; the GUI client. Without a GPU the guest has DX12
  WARP and only GDI's OpenGL 1.1: WezTerm with `front_end = "WebGpu"`,
  and wgpu may need `force_fallback_adapter` to pick WARP.
- Images: Windows 11 Enterprise evaluation ISOs (90 days), Windows Server
  2025 evaluation ISO and VHD (180 days); keep the media local and
  rebuild the image before it expires. Reinstalling a 30 GB image per CI
  run is impractical: CI uses native `windows-latest`, the VM stays on
  the development machine.

| Option | Fit |
|---|---|
| [vitro](https://github.com/lambdalisue/vitro) | Disposable QEMU VMs (Rust, MIT): unattended Windows 11 install with virtio drivers, golden image plus overlays, `exec`/`cp`/sync over SSH, QMP screenshots and input, launching on the desktop. Young, and its x86-64 Linux host path was never run when surveyed (x86-64 boots SeaBIOS while its answer file expects UEFI: set `firmware` to OVMF). Chosen: the user trusts its author |
| QEMU/KVM with our own script | Most control (also `savevm`/`loadvm` back to a logged-in desktop); we write and maintain it. The fallback |
| quickemu/quickget | Fetching images; awkward to automate |
| dockur/windows | Easiest install; hides QMP, needs Docker |
| libvirt, Vagrant, VirtualBox | A daemon, a licence or KVM conflicts for nothing we need |
| GitHub `windows-latest` | The gate in CI, not e2e |

A `scripts/windows-vm.sh` would mirror `scripts/macos-vm.sh`: create
(from a local ISO), provision (Git, VS Build Tools, rustup with
`wasm32-wasip1`, Zig, LLVM, wasi-sdk, Node, WezTerm), save, reset, run,
check, e2e.
