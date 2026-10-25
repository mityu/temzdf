//! A test harness for terminal programs: run one on a pty behind Ghostty's
//! terminal emulation (`libghostty-vt`, kitty graphics included), drive it with
//! keys, and read its screen as text, cells, cursor and image placements.
//! [`Term::inside_tmux`] puts a private tmux between the two, so programs can
//! also be tested inside tmux. See doc/testing.md.
//!
//! The harness answers the queries a program makes at startup (device
//! attributes, XTVERSION, cell size, kitty graphics), so programs that probe
//! the terminal (like temzdf) start as they would in Ghostty.
//!
//! Waiting is by condition ([`Term::wait_for`]), never by fixed sleeps; a
//! failed wait panics with the screen, and the screen and the bytes received
//! are written to `target/tuitest/<test name>.txt`.

pub mod keys;

use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use libghostty_vt::render::{CellIterator, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::StyleColor;
use libghostty_vt::terminal::{
    ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, PrimaryDeviceAttributes,
    SecondaryDeviceAttributes, SizeReportSize, TertiaryDeviceAttributes,
};
use libghostty_vt::{RenderState, Terminal, TerminalOptions, key};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// How long [`Term::wait_for`] waits before failing.
pub const TIMEOUT: Duration = Duration::from_secs(15);

/// The terminal the harness plays.
#[derive(Clone, Debug)]
pub struct Options {
    pub cols: u16,
    pub rows: u16,
    /// Cell size in pixels.
    pub cell: (u32, u32),
    /// The answer to XTVERSION (`CSI > q`).
    pub xtversion: &'static str,
    /// Environment for the program (on top of a cleaned-up copy of ours).
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self { cols: 100, rows: 30, cell: (10, 20), xtversion: "ghostty(tuitest)", env: Vec::new(), cwd: None }
    }
}

/// A colour as the terminal stores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Default,
    Palette(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// Empty for blank cells and the right half of wide characters.
    pub text: String,
    /// The right half of a wide character.
    pub spacer: bool,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub italic: bool,
    pub inverse: bool,
}

/// An image placed with the kitty graphics protocol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    pub image: u32,
    pub placement: u32,
    /// Top-left cell in the viewport (may be negative when scrolled off).
    pub col: i32,
    pub row: i32,
    /// Cells covered.
    pub cols: u32,
    pub rows: u32,
    /// Size of the image itself in pixels.
    pub width: u32,
    pub height: u32,
    pub z: i32,
}

/// What the terminal shows.
#[derive(Clone, Debug, Default)]
pub struct Screen {
    pub cells: Vec<Vec<Cell>>,
    pub cursor: Option<(u16, u16)>,
    pub images: Vec<Placement>,
    pub title: String,
}

impl Screen {
    /// Each row as text, wide characters once, trailing blanks removed.
    pub fn lines(&self) -> Vec<String> {
        self.cells
            .iter()
            .map(|row| {
                let s: String = row
                    .iter()
                    .filter(|c| !c.spacer)
                    .map(|c| if c.text.is_empty() { " " } else { c.text.as_str() })
                    .collect();
                s.trim_end().to_owned()
            })
            .collect()
    }

    /// The whole screen as text, one line per row.
    pub fn text(&self) -> String {
        self.lines().join("\n")
    }

    pub fn contains(&self, needle: &str) -> bool {
        self.lines().iter().any(|l| l.contains(needle))
    }

    /// The first row containing `needle`.
    pub fn find(&self, needle: &str) -> Option<usize> {
        self.lines().iter().position(|l| l.contains(needle))
    }

    pub fn cell(&self, col: u16, row: u16) -> &Cell {
        &self.cells[row as usize][col as usize]
    }

    /// The text of a row from `col` on (cells, so wide characters keep their columns).
    /// Cells showing an image through a kitty Unicode placeholder (U+10EEEE,
    /// as inside tmux), as `(col, row)`. Their placements are not in
    /// [`Screen::images`].
    pub fn placeholder_cells(&self) -> Vec<(u16, u16)> {
        let mut out = Vec::new();
        for (y, row) in self.cells.iter().enumerate() {
            for (x, c) in row.iter().enumerate() {
                if c.text.starts_with('\u{10EEEE}') {
                    out.push((x as u16, y as u16));
                }
            }
        }
        out
    }

    pub fn row_from(&self, row: u16, col: u16) -> String {
        let s: String = self.cells[row as usize][col as usize..]
            .iter()
            .filter(|c| !c.spacer)
            .map(|c| if c.text.is_empty() { " " } else { c.text.as_str() })
            .collect();
        s.trim_end().to_owned()
    }
}

/// A directory removed when dropped (for sockets, logs and state of one test).
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(label: &str) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        // Short: sockets live inside (Unix socket paths are limited to ~100
        // bytes, and macOS's per-user temporary directory is long).
        let base = if Path::new("/tmp").is_dir() { PathBuf::from("/tmp") } else { std::env::temp_dir() };
        let dir = base.join(format!("tt-{}-{n}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temporary directory");
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A private tmux server (removed on drop).
struct Tmux {
    socket: String,
}

impl Tmux {
    fn cmd(&self) -> std::process::Command {
        let mut c = std::process::Command::new("tmux");
        c.args(["-L", &self.socket]);
        c
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        let _ = self.cmd().arg("kill-server").stderr(std::process::Stdio::null()).status();
    }
}

/// Whether `tmux` can be run (tests inside tmux are skipped without it).
pub fn have_tmux() -> bool {
    std::process::Command::new("tmux").arg("-V").stdout(std::process::Stdio::null()).status().is_ok_and(|s| s.success())
}

pub struct Term {
    terminal: Terminal<'static, 'static>,
    render: RenderState<'static>,
    rows_it: RowIterator<'static>,
    cells_it: CellIterator<'static>,
    placements: libghostty_vt::kitty::graphics::PlacementIterator<'static>,
    encoder: key::Encoder<'static>,
    writer: Rc<RefCell<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    output: Receiver<Vec<u8>>,
    /// The pty closed.
    eof: bool,
    /// Everything received (for the failure dump).
    received: Vec<u8>,
    size: Rc<RefCell<(u16, u16, u32, u32)>>,
    tmux: Option<Tmux>,
}

impl Term {
    /// Run `argv` on a new pty.
    pub fn spawn(argv: &[&str], opts: &Options) -> Self {
        Self::start(argv, opts, None)
    }

    /// Run `argv` inside a private tmux server (passthrough on, no status
    /// line), shown in this terminal.
    pub fn inside_tmux(argv: &[&str], opts: &Options) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let socket = format!("tuitest-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
        let conf = std::env::temp_dir().join(format!("{socket}.conf"));
        std::fs::write(
            &conf,
            "set -g allow-passthrough on\nset -g status off\nset -sg escape-time 0\nset -g default-terminal tmux-256color\n",
        )
        .expect("tmux configuration");
        let mut full: Vec<String> = vec!["tmux".into(), "-L".into(), socket.clone(), "-f".into()];
        full.push(conf.display().to_string());
        full.push("new-session".into());
        // tmux passes the command to a shell: quote the words.
        full.push(argv.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" "));
        let words: Vec<&str> = full.iter().map(String::as_str).collect();
        Self::start(&words, opts, Some(Tmux { socket }))
    }

    fn start(argv: &[&str], opts: &Options, tmux: Option<Tmux>) -> Self {
        let size = PtySize {
            rows: opts.rows,
            cols: opts.cols,
            pixel_width: opts.cols * opts.cell.0 as u16,
            pixel_height: opts.rows * opts.cell.1 as u16,
        };
        let pair = native_pty_system().openpty(size).expect("pty");
        let mut cmd = CommandBuilder::new(argv[0]);
        cmd.args(&argv[1..]);
        if let Some(dir) = &opts.cwd {
            cmd.cwd(dir);
        } else if let Ok(dir) = std::env::current_dir() {
            cmd.cwd(dir);
        }
        // Whatever terminal or multiplexer runs the tests must not leak in.
        for var in [
            "TMUX",
            "TMUX_PANE",
            "ZELLIJ",
            "ZELLIJ_SESSION_NAME",
            "ZELLIJ_PANE_ID",
            "KITTY_WINDOW_ID",
            "KITTY_PID",
            "WEZTERM_PANE",
            "TERM_PROGRAM",
            "TERM_PROGRAM_VERSION",
            "TERMPIX_BACKEND",
            "TERMPIX_MEDIUM",
            "TERMPIX_TRACE",
        ] {
            cmd.env_remove(var);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd).unwrap_or_else(|e| panic!("cannot run {argv:?}: {e}"));
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().expect("pty reader");
        let writer: Rc<RefCell<Box<dyn Write + Send>>> =
            Rc::new(RefCell::new(pair.master.take_writer().expect("pty writer")));
        let (tx, output) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 1 << 16];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });

        let mut terminal = Terminal::new(TerminalOptions { cols: opts.cols, rows: opts.rows, max_scrollback: 1000 })
            .expect("terminal");
        terminal.resize(opts.cols, opts.rows, opts.cell.0, opts.cell.1).expect("resize");
        terminal.set_kitty_image_storage_limit(1 << 30).expect("graphics");
        let _ = terminal.set_kitty_image_from_shared_mem_allowed(true);
        let _ = terminal.set_kitty_image_from_temp_file_allowed(true);
        let _ = terminal.set_kitty_image_from_file_allowed(true);
        let w = writer.clone();
        terminal
            .on_pty_write(move |_, data| {
                let _ = w.borrow_mut().write_all(data);
            })
            .expect("callback");
        let xtversion = opts.xtversion;
        terminal.on_xtversion(move |_| Some(xtversion)).expect("callback");
        let size = Rc::new(RefCell::new((opts.rows, opts.cols, opts.cell.0, opts.cell.1)));
        let s = size.clone();
        terminal
            .on_size(move |_| {
                let (rows, columns, cell_width, cell_height) = *s.borrow();
                Some(SizeReportSize { rows, columns, cell_width, cell_height })
            })
            .expect("callback");
        terminal
            .on_device_attributes(|_| {
                Some(DeviceAttributes {
                    primary: PrimaryDeviceAttributes::new(
                        ConformanceLevel::LEVEL_2,
                        &[DeviceAttributeFeature::COLUMNS_132, DeviceAttributeFeature::SELECTIVE_ERASE],
                    ),
                    secondary: SecondaryDeviceAttributes {
                        device_type: DeviceType::VT220,
                        firmware_version: 1,
                        rom_cartridge: 0,
                    },
                    tertiary: TertiaryDeviceAttributes::default(),
                })
            })
            .expect("callback");
        Self {
            terminal,
            render: RenderState::new().expect("render state"),
            rows_it: RowIterator::new().expect("rows"),
            cells_it: CellIterator::new().expect("cells"),
            placements: libghostty_vt::kitty::graphics::PlacementIterator::new().expect("placements"),
            encoder: key::Encoder::new().expect("encoder"),
            writer,
            master: pair.master,
            child,
            output,
            eof: false,
            received: Vec::new(),
            size,
            tmux,
        }
    }

    /// Interpret output that arrived within `wait`. Returns whether any did.
    pub fn pump(&mut self, wait: Duration) -> bool {
        let mut got = false;
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.output.recv_timeout(left) {
                Ok(bytes) => {
                    self.received.extend_from_slice(&bytes);
                    self.terminal.vt_write(&bytes);
                    got = true;
                    // Take what else is there right away.
                    while let Ok(more) = self.output.try_recv() {
                        self.received.extend_from_slice(&more);
                        self.terminal.vt_write(&more);
                    }
                }
                Err(RecvTimeoutError::Timeout) => return got,
                Err(RecvTimeoutError::Disconnected) => {
                    self.eof = true;
                    return got;
                }
            }
            if Instant::now() >= deadline {
                return got;
            }
        }
    }

    /// What the terminal shows now (after interpreting what has arrived).
    pub fn screen(&mut self) -> Screen {
        self.pump(Duration::ZERO);
        let mut cells = Vec::new();
        let mut cursor = None;
        if let Ok(snapshot) = self.render.update(&self.terminal) {
            if let Ok(mut rows) = self.rows_it.update(&snapshot) {
                while let Some(row) = rows.next() {
                    let mut line = Vec::new();
                    if let Ok(mut it) = self.cells_it.update(row) {
                        while let Some(c) = it.next() {
                            let spacer = matches!(c.raw_cell().and_then(|r| r.wide()), Ok(CellWide::SpacerTail));
                            let text = if spacer {
                                String::new()
                            } else {
                                c.graphemes().map(|g| g.into_iter().collect()).unwrap_or_default()
                            };
                            let (fg, bg, bold, italic, inverse) = match c.style() {
                                Ok(s) => (color(s.fg_color), color(s.bg_color), s.bold, s.italic, s.inverse),
                                Err(_) => (Color::Default, Color::Default, false, false, false),
                            };
                            line.push(Cell { text, spacer, fg, bg, bold, italic, inverse });
                        }
                    }
                    cells.push(line);
                }
            }
            if snapshot.cursor_visible().unwrap_or(false)
                && let Ok(Some(c)) = snapshot.cursor_viewport()
            {
                cursor = Some((c.x, c.y));
            }
        }
        let images = self.images();
        let title = self.terminal.title().map(str::to_owned).unwrap_or_default();
        Screen { cells, cursor, images, title }
    }

    fn images(&mut self) -> Vec<Placement> {
        let mut out = Vec::new();
        let Ok(graphics) = self.terminal.kitty_graphics() else { return out };
        let Ok(mut it) = self.placements.update(&graphics) else { return out };
        while let Some(p) = it.next() {
            let Ok(id) = p.image_id() else { continue };
            let Some(image) = graphics.image(id) else { continue };
            let pos = p.viewport_pos(&image, &self.terminal).ok().flatten();
            let grid = p.grid_size(&image, &self.terminal).unwrap_or_default();
            let Some(pos) = pos else { continue };
            out.push(Placement {
                image: id,
                placement: p.placement_id().unwrap_or(0),
                col: pos.col,
                row: pos.row,
                cols: grid.cols,
                rows: grid.rows,
                width: image.width().unwrap_or(0),
                height: image.height().unwrap_or(0),
                z: p.z().unwrap_or(0),
            });
        }
        out.sort_by_key(|p| (p.z, p.row, p.col));
        out
    }

    /// Wait until `done` holds for the screen; panics (with a dump) after
    /// [`TIMEOUT`]. `what` names the condition in the failure message.
    pub fn wait_for(&mut self, what: &str, done: impl Fn(&Screen) -> bool) -> Screen {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let screen = self.screen();
            if done(&screen) {
                return screen;
            }
            if Instant::now() >= deadline || (self.eof && !self.pump(Duration::ZERO)) {
                let why = if self.eof { "the program exited" } else { "timed out" };
                self.fail(&format!("waiting for {what}: {why}"), &screen);
            }
            self.pump(Duration::from_millis(50));
        }
    }

    /// Wait until `needle` is on the screen.
    pub fn wait_text(&mut self, needle: &str) -> Screen {
        self.wait_for(&format!("{needle:?}"), |s| s.contains(needle))
    }

    /// Wait until nothing arrives for `quiet` (e.g. after the last frame).
    pub fn settle(&mut self, quiet: Duration) -> Screen {
        let deadline = Instant::now() + TIMEOUT;
        while self.pump(quiet) && Instant::now() < deadline {}
        self.screen()
    }

    /// Fail the test with the screen, and leave the screen and the raw output
    /// in `target/tuitest/`.
    pub fn fail(&self, message: &str, screen: &Screen) -> ! {
        let name = std::thread::current().name().unwrap_or("tuitest").replace("::", "-");
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tuitest");
        let _ = std::fs::create_dir_all(&dir);
        let dump = format!(
            "{message}\n--- screen ({} images, cursor {:?}) ---\n{}\n---\n",
            screen.images.len(),
            screen.cursor,
            screen.text()
        );
        let _ = std::fs::write(dir.join(format!("{name}.txt")), &dump);
        let _ = std::fs::write(dir.join(format!("{name}.out")), &self.received);
        panic!("{dump}(also in target/tuitest/{name}.txt; raw output in {name}.out)");
    }

    /// Send raw bytes as if typed.
    pub fn write(&mut self, bytes: &[u8]) {
        let mut w = self.writer.borrow_mut();
        w.write_all(bytes).expect("write to the pty");
        let _ = w.flush();
    }

    /// Press keys, in the notation of [`keys::parse`] (`"C-h"`, `"Enter"`,
    /// `"M-x"`, `"%"`), encoded the way the program asked for (legacy or the
    /// kitty keyboard protocol).
    pub fn key(&mut self, spec: &str) {
        self.pump(Duration::ZERO);
        let press = keys::parse(spec).unwrap_or_else(|e| panic!("{e}"));
        self.encoder.set_options_from_terminal(&self.terminal);
        let mut bytes = Vec::new();
        self.encoder.encode_to_vec(&press.event(), &mut bytes).expect("encode");
        assert!(!bytes.is_empty(), "no encoding for {spec:?}");
        self.write(&bytes);
    }

    /// Click the left button at the centre of a cell, encoded as the program
    /// asked (SGR, SGR-pixels, ...); nothing is sent if it tracks no mouse.
    pub fn click(&mut self, col: u16, row: u16) {
        use libghostty_vt::mouse;
        self.pump(Duration::ZERO);
        let (rows, cols, cw, ch) = *self.size.borrow();
        let mut encoder = mouse::Encoder::new().expect("mouse encoder");
        encoder.set_options_from_terminal(&self.terminal);
        encoder.set_size(mouse::EncoderSize {
            screen_width: cols as u32 * cw,
            screen_height: rows as u32 * ch,
            cell_width: cw,
            cell_height: ch,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        });
        let position = mouse::Position { x: (col as f32 + 0.5) * cw as f32, y: (row as f32 + 0.5) * ch as f32 };
        for (action, pressed) in [(mouse::Action::Press, true), (mouse::Action::Release, false)] {
            let mut event = mouse::Event::new().expect("mouse event");
            event.set_action(action).set_button(Some(mouse::Button::Left)).set_position(position);
            encoder.set_any_button_pressed(pressed);
            let mut bytes = Vec::new();
            if encoder.encode_to_vec(&event, &mut bytes).is_ok() && !bytes.is_empty() {
                self.write(&bytes);
            }
        }
    }

    pub fn keys(&mut self, specs: &[&str]) {
        for s in specs {
            self.key(s);
        }
    }

    /// Type text key by key, then Enter if it ends with a newline.
    pub fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            match c {
                '\n' => self.key("Enter"),
                ' ' => self.key("Space"),
                c => self.key(&c.to_string()),
            }
        }
    }

    /// Paste, bracketed when the program asked for it.
    pub fn paste(&mut self, text: &str) {
        self.pump(Duration::ZERO);
        let bracketed = self.terminal.mode(libghostty_vt::terminal::Mode::BRACKETED_PASTE).unwrap_or(false);
        let mut data = text.as_bytes().to_vec();
        let mut buf = vec![0u8; data.len() + 16];
        let n = libghostty_vt::paste::encode(&mut data, bracketed, &mut buf).expect("paste");
        self.write(&buf[..n]);
    }

    /// Resize the window (the pty reports it to the program).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (_, _, cw, ch) = *self.size.borrow();
        *self.size.borrow_mut() = (rows, cols, cw, ch);
        self.terminal.resize(cols, rows, cw, ch).expect("resize");
        self.master
            .resize(PtySize { rows, cols, pixel_width: cols * cw as u16, pixel_height: rows * ch as u16 })
            .expect("pty resize");
    }

    /// Wait for the program to exit; its exit code.
    pub fn wait_exit(&mut self) -> u32 {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.pump(Duration::from_millis(20));
            if let Ok(Some(status)) = self.child.try_wait() {
                self.pump(Duration::from_millis(50));
                return status.exit_code();
            }
            if Instant::now() >= deadline {
                let screen = self.screen();
                self.fail("waiting for the program to exit", &screen);
            }
        }
    }

    /// The pane as tmux sees it (`capture-pane -p`), for [`Term::inside_tmux`].
    pub fn capture_pane(&self) -> String {
        let tmux = self.tmux.as_ref().expect("not inside tmux");
        let out = tmux.cmd().args(["capture-pane", "-p"]).output().expect("tmux capture-pane");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.try_wait();
    }
}

fn color(c: StyleColor) -> Color {
    match c {
        StyleColor::None => Color::Default,
        StyleColor::Palette(i) => Color::Palette(i.0),
        StyleColor::Rgb(c) => Color::Rgb(c.r, c.g, c.b),
    }
}

/// `s` quoted for `sh`.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c)) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_output_colours_and_wide_characters() {
        let mut t = Term::spawn(&["sh", "-c", "printf 'a \\033[31mred\\033[0m 数式\\n'; sleep 5"], &Options::default());
        let s = t.wait_text("red");
        assert_eq!(s.lines()[0], "a red 数式");
        assert_eq!(s.cell(2, 0).fg, Color::Palette(1));
        assert!(s.cell(7, 0).spacer, "数 is wide");
        assert_eq!(s.cell(8, 0).text, "式");
    }

    #[test]
    fn keys_follow_the_programs_keyboard_mode() {
        // `od` shows what arrives: legacy C-h is 0x08; with the kitty keyboard
        // protocol on, `CSI 104;5u`.
        let script = "stty raw -echo; printf 'ready\\n'; head -c 1 | od -An -c; printf '\\033[>1u'; \
                      printf 'kitty\\n'; head -c 8 | od -An -c; sleep 5";
        let mut t = Term::spawn(&["sh", "-c", script], &Options::default());
        t.wait_text("ready");
        t.key("C-h");
        t.wait_text("kitty");
        t.key("C-h");
        let s = t.wait_for("the kitty encoding", |s| s.contains("u"));
        let text = s.text();
        assert!(text.contains("\\b"), "{text}");
        assert!(text.contains("033   [   1   0   4   ;   5   u"), "{text}");
    }

    #[test]
    fn exit_codes() {
        let mut t = Term::spawn(&["sh", "-c", "exit 3"], &Options::default());
        assert_eq!(t.wait_exit(), 3);
    }

    #[test]
    fn inside_tmux() {
        if !have_tmux() {
            eprintln!("SKIPPED: tmux not found");
            return;
        }
        let mut t = Term::inside_tmux(&["sh", "-c", "echo in-tmux; sleep 30"], &Options::default());
        t.wait_text("in-tmux");
        assert!(t.capture_pane().contains("in-tmux"));
    }
}
