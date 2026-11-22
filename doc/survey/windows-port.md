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
| Shared-memory pixels (`shared.rs`) | memfd/`shm_open`, `SCM_RIGHTS` | Off on Windows at first (`shared_memory: false`, pixels in the messages); later an unnamed file mapping handed over with `DuplicateHandle`, read-only | S, then M |
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
